"""Collect this Mac's readings. Other Macs arrive as snapshot files, never over SSH.

The probe reads known state files and, for Syncthing only, localhost REST.
It does not walk the disk with du or find, and it does not open a connection
to another machine. A snapshot older than 3 h is stale; that judgement is
in evaluate, from `collected_at`.
"""
import json
import math
import os
import shutil
import subprocess
import time
from datetime import datetime
from pathlib import Path

from fleet import site
from fleet.morning.discover import UNREADABLE, discover
from fleet.morning.snapshot import merge_snapshots, read_snapshots, snapshot_dir

_HOST = __import__("re").compile(r"^[A-Za-z0-9._-]+$")


def collect(config, *, here, now, state=None, run=None, home=None, gather=None, snapshots=None):
    """Digest snapshot: every pushed file, plus this Mac's live readings.

    `state` is the digest directory the caller writes. Freshness comes from
    each snapshot's collected_at, not from a previous summary.
    """
    del state
    run = run or subprocess.run
    home = Path(home or Path.home())
    folder = Path(snapshots) if snapshots else snapshot_dir(home)
    live = collect_local(config, here=here, now=now, home=home, run=run, gather=gather)
    merged = merge_snapshots(read_snapshots(folder), live)
    merged["placement"] = _placement(home, {})
    merged["coverage"] = _coverage(home, {})
    merged["capacity"] = _capacity(home, config, now, run)
    merged["board"] = _board(home, run)
    merged["agentkit"] = _agentkit(run)
    merged["upstream"] = _upstream(home)
    merged.update(_hub_context(home, config))
    merged["job_reviews"] = job_reviews(
        Path(config.get("registry") or Path(home) / ".local/state/mac-sync/registry"))
    return merged


def _hub_context(home, config):
    """Site facts the evaluator needs beside the readings. Read on the Mac that writes the digest,
    so a snapshot pushed by an older collector is judged by today's registry and targets."""
    context = {"disk_targets": _disk_targets(home, config)}
    registered = _registered_jobs(Path(config.get("registry") or Path(home) / ".local/state/mac-sync/registry"))
    if registered is not None:
        context["registry_jobs"] = registered
    return context


def job_reviews(registry_dir):
    """Each job's project and next review date, from the registry's jobs.tsv.

    Columns are label, home Mac, what it does, project, review date. Older lines carry
    only the first three. None when the file cannot be read.
    """
    try:
        lines = (Path(registry_dir) / "jobs.tsv").read_text().splitlines()
    except OSError:
        return None
    rows = []
    for line in lines:
        if not line.strip() or line.startswith("#"):
            continue
        field = (line.split("\t") + [""] * 5)[:5]
        rows.append({"label": field[0].strip(), "project": field[3].strip(), "review": field[4].strip()})
    return rows


def collect_local(config, *, here, now, home, run=None, gather=None):
    """Readings for this Mac only: disk, backups, jobs, sync, updates, mail, heavy-slot, local git."""
    run = run or subprocess.run
    home = Path(home)
    here = str(here or "")
    if not _HOST.fullmatch(here):
        raise ValueError(f"bad host id: {here}")
    raw = (gather or _default_gather)()
    if not isinstance(raw, dict):
        raw = {}
    registry = _registry(home, config)
    health = _read_json(home / ".local" / "state" / "fleet" / "health.json") or {}
    registered = _registered_jobs(Path(config.get("registry") or home / ".local/state/mac-sync/registry"))
    reading = _host_snapshot(here, raw, registry.get(here) or {}, config, health, now, registered=registered)
    return {
        "version": 1,
        "host": here,
        "collected_at": int(now),
        "reading": reading,
        "projects": _projects_here(config, home, here, reading, run, now),
    }


def _default_gather():
    from fleet.morning.probe import _gather
    return _gather()


