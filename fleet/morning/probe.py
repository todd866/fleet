"""Read-only morning probe. Run with python3 on one Mac. Prints one JSON object.

No du, no find, no writes. Syncthing is a localhost GET. The API key stays in this process.
"""
import datetime
import fnmatch
import json
import os
import pathlib
import plistlib
import re
import shutil
import subprocess
import time
import urllib.error
import urllib.parse
import urllib.request
import xml.parsers.expat

try:
    import tomllib
except ImportError:  # pragma: no cover - Python <3.11 fallback
    tomllib = None

HOME = pathlib.Path.home()


def _site():
    """~/.config/fleet/site.json (see fleet/site.py); read here directly so the probe runs alone."""
    try:
        data = json.loads((HOME / ".config/fleet/site.json").read_text())
    except (OSError, ValueError):
        data = {}
    return data if isinstance(data, dict) else {}


def _drives_project():
    raw = str(_site().get("drives_project") or "~/Projects/drives")
    return HOME / raw[2:] if raw.startswith("~/") else pathlib.Path(raw)
# macOS volumes that share the boot disk's container. Their free space is the internal disk's,
# so none of them is a disk of its own.
SYSTEM_VOLUMES = frozenset((
    "Macintosh HD", "Data", "Recovery", "Preboot", "VM", "Update", "xarts", "iSCPreboot", "Hardware",
))


def main():
    print(json.dumps(_gather()))


def _gather():
    idrive = _idrive_reading()
    reading = {
        "disks": _disks(),
        "disks_authoritative": True,
        "glancebar": _json(HOME / ".local/state/glancebar-update/status.json"),
        "drive_mirror_epoch": _mtime(HOME / ".local/state/drive-mirror/last-ok"),
        "hdd_backup_epoch": _newest(HOME / ".local/state/hdd-backup", "last-success*"),
        "samples": _jsonl(HOME / ".local/state/machine-analytics" / f"{datetime.date.today().isoformat()}.jsonl", 400),
        "heavy": _jsonl(HOME / ".local/state/heavy-slot/jobs.jsonl", 200),
        "jobs": _jobs(),
        "mail": _mail(),
        "syncthing": _syncthing(),
        "owner_active": _owner_active(),
        "placement": _json(HOME / ".local/state/placement/status.json"),
        "placement_datasets": _placement_datasets(),
        "unclaimed": _json(HOME / ".local/state/placement/unclaimed.json"),
        "cloud": _json(HOME / ".local/state/placement/cloud.json"),
        "gdrive_backup": _gdrive_backup(),
        "coverage": _json(_drives_project() / "coverage.json"),
        "idrive_epoch": idrive.get("last_epoch"),
        "idrive": idrive,
        # The IDrive Linux client's daily backup, where this Mac hosts it (idrive-ingest-tick).
        "idrive_ingest": _json(HOME / ".local/state/idrive-ingest/status.json"),
        # Rented servers outside the fleet, where this Mac reads them (bin/server-health).
        "server_health": _json(HOME / ".local/state/fleet/server-health/status.json"),
        "data_map": _json(HOME / ".local/state/fleet/data-map/status.json"),
        # A laptop's copy of its home on the hub (home-mirror), where this Mac pushes one.
        "home_mirror": _json(HOME / ".local/state/home-mirror/status.json"),
        "offload_queue": _json(HOME / str(_site().get("offload_status_path") or ".local/state/offload/status.json")),
        # free-space-guard's last run on this Mac (storage/free-space-guard).
        "free_space_guard": _json(HOME / ".local/state/fleet/free-space-guard/status.json"),
        # mac-setup's last pass on this Mac: drift from the reference Mac's setup (bin/mac-setup).
        "mac_setup": _json(HOME / ".local/state/fleet/mac-setup/status.json"),
        "icloud": _icloud(),
        # Full Disk Access and the trees diskmap leaves unmeasured without it (diskmap summary).
        "diskmap_access": _diskmap_access(),
        "cool_c": _cool_target(),
        "queue": _queue(),
        "vpn_gateway": _vpn_gateway(),
    }
    if not (HOME / "Library/Application Support/IDriveforMac").exists() and not pathlib.Path("/Applications/IDrive.app").exists():
        reading.pop("idrive")
    return reading


