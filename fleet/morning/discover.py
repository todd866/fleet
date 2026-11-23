"""Per-repo `.fleet/check` discovery, and where a check is allowed to run."""
import re
import shlex
import tomllib
from dataclasses import dataclass
from pathlib import Path

from fleet import site

UNREADABLE = "\0unreadable"  # `result` of a check file that did not parse
HUB_NAMES = ("hub", site.hub())
PRIMARY_NAMES = ("primary", site.primary())


@dataclass(frozen=True)
class Check:
    name: str
    command: str | None
    max_runtime_s: int | None
    where: str
    default: bool
    # A file the repo's own scheduled check writes. The collector reads it; it never starts `command`.
    result: str | None = None
    max_age_h: float | None = None


@dataclass(frozen=True)
class Plan:
    argv: list
    host: str
    deferred: bool


def discover(registry, projects):
    """Active registry projects, plus any repo that carries `.fleet/check`.

    Archive and dormant projects are skipped unless they declare a check.
    A project without a check file gets a default (no command).
    """
    registry = Path(registry)
    projects = Path(projects)
    described = _descriptions(registry / "descriptions.tsv")
    found = []
    seen = set()
    names = list(described)
    if projects.is_dir():
        for child in sorted(projects.iterdir()):
            if child.name not in described and (child / ".fleet" / "check").is_file():
                names.append(child.name)
    for name in names:
        status = (described.get(name) or {}).get("status", "")
        path = projects / name / ".fleet" / "check"
        if path.is_file():
            try:
                found.append(_parse(name, path.read_text()))
            except (OSError, ValueError, TypeError):
                # A check file that does not parse is this repo's fault, not a reason to lose
                # every other project's row: it is reported as an unreadable check.
                found.append(Check(name, None, None, "any", False, UNREADABLE))
            seen.add(name)
            continue
        if status in ("archive", "dormant"):
            continue
        if name in described or status in ("active", "unknown", ""):
            if name in described:
                found.append(Check(name, None, None, "any", True))
                seen.add(name)
    return found


def plan_check(check, *, here, owner_active, heavy_host="hub", active_host="primary", heavy_runtime_s=60):
    """Place a check. Heavy work and `where = hub|<the hub's name>` run on the hub.

    Nothing runs on the active primary while its owner is there. A check that
    can only run on that Mac is deferred instead of moved.
    """
    hub_names = set(HUB_NAMES) | {heavy_host}
    primary_names = set(PRIMARY_NAMES) | {active_host}
    where = check.where or "any"
    heavy = check.max_runtime_s is not None and check.max_runtime_s >= heavy_runtime_s
    on_hub = where in hub_names
    on_primary = where in primary_names
    if on_primary and owner_active:
        return Plan([], active_host, True)
    if on_hub or heavy or (where == "any" and here == active_host and owner_active):
        target = heavy_host
    elif on_primary:
        target = active_host
    else:
        target = here
    command = shlex.split(check.command or "")
    if target != here:
        if target == heavy_host:
            argv = ["remote-run", "--", *command]
        else:
            argv = ["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=8", target, *command]
        return Plan(argv, target, False)
    return Plan(command, here, False)


def _descriptions(path):
    rows = {}
    if not path.is_file():
        return rows
    for line in path.read_text().splitlines():
        if not line.strip() or line.startswith("#"):
            continue
        parts = line.split("\t")
        name = parts[0].strip()
        if not name or not re.fullmatch(r"[A-Za-z0-9._-]+", name) or name in (".", ".."):
            continue                                   # a project name is one plain folder name, nothing else
        status = parts[2].strip().lower() if len(parts) > 2 else ""
        rows[name] = {"status": status}
    return rows


def _parse(name, text):
    data = tomllib.loads(text)
    runtime = data.get("max_runtime_s")
    result = str(data.get("result") or "").strip()
    age = data.get("max_age_h")
    return Check(
        name,
        str(data.get("command") or "").strip() or None,
        int(runtime) if runtime is not None else None,
        str(data.get("where") or "any").strip(),
        False,
        result or None,
        float(age) if isinstance(age, (int, float)) and not isinstance(age, bool) and age > 0 else None,
    )