def _projects_here(config, home, here, reading, run, now=None):
    """Git and CI for repos checked out on this Mac, and the result file of the repo's own
    check when its `.fleet/check` names one. No remote-run, no SSH, and no check is started."""
    projects_root = Path(config.get("projects_dir") or home / "Projects")
    registry_dir = Path(config.get("registry") or home / ".local/state/mac-sync/registry")
    if not registry_dir.is_dir() and not projects_root.is_dir():
        return []
    rows = []
    shaped = {"hosts": {here: reading}}
    for spec in discover(registry_dir, projects_root):
        path = projects_root / spec.name
        if not (path / ".git").exists():
            continue
        git = _git(path, run) or {"dirty": False, "ahead": 0, "behind": 0, "unpushed": 0, "remote": ""}
        rows.append({
            "name": spec.name,
            "host": here,
            "git": git,
            "ci": _ci(path, git.get("remote") or "", run),
            "jobs": _project_jobs(shaped, spec.name),
            "check_declared": bool(spec.result),
            "check_result": _safe_check_result(spec, path, home, here, now, config),
        })
    return rows


CHECK_STATUSES = ("pass", "warn", "fail")


def _safe_check_result(spec, repo, home, here, now, config):
    """A repo's result file is someone else's data. Whatever is in it, this Mac's snapshot is
    still written: anything unforeseen is a warn row, not a crash of the collector."""
    try:
        return _check_result(spec, repo, home, here, now, config)
    except Exception as error:  # noqa: BLE001 - the hourly snapshot must not die on one repo's file
        return _check_warn(here, "unreadable", "result", f"{spec.result}: {type(error).__name__}: {error}")


def _check_result(spec, repo, home, here, now, config):
    """What the repo's own check last wrote, as the `check_result` the digest reads.

    The file is JSON: `status` (pass, warn or fail), `value`, `datum`, `detail`, and `generated`
    (epoch seconds; the file's mtime when absent). A file this Mac does not have is None: the
    check may run on another Mac, and the digest says so only when no Mac has it. A file that
    cannot be read, that is older than `max_age_h`, or that is dated in the future, is a warn,
    never a pass.
    """
    if not spec.result:
        return None
    if spec.result == UNREADABLE:
        return _check_warn(here, "unreadable", ".fleet/check", f"{repo / '.fleet' / 'check'} does not parse")
    path = _result_path(spec.result, repo, home)
    try:
        if not path.is_file():
            return None
        mtime = path.stat().st_mtime
        data = json.loads(path.read_text())
    except (OSError, ValueError) as error:
        return _check_warn(here, "unreadable", "result", f"{spec.result}: {error}")
    if not isinstance(data, dict) or data.get("status") not in CHECK_STATUSES:
        return _check_warn(here, "unreadable", "result", f"{spec.result}: no status of pass, warn or fail")
    if now is None:
        now = time.time()
    generated = data.get("generated")
    if (isinstance(generated, bool) or not isinstance(generated, (int, float))
            or not math.isfinite(generated) or generated <= 0):
        generated = mtime
    if generated > now + FUTURE_SLACK_S:
        # A clock that is ahead can never go stale, so it is not believed.
        return _check_warn(here, "unreadable", "result", f"{spec.result}: dated in the future")
    limit = spec.max_age_h or _positive_float((config.get("projects") or {}).get("check_max_age_h"), 30.0)
    status = data["status"]
    detail = str(data.get("detail") or "")
    if now - generated > limit * 3600:
        age_h = (now - generated) / 3600
        age = f"{age_h:.0f} h" if age_h < 48 else f"{age_h / 24:.0f} d"
        since = datetime.fromtimestamp(generated).astimezone().strftime("%-d %b %H:%M")
        last = f"last result {status}" + (f": {detail}" if detail else "")
        stale = _check_warn(here, "stale", f"since {since}", f"result written {age} ago; limit {limit:g} h; {last}")
        stale["generated"] = int(generated)
        return stale
    return {
        "status": status,
        "value": str(data.get("value") or status),
        "datum": str(data.get("datum") or "check"),
        "detail": detail,
        "generated": int(generated),
        "host": here,
    }


FUTURE_SLACK_S = 300


def _positive_float(value, default):
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        return default
    return float(value) if math.isfinite(value) and value > 0 else default


