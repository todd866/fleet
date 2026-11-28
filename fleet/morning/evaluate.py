"""Turn a collected snapshot into one finding per instrument. No I/O."""
from fleet import site
import re
from dataclasses import dataclass
from datetime import date, datetime, timezone

try:
    from zoneinfo import ZoneInfo
except ImportError:  # pragma: no cover
    ZoneInfo = None

MONTHS = ("Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec")
RANK = {"pass": 0, "warn": 1, "fail": 2}
# A sleeping laptop is info once its snapshot is older than 3 h. Warn only after 48 h.
SNAPSHOT_INFO_S = 3 * 3600
SNAPSHOT_WARN_S = 48 * 3600
ORDER = ("host", "disk", "backup", "sync", "update", "job", "thermal", "capacity", "agent", "project", "check", "security", "place", "cloud", "kit", "upstream")
REGISTERED = re.compile(r"^[0-9a-f]{64}$")
# macOS volumes that live in the boot disk's container: the internal disk again, not another one.
SYSTEM_VOLUMES = frozenset((
    "Macintosh HD", "Data", "Recovery", "Preboot", "VM", "Update", "xarts", "iSCPreboot", "Hardware",
))
# SIGTERM as launchd reports it (-15), as wrapper binaries forward it (15), and as a shell reports a child it ended (143).
TERM_EXITS = ("-15", "15", "143")


@dataclass(frozen=True)
class Finding:
    key: str
    status: str
    area: str
    name: str
    value: str
    datum: str
    detail: str
    symbol: str


def evaluate(snapshot, config, now):
    now = int(now)
    tz = _zone(config.get("tz"))
    rows = []
    rows.extend(_hosts(snapshot.get("hosts") or {}, now, tz))
    rows.extend(_disks_fleet(snapshot.get("hosts") or {}, config, now, tz, snapshot.get("disk_targets") or {}))
    registry = snapshot.get("registry_jobs")
    prefixes = ("com.example.", *((config.get("jobs") or {}).get("label_prefixes") or ()))
    for name, host in (snapshot.get("hosts") or {}).items():
        if not host.get("ok"):
            continue
        rows.extend(_backups(name, host, config, now))
        rows.extend(_sync(name, host))
        rows.extend(_updates(name, host))
        rows.extend(_diskmap_access(name, host.get("diskmap_access")))
        rows.extend(_jobs(name, host, config, now, tz, registry, prefixes))
        rows.extend(_queue(name, host, now, tz))
        rows.extend(_thermal(name, host, config, now))
        rows.extend(_mail(name, host, config, now))
        rows.extend(_vpn_gateway(name, host.get("vpn_gateway")))
    rows.extend(_capacity(snapshot.get("capacity"), config, now, tz))
    rows.extend(_ai_usage(snapshot.get("ai_usage"), now))
    rows.extend(_agents(snapshot.get("board"), now))
    rows.extend(_projects(snapshot.get("projects") or [], _reported_jobs(snapshot.get("hosts") or {}), prefixes))
    # One map of copies, not two. When a Mac has a current data map, its data:map row (and map.md) is the
    # answer to "how many copies, where": placement's own copy counts never saw the cloud receipts, so its
    # rows said "no offsite" for sets the map shows on Google. Placement keeps the rows only it can give
    # (free space, where bulk lives, unplaced and unclaimed data).
    mapped = any(isinstance(h.get("data_map"), dict) and _data_map_fresh(h["data_map"], now)
                 for h in (snapshot.get("hosts") or {}).values() if isinstance(h, dict))
    rows.extend(_placement(snapshot.get("placement"), rows, now, copies_mapped=mapped))
    rows.extend(_cloud(snapshot.get("cloud"), now))
    rows.extend(_coverage(snapshot.get("coverage"), now))
    rows.extend(_agentkit(snapshot.get("agentkit")))
    rows.extend(_security_evidence(snapshot.get("security_evidence"), now))
    rows.extend(_surface_probes(snapshot.get("surface_probes"), config))
    rows.extend(_job_reviews(snapshot.get("job_reviews"), now))
    rows.extend(_upstream(snapshot.get("upstream"), config, now))
    rows.extend(_unregistered_jobs(snapshot.get("hosts") or {}, registry))
    rows.sort(key=lambda row: (ORDER.index(row.area) if row.area in ORDER else 99, row.name, row.key))
    return rows


def _hosts(hosts, now, tz):
    rows = []
    for name, host in hosts.items():
        if not isinstance(host, dict):
            continue
        if not host.get("ok"):
            since = host.get("away_since")
            rows.append(_row(
                f"host:{name}", "warn", "host", name, "away", _since(since, now, tz),
                host.get("error") or "no answer", away=True,
            ))
            continue
        collected = host.get("collected_at")
        if collected is None:
            continue
        try:
            collected = int(collected)
        except (TypeError, ValueError):
            continue
        age = now - collected
        if age <= SNAPSHOT_INFO_S:
            continue
        status = "warn" if age > SNAPSHOT_WARN_S else "info"
        info_h = SNAPSHOT_INFO_S // 3600
        warn_h = SNAPSHOT_WARN_S // 3600
        rows.append(_row(
            f"host:{name}", status, "host", name, "stale", _since(collected, now, tz),
            f"snapshot {_hours(age / 3600)}; info after {info_h} h; warn after {warn_h} h",
        ))
    return rows


def _disks_fleet(hosts, config, now, tz, targets=None):
    """One row per physical drive, on the host that has it mounted.

    The same volume UUID on several Macs is one drive. Copies with
    `mounted: false` are not free-space readings. A drive mounted nowhere
    is away, not a floor breach. Rows without a mount flag stay per host,
    which is how the older snapshots were judged.
    """
    groups = []
    local = []
    missing = []
    for name, host in hosts.items():
        if not isinstance(host, dict) or not host.get("ok") or "disks" not in host:
            continue
        disks = host.get("disks") or []
        if not disks:
            missing.append((name, host.get("disk_error") or "disk summary missing"))
            continue
        for disk in disks:
            if not isinstance(disk, dict) or disk.get("name") in SYSTEM_VOLUMES:
                continue
            uuids = _uuids(disk)
            if uuids and "mounted" in disk:
                _add_drive(groups, uuids, name, disk)
            else:
                local.append((name, disk))
    rows = []
    for name, reason in missing:
        rows.append(_row(f"disk:{name}:none", "warn", "disk", name, "none", "no reading", reason))
    for group in groups:
        rows.append(_drive_row(group, config, now, tz, targets))
    for name, disk in local:
        rows.append(_disk_reading(name, disk, config, targets))
    return rows


def _uuids(disk):
    found = []
    for raw in disk.get("volume_uuids") or []:
        text = str(raw or "").strip().upper()
        if text and text not in found:
            found.append(text)
    single = str(disk.get("volume_uuid") or "").strip().upper()
    if single and single not in found:
        found.append(single)
    return found


def _add_drive(groups, uuids, host, disk):
    ids = set(uuids)
    hits = [group for group in groups if ids & group["uuids"]]
    if not hits:
        groups.append({"uuids": ids, "rows": [(host, disk)], "name": disk.get("name") or "disk"})
        return
    primary = hits[0]
    primary["uuids"] |= ids
    primary["rows"].append((host, disk))
    if disk.get("mounted") and disk.get("name"):
        primary["name"] = disk["name"]
    for extra in hits[1:]:
        primary["uuids"] |= extra["uuids"]
        primary["rows"].extend(extra["rows"])
        groups.remove(extra)


def _drive_row(group, config, now, tz, targets=None):
    attached = [(host, disk) for host, disk in group["rows"] if disk.get("mounted") is True]
    drive = group["name"] or "disk"
    if not attached:
        seen = max((int(disk.get("last_seen") or 0) for _, disk in group["rows"]), default=0)
        datum = _since(seen, now, tz) if seen else "unseen"
        return _row(f"disk:{drive}", "info", "disk", drive, "away", datum, "not attached")
    host, disk = max(attached, key=lambda item: item[1].get("free_bytes") or 0)
    return _disk_reading(host, disk, config, targets)


def _disk_reading(name, disk, config, targets=None):
    disk_cfg = config.get("disk") or {}
    ratio = float(disk_cfg.get("fail_ratio", 0.5))
    trend_warn = float(disk_cfg.get("trend_warn_gb", -20))
    trend_fail = float(disk_cfg.get("trend_fail_gb", -80))
    drive = disk.get("name") or "disk"
    # warn_line: free space below this is a warning. fail_line: below this is a failure.
    # A Mac with its own target row is judged by that row (target, then floor); a floor written
    # in the report's config, or no row at all, keeps the single floor and its ratio.
    target = None if drive in ((disk_cfg.get("floors") or {}).get(name) or {}) else _target(targets, name, drive)
    if target:
        warn_line, fail_line = target["target_gb"], target["floor_gb"]
        lines = f"target {_num(warn_line)} GB; floor {_num(fail_line)} GB"
    else:
        warn_line = _floor(disk_cfg, name, drive)
        fail_line = warn_line * ratio
        lines = f"floor {_num(warn_line)} GB"
    free = None if disk.get("free_bytes") is None else disk["free_bytes"] / 1e9
    trend = None if disk.get("trend_h24") is None else disk["trend_h24"] / 1e9
    free_status = "pass"
    if free is None:
        free_status = "warn"
    elif free < fail_line:
        free_status = "fail"
    elif free < warn_line:
        free_status = "warn"
    trend_status = "pass"
    if trend is not None:
        if trend <= trend_fail:
            trend_status = "fail"
        elif trend <= trend_warn:
            trend_status = "warn"
    status = _worse(free_status, trend_status)
    if disk.get("alert"):
        status = _worse(status, "warn")
    value = "none" if free is None else _gb(free)
    trend_text = None if trend is None else f"24h {_signed_gb(trend)}"
    if free_status != "pass" and RANK[free_status] >= RANK[trend_status]:
        if free is None:
            datum = "no reading"
        elif target and free_status == "warn":
            datum = f"target {_num(warn_line)} GB"
        else:
            datum = f"floor {_num(fail_line if target else warn_line)} GB"
    elif trend_text is not None:
        datum = trend_text
    else:
        datum = "no trend"
    detail = f"free {value}; {lines}; {trend_text or 'no trend'}"
    if disk.get("alert"):
        detail = f"{detail}; {disk['alert']}"
    return _row(f"disk:{name}:{drive}", status, "disk", f"{name} {drive}", value, datum, detail)