def _disks():
    summary_path = HOME / ".local/state/diskmap/summary.json"
    summary = _json(summary_path)
    if isinstance(summary, dict) and summary.get("locations"):
        rows = _disks_from_summary(summary)
        # Cached diskmap sizes can predate a large download. Never bless them
        # with this probe's timestamp as current boot-volume free space.
        internal = next((row for row in rows if row["name"] == "internal"), None)
        try:
            free = shutil.disk_usage(HOME).free
        except OSError:
            rows = [row for row in rows if row["name"] != "internal"]
        else:
            if internal is None:
                internal = {"name": "internal", "volume_uuids": [], "trend_h24": None, "alert": None}
                rows.append(internal)
            internal.update(free_bytes=free, mounted=True, last_seen=int(time.time()))
        return rows
    mounts = _mounted_volumes()
    drives = _json(_drives_path()) or {}
    if mounts:
        return disks_from_mounts(drives, mounts, int(time.time()))
    return []


def _diskmap_access():
    summary = _json(HOME / ".local/state/diskmap/summary.json")
    access = summary.get("access") if isinstance(summary, dict) else None
    return access if isinstance(access, dict) else None


def _drives_path():
    return _drives_project() / "config/drives.json"


def _disks_from_summary(summary):
    by_name = {}
    for drive in summary.get("drives") or []:
        if isinstance(drive, dict) and drive.get("name"):
            by_name[drive["name"]] = [str(item) for item in drive.get("volume_uuids") or [] if item]
    rows = []
    for loc in summary.get("locations") or []:
        if not isinstance(loc, dict) or loc.get("kind") not in (None, "volume"):
            continue
        name = loc.get("name") or "disk"
        if name in SYSTEM_VOLUMES:
            continue
        uuids = list(by_name.get(name) or [])
        if loc.get("volume_uuid") and loc["volume_uuid"] not in uuids:
            uuids.append(loc["volume_uuid"])
        trend = loc.get("trend_bytes") or {}
        mounted = True if "mounted" not in loc else bool(loc.get("mounted"))
        if mounted and name != "internal" and name not in by_name:
            vol = pathlib.Path("/Volumes") / name
            if vol.exists() and _is_readonly_disk_image(_diskutil(vol)):
                continue
        rows.append({
            "name": name,
            "free_bytes": loc.get("free_bytes"),
            "trend_h24": trend.get("h24"),
            "alert": _alert(summary, loc),
            "mounted": mounted,
            "volume_uuids": uuids,
            "last_seen": loc.get("last_seen") or 0,
        })
    return rows


def disks_from_mounts(drives_doc, mounts, now):
    """Roll mounted volumes into one row per physical drive. Unmounted drives are omitted.

    Shared-container volumes are one pool (the larger free space). A sparsebundle
    is not added on top of the volume that holds it.
    """
    defs = []
    for drive in (drives_doc or {}).get("drives") or []:
        if not isinstance(drive, dict):
            continue
        vols = []
        for spec in (drive.get("volumes") or {}).values():
            if not isinstance(spec, dict) or not spec.get("uuid"):
                continue
            vols.append({
                "uuid": str(spec["uuid"]).upper(),
                "shared": bool(spec.get("shared_container")),
                "bundle": bool(spec.get("bundle")),
            })
        defs.append({
            "name": drive.get("name") or "disk",
            "vols": vols,
            "shared": any(vol["shared"] for vol in vols),
        })
    by_uuid = {}
    for mount in mounts or []:
        uuid = str(mount.get("uuid") or "").upper()
        if uuid:
            by_uuid[uuid] = mount
    rows = []
    claimed = set()
    for drive in defs:
        matched = []
        for vol in drive["vols"]:
            mount = by_uuid.get(vol["uuid"])
            if mount:
                claimed.add(vol["uuid"])
                matched.append((vol, mount))
        if not matched:
            continue
        if drive["shared"]:
            free = max(int(mount.get("free_bytes") or 0) for _, mount in matched)
        else:
            free = 0
            for vol, mount in matched:
                if vol["bundle"]:
                    continue
                free += int(mount.get("free_bytes") or 0)
        rows.append({
            "name": drive["name"],
            "free_bytes": free,
            "trend_h24": None,
            "alert": None,
            "mounted": True,
            "volume_uuids": [vol["uuid"] for vol in drive["vols"]],
            "last_seen": int(now),
        })
    for mount in mounts or []:
        uuid = str(mount.get("uuid") or "").upper()
        if uuid and uuid in claimed:
            continue
        name = "internal" if mount.get("internal") else (mount.get("name") or "disk")
        if name in SYSTEM_VOLUMES:
            continue
        rows.append({
            "name": name,
            "free_bytes": mount.get("free_bytes"),
            "trend_h24": None,
            "alert": None,
            "mounted": True,
            "volume_uuids": [uuid] if uuid else [],
            "last_seen": int(now),
        })
    return rows