def _check_warn(here, value, datum, detail):
    return {"status": "warn", "value": value, "datum": datum, "detail": detail, "generated": 0, "host": here}


def _result_path(result, repo, home):
    """`~/…` is under this run's home, a relative path is inside the repo."""
    if result == "~" or result.startswith("~/"):
        return Path(home) / result[2:]
    path = Path(result)
    return path if path.is_absolute() else Path(repo) / path


def _host_snapshot(name, raw, registry_host, config, health, now, registered=None):
    health_host = ((health.get("hosts") or {}).get(name) or {}) if isinstance(health, dict) else {}
    disks = raw.get("disks")
    if not disks and not raw.get("disks_authoritative"):
        disks = _disks_from_summary((health_host.get("diskmap") or {}))
    row = {"ok": True, "disks": disks or [], "owner_active": bool(raw.get("owner_active"))}
    if not row["disks"] and raw.get("disk_error"):
        row["disk_error"] = raw["disk_error"]
    backups = config.get("backups") or {}
    idrive = raw.get("idrive_epoch")
    if idrive is None and "idrive" not in raw:
        latest = ((health_host.get("idrive") or {}).get("latest") or {})
        if latest.get("observed_mtime"):
            idrive = int(latest["observed_mtime"])
    if "idrive" in raw or name in (backups.get("idrive_hosts") or []) or idrive is not None or _has_kind(health_host, "idrive"):
        row["idrive"] = raw.get("idrive") if "idrive" in raw else (None if idrive is None else {"last_epoch": idrive})
    for key in ("idrive_ingest", "home_mirror"):
        if isinstance(raw.get(key), dict):
            row[key] = raw[key]
    if name in (backups.get("mirror_hosts") or []):
        row["drive_mirror_epoch"] = raw.get("drive_mirror_epoch")
    hdd = raw.get("hdd_backup_epoch")
    if hdd is None:
        stamps = list((health_host.get("backup_stamps") or {}).values())
        hdd = max(stamps) if stamps else None
    if name in (backups.get("hdd_hosts") or []) or hdd is not None:
        row["hdd_backup_epoch"] = hdd
    icloud = raw.get("icloud")
    if icloud is None and name in (backups.get("icloud_hosts") or []):
        live = health_host.get("cloud_live") or {}
        if live:
            icloud = {
                "pending_upload": live.get("pending_upload_records") or 0,
                "last_sync_epoch": None,
            }
        else:
            icloud = None
            row["icloud"] = None
    if icloud is not None:
        row["icloud"] = icloud
    if raw.get("syncthing") is not None or name in (backups.get("syncthing_hosts") or []):
        if raw.get("syncthing") is not None:
            row["syncthing"] = raw["syncthing"]
    glance = _glancebar(raw.get("glancebar"))
    row["glancebar"] = glance or {"behind": None, "release": None, "installed": None}
    row["jobs"] = _merge_jobs(registry_host.get("jobs") or {}, raw.get("jobs") or [], registered=registered, here=name)
    if raw.get("cool_c") is not None:
        row["cool_c"] = raw["cool_c"]
    if raw.get("queue") is not None:
        row["queue"] = raw["queue"]
    if raw.get("samples"):
        row["samples"] = raw["samples"]
    if raw.get("heavy"):
        row["heavy"] = raw["heavy"]
    if raw.get("mail") is not None:
        row["mail"] = raw["mail"]
    return row


def local_host_id(home, local_name=None):
    """Fleet id for this Mac: diskmap summary, then the host map in diskmap's config.

    A Mac in neither is known by its LocalHostName. The ids themselves are site values.
    """
    home = Path(home)
    summary = _read_json(home / ".local/state/diskmap/summary.json")
    if isinstance(summary, dict) and summary.get("host_id"):
        return str(summary["host_id"])
    cfg = _read_json(home / ".config/diskmap/config.json") or {}
    name = local_name if local_name is not None else _local_name()
    mapped = ((cfg.get("hosts") or {}).get(name) or {}).get("id")
    if mapped:
        return str(mapped)
    return name or "local"