def _backups(name, host, config, now):
    cfg = config.get("backups") or {}
    rows = []
    if "idrive" in host:
        rows.append(_age_row(
            f"backup:{name}:idrive", "backup", f"{name} idrive", host.get("idrive"), "last_epoch",
            cfg.get("idrive_warn_h", 36), cfg.get("idrive_fail_h", 72), now, "no backup",
        ))
        attempt = host.get("idrive") or {}
        if attempt.get("failed") or attempt.get("error"):
            detail = attempt.get("error") or f'{attempt["failed"]} files failed in latest IDrive attempt'
            rows[-1] = _row(f"backup:{name}:idrive", "fail", "backup", f"{name} idrive",
                            "incomplete", "check backup", detail)
        if attempt.get("held_since"):
            # A deliberate hold is not a broken backup: say since when and what it costs.
            rows[-1] = _row(f"backup:{name}:idrive", "warn", "backup", f"{name} idrive",
                            "held", "owner hold",
                            f'macOS IDrive held since {attempt["held_since"]} (owner hold). Keep it held; Linux ingest and home-mirror coverage are evaluated separately.')
    if host.get("idrive_ingest") is not None:
        rows.append(_idrive_ingest(name, host["idrive_ingest"], cfg, now))
    if isinstance(host.get("server_health"), dict):
        rows.extend(_server_health(host["server_health"], now))
    if isinstance(host.get("data_map"), dict):
        rows.append(_data_map(name, host["data_map"], now))
    if isinstance(host.get("signatures"), dict):
        rows.append(_signatures(name, host["signatures"]))
    if host.get("offload_queue") is not None:
        rows.append(_offload_queue(name, host["offload_queue"], now))
        rows.extend(_offload_autopilot(name, host["offload_queue"].get("autopilot"), now))
    if isinstance(host.get("free_space_guard"), dict):
        rows.append(_free_space_guard(name, host["free_space_guard"], now))
    if isinstance(host.get("mac_setup"), dict):
        rows.append(_mac_setup(name, host["mac_setup"], now))
    if host.get("home_mirror") is not None:
        rows.extend(_home_mirror(name, host["home_mirror"], cfg, now))
    if "drive_mirror_epoch" in host:
        rows.append(_stamp(
            f"backup:{name}:drive-mirror", f"{name} drive-mirror", host.get("drive_mirror_epoch"),
            cfg.get("drive_mirror_warn_h", 26), cfg.get("drive_mirror_fail_h", 48), now, "no stamp",
        ))
    if "hdd_backup_epoch" in host and name not in cfg.get("retired_hdd_hosts", []):
        rows.append(_stamp(
            f"backup:{name}:hdd-backup", f"{name} hdd-backup", host.get("hdd_backup_epoch"),
            cfg.get("hdd_backup_warn_h", 36), cfg.get("hdd_backup_fail_h", 168), now, "no stamp",
        ))
    if "icloud" in host:
        cloud = host.get("icloud") or {}
        if not cloud:
            rows.append(_row(f"backup:{name}:icloud", "warn", "backup", f"{name} icloud", "none", "no reading", "iCloud status missing"))
            return rows
        pending = int(cloud.get("pending_upload") or 0)
        last = cloud.get("last_sync_epoch")
        stall = float(cfg.get("icloud_stall_h", 6))
        age = None if not last else (now - int(last)) / 3600
        status = "pass"
        if pending and (age is None or age >= stall):
            status = "warn"
        value = f"{pending} up" if pending else "0 up"
        datum = "caught up" if not pending else ("no sync" if age is None else f"sync {_hours(age)}")
        detail = f"pending upload {pending}; {datum}; stall { _num(stall) } h"
        rows.append(_row(f"backup:{name}:icloud", status, "backup", f"{name} icloud", value, datum, detail))
    return rows


def _sync(name, host):
    if "syncthing" not in host or host.get("syncthing") is None:
        return []
    sync = host["syncthing"] or {}
    errors = int(sync.get("errors") or 0)
    out = int(sync.get("out_of_sync") or 0)
    local = int(sync.get("receive_only_local") or 0)
    if sync.get("down"):
        return [_row(f"sync:{name}", "warn", "sync", name, "down", "no answer", "Syncthing API did not answer")]
    status = "pass"
    if errors or local:
        status = "fail"
    elif out:
        status = "warn"
    if local:
        value, datum = str(local), "receive-only"
    elif errors:
        value, datum = str(errors), "errors"
    elif out:
        value, datum = str(out), "out of sync"
    else:
        value, datum = "0", "in sync"
    detail = f"errors {errors}; out of sync {out}; receive-only local changes {local}"
    return [_row(f"sync:{name}", status, "sync", name, value, datum, detail)]


def _diskmap_access(name, access):
    """One row while diskmap lacks Full Disk Access: without it the walker leaves consent-gated
    folders and other disks unmeasured instead of making macOS ask the owner (18 Nov 2026)."""
    if not isinstance(access, dict) or access.get("full_disk") is not False:
        return []
    trees = [t for t in access.get("unmeasured") or [] if isinstance(t, dict)]
    n = len(trees)
    biggest = sorted(trees, key=lambda t: -(t.get("bytes") or 0))[:5]
    listed = ", ".join(f"{t.get('path')} {_gb(round((t.get('bytes') or 0) / 1e9, 1))}" for t in biggest)
    detail = (f"diskmap: {n} trees unmeasured, switch on Full Disk Access for diskmap to measure them"
              f" (System Settings > Privacy & Security > Full Disk Access); sizes shown are the last measured"
              + (f"; largest: {listed}" if listed else ""))
    return [_row(f"disk:{name}:diskmap-access", "warn", "disk", f"{name} diskmap", f"{n} trees", "no FDA", detail)]


def _updates(name, host):
    if "glancebar" not in host or host.get("glancebar") is None:
        return []
    info = host["glancebar"] or {}
    behind = info.get("behind")
    release = info.get("release") if info.get("release") not in (None, "") else None
    origin = info.get("origin") or release
    installed = info.get("installed")
    state = str(info.get("state") or "")
    note = str(info.get("detail") or "")
    try:
        behind_n = None if behind in (None, "") else int(behind)
    except (TypeError, ValueError):
        behind_n = None
    if behind_n is not None and behind_n < 0:
        behind_n = None
    if behind_n is None and not origin and not installed and not state:
        return [_row(f"update:{name}", "warn", "update", name, "none", "no status", "glancebar-update status missing")]
    # A version-only file with no `state` still means the release name was not recorded.
    if not state and not release and installed:
        return [_row(f"update:{name}", "warn", "update", name, "none", "no release", "no release recorded")]
    if state == "ok" and (behind_n is None or behind_n <= 0):
        detail = f"installed {installed or '—'}; origin {origin or '—'}; behind {0 if behind_n is None else behind_n}"
        value = "0" if behind_n == 0 else "ok"
        return [_row(f"update:{name}", "pass", "update", name, value, "current", detail)]
    if behind_n is not None and behind_n > 0:
        detail = f"installed {installed or '—'}; origin {origin or '—'}; behind {behind_n}"
        if state and state not in ("ok", "current"):
            detail = f"{detail}; state {state}"
        if note:
            detail = f"{detail}; {note}"
        return [_row(f"update:{name}", "warn", "update", name, str(behind_n), "behind", detail)]
    if state and state not in ("ok", "current"):
        text = state.replace("-", " ")
        detail = note or text
        return [_row(f"update:{name}", "warn", "update", name, "none", text, detail)]
    behind_n = int(behind_n or 0)
    status = "warn" if behind_n > 0 else "pass"
    value = str(behind_n) if behind_n else (origin or "current")
    datum = "behind" if behind_n else "current"
    detail = f"installed {installed or '—'}; origin {origin or '—'}; behind {behind_n}"
    return [_row(f"update:{name}", status, "update", name, value, datum, detail)]


def _jobs(name, host, config, now, tz, registry=None, prefixes=("com.example.",)):
    if "jobs" not in host:
        return []
    grace_h = float((config.get("jobs") or {}).get("overdue_h", 2))
    rows = []
    for job in host.get("jobs") or []:
        label = str(job.get("label") or "job")
        if _retired(job, label, name, registry):
            continue
        short = _short_label(label, prefixes)
        utc = _on_utc(job)
        loaded = job.get("loaded", True)
        exit_code = job.get("exit", job.get("status"))
        when = str(job.get("when") or "")
        last = job.get("last_epoch")
        runs = job.get("runs")
        state = str(job.get("state") or "")
        if not loaded:
            status, value, datum = "fail", "unloaded", "not loaded"
        elif state == "running" and (
            str(exit_code) in TERM_EXITS
            or (isinstance(job.get("uptime_s"), (int, float)) and not isinstance(job.get("uptime_s"), bool) and job["uptime_s"] >= 300)
        ):
            # SIGTERM is what launchctl sends when a job is restarted or reloaded, and a process
            # that has been running stably for 5+ minutes is up now; the exit code belongs to a
            # previous instance (KeepAlive daemons keep their prior LastExitStatus forever).
            status, value, datum = "pass", "ran", when or "running"
        elif job.get("retry_pending") and str(exit_code) in TERM_EXITS:
            # A UTC run cut short by a restart. The wrapper has reopened its slot and runs it
            # once more at the next tick; only that second result is the job's.
            status, value, datum = "pass", "cut", "retry due"
        elif str(exit_code) == "75":
            # EX_TEMPFAIL: the job chose not to run this time (a drive another job holds, a
            # metered link, no capacity) and tries again at its next slot. That is a deferral
            # to watch, not a failed job: heavy-slot's rows already read 75 this way.
            status, value, datum = "warn", "deferred", short
        elif _bad_exit(exit_code):
            status, value, datum = "fail", f"exit {exit_code}", short
        elif state == "running":
            status, value, datum = "pass", "ran", when or "running"
        else:
            # A job on the utc-daily wrapper keeps its slot in UTC; anything still on launchd's
            # calendar is in the Mac's own zone.
            missed, datum = _missed(when, last, now, grace_h, timezone.utc if utc else tz, runs)
            if utc and datum:
                datum += "Z"
            if missed:
                status, value = "fail", "not run"
            else:
                status, value, datum = "pass", "ran", datum or when or "ok"
        detail = f"{label}; when {when or '—'}; exit {exit_code}; loaded {bool(loaded)}"
        rows.append(_row(f"job:{name}:{short}", status, "job", f"{name} {short}", value, datum, detail))
    return rows


def _job_reviews(reviews, now):
    """One row for jobs whose review date has come, one for jobs no project owns.

    A review asks whether the job should still exist. It is a prompt for an agent or the
    owner, so it is a warn and never a fail.
    """
    if reviews is None:
        return []
    today = datetime.fromtimestamp(int(now), timezone.utc).date()
    due, undated, upcoming, unowned = [], [], [], []
    for job in reviews:
        label = str(job.get("label") or "job")
        project = str(job.get("project") or "")
        if not project:
            unowned.append(label)
        try:
            when = date.fromisoformat(str(job.get("review") or ""))
        except ValueError:
            undated.append(label)
            continue
        (due if when <= today else upcoming).append((when, label, project or "no project"))
    due.sort()
    rows = []
    if due:
        detail = "; ".join(f"{label} ({project}) due {when.isoformat()}" for when, label, project in due)
        if undated:
            detail += "; no review date: " + ", ".join(undated)
        rows.append(_row("job:reviews", "warn", "job", "reviews", f"{len(due)} due",
                         f"oldest {_day(due[0][0])}", detail))
    elif undated:
        rows.append(_row("job:reviews", "warn", "job", "reviews", f"{len(undated)} undated",
                         "no review date", "no review date: " + ", ".join(undated)))
    else:
        nxt = min(upcoming)[0] if upcoming else None
        rows.append(_row("job:reviews", "pass", "job", "reviews", "none due",
                         f"next {_day(nxt)}" if nxt else "no jobs", "no job is due for review"))
    if unowned:
        rows.append(_row("job:review-projects", "warn", "job", "owners", f"{len(unowned)} unowned",
                         "no project", "no project in jobs.tsv: " + ", ".join(unowned)))
    return rows


def _unregistered_jobs(hosts, registry):
    """Jobs installed on a Mac that jobs.tsv does not list. They have no project and no review
    date, so the review rows cannot see them. Without the registry list nothing is reported."""
    if registry is None:
        return []
    where = {}
    for name, host in (hosts or {}).items():
        if not isinstance(host, dict) or not host.get("ok"):
            continue
        for job in host.get("jobs") or []:
            label = str(job.get("label") or "")
            seen = job.get("plist")
            if seen is None:   # snapshots from before the flag, as in _retired
                seen = "runs" in job or "state" in job
            if label and seen and label not in registry:
                where.setdefault(label, set()).add(name)
    if not where:
        return []
    detail = "installed but not in jobs.tsv: " + ", ".join(
        f"{label} ({', '.join(sorted(names))})" for label, names in sorted(where.items()))
    return [_row("job:unregistered", "warn", "job", "unregistered", f"{len(where)} unregistered",
                 "not in jobs.tsv", detail)]