def _mounted_volumes():
    rows = []
    data = pathlib.Path("/System/Volumes/Data")
    root = data if data.is_dir() else pathlib.Path("/")
    rows.append(_mount_row(root, "internal", internal=True))
    volumes = pathlib.Path("/Volumes")
    if not volumes.is_dir():
        return [row for row in rows if row]
    for child in sorted(volumes.iterdir()):
        if child.name.startswith(".") or child.name in SYSTEM_VOLUMES:
            continue
        try:
            mounted = child.is_mount()
        except OSError:
            continue
        if not mounted:
            continue
        row = _mount_row(child, child.name, internal=False)
        if row:
            rows.append(row)
    return [row for row in rows if row]


def _is_readonly_disk_image(info):
    proto = info.get("BusProtocol") or info.get("Protocol")
    if proto != "Disk Image":
        return False
    return bool(
        info.get("ReadOnly")
        or info.get("WritableVolume") is False
        or info.get("WritableMedia") is False
        or info.get("Writable") is False
    )


def _mount_row(path, name, internal):
    try:
        usage = shutil.disk_usage(path)
    except OSError:
        return None
    info = _diskutil(path)
    if not internal and _is_readonly_disk_image(info):
        return None
    return {
        "name": name,
        "uuid": info.get("VolumeUUID") or "",
        "free_bytes": usage.free,
        "total_bytes": usage.total,
        "internal": internal,
    }


def _diskutil(path):
    try:
        result = subprocess.run(
            ["diskutil", "info", "-plist", str(path)], capture_output=True, timeout=8,
        )
    except (OSError, subprocess.TimeoutExpired):
        return {}
    if result.returncode != 0 or not result.stdout:
        return {}
    try:
        return plistlib.loads(result.stdout)
    except (plistlib.InvalidFileException, ValueError):
        return {}


def _alert(summary, loc):
    for alert in summary.get("alerts") or []:
        if not isinstance(alert, dict):
            continue
        if alert.get("location") in (loc.get("name"), loc.get("kind")):
            return alert.get("message") or alert.get("cause")
    return None


def _idrive_hold():
    """When the owner has IDrive held on purpose (`idrive-hold on`): the date it was held from, else None."""
    tool = shutil.which("idrive-hold") or str(HOME / ".local/bin/idrive-hold")
    try:
        # errors="replace": the tool can echo other processes' arguments, which are not always
        # UTF-8; one stray byte must not take the whole probe down (15 Nov 2026).
        out = subprocess.run([tool, "status"], capture_output=True, text=True, errors="replace",
                             timeout=10).stdout
    except (OSError, subprocess.SubprocessError):
        return None
    found = re.search(r"^HELD: held from (\d{4}-\d{2}-\d{2}(?: \d{2}:\d{2}:\d{2})?)", out, re.M)
    return found.group(1) if found else None


def _idrive_reading():
    reading = _idrive_summary()
    held = _idrive_hold()
    if held:
        reading["held_since"] = held
    return reading


def _idrive_summary():
    """Latest completed local attempt, not a rounded age from a cached dashboard.

    Only read one bounded summary, never filenames from the backup inventory.
    A recent partial/failed backup remains a failure, not a fresh green stamp.
    """
    root = HOME / "Library/Application Support/IDriveforMac/SessionLogsNew/Backup"
    latest = None
    stamp = 0
    try:
        for path in root.glob("*.summary"):
            observed = path.stat().st_mtime
            if observed > stamp:
                latest, stamp = path, observed
        if latest is None:
            return {"last_epoch": None, "error": "no local IDrive summary"}
        with latest.open(errors="replace") as handle:
            text = handle.read(65536)
    except OSError:
        return {"last_epoch": None, "error": "IDrive summary unreadable"}
    failed = re.search(r"Files failed to backup:\s*(\d+)", text)
    ended = re.search(r"End Time:\s*([^\r\n]+)", text)
    # Successful summaries omit the failure line. Accept omission only when
    # the considered count reconciles with already-present plus backed-up files.
    failed_count = int(failed.group(1)) if failed else None
    if failed_count is None:
        counts = [re.search(label + r":\s*(\d+)", text) for label in (
            "Files considered for backup", "Files already present in your account", "Files backed up now")]
        if all(counts) and int(counts[0].group(1)) == sum(int(item.group(1)) for item in counts[1:]):
            failed_count = 0
    if failed_count is None or not ended or stamp > time.time() + 300:
        return {"last_epoch": None, "error": "IDrive summary incomplete or future-dated"}
    return {"last_epoch": int(stamp), "failed": failed_count, "source": "local-summary"}


def _idrive_epoch():
    return _idrive_reading().get("last_epoch")