def _local_name():
    try:
        result = subprocess.run(
            ["scutil", "--get", "LocalHostName"], capture_output=True, text=True, timeout=2,
        )
    except (OSError, subprocess.TimeoutExpired):
        return ""
    if result.returncode != 0:
        return ""
    return (result.stdout or "").strip()


def _capacity(home, config, now, run):
    binary = shutil.which("ai-capacity") or str(home / ".local/bin/ai-capacity")
    if not Path(binary).is_file() and shutil.which("ai-capacity") is None:
        return None
    try:
        result = run([binary, "--json"], capture_output=True, text=True, timeout=20)
    except (OSError, subprocess.TimeoutExpired):
        return {"pools": [{"engine": "capacity", "pool": "read", "remaining_pct": None, "jobs_left": None, "reset_epoch": None, "stale": True}]}
    try:
        document = json.loads(result.stdout or "")
    except ValueError:
        return {"pools": [{"engine": "capacity", "pool": "read", "remaining_pct": None, "jobs_left": None, "reset_epoch": None, "stale": True}]}
    stale_h = float((config.get("capacity") or {}).get("stale_h") or 3)
    pools = []
    services = document.get("services") or {}
    if not isinstance(services, dict):
        return None
    for engine, service in services.items():
        for pool in (service or {}).get("pools") or []:
            if not isinstance(pool, dict):
                continue
            as_of = _epoch(pool.get("asOf") or pool.get("as_of"))
            stale = bool(pool.get("stale")) or (as_of is not None and now - as_of > stale_h * 3600)
            pools.append({
                "engine": engine,
                "pool": pool.get("pool") or "",
                "remaining_pct": pool.get("remainingPct", pool.get("remaining_pct")),
                "jobs_left": pool.get("jobsLeft", pool.get("jobs_left")),
                "reset_epoch": _epoch(pool.get("reset")),
                "stale": stale,
                "absent": pool.get("freshness") == "none",
            })
    return {"pools": pools} if pools else None


def _board(home, run):
    state = os.environ.get("COWORK_STATE")
    candidates = []
    if state:
        candidates.append(Path(state) / "board.json")
    candidates.append(home / ".local/state/cowork/board.json")
    for path in candidates:
        document = _read_json(path)
        if isinstance(document, dict) and ("claims" in document or "presence" in document):
            return document
    taskhub = home / ".local/state/taskhub/board.json"
    document = _read_json(taskhub)
    if isinstance(document, dict):
        return document
    binary = shutil.which("taskhub")
    if not binary:
        return None
    try:
        result = run([binary, "json"], capture_output=True, text=True, timeout=15)
    except (OSError, subprocess.TimeoutExpired):
        return None
    try:
        document = json.loads(result.stdout or "")
    except ValueError:
        return None
    return document if isinstance(document, dict) else None


def _agentkit(run):
    binary = shutil.which("agentkit")
    if not binary:
        return {"absent": True}
    try:
        result = run([binary, "doctor", "--json"], capture_output=True, text=True, timeout=45)
    except (OSError, subprocess.TimeoutExpired):
        return {"ok": False, "issues": ["doctor timed out"]}
    try:
        document = json.loads(result.stdout or "")
    except ValueError:
        if result.returncode == 0:
            return {"ok": True, "issues": []}
        return {"ok": False, "issues": ["doctor failed"]}
    if isinstance(document, dict):
        return document
    return {"absent": True}


def _git(path, run):
    if not (path / ".git").exists():
        return None
    dirty = _git_out(run, path, "status", "--porcelain", "--untracked-files=no")
    ahead = _git_count(run, path, "@{u}..HEAD")
    behind = _git_count(run, path, "HEAD..@{u}")
    remote = _git_out(run, path, "remote", "get-url", "origin") or ""
    return {
        "dirty": bool(dirty.strip()) if dirty is not None else False,
        "ahead": ahead,
        "behind": behind,
        "unpushed": ahead,
        "remote": remote.strip(),
    }