def _day(when):
    return f"{when.day} {MONTHS[when.month - 1]}"


def _thermal(name, host, config, now):
    if "heavy" not in host and "samples" not in host:
        return []
    heavy = host.get("heavy") or []
    samples = host.get("samples") or []
    if not heavy and not samples:
        return []
    cfg = config.get("thermal") or {}
    window = now - float(cfg.get("window_h", 24)) * 3600
    recent = [job for job in heavy if job.get("end") in (None, "") or int(job.get("end") or 0) >= window]
    # A job that was stopped on purpose (SIGINT, SIGKILL, SIGTERM) is not a failure, and a job
    # that failed on its own account (a red test run) says nothing about this Mac: it is a
    # warning for whoever ran it, never a machine fault.
    fails = [job for job in recent if _bad_exit(job.get("rc")) and int(job.get("rc") or 0) not in (75, 130, 137, 143, -2, -9, -15)]
    starved = [job for job in recent if int(job.get("rc") or 0) == 75 or int(job.get("waited") or 0) >= float(cfg.get("wait_warn_s", 600))]
    hot_c = _hot_line(host, cfg)
    line = f"≥{_num(hot_c)}C" if hot_c is not None else "no cool target"
    hours = _hot_hours(samples, now, hot_c, float(cfg.get("gap_s", 900)))
    status = "pass"
    if hours >= float(cfg.get("hot_hours_fail", 6)):
        status = "fail"
    elif fails or starved or hours >= float(cfg.get("hot_hours_warn", 2)):
        status = "warn"
    if hours >= float(cfg.get("hot_hours_warn", 2)):
        # heat is what makes this row red; say so rather than naming someone's failed test run
        value, datum = _hours(hours), line
    elif fails:
        value, datum = f"{len(fails)} fail", str(fails[0].get("label") or "job")
    elif starved:
        value, datum = "wait", str(starved[0].get("label") or "slot")
    else:
        value, datum = _hours(hours), line
    basis = f"{line} or thermal pressure" if hot_c is not None else "thermal pressure only; this Mac sent no cool target"
    detail = f"failed jobs {len(fails)}; slot waits {len(starved)}; hot {_hours(hours)} ({basis})"
    return [_row(f"thermal:{name}", status, "thermal", name, value, datum, detail)]


def _hot_line(host, cfg):
    """The chip temperature that counts as hot on this Mac, or None when nothing sets one.

    heavy-slot's `cool` file is the slot policy's own line, so it is the report's: the probe
    sends it as `cool_c`. `thermal.hot_c` in the config speaks only for a Mac that sent none.
    """
    for value in (host.get("cool_c"), cfg.get("hot_c")):
        if isinstance(value, (int, float)) and not isinstance(value, bool) and value > 0:
            return float(value)
    return None


def _queue(name, host, now, tz):
    """One failure per attempt that holds this Mac's job queue.

    The queue refuses every dispatch while an attempt's ownership is uncertain. That is on
    purpose; staying silent about it was not.
    """
    rows = []
    for stall in (host.get("queue") or {}).get("stalls") or []:
        if not isinstance(stall, dict) or not stall.get("id"):
            continue
        ident = str(stall["id"])
        since = stall.get("since")
        try:
            since = int(float(since)) if since is not None else None
        except (TypeError, ValueError):
            since = None
        if since:
            stamp = datetime.fromtimestamp(since, timezone.utc).strftime("%Y-%m-%d %H:%MZ")
            datum = _since(since, now, tz)
        else:
            stamp, datum = "an unrecorded time", "since unknown"
        clear = stall.get("clear") or f"fleet-queue show {ident}"
        detail = (
            f"attempt {ident} ({stall.get('state') or 'needs_review'}, ownership uncertain) has stopped every "
            f"dispatch on {name} since {stamp}: {stall.get('blocker') or 'no reason recorded'}. "
            f"Clear it on {name}: {clear}"
        )
        rows.append(_row(f"queue:{name}:{ident}", "fail", "job", f"{name} queue", "blocked", datum, detail))
    return rows


def _mail(name, host, config, now):
    if "mail" not in host or host.get("mail") is None:
        return []
    limit = float((config.get("agents") or {}).get("unread_mail_h", 12))
    old = []
    for note in host.get("mail") or []:
        if note.get("read"):
            continue
        at = note.get("at")
        if at is None:
            continue
        age = (now - int(at)) / 3600
        if age >= limit:
            old.append(age)
    if not old:
        return [_row(f"agent:mail:{name}", "pass", "agent", f"{name} mail", "0", f"under {_num(limit)} h", "no unread mail past the limit")]
    oldest = max(old)
    return [_row(
        f"agent:mail:{name}", "warn", "agent", f"{name} mail", str(len(old)),
        f"oldest {_hours(oldest)}", f"{len(old)} unread older than {_num(limit)} h",
    )]


def _capacity(capacity, config, now, tz):
    if not capacity:
        return []
    cfg = config.get("capacity") or {}
    near_pct = float(cfg.get("near_empty_pct", 10))
    near_jobs = float(cfg.get("near_empty_jobs", 5))
    paused = {str(p).lower() for p in (capacity.get("paused") or cfg.get("paused") or [])}
    rows = []
    for pool in capacity.get("pools") or []:
        engine = str(pool.get("engine") or "pool")
        name = str(pool.get("pool") or "pool")
        pct = pool.get("remaining_pct")
        jobs = pool.get("jobs_left")
        reset = pool.get("reset_epoch")
        fresh = not pool.get("stale") and (reset is None or int(reset) > now)
        key = f"capacity:{engine}:{name}"
        label = f"{engine} {name}"
        if pool.get("paused") or engine.lower() in paused:
            rows.append(_row(key, "pass", "capacity", label, "paused", "paused",
                             "provider is paused in paused-providers"))
            continue
        if pool.get("absent") and not pool.get("stale"):
            rows.append(_row(key, "pass", "capacity", label, "none", "no such window",
                             "the provider reports no such window for this account"))
            continue
        if pool.get("stale") or (reset is not None and int(reset) <= now):
            value = "unread" if pct is None else "stale"
            rows.append(_row(key, "warn", "capacity", label, value, "reset" if reset else "unread", "reading is stale or the window has reset"))
            continue
        if not fresh:
            rows.append(_row(key, "warn", "capacity", label, "unread", "unknown", "no fresh capacity reading"))
            continue
        status = "pass"
        if pct is not None and float(pct) <= 0:
            status = "fail"
        elif jobs is not None and float(jobs) <= 0:
            status = "fail"
        elif pct is not None and float(pct) <= near_pct:
            status = "warn"
        elif jobs is not None and float(jobs) <= near_jobs:
            status = "warn"
        if jobs is not None and (pct is None or float(jobs) <= near_jobs) and status != "pass":
            value = f"{_num(jobs)} jobs"
        elif pct is None:
            value = "unread"
        else:
            value = _pct(pct)
        datum = _resets(reset, now, tz) if reset else "no reset"
        detail = f"{value}; {datum}; near { _num(near_pct) }% or {_num(near_jobs)} jobs"
        rows.append(_row(key, status, "capacity", label, value, datum, detail))
    return rows


def _agents(board, now):
    if not board:
        return []
    stale = []
    conflicts = []
    for claim in board.get("claims") or []:
        state = claim.get("state") or claim.get("status") or "active"
        if state in ("done", "released"):
            continue
        if state == "stale" or _claim_expired(claim, now):
            stale.append(claim.get("work") or "claim")
        if claim.get("conflicts"):
            conflicts.append(claim.get("work") or "claim")
    unregistered = []
    for row in board.get("presence") or []:
        if (row.get("event") or "") == "end":
            continue
        ident = str(row.get("id") or "")
        if REGISTERED.fullmatch(ident):
            continue
        unregistered.append(ident or row.get("project") or "session")
    rows = []
    if stale:
        rows.append(_row("agent:stale", "warn", "agent", "claims", f"{len(stale)} stale", stale[0], ", ".join(stale)))
    if conflicts:
        rows.append(_row("agent:conflict", "fail", "agent", "conflicts", f"{len(conflicts)}", conflicts[0], ", ".join(conflicts)))
    if unregistered:
        rows.append(_row("agent:session", "warn", "agent", "sessions", f"{len(unregistered)}", "unregistered", ", ".join(unregistered)))
    return rows


def _reported_jobs(hosts):
    """Labels that already have a job row of their own."""
    found = set()
    for host in hosts.values():
        if not isinstance(host, dict) or not host.get("ok"):
            continue
        for job in host.get("jobs") or []:
            if isinstance(job, dict) and job.get("label"):
                found.add(str(job["label"]))
    return found


def _projects(projects, reported=frozenset(), prefixes=("com.example.",)):
    rows = []
    for project in projects:
        name = project.get("name") or "project"
        # The repo's own check is a row of its own, so a passing check cannot hide a dirty or
        # failing checkout on the project row below, and a failing one does not replace it.
        result = project.get("check_result")
        if isinstance(result, dict) and result:
            status = result.get("status") if result.get("status") in ("pass", "warn", "fail") else "fail"
            detail = str(result.get("detail") or "")
            host = str(result.get("host") or "")
            rows.append(_row(
                f"check:{name}", status, "check", name,
                str(result.get("value") or status),
                str(result.get("datum") or "check"),
                f"{detail} (read on {host})" if detail and host else detail or (f"read on {host}" if host else ""),
            ))
        elif project.get("check_declared"):
            rows.append(_row(
                f"check:{name}", "warn", "check", name, "missing", "no result",
                "this repo's .fleet/check names a result file, and no Mac has written one",
            ))
        git = project.get("git") or {}
        ci = project.get("ci") or {}
        conclusion = (ci.get("conclusion") or "") if ci else ""
        failed_jobs = [job for job in project.get("jobs") or [] if _bad_exit(job.get("exit", job.get("status")))]
        # A job on a Mac that reported in is judged in its own job row, with what launchd says
        # now. Failing the project for it as well counts one fault twice.
        bad_jobs = [job for job in failed_jobs if str(job.get("label") or "") not in reported]
        noted = [job for job in failed_jobs if job not in bad_jobs]
        dirty = bool(git.get("dirty"))
        ahead = _int(git.get("ahead"))
        behind = _int(git.get("behind"))
        unpushed = _int(git.get("unpushed"))
        if conclusion in ("failure", "failed", "cancelled") or bad_jobs:
            status = "fail"
            if bad_jobs:
                value, datum = "job", _short_label(str(bad_jobs[0].get("label") or "job"), prefixes)
            else:
                value, datum = "ci", conclusion or "failure"
        elif dirty or ahead or behind or unpushed:
            status = "warn"
            if dirty:
                value = "dirty"
            elif behind:
                value = "behind"
            else:
                value = "ahead"
            bits = []
            if ahead or unpushed:
                bits.append(f"↑{max(ahead, unpushed)}")
            if behind:
                bits.append(f"behind {behind}")
            datum = " ".join(bits) or "uncommitted"
        else:
            status, value, datum = "pass", "clean", "ci pass" if conclusion == "success" else "no ci"
        detail = f"{'dirty' if dirty else 'clean'}; ahead {ahead}; behind {behind}; unpushed {unpushed}; ci {conclusion or '—'}"
        if bad_jobs:
            detail = f"{detail}; job {bad_jobs[0].get('label')} exit {bad_jobs[0].get('exit')}"
        for job in noted:
            detail = f"{detail}; job {job.get('label')} exit {job.get('exit', job.get('status'))} (see its job row)"
        rows.append(_row(f"project:{name}", status, "project", name, value, datum, detail))
    return rows


PLACEMENT_STALE_S = 3 * 3600
UNCLAIMED_WARN_GB = 50
MAP_STALE_S = 26 * 3600
_PERSONAL_CODES = {"personal-copies", "offsite"}