def _icloud():
    try:
        result = subprocess.run(["/usr/bin/brctl", "status"], capture_output=True, text=True, timeout=8)
    except (OSError, subprocess.TimeoutExpired):
        return None
    raw = result.stdout or ""
    if not raw.strip():
        return None
    return {"pending_upload": raw.count("needs-sync-up"), "last_sync_epoch": None}


def _jobs():
    listed = _launchctl()
    rows = []
    agents = HOME / "Library/LaunchAgents"
    if not agents.is_dir():
        return rows
    specs = []
    labels = list(_site().get("job_label_prefixes") or []) + ["syncthing", "example"]
    for path in sorted(agents.glob("*.plist")):
        if not any(token in path.name for token in labels):
            continue
        try:
            with path.open("rb") as handle:
                plist = plistlib.load(handle)
        except (OSError, plistlib.InvalidFileException, xml.parsers.expat.ExpatError):
            continue
        label = plist.get("Label") or path.stem
        specs.append((label, plist))
    printed = _launchctl_print([label for label, _plist in specs])
    for label, plist in specs:
        info = printed.get(label) or {}
        known = label in listed or label in printed
        logs = [info.get("stdout"), info.get("stderr"), plist.get("StandardOutPath"), plist.get("StandardErrorPath")]
        pid = (listed.get(label) or {}).get("pid")
        uptime_s = _pid_uptime_s(pid) if (info.get("state") == "running" and pid and str(pid).isdigit()) else None
        row = {
            "label": label,
            "loaded": known,
            "exit": _exit_code(info, listed.get(label), known),
            "runs": info.get("runs"),
            "state": info.get("state") or "",
            "last_epoch": _activity(logs),
            "when": _when(plist),
            "cmd": " ".join(str(part) for part in (plist.get("ProgramArguments") or [])),
        }
        if uptime_s is not None:
            row["uptime_s"] = uptime_s
        if _utc_daily(plist):
            # The plist ticks every few minutes; launchd's exit code is the last tick's, almost
            # always a quiet 0. The wrapper's own record is the job's real last run.
            row["utc"] = True
            last = _utc_last(label)
            run = _utc_running(label)
            if run == "running":
                # launchd forgets a job's state when it is reloaded; the wrapper's own record
                # of a live run does not.
                row["state"] = "running"
            if last:
                row["last_epoch"] = last["start"]
                if row["state"] != "running":
                    row["exit"] = last["rc"]
            if run == "retry" and row["state"] != "running":
                # Cut short from outside (a restart); the wrapper runs it once more at its next tick.
                row["retry_pending"] = True
        rows.append(row)
    return rows


def _exit_code(printed, listed, known):
    """Last exit from `launchctl print`. The list status is the fallback, never the PID column."""
    if printed.get("exit") is not None:
        return printed["exit"]
    status = (listed or {}).get("status")
    if not known:
        return None
    if status in ("", "-", None):
        return 0
    try:
        return int(status)
    except (TypeError, ValueError):
        return status


def _activity(paths):
    """Newest mtime of the job's log or of files in its own state directory.

    A log that lives directly in `~/.local/state` does not pull in every other
    state file. Empty logs still count, and a fresh summary.json next to them
    counts too.
    """
    times = []
    home = HOME.resolve()
    state_root = (HOME / ".local/state").resolve()
    parents = []
    for raw in paths:
        if not raw:
            continue
        path = pathlib.Path(str(raw)).expanduser()
        try:
            if path.is_file():
                times.append(path.stat().st_mtime)
                parents.append(path.parent)
            elif path.is_dir():
                parents.append(path)
        except OSError:
            continue
    seen = set()
    for directory in parents:
        try:
            resolved = directory.resolve()
        except OSError:
            continue
        if resolved in seen or resolved == state_root or not directory.is_dir():
            continue
        if resolved != home and home not in resolved.parents:
            continue
        seen.add(resolved)
        try:
            children = list(directory.iterdir())
        except OSError:
            continue
        for child in children:
            try:
                # A job whose only output is a dated folder (backups/2026-11-14/) has run too.
                if child.is_file() or child.is_dir():
                    times.append(child.stat().st_mtime)
            except OSError:
                continue
    return int(max(times)) if times else None


def _launchctl_print(labels):
    """Per-label print. The domain print is only a PID/status table, not runs or last exit."""
    uid = os.getuid()
    jobs = {}
    for label in labels:
        try:
            result = subprocess.run(
                ["launchctl", "print", f"gui/{uid}/{label}"],
                capture_output=True, text=True, timeout=3,
            )
        except (OSError, subprocess.TimeoutExpired):
            continue
        if result.returncode != 0:
            continue
        jobs.update(parse_launchctl_print(result.stdout or ""))
    return jobs


