"""Per-Mac morning snapshots. Syncthing copies the directory; nothing here opens SSH."""
import json
import os
import re
from pathlib import Path

_HOST = re.compile(r"^[A-Za-z0-9._-]+$")


def snapshot_dir(home):
    """Shared folder. mac-sync is the tree Syncthing already replicates."""
    return Path(home) / ".local" / "state" / "mac-sync" / "fleet-snapshots"


def write_snapshot(directory, document):
    """Atomically publish `<host>.json`. A crash keeps the previous file."""
    host = str((document or {}).get("host") or "")
    if not _HOST.fullmatch(host):
        raise ValueError(f"bad host id: {host}")
    directory = Path(directory)
    directory.mkdir(parents=True, exist_ok=True)
    path = directory / f"{host}.json"
    payload = json.dumps(document, indent=2) + "\n"
    tmp = directory / f".{host}.{os.getpid()}.tmp"
    try:
        tmp.write_text(payload)
        os.replace(tmp, path)
    finally:
        if tmp.exists():
            tmp.unlink()
    return path


def read_snapshots(directory):
    """Host id → document. A corrupt file is skipped so one Mac cannot blank the digest."""
    directory = Path(directory)
    found = {}
    if not directory.is_dir():
        return found
    for path in sorted(directory.glob("*.json")):
        if not path.is_file() or path.name.startswith("."):
            continue
        try:
            document = json.loads(path.read_text())
        except (OSError, ValueError):
            continue
        if not isinstance(document, dict):
            continue
        host = str(document.get("host") or path.stem)
        if not _HOST.fullmatch(host):
            continue
        found[host] = document
    return found


def merge_snapshots(saved, live):
    """Union of pushed snapshots. The live document replaces that host's file."""
    live = live if isinstance(live, dict) else {}
    live_host = str(live.get("host") or "")
    hosts = {}
    projects = {}
    for host_id, doc in (saved or {}).items():
        if not isinstance(doc, dict):
            continue
        name = str(doc.get("host") or host_id)
        if not _HOST.fullmatch(name) or name == live_host:
            continue
        reading = dict(doc.get("reading") or {})
        reading["ok"] = bool(reading.get("ok", True))
        if doc.get("collected_at") is not None:
            reading["collected_at"] = int(doc["collected_at"])
        hosts[name] = reading
        for row in doc.get("projects") or []:
            if isinstance(row, dict):
                _fold_project(projects, row)
    if live_host and _HOST.fullmatch(live_host):
        reading = dict(live.get("reading") or {})
        reading["ok"] = True
        if live.get("collected_at") is not None:
            reading["collected_at"] = int(live["collected_at"])
        hosts[live_host] = reading
        for row in live.get("projects") or []:
            if isinstance(row, dict):
                _fold_project(projects, row)
    return {"hosts": hosts, "projects": list(projects.values())}


def _fold_project(projects, row):
    name = str(row.get("name") or "")
    if not name:
        return
    incoming = json.loads(json.dumps(row))
    current = projects.get(name)
    if current is None:
        projects[name] = incoming
        return
    git = dict(current.get("git") or {})
    other = incoming.get("git") or {}
    git["dirty"] = bool(git.get("dirty")) or bool(other.get("dirty"))
    for field in ("ahead", "behind", "unpushed"):
        git[field] = max(_as_int(git.get(field)), _as_int(other.get(field)))
    git["remote"] = git.get("remote") or other.get("remote") or ""
    current["git"] = git
    if _ci_rank(incoming.get("ci")) > _ci_rank(current.get("ci")):
        current["ci"] = incoming.get("ci")
    jobs = list(current.get("jobs") or [])
    seen = {job.get("label") for job in jobs if isinstance(job, dict)}
    for job in incoming.get("jobs") or []:
        if isinstance(job, dict) and job.get("label") not in seen:
            jobs.append(job)
            seen.add(job.get("label"))
    current["jobs"] = jobs
    current["check_declared"] = bool(current.get("check_declared")) or bool(incoming.get("check_declared"))
    current["check_result"] = _worse_check(current.get("check_result"), incoming.get("check_result"))


_CHECK_RANK = {"pass": 1, "warn": 2, "fail": 3}


def _worse_check(left, right):
    """One check result for a project seen on two Macs: the worse one, and of two equally bad
    the newer. A Mac that has no result file says nothing."""
    if not isinstance(left, dict) or not left:
        return right if isinstance(right, dict) and right else None
    if not isinstance(right, dict) or not right:
        return left
    a, b = _CHECK_RANK.get(left.get("status"), 3), _CHECK_RANK.get(right.get("status"), 3)
    if a != b:
        return left if a > b else right
    return left if _as_int(left.get("generated")) >= _as_int(right.get("generated")) else right


def _ci_rank(ci):
    if not isinstance(ci, dict):
        return 0
    conclusion = ci.get("conclusion")
    if conclusion in ("failure", "failed"):
        return 3
    if conclusion in ("cancelled", "canceled"):
        return 2
    return 1


def _as_int(value):
    try:
        return int(value or 0)
    except (TypeError, ValueError):
        return 0