def _signatures(host, sig):
    """Every installed fleet binary is signed with the fleet certificate (`fleet-install check`).
    An ad-hoc copy, put in by hand from a cargo build, makes macOS forget the owner's privacy grants
    for it; the fix is `bin/fleet-install TOOL` on the Mac with the certificate, never a re-grant."""
    if sig.get("ok"):
        return _row(f"sec:{host}:signatures", "pass", "security", "fleet binaries", "signed", "fleet-install check",
                    "every installed fleet binary carries the fleet certificate's signature")
    bad = [str(b) for b in sig.get("bad") or []]
    return _row(f"sec:{host}:signatures", "fail", "security", "fleet binaries", f"{len(bad)} unsigned",
                "fleet-install check",
                "not signed with the fleet certificate (privacy grants are lost until fixed; reinstall with "
                "`bin/fleet-install TOOL` on the Mac with the certificate, do not re-grant): " + "; ".join(bad))


def _data_map_fresh(status, now):
    generated = status.get("generated")
    return (isinstance(generated, (int, float)) and not isinstance(generated, bool)
            and 0 <= int(now) - int(generated) <= 36 * 3600)


def _placement(placement, existing, now=None, copies_mapped=False):
    """Placement gaps, condensed: one row per data set naming every gap (and the hosts where a
    gap is host-specific), and one row for all personal sets that only lack their encrypted
    second copy and off-site copy. A status row gives the result's age; past 3 h it is stale.
    `unclaimed` is a separate reading (placement-unclaimed --write): one row per Mac."""
    if not placement:
        return []
    if placement.get("measured_only"):
        return _unclaimed_rows(placement.get("unclaimed"), now)
    covered = {row.name.split(" ", 1)[0] for row in existing if row.area == "disk"}
    rows = []
    try:
        generated = int(placement.get("generated") or 0)
    except (TypeError, ValueError):
        generated = 0
    source = str(placement.get("source") or "")
    if now is not None:
        age = (int(now) - generated) / 3600 if generated else None
        where = f" from {source}" if source else ""
        if age is None:
            rows.append(_row("place:status", "warn", "place", "placement", "stale", "no time",
                             f"placement status{where} has no generated time"))
        elif age * 3600 > PLACEMENT_STALE_S:
            rows.append(_row("place:status", "warn", "place", "placement", "stale", _hours(age),
                             f"placement status{where} is {_num(age)} h old (stale past 3 h); "
                             "the gaps below are as of then"))
        else:
            rows.append(_row("place:status", "pass", "place", "placement", "fresh", _hours(max(age, 0)),
                             f"placement status{where}, {_num(max(age, 0))} h old"))
    for host in placement.get("hosts") or []:
        state = host.get("state")
        ident = str(host.get("id") or "")
        if state not in ("below-floor", "below-band") or ident in covered:
            continue
        status = "fail" if state == "below-floor" else "warn"
        free = host.get("free_gb")
        rows.append(_row(
            f"place:{ident}", status, "place", ident,
            _gb(free) if isinstance(free, (int, float)) else "—", state,
            f"{ident} {free} GB {state}",
        ))
    google_covered = _gdrive_covered_datasets(placement)
    sets = {}
    for viol in placement.get("violations") or []:
        if not isinstance(viol, dict):
            continue
        name = str(viol.get("dataset") or viol.get("host") or "placement")
        if copies_mapped and str(viol.get("code") or "") in _PERSONAL_CODES:
            continue                              # the data map's row carries copy counts
        if str(viol.get("code") or "") == "offsite" and name in google_covered:
            continue
        sets.setdefault(name, []).append(viol)
    personal = [name for name, viols in sets.items() if _personal_only(viols)]
    if len(personal) < 2:
        personal = []
    if personal:
        rows.append(_row(
            "place:personal", "fail", "place", f"{len(personal)} personal sets", "gap",
            "no encrypted second copy, no offsite",
            "no encrypted second copy, no offsite: " + ", ".join(personal),
        ))
    for name, viols in sets.items():
        if name in personal:
            continue
        gaps = _gap_labels(viols)
        detail = f"{name}: {gaps}"
        if name in google_covered:
            detail += " (encrypted offsite on Google)"
        rows.append(_row(f"place:{name}", "fail", "place", name, "gap", gaps, detail))
    rows.extend(_unclaimed_rows(placement.get("unclaimed"), now))
    return rows


def _unclaimed_rows(doc, now):
    """One row per Mac. Under 50 GB is ok; 50 GB and over warns. Older than 26 h is stale."""
    if not isinstance(doc, dict) or not doc:
        return []
    hosts = doc.get("hosts") if isinstance(doc.get("hosts"), dict) else {}
    stale, when = _map_age(doc.get("generated"), now)
    if not hosts:
        if not stale:
            return []
        return [_row("place:unclaimed", "warn", "place", "unclaimed", "stale", when,
                     f"unclaimed folders are stale ({when})")]
    rows = []
    for host in sorted(hosts):
        entry = hosts[host] if isinstance(hosts[host], dict) else {}
        if stale:
            rows.append(_row(
                f"place:unclaimed:{host}", "warn", "place", str(host), "stale", when,
                f"unclaimed folders on {host} are stale ({when})",
            ))
            continue
        total = _gb_total(entry)
        top = sorted(
            (item for item in (entry.get("top") or []) if isinstance(item, dict)),
            key=lambda item: -_float(item.get("gb")),
        )
        status = "warn" if total >= UNCLAIMED_WARN_GB else "pass"
        rows.append(_row(
            f"place:unclaimed:{host}", status, "place", str(host),
            f"{_num(total)} GB unclaimed", _home_datum(top),
            f"{_num(total)} GB on {host} is not in placement.toml (warn at {UNCLAIMED_WARN_GB:g} GB)",
        ))
    return rows


def _cloud(cloud, now=None):
    """One row per remote: total, largest folder, and folders no dataset offsite or path claims."""
    if not isinstance(cloud, dict) or not cloud:
        return []
    remotes = cloud.get("remotes") if isinstance(cloud.get("remotes"), dict) else {}
    claims = [str(item).strip() for item in (cloud.get("claims") or []) if str(item).strip()]
    stale, when = _map_age(cloud.get("generated"), now)
    if not remotes:
        if not stale:
            return []
        return [_row("cloud:inventory", "warn", "cloud", "cloud", "stale", when,
                     f"cloud inventory is stale ({when})")]
    rows = []
    for name in sorted(remotes):
        entry = remotes[name] if isinstance(remotes[name], dict) else {}
        if stale:
            rows.append(_row(f"cloud:{name}", "warn", "cloud", str(name), "stale", when,
                             f"{name} inventory is stale ({when})"))
            continue
        error = str(entry.get("error") or "").strip()
        if error:
            rows.append(_row(f"cloud:{name}", "warn", "cloud", str(name), "unread", error.splitlines()[0], error))
            continue
        folders = [item for item in (entry.get("folders") or []) if isinstance(item, dict)]
        total = entry.get("total_gb")
        if isinstance(total, bool) or not isinstance(total, (int, float)):
            total = sum(_float(item.get("gb")) for item in folders if "/" not in str(item.get("path") or "").strip("/"))
        missing = _unclaimed_cloud(str(name), entry, folders, claims)
        rows.append(_row(
            f"cloud:{name}", "warn" if missing else "pass", "cloud", str(name),
            _gb(total), _cloud_datum(folders, missing),
            _cloud_detail(str(name), total, folders, missing),
        ))
    return rows


def _map_age(generated, now):
    """(stale, datum). Age is not judged when the caller passed no `now`."""
    if now is None:
        return False, ""
    try:
        generated = int(generated)
    except (TypeError, ValueError):
        generated = 0
    if not generated:
        return True, "no time"
    age = (int(now) - generated) / 3600
    if age * 3600 > MAP_STALE_S:
        return True, _hours(age)
    return False, ""


def _gb_total(entry):
    total = entry.get("gb_total")
    if isinstance(total, bool) or not isinstance(total, (int, float)):
        return sum(_float(item.get("gb")) for item in (entry.get("top") or []) if isinstance(item, dict))
    return float(total)


def _float(value):
    try:
        return float(value)
    except (TypeError, ValueError):
        return 0.0


def _home_datum(top):
    parts = []
    for item in list(top)[:2]:
        path = str(item.get("path") or "").strip("/")
        if path.startswith("~") or path.startswith("/"):
            shown = path
        else:
            shown = f"~/{path}"
        parts.append(f"{shown} {_num(_float(item.get('gb')))} GB")
    return ", ".join(parts)


def _cloud_datum(folders, missing):
    ranked = sorted(folders, key=lambda item: -_float(item.get("gb")))
    if ranked:
        path = str(ranked[0].get("path") or "").strip("/")
        head = f"{path} {_num(_float(ranked[0].get('gb')))} GB"
    else:
        head = "no folders"
    if missing:
        return head + ", unclaimed: " + ", ".join(missing)
    return head


def _cloud_detail(name, total, folders, missing):
    text = f"{name} {_num(_float(total))} GB"
    if missing:
        text += "; unclaimed: " + ", ".join(missing)
    elif folders:
        text += "; every folder is claimed"
    return text


def _unclaimed_cloud(name, entry, folders, claims):
    paths = [str(item.get("path") or "").strip("/") for item in folders]
    paths = [path for path in paths if path]
    size = {str(item.get("path") or "").strip("/"): _float(item.get("gb")) for item in folders}
    missing = []
    for path in paths:
        if _cloud_claimed(name, entry, path, claims):
            continue
        if any(other != path and path.startswith(other + "/") and not _cloud_claimed(name, entry, other, claims)
               for other in paths):
            continue
        missing.append(path)
    missing.sort(key=lambda path: (-size.get(path, 0), path))
    return missing


def _cloud_claimed(name, entry, path, claims):
    ids = {name}
    stem = str(entry.get("remote") or "").split(":", 1)[0].strip()
    if stem:
        ids.add(stem)
    for claim in claims:
        remote, claim_path, whole = _parse_claim(claim, ids)
        if remote and remote not in ids:
            continue
        if whole or _path_covers(path, claim_path):
            return True
    return False


def _parse_claim(claim, ids):
    claim = claim.strip().strip("/")
    if ":" in claim:
        remote, path = claim.split(":", 1)
        path = path.strip().strip("/")
        return remote.strip(), path, path == ""
    if claim in ids:
        return claim, "", True
    return None, claim, False


def _path_covers(path, claim_path):
    if not claim_path:
        return False
    return path == claim_path or path.startswith(claim_path + "/") or claim_path.startswith(path + "/")


def _norm_rel(path):
    text = str(path or "").strip()
    if text.startswith("~/"):
        text = text[2:]
    return text.strip("/")


def _under(path, root):
    path = str(path or "").rstrip("/")
    root = str(root or "").rstrip("/")
    return bool(root) and (path == root or path.startswith(root + "/"))


def _gdrive_run_ok(rec):
    if not isinstance(rec, dict):
        return False
    ok = rec.get("last_ok")
    return not isinstance(ok, bool) and isinstance(ok, (int, float)) and ok > 0


def _gdrive_pack_ok(rec):
    """A pack counts only with last_ok and (when shards are recorded) non-empty shards with none partial."""
    if not _gdrive_run_ok(rec):
        return False
    shards = rec.get("shards")
    if shards is not None:
        if not isinstance(shards, dict) or not shards:
            return False
        for shard in shards.values():
            if not isinstance(shard, dict) or shard.get("partial"):
                return False
    return True