def parse_launchctl_print(text):
    """`runs`, `last exit code`, and `state` for each gui service. No PID column."""
    jobs = {}
    lines = text.splitlines()
    index = 0
    header = re.compile(r"^[^/\s]+/\d+/(\S+) = \{$")
    while index < len(lines):
        match = header.match(lines[index])
        if not match:
            index += 1
            continue
        label = match.group(1)
        depth = 1
        body = []
        index += 1
        while index < len(lines) and depth:
            line = lines[index]
            depth += line.count("{") - line.count("}")
            if depth > 0:
                body.append(line)
            index += 1
        jobs[label] = _print_fields(body)
    return jobs


def _print_fields(body):
    fields = {}
    for line in body:
        if not line.startswith("\t") or line.startswith("\t\t") or " = " not in line:
            continue
        key, value = line[1:].split(" = ", 1)
        fields[key] = value
    exit_text = fields.get("last exit code")
    exit_code = None
    if exit_text not in (None, "", "-") and "never" not in exit_text:
        try:
            exit_code = int(exit_text)
        except ValueError:
            exit_code = None
    runs = None
    if fields.get("runs") not in (None, ""):
        try:
            runs = int(fields["runs"])
        except ValueError:
            runs = None
    return {
        "state": fields.get("state") or "",
        "runs": runs,
        "exit": exit_code,
        "stdout": fields.get("stdout path"),
        "stderr": fields.get("stderr path"),
    }


def _parse_etime(text):
    """Parse `ps -o etime=` (`[[dd-]hh:]mm:ss`) into seconds, or None."""
    s = (text or "").strip()
    if not s:
        return None
    days = 0
    if "-" in s:
        d, s = s.split("-", 1)
        if not d.isdigit():
            return None
        days = int(d)
    parts = s.split(":")
    if not (2 <= len(parts) <= 3) or not all(p.isdigit() for p in parts):
        return None
    nums = [int(p) for p in parts]
    if len(nums) == 2:
        hours, mins, secs = 0, nums[0], nums[1]
    else:
        hours, mins, secs = nums
    return days * 86400 + hours * 3600 + mins * 60 + secs


def _pid_uptime_s(pid):
    try:
        pid_int = int(pid)
    except (TypeError, ValueError):
        return None
    if pid_int <= 0:
        return None
    try:
        res = subprocess.run(["/bin/ps", "-o", "etime=", "-p", str(pid_int)], capture_output=True, text=True, timeout=3)
    except (OSError, subprocess.TimeoutExpired):
        return None
    if res.returncode != 0:
        return None
    return _parse_etime(res.stdout)


def _utc_daily(plist):
    """Options of the utc-daily wrapper when the job runs through it, else None."""
    args = [str(part) for part in (plist.get("ProgramArguments") or [])]
    if not any(pathlib.Path(part).name == "utc-daily" for part in args[:2]):
        return None
    head = args[:args.index("--")] if "--" in args else args
    slots = sorted(head[i + 1] for i, part in enumerate(head[:-1]) if part == "--at")
    weekday = next((head[i + 1] for i, part in enumerate(head[:-1]) if part == "--weekday"), None)
    return {"slots": slots, "weekday": weekday} if slots else None


def _utc_last(label):
    path = HOME / ".local/state/utc-daily" / label / "last"
    try:
        start, _end, _slot, rc = path.read_text().split()
        return {"start": int(start), "rc": int(rc)}
    except (OSError, ValueError):
        return None


def _utc_running(label):
    """The wrapper's `running` file: "running" for a live run, "retry" for a run that was cut
    short and is owed one more try, None when idle. Fields: slot, date, pid, boot time, attempt."""
    path = HOME / ".local/state/utc-daily" / label / "running"
    try:
        fields = path.read_text().split()
    except OSError:
        return None
    if len(fields) < 4:
        return None
    pid, boot = fields[2], fields[3]
    if pid == "-":
        return "retry"
    if pid.isdigit() and boot == _boot_time() and _pid_alive(int(pid)):
        return "running"
    return None


def _boot_time():
    try:
        result = subprocess.run(["/usr/sbin/sysctl", "-n", "kern.boottime"], capture_output=True, text=True, timeout=3)
    except (OSError, subprocess.TimeoutExpired):
        return ""
    match = re.search(r"sec = (\d+)", result.stdout or "")
    return match.group(1) if match else ""


def _pid_alive(pid):
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except OSError:
        return True
    return True