def _git_out(run, path, *args):
    try:
        result = run(["git", "-C", str(path), *args], capture_output=True, text=True, timeout=8)
    except (OSError, subprocess.TimeoutExpired):
        return None
    if result.returncode != 0:
        return None
    return result.stdout or ""


def _git_count(run, path, spec):
    text = _git_out(run, path, "rev-list", "--count", spec)
    try:
        return int((text or "0").strip() or 0)
    except ValueError:
        return 0


def _ci(path, remote, run):
    if not remote or "github.com" not in remote or not path.is_dir():
        return None
    if shutil.which("gh") is None:
        return None
    try:
        result = run(
            ["gh", "run", "list", "--limit", "1", "--json", "conclusion,status"],
            cwd=str(path), capture_output=True, text=True, timeout=20,
        )
    except (OSError, subprocess.TimeoutExpired):
        return None
    try:
        rows = json.loads(result.stdout or "")
    except ValueError:
        return None
    if not rows:
        return None
    return {"conclusion": (rows[0] or {}).get("conclusion"), "status": (rows[0] or {}).get("status")}


def _project_jobs(snapshot, name):
    found = []
    needle = name.lower()
    for host in (snapshot.get("hosts") or {}).values():
        if not isinstance(host, dict):
            continue
        for job in host.get("jobs") or []:
            label = str(job.get("label") or "")
            cmd = str(job.get("cmd") or "")
            if needle in label.lower() or f"/{name}" in cmd or f"Projects/{name}" in cmd:
                found.append(job)
    return found


def _merge_jobs(registry_jobs, probe_jobs, registered=None, here=None):
    """Jobs the registry recorded for this Mac, overlaid with what launchd says now.

    `registered` is jobs.tsv (label → home Macs). A job with no plist here that jobs.tsv does not
    place on this Mac has been retired: the host record that still lists it is only out of date.
    """
    seen = {job.get("label") for job in probe_jobs or [] if isinstance(job, dict)}
    by_label = {}
    for label, job in (registry_jobs or {}).items():
        if not isinstance(job, dict):
            continue
        if registered is not None and label not in seen and not _homed(registered, label, here):
            continue
        exit_code = job.get("status")
        if exit_code in ("", "-"):
            exit_code = 0
        by_label[label] = {
            "label": label,
            "loaded": bool(job.get("loaded")),
            "exit": exit_code,
            "last_epoch": job.get("last_epoch"),
            "when": job.get("when") or "",
            "cmd": job.get("cmd") or "",
        }
    for job in probe_jobs or []:
        label = job.get("label")
        if not label:
            continue
        current = by_label.get(label, {"label": label, "when": "", "cmd": ""})
        current["loaded"] = job.get("loaded", current.get("loaded", False))
        if job.get("exit") is not None:
            current["exit"] = job.get("exit")
        if "runs" in job:
            current["runs"] = job.get("runs")
        if job.get("state"):
            current["state"] = job["state"]
        if job.get("last_epoch"):
            current["last_epoch"] = job["last_epoch"]
        if job.get("when"):
            current["when"] = job["when"]
        if job.get("cmd"):
            current["cmd"] = job["cmd"]
        for flag in ("utc", "retry_pending"):
            if job.get(flag):
                current[flag] = True
        current["plist"] = True
        current["label"] = label
        by_label[label] = current
    return list(by_label.values())


def _homed(registered, label, here):
    homes = registered.get(label)
    return bool(homes) and ("all" in homes or here in homes)


def _registered_jobs(registry_dir):
    """jobs.tsv as label → list of home Macs ("all" for every Mac). None when there is no file."""
    try:
        lines = (Path(registry_dir) / "jobs.tsv").read_text().splitlines()
    except OSError:
        return None
    found = {}
    for line in lines:
        if not line.strip() or line.startswith("#"):
            continue
        parts = line.split("\t")
        if len(parts) < 2 or not parts[0].strip():
            continue
        found[parts[0].strip()] = [home.strip() for home in parts[1].split(",") if home.strip()]
    return found