def _gdrive_path_covered(entry, dest, rel_path):
    """True when one host's Google Drive backup receipts cover DEST without gaps or partial packs."""
    if not isinstance(entry, dict):
        return False
    receipts = entry.get("receipts")
    if not isinstance(receipts, dict) or not receipts:
        return False
    dest = str(dest or "").rstrip("/")
    rel_path = _norm_rel(rel_path)
    sources = [s for s in (entry.get("sources") or []) if isinstance(s, dict)]

    # Configured sources (prefer longest matching root first, mirroring gdrive-backup/proof.py).
    matching = []
    for src in sources:
        root = str(src.get("path") or "").rstrip("/")
        if root and _under(dest, root):
            matching.append((len(root), src, root))
    matching.sort(key=lambda item: item[0], reverse=True)

    for _length, src, root in matching:
        name = str(src.get("name") or "")
        packs = {str(p).strip("/") for p in (src.get("packs") or []) if str(p).strip("/")}
        prefix = f"{name}:pack:"
        for key in receipts:
            if isinstance(key, str) and key.startswith(prefix):
                rel = key[len(prefix):].strip("/")
                if rel:
                    packs.add(rel)

        # Packs decide when DEST is inside a pack; packs inside DEST must all be complete.
        enclosing = []
        inner_incomplete = False
        for pack_rel in packs:
            pack_abs = f"{root}/{pack_rel}"
            done = _gdrive_pack_ok(receipts.get(f"{name}:pack:{pack_rel}"))
            if _under(dest, pack_abs):
                enclosing.append((len(pack_abs), done))
            elif _under(pack_abs, dest) and not done:
                inner_incomplete = True
        if enclosing:
            enclosing.sort(key=lambda item: item[0], reverse=True)
            if enclosing[0][1]:
                return True
            continue
        if inner_incomplete:
            continue

        # Priority requests (req:PATH) cover DEST when finished (and no inner pack is incomplete).
        for key, rec in receipts.items():
            if isinstance(key, str) and key.startswith("req:"):
                req_root = key[4:].rstrip("/")
                if _under(dest, req_root) and _gdrive_run_ok(rec):
                    return True

        # Source-level file sync covers unmasked paths.
        masks = [str(m).strip("/") for m in (src.get("masks") or []) if str(m).strip("/")]
        if any(_under(dest, f"{root}/{m}") for m in masks):
            continue
        if _gdrive_run_ok(receipts.get(name)):
            return True

    # Standalone req:PATH or pack receipts when no source config was attached for this host.
    if not matching:
        if not sources and rel_path:
            enclosing = []
            inner_incomplete = False
            for key, rec in receipts.items():
                if not isinstance(key, str) or ":pack:" not in key:
                    continue
                pack_rel = key.split(":pack:", 1)[1].strip("/")
                if not pack_rel:
                    continue
                done = _gdrive_pack_ok(rec)
                if _under(rel_path, pack_rel):
                    enclosing.append((len(pack_rel), done))
                elif _under(pack_rel, rel_path) and not done:
                    inner_incomplete = True
            if enclosing:
                enclosing.sort(key=lambda item: item[0], reverse=True)
                return enclosing[0][1]
            if inner_incomplete:
                return False
        for key, rec in receipts.items():
            if isinstance(key, str) and key.startswith("req:"):
                req_root = key[4:].rstrip("/")
                if _under(dest, req_root) and _gdrive_run_ok(rec):
                    return True
    return False


def _gdrive_dataset_covered(dataset, gdrive):
    """True when every path of DATASET has a finished, non-partial Google copy on a host holding it."""
    paths = [_norm_rel(p) for p in (dataset.get("paths") or []) if _norm_rel(p)]
    if not paths:
        return False
    master = str(dataset.get("master_host") or "")
    volumes = []
    for vol in [dataset.get("master_volume"), *(dataset.get("replica_volumes") or [])]:
        text = str(vol or "").strip()
        if text and text not in volumes:
            volumes.append(text)
    keep = [str(h) for h in (dataset.get("keep") or []) if str(h)]
    if "*" in keep:
        holders = list(gdrive.keys())
    else:
        holders = []
        for host in [*keep, master]:
            if host and host in gdrive and host not in holders:
                holders.append(host)
        if not holders and not master and not keep:
            holders = list(gdrive.keys())
    if not holders:
        return False
    for rel_path in paths:
        covered = False
        for host in holders:
            entry = gdrive.get(host)
            if not isinstance(entry, dict):
                continue
            home = str(entry.get("home") or "").rstrip("/")
            candidates = []
            if home:
                candidates.append(f"{home}/{rel_path}")
            if host == master:
                for vol in volumes:
                    candidates.append(f"/Volumes/{vol}/{rel_path}")
            if not candidates:
                candidates.append(rel_path)
            if any(_gdrive_path_covered(entry, cand, rel_path) for cand in candidates):
                covered = True
                break
        if not covered:
            return False
    return True


def _gdrive_covered_datasets(placement):
    datasets = placement.get("datasets") or []
    gdrive = placement.get("gdrive") or {}
    if not isinstance(datasets, list) or not isinstance(gdrive, dict) or not gdrive:
        return set()
    return {
        str(ds["name"])
        for ds in datasets
        if isinstance(ds, dict) and ds.get("name") and _gdrive_dataset_covered(ds, gdrive)
    }


def _personal_only(viols):
    codes = {str(v.get("code") or "") for v in viols}
    if codes != _PERSONAL_CODES:
        return False
    return all(str(v.get("detail") or "").startswith("0 ") for v in viols if v.get("code") == "personal-copies")


def _gap_labels(viols):
    """`master empty on hub, not placed on hub,shared`: one label per kind of gap, in first-seen
    order; host-specific gaps name their hosts."""
    labels = {}
    for viol in viols:
        code = str(viol.get("code") or "")
        detail = str(viol.get("detail") or code or "gap")
        host = str(viol.get("host") or "")
        if code == "offsite":
            label, specific = "no offsite", False
        elif code == "personal-copies":
            label, specific = detail, False
        elif code == "misplaced" and detail == "archive":
            label, specific = "archive held", True
        else:
            label, specific = detail, code in ("master", "misplaced")
        hosts = labels.setdefault(label, [])
        if specific and host and host not in hosts:
            hosts.append(host)
    return ", ".join(f"{label} on {','.join(hosts)}" if hosts else label for label, hosts in labels.items())


def _coverage(coverage, now):
    if not coverage:
        return []
    generated = coverage.get("generated_epoch") or coverage.get("generated") or 0
    try:
        generated = int(generated)
    except (TypeError, ValueError):
        generated = 0
    if generated and now - generated <= 24 * 3600:
        return [_row("coverage:evidence", "pass", "place", "coverage", "fresh", _hours((now - generated) / 3600), "coverage.json is under a day old")]
    return [_row("coverage:evidence", "warn", "place", "coverage", "stale", "over 24 h", "coverage.json missing or older than a day")]


def _security_evidence(doc, now):
    if not isinstance(doc, dict):
        return []
    try:
        generated = int(doc.get("generated") or 0)
    except (TypeError, ValueError):
        generated = 0
    if doc.get("error"):
        return [_row("sec:verify", "warn", "security", "security evidence", "unread", "check.json", str(doc["error"]))]
    if generated <= 0 or generated > now + 300:
        return [_row("sec:verify", "warn", "security", "security evidence", "invalid time", "check.json",
                     "verifier output has no usable collection timestamp")]
    age_s = max(0, now - generated)
    if age_s > 2 * 3600:
        age_label = f"{age_s // 3600} h"
        return [_row(
            "sec:verify", "warn", "security", "security evidence", "stale", age_label,
            f"security evidence check.json is {age_label} old (verifier runs every 15 min)",
        )]
    rows = []
    for item in doc.get("rows") or []:
        if not isinstance(item, dict):
            continue
        key = str(item.get("key") or "sec:row")
        status = str(item.get("status") or "warn")
        if status not in ("pass", "warn", "fail"):
            status = "warn"
        name = str(item.get("name") or key)
        value = str(item.get("value") or "")
        datum = str(item.get("datum") or "")
        detail = str(item.get("detail") or "")
        rows.append(_row(key, status, "security", name, value, datum, detail))
    return rows or [_row("sec:verify", "warn", "security", "security evidence", "empty", "check.json",
                         "verifier output contains no check results")]


def _surface_probes(doc, config):
    """Evaluate public Funnel and tailnet rejection log summaries against burst thresholds."""
    if not isinstance(doc, dict):
        return []
    surfaces = doc.get("surfaces") or []
    if not surfaces:
        return []
    cfg = (config or {}).get("surface_probes") or {}
    ip_warn = int(cfg.get("ip_warn", 20))
    ip_fail = int(cfg.get("ip_fail", 100))
    total_warn = int(cfg.get("total_warn", 50))
    total_fail = int(cfg.get("total_fail", 300))
    window_h = int(doc.get("window_h") or cfg.get("window_h", 24))
    rows = []
    for s in surfaces:
        if not isinstance(s, dict):
            continue
        name = str(s.get("name") or "surface")
        slug = re.sub(r"[^a-z0-9_-]+", "-", name.lower()).strip("-") or "surface"
        key = f"sec:probes:{slug}"
        if not s.get("present"):
            reason = s.get("error") or "missing"
            rows.append(_row(key, "warn", "security", f"{name} probes", reason, "no reading", f"{name} rejection log {reason}"))
            continue
        total = int(s.get("total") or 0)
        top_ip = s.get("top_ip")
        top_count = int(s.get("top_ip_count") or 0)
        unique_ips = int(s.get("unique_ips") or 0)
        if top_count >= ip_fail or total >= total_fail:
            status = "fail"
        elif top_count >= ip_warn or total >= total_warn:
            status = "warn"
        else:
            status = "pass"
        by_status = s.get("by_status") or {}
        codes = " ".join(f"{k}:{v}" for k, v in sorted(by_status.items()))
        if total == 0:
            value, datum = "0 probes", f"last {window_h}h"
            detail = f"{name}: 0 rejected requests in last {window_h}h"
        else:
            ip_noun = "IP" if unique_ips == 1 else "IPs"
            value = f"{total} err ({unique_ips} {ip_noun})" if unique_ips else f"{total} err"
            datum = f"top {top_ip} x{top_count}" if top_ip else f"last {window_h}h"
            detail = f"{name}: {total} rejected in {window_h}h ({codes}); top IP {top_ip} ({top_count}x)" if top_ip else f"{name}: {total} rejected in {window_h}h ({codes})"
        if s.get("partial"):
            if status == "pass":
                status = "warn"
            datum = "partial log window"
            value = f"at least {total} rejected"
            detail = f"{name}: {total} rejected in the available tail; earlier entries in the {window_h}h window were not read"
        rows.append(_row(key, status, "security", f"{name} probes", value, datum, detail))
    return rows


def _vpn_gateway(host, doc):
    """Evaluate a host's local split-tunnel VPN gateway (PAC, Gluetun exit IP, Transmission UI)."""
    if not isinstance(doc, dict) or not doc:
        return []
    pac_ok = bool(doc.get("pac_ok"))
    vpn_ok = bool(doc.get("vpn_ok"))
    tx_ok = bool(doc.get("transmission_ok"))
    ip = str(doc.get("vpn_ip") or "")
    country = str(doc.get("vpn_country") or "")
    try:
        domains = int(doc.get("domains") or 0)
    except (TypeError, ValueError):
        domains = 0
    key, name = f"sec:vpn:{host}", f"{host} vpn gateway"
    if not pac_ok or not vpn_ok:
        bad = []
        if not pac_ok:
            bad.append("pac")
        if not vpn_ok:
            bad.append("gluetun")
        return [_row(
            key, "fail", "security", name, "down", ",".join(bad),
            f"split-tunnel VPN gateway unhealthy ({', '.join(bad)}); check vpn-gw status / vpn-gw up",
        )]
    loc = f"{ip} {country}".strip()
    if not tx_ok:
        return [_row(
            key, "warn", "security", name, f"up ({ip})", "transmission down",
            f"PAC (:8899, {domains} domains) and Gluetun ({loc}) up, but Transmission (:9091) unreachable",
        )]
    return [_row(
        key, "pass", "security", name, f"up ({ip})", f"{domains} domains",
        f"PAC (:8899, {domains} domains), Gluetun ({loc}), Transmission (:9091) all up",
    )]