def _cool_target():
    """heavy-slot's own `cool` file: the chip temperature this Mac's slot policy waits for.

    Same file, same first-number reading and same "0 or missing means no gate" as heavy-slot.
    """
    folder = os.environ.get("HEAVY_SLOT_DIR") or HOME / ".local/state/heavy-slot"
    try:
        first = (pathlib.Path(folder) / "cool").read_text().splitlines()[0].split()[0]
        value = int(float(first))
    except (OSError, IndexError, ValueError):
        return None
    return value if value > 0 else None


def _queue():
    """Attempts that hold the job queue on this Mac, from the queue's own summary. None: no queue here."""
    summary = _json(HOME / ".local/state/fleet/jobs/summary.json")
    if not isinstance(summary, dict):
        return None
    stalls = [row for row in summary.get("dispatch_stalls") or [] if isinstance(row, dict)]
    return {"observed_at": summary.get("observed_at"), "stalls": stalls}


def _when(plist):
    wrapped = _utc_daily(plist)
    if wrapped:
        # Times are UTC. The evaluator is told so by the job's "utc" flag.
        return ("weekly " if wrapped["weekday"] is not None else "daily ") + wrapped["slots"][0]
    if "StartCalendarInterval" in plist:
        raw = plist["StartCalendarInterval"]
        slot = raw[0] if isinstance(raw, list) and raw else raw
        if not isinstance(slot, dict):
            return ""
        hour = slot.get("Hour")
        minute = int(slot.get("Minute") or 0)
        if "Month" in slot and "Day" in slot:
            prefix = "yearly "       # a one-shot date; never judged as a missed daily run
        elif "Weekday" in slot:
            prefix = "weekly "
        elif hour is not None:
            prefix = "daily "
        else:
            prefix = "hourly "
        clock = f"{int(hour):02d}:{minute:02d}" if hour is not None else f":{minute:02d}"
        return prefix + clock
    if "StartInterval" in plist:
        seconds = int(plist["StartInterval"])
        if seconds >= 3600 and seconds % 3600 == 0:
            return f"every {seconds // 3600}h"
        if seconds >= 60 and seconds % 60 == 0:
            return f"every {seconds // 60}m"
        return f"every {seconds}s"
    if plist.get("KeepAlive"):
        return "always on"
    return "at login"


def _launchctl():
    try:
        result = subprocess.run(["launchctl", "list"], capture_output=True, text=True, timeout=8)
    except (OSError, subprocess.TimeoutExpired):
        return {}
    found = {}
    for line in result.stdout.splitlines():
        parts = line.split("\t")
        if len(parts) >= 3:
            found[parts[2]] = {"pid": parts[0], "status": parts[1]}
    return found


def _mail():
    inbox = HOME / ".local/state/agent-mail/inbox.jsonl"
    if not inbox.is_file():
        return None
    read_ids = set()
    for name in ("read.txt",):
        path = inbox.parent / name
        try:
            read_ids.update(line.strip() for line in path.read_text().splitlines() if line.strip())
        except OSError:
            pass
    notes = []
    for row in _jsonl(inbox, 2000):
        ident = row.get("id")
        if ident and ident in read_ids:
            continue
        at = _mail_time(row.get("at"))
        if at is None:
            continue
        notes.append({"at": at, "read": False, "id": ident})
    return notes


def _mail_time(text):
    if isinstance(text, (int, float)):
        return int(text)
    try:
        return int(time.mktime(time.strptime(str(text), "%Y-%m-%d %H:%M:%S")))
    except (TypeError, ValueError):
        return None


def _syncthing():
    xml = None
    for path in (
        HOME / "Library/Application Support/Syncthing/config.xml",
        HOME / ".config/syncthing/config.xml",
    ):
        try:
            xml = path.read_text(errors="replace")
            break
        except OSError:
            continue
    if not xml:
        return None
    match = re.search(r"<apikey>([^<]+)</apikey>", xml)
    if not match:
        return {"errors": 0, "out_of_sync": 0, "receive_only_local": 0, "down": True}
    key = match.group(1).strip()

    def get(path):
        request = urllib.request.Request(
            "http://127.0.0.1:8384" + path, headers={"X-API-Key": key})
        with urllib.request.urlopen(request, timeout=2) as response:
            return json.loads(response.read().decode())

    try:
        errors = get("/rest/system/error").get("errors") or []
        folders = get("/rest/config/folders") or []
    except (OSError, ValueError, TimeoutError):
        return {"errors": 0, "out_of_sync": 0, "receive_only_local": 0, "down": True}
    err_n = len(errors)
    out_of_sync = 0
    local = 0
    for folder in folders[:40]:
        if not isinstance(folder, dict) or not folder.get("id"):
            continue
        try:
            status = get("/rest/db/status?folder=" + urllib.parse.quote(str(folder["id"])))
        except (OSError, ValueError, TimeoutError):
            err_n += 1
            continue
        if status.get("errors") or status.get("pullErrors") or status.get("error") or status.get("state") == "error":
            err_n += 1
        if int(status.get("needBytes") or 0) > 0:
            out_of_sync += 1
        kind = folder.get("type") or ""
        changed = int(status.get("receiveOnlyChangedBytes") or 0) + int(status.get("receiveOnlyChangedFiles") or 0)
        if kind in ("receiveonly", "receiveencrypted") and changed > 0:
            local += 1
    return {"errors": err_n, "out_of_sync": out_of_sync, "receive_only_local": local, "down": False}