def _disk_targets(home, config):
    """Per-Mac free-space lines for the internal disk, from the site's targets file.

    Columns: host, target_empty, target_with_purgeable, floor_empty, note (GB, tab-separated).
    Below the target is a warning; below the floor is a failure. A Mac with no row keeps
    `disk.floor_gb`. The path is `disk.targets_file` in morning.toml.
    """
    configured = (config.get("disk") or {}).get("targets_file")
    path = Path(configured).expanduser() if configured else Path(home) / ".local/state/mac-sync/storage-audit/free-targets.tsv"
    try:
        lines = path.read_text().splitlines()
    except OSError:
        return {}
    found = {}
    for line in lines:
        if not line.strip() or line.startswith("#"):
            continue
        parts = [part.strip() for part in line.split("\t")]
        try:
            target, purgeable, floor = (float(part) for part in parts[1:4])
        except ValueError:
            continue
        if parts[0]:
            found[parts[0]] = {"target_gb": target, "with_purgeable_gb": purgeable, "floor_gb": floor}
    return found


def _glancebar(document):
    if not isinstance(document, dict):
        return None
    return {
        "behind": document.get("behind", document.get("commits_behind")),
        "release": document.get("release", document.get("latest")),
        "origin": document.get("origin"),
        "installed": document.get("installed", document.get("version")),
        "state": document.get("state") or "",
        "detail": document.get("detail") or "",
    }


def _placement(home, probed):
    local = _read_json(home / ".local/state/placement/status.json")
    if isinstance(local, dict) and local:
        return local
    for raw in probed.values():
        if isinstance(raw.get("placement"), dict) and raw["placement"]:
            return raw["placement"]
    return None


def _upstream(home):
    from fleet.upstream import statuses
    try:
        return statuses(home) or None
    except Exception:  # a bad upstream.toml is not a reason to lose the digest
        return [{"name": "config", "state": {"error": "upstream.toml unreadable", "new": []}}]


def _coverage(home, probed):
    local = _read_json(site.drives_project(home) / "coverage.json")
    if isinstance(local, dict):
        return local
    for raw in probed.values():
        if isinstance(raw.get("coverage"), dict):
            return raw["coverage"]
    return None


def _disks_from_summary(summary):
    rows = []
    for loc in (summary or {}).get("locations") or []:
        if not isinstance(loc, dict) or loc.get("kind") not in (None, "volume"):
            continue
        trend = loc.get("trend_bytes") or {}
        rows.append({
            "name": loc.get("name") or "disk",
            "free_bytes": loc.get("free_bytes"),
            "trend_h24": trend.get("h24"),
        })
    return rows


def _has_kind(health_host, kind):
    for loc in ((health_host.get("diskmap") or {}).get("locations") or []):
        if isinstance(loc, dict) and loc.get("kind") == kind:
            return True
    return False


def _host_list(config, home, here):
    names = list(config.get("hosts") or [])
    if names:
        return names
    found = []
    for path in (home / ".config/agent-mail/hosts", home / ".config/remote-run/hosts"):
        try:
            lines = path.read_text().splitlines()
        except OSError:
            continue
        for line in lines:
            line = line.split("#", 1)[0].strip().split()
            if line and line[0] not in found:
                found.append(line[0])
    if here and here not in found:
        found.insert(0, here)
    return found


def _registry(home, config):
    root = Path(config.get("registry") or home / ".local/state/mac-sync/registry")
    hosts = {}
    folder = root / "hosts"
    if not folder.is_dir():
        return hosts
    for path in sorted(folder.glob("*.json")):
        document = _read_json(path)
        if isinstance(document, dict):
            hosts[path.stem] = document
    return hosts


def _epoch(value):
    if value is None or value == "":
        return None
    if isinstance(value, (int, float)) and not isinstance(value, bool):
        return int(value)
    text = str(value).replace("Z", "+00:00")
    try:
        return int(datetime.fromisoformat(text).timestamp())
    except ValueError:
        return None


def _read_json(path):
    try:
        return json.loads(Path(path).read_text())
    except (OSError, ValueError, TypeError):
        return None