def _agentkit(kit):
    if not kit or kit.get("absent"):
        return []
    rows = []
    issues = kit.get("issues") or []
    if kit.get("ok") is False or issues:
        text = ", ".join(str(item) for item in issues) or "doctor failed"
        rows.append(_row("kit:doctor", "fail", "other", "agentkit", "fail", "doctor", text))
    else:
        rows.append(_row("kit:doctor", "pass", "other", "agentkit", "ok", "doctor", "agentkit doctor ok"))
    conformance = kit.get("conformance") or {}
    providers = conformance.get("providers") or []
    for block in providers:
        pid = str(block.get("id") or "agentkit")
        for guard in block.get("guards") or []:
            name = str(guard.get("name") or "guard")
            installed = guard.get("installed")
            trusted = guard.get("trusted")
            firing = guard.get("firing")
            ok = installed and trusted and firing
            detail = str(guard.get("detail") or "")
            # The digest's own Mac runs the doctor. A guard the owner turned off in that Mac's
            # site config is a choice on record, not a fault to file every morning.
            if not ok and detail.startswith("disabled (site)"):
                rows.append(_row(f"kit:{pid}:{name}", "info", "other", f"{site.hub()}:fleet", "off",
                                 f"{pid}/{name}", f"{detail}: turned off in this Mac's agentkit.toml"))
                continue
            rows.append(
                _row(
                    f"kit:{pid}:{name}",
                    "pass" if ok else "fail",
                    "other",
                    f"{site.hub()}:fleet",
                    "ok" if ok else "fail",
                    f"{pid}/{name}",
                    detail or ("conformance ok" if ok else "not installed/trusted/firing"),
                )
            )
    return rows


def _data_map(host, status, now):
    """The data map (`bin/data-map`): every data set, where its copies are, and its verdict.

    One row. It fails when personal data sets are short of two verified copies with one off-site,
    and it fails when the map is more than 36 hours old: the 21 Nov 2026 map was written once
    and never refreshed, so a week later nobody could say what was where.
    """
    generated = status.get("generated")
    if not _data_map_fresh(status, now):
        age = _hours((now - int(generated)) / 3600) + " ago" if isinstance(generated, (int, float)) and not isinstance(generated, bool) else "never"
        return _row(f"data:map:{host}", "fail", "backup", "data map", "stale", "not refreshed",
                    f"the data map on {host} was last generated {age}; run data-map and check its job")
    word = {"fail": "fail", "warn": "warn"}.get(status.get("status"), "pass")
    return _row(f"data:map:{host}", word, "backup", "data map", status.get("value") or "generated",
                "copies", status.get("detail") or "")


def _server_health(status, now):
    """Rented servers outside the fleet, as `bin/server-health` last read them over ssh.

    Three rows a server: its backups (each unit's last result and age), its services (units
    systemd marks failed, named services not active) and its root disk. A reading older than
    two hours, or a server ssh could not reach, fails: an unwatched server is the fault this
    exists for (28 Nov 2026: a nightly database backup failed after a migration, unseen).
    """
    rows = []
    checked = status.get("checked_at")
    stale = not checked or now - int(checked) > 2 * 3600
    if status.get("config_error"):
        rows.append(_row("server:config", "fail", "backup", "server list", "unreadable", "servers.json",
                         str(status["config_error"])))
    for server in status.get("servers") or []:
        name = str(server.get("name") or "server")
        if stale:
            age = "never" if not checked else _hours((now - int(checked)) / 3600)
            rows.append(_row(f"server:{name}:reach", "fail", "backup", f"{name} health", "stale", "tick stopped",
                             f"server-health last read {name} {age} ago; the job is not ticking"))
            continue
        if not server.get("reachable"):
            rows.append(_row(f"server:{name}:reach", "fail", "backup", f"{name} health", "unreachable", "ssh",
                             f"server-health could not read {name}: {server.get('error') or 'no reply'}"))
            continue
        worst, parts, newest = "pass", [], None
        for backup in server.get("backups") or []:
            unit = str(backup.get("unit"))
            last = backup.get("last_exit_epoch")
            if backup.get("loadstate") not in (None, "loaded"):
                word, note = "fail", f"{unit}: unit not loaded"
            elif backup.get("result") not in ("success", None) or backup.get("activestate") == "failed":
                word, note = "fail", f"{unit}: last run {backup.get('result') or 'failed'}"
            elif last is None:
                word, note = "fail", f"{unit}: never ran"
            else:
                age = (now - int(last)) / 3600
                word = ("fail" if age >= float(backup.get("fail_h", 54))
                        else "warn" if age >= float(backup.get("warn_h", 30)) else "pass")
                note = f"{unit}: ok {_hours(age)} ago"
                newest = age if newest is None else min(newest, age)
            worst = _worse(worst, word)
            parts.append(note)
        if parts:
            value = "ok" if worst == "pass" else "late" if worst == "warn" else "failed"
            datum = f"{len(parts)} backups" if newest is None else f"newest {_hours(newest)}"
            rows.append(_row(f"server:{name}:backup", worst, "backup", f"{name} backup", value, datum, "; ".join(parts)))
        ignore = {str(unit) for unit in server.get("ignore_failed") or []}
        failed = [str(unit) for unit in server.get("failed_units") or [] if str(unit) not in ignore]
        down = [f"{unit} {state}" for unit, state in sorted((server.get("services") or {}).items()) if state != "active"]
        if failed or down:
            detail = "; ".join(([f"failed units: {', '.join(failed)}"] if failed else [])
                               + ([f"not active: {', '.join(down)}"] if down else []))
            rows.append(_row(f"server:{name}:services", "fail", "job", f"{name} services",
                             f"{len(failed) + len(down)} down", (failed + down)[0], detail))
        else:
            count = len(server.get("services") or {})
            rows.append(_row(f"server:{name}:services", "pass", "job", f"{name} services", "ok",
                             f"{count} active", f"{count} named services active; no failed units"))
        pct = server.get("disk_pct")
        if pct is not None:
            warn_pct = float(server.get("disk_warn_pct", 80))
            fail_pct = float(server.get("disk_fail_pct", 90))
            word = "fail" if pct >= fail_pct else "warn" if pct >= warn_pct else "pass"
            rows.append(_row(f"server:{name}:disk", word, "disk", f"{name} disk", _pct(pct),
                             f"warn {_num(warn_pct)}%", f"root disk {_pct(pct)} used; warn {_num(warn_pct)}%, fail {_num(fail_pct)}%"))
    return rows


def _idrive_ingest(name, status, cfg, now):
    """The daily IDrive Linux-client backup (`idrive-ingest-tick` writes status.json every 30 min).

    Judged by its last successful run: warn and fail at the IDrive ages. A first backup of a
    large set runs for days; while it runs and is younger than `idrive_seed_fail_d` it is a
    warning that says how far it has got, not a failure.
    """
    key, label = f"backup:{name}:idrive-ingest", f"{name} idrive-ingest"
    warn_h = float(cfg.get("idrive_warn_h", 36))
    fail_h = float(cfg.get("idrive_fail_h", 72))
    seed_fail_h = float(cfg.get("idrive_seed_fail_d", 7)) * 24
    storage = status.get("storage") or {}
    used = (f"; IDrive {_num(storage['used_gb'])} of {_num(storage['total_gb'])} GB"
            if storage.get("used_gb") is not None and storage.get("total_gb") else "")
    checked = status.get("checked_at")
    if not checked or now - int(checked) > 2 * 3600:
        age = "never" if not checked else _hours((now - int(checked)) / 3600)
        return _row(key, "fail", "backup", label, "stale", "tick stopped",
                    f"status.json last written {age} ago; the idrive-ingest job is not ticking{used}")
    problems = [str(item) for item in status.get("problems") or []]
    if problems:
        return _row(key, "fail", "backup", label, "down", "check client", "; ".join(problems) + used)
    last = status.get("last_success_epoch")
    success = status.get("last_success") or {}
    if status.get("cycle"):
        last = status.get("last_full_success_epoch")
        phase = status["cycle"].get("phase", "unknown")
        used += f"; phase {phase}; priority completions do not count as a full backup"
        success = ((status["cycle"].get("receipts", {}).get("bulk") or {}).get("success") or {})
    running = bool(status.get("running"))
    since = status.get("started_at")
    progress = status.get("progress") or {}
    done = ", ".join(f"{_num(progress[k])} {k}" for k in ("scanned", "backed_up") if progress.get(k) is not None)
    if last is None:
        if running and since:
            ran = (now - int(since)) / 3600
            status_word = "fail" if ran >= seed_fail_h else "warn"
            return _row(key, status_word, "backup", label, "seeding", f"running {_hours(ran)}",
                        f"first backup running {_hours(ran)}{'; ' + done if done else ''}; fail after "
                        f"{_num(seed_fail_h / 24)} d{used}")
        return _row(key, "fail", "backup", label, "none", "no success",
                    f"no successful backup recorded{'; running' if running else ''}{used}")
    row = _stamp(key, label, last, warn_h, fail_h, now, "no backup")
    detail = f"{row.detail}; {_num(success.get('considered') or 0)} files considered"
    status_word = row.status
    # Coverage: nothing may miss the backup unnoticed (owner, 15 Nov 2026).
    audit = status.get("coverage") or {}
    gaps = []
    if audit.get("unplaced"):
        gaps.append("not placed: " + ", ".join(str(v) for v in audit["unplaced"]))
    if audit.get("unexplained_masks"):
        gaps.append("masks without a reason: " + ", ".join(str(v) for v in audit["unexplained_masks"]))
    missing = [s.get("name") for s in status.get("sources") or [] if s.get("missing") and not s.get("waiting")]
    if missing:
        gaps.append("sources missing: " + ", ".join(str(v) for v in missing))
    if audit.get("gap") is not None and float(audit["gap"]) > float(cfg.get("idrive_gap_warn", 0.005)):
        gaps.append(f"{_num(float(audit['gap']) * 100)}% of files on disk not considered")
    if gaps:
        status_word = _worse(status_word, "warn")
        detail += "; " + "; ".join(gaps)
    if success.get("failed"):
        status_word = _worse(status_word, "warn")
        detail += f"; {_num(success['failed'])} failed"
    if running and since:
        detail += f"; next run going {_hours((now - int(since)) / 3600)}{' (' + done + ')' if done else ''}"
        if status_word == "fail" and now - int(since) < seed_fail_h:
            status_word = "warn"   # a long run is under way; the stale success is not the last word
    return _row(key, status_word, "backup", label, row.value, row.datum, detail + used)


def _home_mirror(name, status, cfg, now):
    """A laptop's copy of its home on the hub, as two rows that never colour each other: the full
    home, and the personal pass (placement's personal-unique data sets, copied first, one at a
    time, each with its own space check; `personal` in the status). A laptop away from home for a
    few days is normal: amber at two days, red at a week; the reason a pass was skipped is in the
    detail. A status with no personal record comes from a home-mirror older than the personal
    pass: amber until that pass has run. A Mac with no personal data sets ("not applicable") has
    no personal row."""
    rows = [_home_mirror_row(f"backup:{name}:home-mirror", f"{name} home-mirror", status, cfg, now)]
    personal = status.get("personal")
    if not isinstance(personal, dict):
        personal = {"state": "no personal pass recorded; this home-mirror has not run one"}
    if not str(personal.get("state") or "").startswith("not applicable"):
        rows.append(_home_mirror_personal(f"backup:{name}:home-mirror-personal", f"{name} home-mirror personal",
                                          personal, cfg, now))
    return rows