def _owner_active():
    stamp = _mtime(HOME / ".local/state/focus-guard/last-active")
    if stamp and int(time.time()) - stamp < 600:
        return True
    return (HOME / ".local/state/focus-guard/active").exists()


def _newest(root, pattern):
    if not root.is_dir():
        return None
    stamps = [int(path.stat().st_mtime) for path in root.glob("**/" + pattern) if path.is_file()]
    return max(stamps) if stamps else None


def _jsonl(path, limit):
    try:
        lines = path.read_text(errors="replace").splitlines()[-limit:]
    except OSError:
        return []
    rows = []
    for line in lines:
        try:
            rows.append(json.loads(line))
        except ValueError:
            continue
    return rows


def _expand_home(raw, home):
    text = str(raw or "").strip()
    if text == "~":
        return str(home)
    if text.startswith("~/"):
        return str(home / text[2:])
    return text


def _pack_pruner(src, home):
    """Match gdrive-backup's exclude/exclude_from rules when expanding parent/* packs."""
    pats = []
    lines = []
    ex_from = src.get("exclude_from")
    if ex_from:
        ex_path = pathlib.Path(_expand_home(ex_from, home))
        try:
            lines = ex_path.read_text(errors="replace").splitlines()
        except OSError:
            lines = []
    lines += [str(e).replace("/**", "/") for e in (src.get("exclude") or [])]
    for line in lines:
        if not line or line.startswith("#"):
            continue
        anchored = line.startswith("/")
        cleaned = line.strip("/").replace("**", "*")
        pats.append((anchored, cleaned))
    exact_rel = {p for a, p in pats if a and not any(c in p for c in "*?[")}
    exact_name = {p for a, p in pats if not a and not any(c in p for c in "*?[") and "/" not in p}
    globs = [(a, p) for a, p in pats if any(c in p for c in "*?[") or (not a and "/" in p)]

    def skip(rel):
        base = os.path.basename(rel)
        if rel in exact_rel or base in exact_name:
            return True
        for anchored, pat in globs:
            if anchored and fnmatch.fnmatchcase(rel, pat):
                return True
            if not anchored and (fnmatch.fnmatchcase(base, pat) or fnmatch.fnmatchcase(rel, "*/" + pat)):
                return True
        return False

    return skip


def _gdrive_backup(home=None):
    """Encrypted Google Drive backup receipts and configured sources/packs on this Mac."""
    home = pathlib.Path(home) if home is not None else HOME
    receipts = _json(home / ".local/state/gdrive-backup/receipts.json")
    if not isinstance(receipts, dict) or not receipts:
        return None
    site_cfg = _json(home / ".config/fleet/gdrive-backup.json")
    if not isinstance(site_cfg, dict):
        site_cfg = {}
    raw_sources = site_cfg.get("sources")
    if not isinstance(raw_sources, list) or not raw_sources:
        ingest_cfg = _json(home / ".config/fleet/idrive-ingest.json")
        raw_sources = ingest_cfg.get("sources") if isinstance(ingest_cfg, dict) else []
    sources = []
    for src in raw_sources or []:
        if not isinstance(src, dict):
            continue
        root = _expand_home(src.get("path"), home).rstrip("/")
        if not root:
            continue
        name = str(src.get("name") or os.path.basename(root))
        masks = []
        for item in list(src.get("mask") or []) + list(src.get("masks") or []):
            val = item.get("path", "") if isinstance(item, dict) else str(item)
            val = str(val).strip("/")
            if val:
                masks.append(val)
        for item in (site_cfg.get("extra_masks") or {}).get(name) or []:
            val = str(item).strip("/")
            if val:
                masks.append(val)
        mask_set = set(masks)
        skip = _pack_pruner(src, home)
        packs = set()
        raw_packs = list(src.get("pack") or []) + list((site_cfg.get("extra_packs") or {}).get(name) or [])
        for item in raw_packs:
            p = (item.get("path", "") if isinstance(item, dict) else str(item)).strip("/")
            if not p:
                continue
            if p.endswith("/*"):
                parent = p[:-2]
                parent_abs = os.path.join(root, parent)
                try:
                    kids = sorted(os.listdir(parent_abs))
                except OSError:
                    kids = []
                for kid in kids:
                    rel = f"{parent}/{kid}"
                    full = os.path.join(root, rel)
                    if os.path.isdir(full) and not os.path.islink(full) and rel not in mask_set and not skip(rel):
                        packs.add(rel)
            else:
                packs.add(p)
        prefix = f"{name}:pack:"
        for key in receipts:
            if isinstance(key, str) and key.startswith(prefix):
                rel = key[len(prefix):].strip("/")
                if rel:
                    packs.add(rel)
        sources.append({
            "name": name,
            "path": root,
            "masks": sorted(mask_set),
            "packs": sorted(packs),
        })
    return {"home": str(home), "sources": sources, "receipts": receipts}