def _home_mirror_personal(key, label, personal, cfg, now):
    """Green only when every personal data set has a recent good copy. A set waiting for space is
    amber whatever its age, with the room it needs; so is one never copied, or stopped part-way.
    Red is a set that failed with no copy to its name, or one whose last copy is a week old."""
    sets = [s for s in personal.get("sets") or [] if isinstance(s, dict)]
    if not sets:
        return _home_mirror_row(key, label, personal, cfg, now, "personal ")
    warn_h = float(cfg.get("home_mirror_warn_h", 48))
    fail_h = float(cfg.get("home_mirror_fail_h", 168))
    word, fresh, ages, waiting, others = "pass", 0, [], [], []
    for item in sets:
        state, last = str(item.get("state") or ""), item.get("last_success_epoch")
        age = None if last is None else (now - int(last)) / 3600
        mark = "warn" if age is None else "fail" if age >= fail_h else "warn" if age >= warn_h else "pass"
        if state.startswith("waiting for space"):
            mark = "warn"
            waiting.append(f"{item.get('name')} needs {_num(item.get('required_free_gb') or 0)} GB free "
                           f"({_num((item.get('planned_bytes') or 0) / 1e9)} GB to copy)")
        elif state.startswith("failed"):
            mark = "fail" if age is None else _worse(mark, "warn")
        if mark == "pass":
            fresh += 1
            ages.append(age)
        elif not state.startswith("waiting for space"):
            others.append(f"{item.get('name')}: {state or 'not run'}"
                          + (f", last copied {_hours(age)} ago" if age is not None else ""))
        word = _worse(word, mark)
    state = str(personal.get("state") or "")
    if state.startswith("failed") and not any(str(s.get("state")).startswith("failed") for s in sets):
        word = _worse(word, "warn")   # e.g. placement.toml could not be read this time: the list is stale
        others.append(state)
    copied = sum(1 for s in sets if s.get("last_success_epoch"))
    detail = f"{copied} of {len(sets)} sets copied"
    if waiting:
        detail += "; waiting for space: " + ", ".join(waiting)
    if others:
        detail += "; " + "; ".join(others)
    skipped = "; ".join(f"{item.get('path')} ({item.get('why')})" for item in personal.get("skipped") or []
                        if isinstance(item, dict))
    if skipped:
        detail += f"; left out: {skipped}"
    if word == "pass":
        return _row(key, word, "backup", label, _hours(max(ages)), f"floor {_num(warn_h)} h",
                    f"oldest set copied {_hours(max(ages))} ago; warn {_num(warn_h)} h; fail {_num(fail_h)} h; {detail}")
    datum = f"{len(waiting)} waiting" if waiting else f"{len(sets) - fresh} to copy"
    return _row(key, word, "backup", label, f"{fresh} of {len(sets)} sets", datum, detail)


def _home_mirror_row(key, label, status, cfg, now, what=""):
    """One pass's row from that pass's own record: its last good copy, its last state."""
    warn_h = float(cfg.get("home_mirror_warn_h", 48))
    fail_h = float(cfg.get("home_mirror_fail_h", 168))
    state = str(status.get("state") or "")
    last = status.get("last_success_epoch")
    if last is None:
        word = "fail" if state.startswith("failed") else "warn"
        return _row(key, word, "backup", label, "none", "no copy yet",
                    f"no complete {what}copy on the hub yet; {state or 'not run'}")
    row = _stamp(key, label, last, warn_h, fail_h, now, "no copy")
    word = row.status
    if state.startswith("failed"):
        word = _worse(word, "warn")
    detail = f"{row.detail}; last pass: {state or 'unknown'}"
    if status.get("files") is not None:
        detail += f"; {_num(status['files'])} files sent"
    return _row(key, word, "backup", label, row.value, row.datum, detail)


def _age_row(key, area, name, payload, field, warn_h, fail_h, now, missing):
    if not payload or payload.get(field) is None:
        return _row(key, "fail", area, name, "none", missing, missing)
    return _stamp(key, name, payload.get(field), warn_h, fail_h, now, missing)


def _stamp(key, name, epoch, warn_h, fail_h, now, missing):
    if epoch is None:
        return _row(key, "fail", "backup", name, "none", missing, missing)
    age = (now - int(epoch)) / 3600
    if age >= float(fail_h):
        status = "fail"
        datum = f"floor {_num(fail_h)} h"
    elif age >= float(warn_h):
        status = "warn"
        datum = f"floor {_num(warn_h)} h"
    else:
        status = "pass"
        datum = f"floor {_num(warn_h)} h"
    detail = f"age {_hours(age)}; warn {_num(warn_h)} h; fail {_num(fail_h)} h"
    return _row(key, status, "backup", name, _hours(age), datum, detail)


def _runs(runs):
    if isinstance(runs, bool) or runs is None or runs == "":
        return 0
    try:
        return int(runs)
    except (TypeError, ValueError):
        return 0


def _missed(when, last, now, grace_h, tz, runs=None):
    text = (when or "").strip()
    interval = re.fullmatch(r"every (\d+)([hms])", text)
    if interval:
        count = int(interval.group(1))
        mult = {"h": 3600, "m": 60, "s": 1}[interval.group(2)]
        limit = 2 * count * mult
        # launchctl's run count is the scheduler. A quiet log is not a missed
        # interval when the job has actually been started and last exited 0.
        if _runs(runs) > 0:
            return False, text
        if last is None:
            return (_runs(runs) == 0 and runs is not None), text
        late = now - int(last) > limit
        return late, text
    daily = re.fullmatch(r"daily (\d{2}):(\d{2})", text)
    if daily and last is not None:
        hour, minute = int(daily.group(1)), int(daily.group(2))
        current = datetime.fromtimestamp(now, tz)
        slot = current.replace(hour=hour, minute=minute, second=0, microsecond=0)
        slot_s = int(slot.timestamp())
        if now < slot_s + grace_h * 3600:
            return False, text
        # launchd keeps the time zone the Mac booted in, so after travel a daily job fires
        # hours off its wall-clock slot. It has still run today if it ran within a day.
        if now - int(last) < (24 + grace_h) * 3600:
            return False, text
        return int(last) < slot_s, text
    return False, text


def _hot_hours(samples, now, hot_c, gap_s):
    rows = sorted(samples or [], key=lambda sample: int(sample.get("t") or 0))
    total = 0
    for index, sample in enumerate(rows):
        if not _hot(sample, hot_c):
            continue
        if index + 1 < len(rows):
            gap = int(rows[index + 1].get("t") or 0) - int(sample.get("t") or 0)
        else:
            gap = now - int(sample.get("t") or 0)
        if gap > 0:
            total += min(gap, gap_s)
    return total / 3600


def _hot(sample, hot_c):
    thermal = sample.get("thermal")
    if thermal not in (None, "", "nominal", "ok"):
        return True
    if hot_c is None:
        return False
    chip = sample.get("chipC")
    return isinstance(chip, (int, float)) and not isinstance(chip, bool) and chip >= hot_c


def _claim_expired(claim, now):
    heartbeat = int(claim.get("heartbeat") or 0)
    ttl = int(claim.get("ttl") or 0)
    expires = claim.get("expires")
    if expires is not None:
        return now >= int(expires)
    return bool(ttl) and now > heartbeat + ttl


def _floor(disk_cfg, host, drive):
    floors = (disk_cfg.get("floors") or {}).get(host) or {}
    if drive in floors:
        return float(floors[drive])
    return float(disk_cfg.get("floor_gb", 40))


def _target(targets, host, drive):
    """This Mac's own free-space lines. They are written for the internal disk."""
    row = (targets or {}).get(host) if drive == "internal" else None
    if not isinstance(row, dict):
        return None
    try:
        return {"target_gb": float(row["target_gb"]), "floor_gb": float(row["floor_gb"])}
    except (KeyError, TypeError, ValueError):
        return None


def _retired(job, label, host, registry):
    """A row left over from an out-of-date host record: no plist on that Mac, and the registry
    does not place the job there. Without the registry list nothing is called retired."""
    if registry is None:
        return False
    seen = job.get("plist")
    if seen is None:   # snapshots from before the flag: the probe's fields mark a plist it read
        seen = "runs" in job or "state" in job
    if seen:
        return False
    homes = registry.get(label) or []
    return not ("all" in homes or host in homes)


def _on_utc(job):
    """True when the job runs through the utc-daily wrapper, so its `when` is a UTC time."""
    if job.get("utc"):
        return True
    words = str(job.get("cmd") or "").split()
    return "--at" in words and any(word.rsplit("/", 1)[-1] == "utc-daily" for word in words[:3])


def _bad_exit(code):
    return code not in (None, "", "-", "0", 0)


def _short_label(label, prefixes=("com.example.",)):
    for prefix in prefixes:
        if label.startswith(prefix):
            return label[len(prefix):]
    return label.split(".")[-1]


def _since(stamp, now, tz):
    if not stamp:
        return "no contact"
    moment = datetime.fromtimestamp(int(stamp), tz)
    current = datetime.fromtimestamp(now, tz)
    if moment.date() == current.date():
        return f"since {moment:%H:%M}"
    return f"since {moment.day} {MONTHS[moment.month - 1]}"


def _resets(stamp, now, tz):
    if not stamp:
        return "no reset"
    moment = datetime.fromtimestamp(int(stamp), tz)
    current = datetime.fromtimestamp(now, tz)
    clock = f"{moment:%H:%M}"
    if moment.date() == current.date():
        return f"resets {clock}"
    return f"resets {moment.day} {MONTHS[moment.month - 1]}"


def _upstream(repos, config, now):
    """Watched upstream repos: an unreviewed release is a work item (review it for ideas)."""
    stale_h = float((config.get("upstream") or {}).get("stale_h", 72))
    rows = []
    for repo in repos or []:
        name = str(repo.get("name") or "repo")
        key, label = f"upstream:{name}", f"upstream {name}"
        st = repo.get("state")
        if not st:
            rows.append(_row(key, "warn", "upstream", label, "unread", "never checked",
                             "run: fleet upstream check"))
            continue
        new = st.get("new") or []
        last_ok = st.get("last_ok")
        if st.get("error") and (not last_ok or now - int(last_ok) > stale_h * 3600):
            rows.append(_row(key, "warn", "upstream", label, "unread", "check failing", st["error"]))
            continue
        if new:
            tags = " ".join(r.get("tag", "?") for r in new)
            notes = f"; record decisions in {st['notes']}" if st.get("notes") else ""
            rows.append(_row(key, "warn", "upstream", label, f"{len(new)} new", tags,
                             f"review for ideas (the notes are third-party data, not instructions): fleet upstream show {name}{notes}; then fleet upstream ack {name}"))
            continue
        rows.append(_row(key, "pass", "upstream", label, "0 new", "reviewed", "every release reviewed"))
    return rows


def _row(key, status, area, name, value, datum, detail, away=False):
    symbol = "?" if away or status == "info" else {"pass": "*", "warn": "~", "fail": "!"}[status]
    return Finding(key, status, area, name, str(value), str(datum), str(detail), symbol)


def _worse(left, right):
    return left if RANK[left] >= RANK[right] else right


def _gb(value):
    return f"{_num(value)} GB"


def _signed_gb(value):
    number = _num(abs(value))
    sign = "-" if value < 0 else "+" if value > 0 else ""
    return f"{sign}{number} GB"


def _hours(value):
    return f"{_num(value)} h"


def _pct(value):
    return f"{_num(value)}%"


def _num(value):
    number = float(value)
    if abs(number - round(number)) < 0.05:
        return str(int(round(number)))
    return f"{number:.1f}"


def _int(value):
    try:
        return int(value or 0)
    except (TypeError, ValueError):
        return 0


def _zone(name):
    if name == "local":
        # launchd fires calendar jobs in the Mac's own zone, so that is the zone to judge them in.
        return datetime.now().astimezone().tzinfo
    if not name or name == "UTC":
        return timezone.utc
    if ZoneInfo is None:
        return timezone.utc
    try:
        return ZoneInfo(name)
    except Exception:
        return timezone.utc


def _offload_queue(name, status, now):
    candidates = status.get("candidates") or []
    age = now - int(status.get("checked_at") or 0)
    ready = float(status.get("ready_bytes") or 0) / 1e9
    waiting = float(status.get("waiting_bytes") or 0) / 1e9
    blocked = [r for r in candidates if r.get("state") not in
               ("waiting-idrive", "ready-for-review", "trashed", "verification-deferred")]
    stale = age > 3 * 3600 or age < -120
    level = "warn" if candidates or stale else "pass"
    detail = "; ".join(str(r.get("source", r.get("name", ""))) + ": " + str(r.get("state")) for r in candidates)
    if stale:
        detail = "queue check stale; " + detail
    return _row(f"backup:{name}:offload", level, "backup", f"{name} offload",
                "stale" if stale else f"{ready:.1f} GB review",
                f"{waiting:.1f} GB waiting" + (f"; {len(blocked)} blocked" if blocked else ""),
                detail + "; queued items move to the Bin after the offload tool's final checks; nothing is deleted")


def _offload_autopilot(name, pilot, now):
    """Two rows: the cold-data pipeline (free, queued, waiting on IDrive, ready) and the Bin of verified offloads."""
    if not isinstance(pilot, dict):
        return []
    gb = lambda key, d=pilot: float(d.get(key) or 0) / 1e9
    if pilot.get("error") and "free_bytes" not in pilot:
        return [_row(f"storage:{name}:cold", "warn", "disk", f"{name} cold data", "unread", "autopilot",
                     str(pilot["error"]))]
    flow = pilot.get("pipeline") or {}
    age = now - int(pilot.get("checked_at") or 0)
    stale = age > 3 * 3600 or age < -120
    free, target = gb("free_bytes"), gb("target_bytes")
    picks = pilot.get("picks") or []
    in_flight = sum(float(flow.get(k) or 0) for k in ("queued", "waiting", "ready")) / 1e9
    short = gb("shortfall_bytes")
    uncovered = short > gb("cold_bytes") + 0.5
    level = "warn" if stale or pilot.get("error") or uncovered or flow.get("blocked") else "pass"
    nxt = "; ".join(f'{p["path"]} {float(p["bytes"]) / 1e9:.1f} GB {p.get("class", "?")}' for p in picks[:10])
    detail = (f"target {target:.0f} GB empty; counting the Bin and items in flight {gb('effective_free_bytes'):.0f} GB; "
              f"short {short:.0f} GB; cold and eligible {gb('cold_bytes'):.1f} GB; "
              + (f"next pick: {nxt}" if nxt else "nothing to pick") + "."
              + (f" Cold data cannot cover the shortfall ({short - gb('cold_bytes'):.0f} GB more needed): the rest is warm"
                 " (used in the last 30 days) and needs the owner's call, or a wider scan." if uncovered else "")
              + (f" Blocked {float(flow.get('blocked') or 0) / 1e9:.1f} GB." if flow.get("blocked") else "")
              + (f" {pilot['error']}." if pilot.get("error") else ""))
    rows = [_row(f"storage:{name}:cold", level, "disk", f"{name} cold data",
                 "stale" if stale else f"{free:.0f} GB free",
                 f"queued {float(flow.get('queued') or 0) / 1e9:.0f} · IDrive {float(flow.get('waiting') or 0) / 1e9:.0f} · ready {float(flow.get('ready') or 0) / 1e9:.0f} GB",
                 detail)]
    bin_ = pilot.get("bin") or {}
    held = (float(bin_.get("confirmed_bytes") or 0) + float(bin_.get("named_bytes") or 0)) / 1e9
    rows.append(_row(f"storage:{name}:bin", "warn" if held >= 1 else "pass", "disk", f"{name} Bin",
                     f"{held:.0f} GB offloads", "run empty-bin --verified-offloads" if held >= 1 else "nothing to free",
                     f"The Bin holds {held:.1f} GB of verified offloads ({bin_.get('items', 0)} items). Space returns only when "
                     "the Bin is emptied, and agents never empty it. `empty-bin --verified-offloads` proves each item "
                     "against its verified manifest and deletes only those; everything else stays in the Bin."))
    return rows


def _free_space_guard(name, status, now):
    """One row per laptop: free space against its floor and target, and what the guard freed today.
    Below the target with the guard running is its normal work (pass); below the floor after every step is a failure
    that names the top growers; a guard that has not run for 3 hours is a warning."""
    gb = lambda value: float(value or 0) / 1e9
    key, title = f"storage:{name}:free", f"{name} free space"
    age = now - int(status.get("checked_at") or 0)
    if status.get("state") == "unconfigured":
        return _row(key, "warn", "disk", title, "no targets", "free-space-guard", str(status.get("note") or ""))
    free, floor, target = gb(status.get("free_bytes")), float(status.get("floor_gb") or 0), float(status.get("target_gb") or 0)
    freed = gb(status.get("freed_today_bytes"))
    value = f"{free:.0f} GB free"
    datum = f"floor {floor:.0f}, target {target:.0f}; guard freed {freed:.1f} GB today"
    detail = f"{name} free {free:.0f} GB (floor {floor:.0f}, target {target:.0f}); guard freed {freed:.1f} GB today."
    pilot = status.get("autopilot") or {}
    if pilot.get("bin_bytes"):
        detail += f" The Bin holds {gb(pilot['bin_bytes']):.0f} GB of verified offloads (owner's empty-bin frees it)."
    if status.get("deferred"):
        detail += f" Deferred: {status['deferred']}."
    level = "pass"
    if status.get("state") == "below-floor":
        level = "fail"
        grew = "; ".join(f"{g.get('path')} +{gb(g.get('grew_bytes')):.0f} GB" for g in (status.get("growers") or [])[:5])
        detail += (" Still under the floor after caches, writers and the autopilot. Top growers since "
                   f"{(status.get('growers') or [{}])[0].get('since', '?')}: {grew or 'diskmap has no history'}.")
    if age > 3 * 3600 or age < -300:
        level = "warn" if level == "pass" else level
        value, detail = "stale", f"guard last ran {age / 3600:.0f} h ago. " + detail
    return _row(key, level, "disk", title, value, datum, detail)


def _mac_setup(name, status, now):
    """One row per Mac: how far its setup has drifted from the reference Mac (mac-setup). No drift passes;
    drift warns and names the first items; a pass older than an hour, or a daily package pass older than
    36 hours on a follower, is stale. A refusal fails: the Mac found a file in the synced setup folder
    that the repository's pins do not vouch for and had no verified copy to use instead."""
    key, title = f"kit:{name}:setup", f"{name} setup"
    refused = [str(r) for r in (status.get("refused") or [])]
    if refused:
        return _row(key, "fail", "kit", title, f"{len(refused)} refused", "not vouched for by the repository",
                    f"{name} refused setup files from the synced folder: " + "; ".join(r[:200] for r in refused[:4])
                    + (f"; and {len(refused) - 4} more" if len(refused) > 4 else "")
                    + f". Full list: `mac-setup status` on {name}. If the reference Mac changed them, run `mac-setup pin` "
                    "there, commit and push; otherwise treat the synced folder as altered.")
    drift = [str(d) for d in (status.get("drift") or [])]
    age = now - int(status.get("checked_at") or 0)
    pkg_at = status.get("packages_checked_at")
    level, value = ("pass", "matches") if not drift else ("warn", f"{len(drift)} drift")
    datum = "same as the reference" if not drift else (status.get("packages_deferred") or "see status file")
    detail = (f"{name} setup matches the reference Mac." if not drift else
              f"{name} differs from the reference Mac: " + "; ".join(d[:160] for d in drift[:4])
              + (f"; and {len(drift) - 4} more" if len(drift) > 4 else "")
              + f". Full list: `mac-setup status` on {name}.")
    if status.get("packages_installed"):
        detail += f" Installed today: {', '.join(status['packages_installed'][:8])}."
    if age > 3600 or age < -300:
        level, value = "warn", "stale"
        detail = f"mac-setup last ran {age / 3600:.0f} h ago (its 5-minute launchd job). " + detail
    elif status.get("role") == "follower" and (pkg_at is None or now - int(pkg_at) > 36 * 3600):
        level, value = "warn", "packages stale"
        detail = ("the daily package pass has not run yet (its daily launchd job). " + detail) if pkg_at is None else (
            f"the daily package pass last ran {(now - int(pkg_at)) / 3600:.0f} h ago (its daily launchd job). " + detail)
    return _row(key, level, "kit", title, value, datum, detail)


def _ai_usage(summary, now):
    """Usage history freshness is separate from remaining quota/capacity."""
    rows = []
    for source in (summary or {}).get("sources", []):
        if not isinstance(source, dict):
            rows.append(_row("usage:invalid", "warn", "capacity", "usage history", "unread", "invalid metadata", "Repair the usage metadata import."))
            continue
        try:
            age = (now - float(source["captured_at"])) / 3600
            end = datetime.fromisoformat(source["coverage_end"]).replace(tzinfo=timezone.utc).timestamp()
            lag = (now - end) / 86400
            stale = age > 72 or lag > 5 or age < -1
            collector=(summary or {}).get("local_collector")
            if isinstance(collector,dict) and source["source"] in collector.get("sources",[]):
                check_age=(now-float(collector.get("checked_at",0)))/3600
                stale=collector.get("status")!="ok" or check_age>72 or check_age < -1
            week = source.get("last_full_week") or {}
            total = week.get("total")
            value = f"{total / 1e6:.1f}M observed" if isinstance(total, (int, float)) else "recorded"
            rows.append(_row("usage:" + source["source"], "warn" if stale else "pass", "capacity",
                "usage " + source["provider"] + " " + source["scope"],
                "refresh due" if stale else value, "through " + source["coverage_end"],
                "Token history, not remaining quota. " + str(week.get("start", "")) + " week: " + str(week.get("observed_days", 0)) + " observed dates, source scope " + source["scope"] + ". " + str(source.get("missing_rows", 0)) +
                " missing rows; missing categories/dates are unknown. " + source.get("refresh", "")))
        except (ValueError, TypeError, KeyError):
            rows.append(_row("usage:invalid", "warn", "capacity", "usage history", "unread", "invalid metadata", "Repair the usage metadata import."))
    collector=(summary or {}).get("local_collector")
    if isinstance(collector,dict):
        status=collector.get("status")
        if status not in ("ok","disabled"):
            rows.append(_row("usage:collector", "warn", "capacity", "usage collector", str(status), "local metadata", "Bounded collection incomplete; prior history is preserved, some new records may have been added. Check fleet usage collect-local."))
    for grant in (summary or {}).get("api_grants",[]):
        if not isinstance(grant,dict):continue
        try:
            days=(date.fromisoformat(grant["expires_on"])-datetime.fromtimestamp(now,timezone.utc).date()).days
            rows.append(_row("api-grant:"+str(grant.get("id","unknown")), "warn" if days<=7 else "pass", "capacity", "API credit "+str(grant.get("provider") or "unconfirmed"), "balance unverified" if grant.get("balance") is None else str(grant["balance"])+" "+str(grant.get("currency") or ""), "expires "+grant["expires_on"], "Optional API grant, separate from subscription allowance and token usage; no balance inferred from expiry."))
        except (KeyError,ValueError,TypeError):continue
    return rows