def _placement_datasets(home=None):
    """Dataset definitions (name, class, paths, holders) from placement.toml."""
    if tomllib is None:
        return None
    home = pathlib.Path(home) if home is not None else HOME
    try:
        cfg = tomllib.loads((home / ".config/fleet/placement.toml").read_text())
    except (OSError, ValueError):
        return None
    rows = []
    for ds in cfg.get("dataset") or []:
        if not isinstance(ds, dict) or not ds.get("name"):
            continue
        paths = [str(p).strip() for p in (ds.get("paths") or []) if isinstance(p, str) and p.strip()]
        keep = [str(h).strip() for h in (ds.get("keep") or []) if isinstance(h, str) and h.strip()]
        replicas = [str(v).strip() for v in (ds.get("replica_volumes") or []) if isinstance(v, str) and v.strip()]
        rows.append({
            "name": str(ds["name"]),
            "class": str(ds.get("class") or ""),
            "paths": paths,
            "master_host": str(ds.get("master_host") or ""),
            "master_volume": str(ds.get("master_volume") or ""),
            "replica_volumes": replicas,
            "keep": keep,
        })
    return rows or None


def _vpn_gateway():
    """Probe the local split-tunnel VPN gateway (PAC, Gluetun exit IP, Transmission UI) without touching secrets."""
    has_pac = any((HOME / "Library/LaunchAgents").glob("*.vpn-pac.plist"))
    has_domains = (HOME / ".config/fleet/vpn-domains.txt").is_file()
    if not (has_pac or has_domains):
        return None
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))

    def get_json(url):
        try:
            with opener.open(url, timeout=2) as resp:
                if getattr(resp, "status", 200) != 200:
                    return None
                return json.loads(resp.read().decode("utf-8", "replace"))
        except (OSError, ValueError, urllib.error.URLError):
            return None

    pac = get_json("http://127.0.0.1:8899/health")
    pac_ok = isinstance(pac, dict) and bool(pac.get("ok"))
    domains = 0
    if isinstance(pac, dict):
        raw_count = pac.get("domains_count")
        if raw_count is not None:
            try:
                domains = int(raw_count)
            except (TypeError, ValueError):
                domains = 0
        elif isinstance(pac.get("domains"), list):
            domains = len(pac["domains"])
        else:
            try:
                domains = int(pac.get("domains") or 0)
            except (TypeError, ValueError):
                domains = 0

    gluetun = get_json("http://127.0.0.1:8890/v1/publicip/ip")
    vpn_ip = str(gluetun.get("public_ip") or "").strip() if isinstance(gluetun, dict) else ""
    vpn_country = str(gluetun.get("country") or "").strip() if isinstance(gluetun, dict) else ""
    vpn_ok = bool(vpn_ip)

    transmission_ok = False
    try:
        with opener.open("http://127.0.0.1:9091/transmission/web/", timeout=2) as resp:
            transmission_ok = getattr(resp, "status", 200) in (200, 301, 302, 401, 409)
    except urllib.error.HTTPError as exc:
        transmission_ok = exc.code in (301, 302, 401, 409)
        exc.close()
    except (OSError, urllib.error.URLError):
        transmission_ok = False

    return {
        "pac_ok": pac_ok,
        "domains": domains,
        "vpn_ok": vpn_ok,
        "vpn_ip": vpn_ip,
        "vpn_country": vpn_country,
        "transmission_ok": transmission_ok,
    }


def _json(path):
    try:
        return json.loads(path.read_text())
    except (OSError, ValueError):
        return None


def _mtime(path):
    try:
        return int(path.stat().st_mtime)
    except OSError:
        return None


if __name__ == "__main__":
    main()
