"""Infer a claim from a tool call. Recording stays on this machine; the hub flush is async."""
import os
import shlex
from pathlib import Path

from cowork.model import (
    format_scope,
    held_line,
    parse_scope,
    sanitize,
    session_key,
    session_label,
    volume_name,
)
from cowork.store import Board

EDIT_TOOLS = {
    "edit", "write", "strreplace", "applypatch", "notebookedit", "delete",
    "multiedit", "searchreplace", "editnotebook", "writefile",
}
PUSH_FLAGS = {"-o", "--push-option", "--receive-pack", "--exec", "--repo"}
COPY_FLAGS = {
    "-e", "--rsh", "-t", "--timeout", "--exclude", "--include", "--filter",
    "-f", "--files-from", "--log-file", "--chmod", "--chown", "--bwlimit",
}
SPLIT = {"&&", "||", ";", "|"}
CHOKE_KINDS = {"publish", "output", "drive", "worktree", "service"}


class Inferred:
    def __init__(self, work, scopes, choke, parent=None, ttl=3600):
        self.work = work
        self.scopes = scopes
        self.choke = choke
        self.parent = parent
        self.ttl = ttl


def record_observation(payload, provider, root, volumes, host, hostname, now, forward=False, pid=None):
    """Record what the tool is about to do. Exit 2 when a choke is already held."""
    if not isinstance(payload, dict):
        return 0
    inferred = infer(payload, host)
    if inferred is None:
        return 0
    session = _session(payload)
    if not session:
        return 0
    cwd = payload.get("cwd") if isinstance(payload.get("cwd"), str) else ""
    name = os.environ.get("COWORK_SESSION_NAME") or session_label(cwd) or session_label(os.getcwd())
    board = Board(root=root, volumes=volumes, host=host, hostname=hostname, now=now, pid=pid)
    if inferred.choke and _held_by_someone_else(board, inferred.scopes, provider, session, host):
        return 2
    parent = inferred.parent or os.environ.get("COWORK_WORK") or None
    board.claim(
        inferred.work, inferred.scopes, provider, session, host, inferred.ttl,
        parent=parent, label=name,
    )
    if forward:
        from cowork.transport import enqueue, spawn_flush
        enqueue(_claim_argv(inferred, provider, session, host, name, parent), root=root)
        spawn_flush()
    return 0


def infer(payload, host):
    tool = str(payload.get("tool_name") or payload.get("tool") or payload.get("toolName") or "")
    key = tool.split(".")[-1].replace("_", "").lower()
    raw = payload.get("tool_input") or payload.get("arguments") or payload.get("input") or {}
    if not isinstance(raw, dict):
        raw = {}
    command = raw.get("command") or payload.get("command") or ""
    if not isinstance(command, str):
        command = ""
    cwd = payload.get("cwd") if isinstance(payload.get("cwd"), str) else ""
    parts = []
    if key in EDIT_TOOLS:
        parts.append(_edit(cwd))
    if command.strip():
        parts.extend(_commands(command, host))
    parts = [part for part in parts if part is not None]
    if not parts:
        return None
    return _merge(parts)


def _merge(parts):
    choke = [part for part in parts if part.choke]
    primary = choke[0] if choke else parts[0]
    scopes = []
    seen = set()
    parent = None
    for part in parts:
        parent = parent or part.parent
        for scope in part.scopes:
            ident = (scope["kind"], scope.get("host"), scope["value"])
            if ident in seen:
                continue
            seen.add(ident)
            scopes.append(scope)
    primary.scopes = scopes
    primary.parent = parent
    primary.choke = any(part.choke for part in parts)
    return primary


def _edit(cwd):
    name = _project(cwd) or "repo"
    scopes = [parse_scope(f"repo:{name}")]
    if cwd and ".." not in cwd.split("/"):
        scopes.append(parse_scope(f"path:{cwd}"))
    return Inferred(f"edit-{sanitize(name)}", scopes, choke=False)


def _project(cwd):
    if not cwd:
        return ""
    parts = Path(cwd).parts
    if "worktrees" in parts:
        index = parts.index("worktrees")
        if index + 1 < len(parts):
            return parts[index + 1]
    return Path(cwd).name


def _commands(command, host):
    try:
        tokens = shlex.split(command)
    except ValueError:
        return []
    found = []
    for part in _split(tokens):
        if not part:
            continue
        base = Path(part[0]).name
        if base == "heavy-slot":
            found.append(Inferred(f"slot-{sanitize(host)}", [parse_scope(f"machine:{host}")], choke=False))
            inner = [tok for tok in part[1:] if not tok.startswith("-")]
            if inner:
                item = _one(inner, host)
                if item is not None:
                    found.append(item)
            continue
        item = _one(part, host)
        if item is not None:
            found.append(item)
    return found


def _one(tokens, host):
    base = Path(tokens[0]).name
    if base == "git" and "push" in tokens:
        return _push(tokens)
    if base in ("cp", "mv", "rsync"):
        return _copy(tokens, host)
    if base in ("cursor-worker", "cursor-remote"):
        return _worker(tokens)
    if base == "launchctl":
        return _launchctl(tokens)
    if "deploy" in base or base.endswith("-release"):
        return _deploy(tokens)
    if base == "heavy-slot":
        return Inferred(f"slot-{sanitize(host)}", [parse_scope(f"machine:{host}")], choke=False)
    return None


def _push(tokens):
    index = tokens.index("push")
    remote = "origin"
    cursor = index + 1
    while cursor < len(tokens):
        token = tokens[cursor]
        if token in SPLIT:
            break
        if token in PUSH_FLAGS:
            cursor += 2
            continue
        if token.startswith("-"):
            cursor += 1
            continue
        remote = token
        break
    scopes = []
    if remote and ":" not in remote:
        scopes.append(parse_scope(f"publish:{remote}"))
    if not any(scope["value"] == "public-mirror" for scope in scopes):
        scopes.append(parse_scope("publish:public-mirror"))
    work = "push-" + sanitize(remote if ":" not in remote else "public-mirror")
    return Inferred(work, scopes, choke=True)


def _copy(tokens, host):
    positionals = []
    cursor = 1
    while cursor < len(tokens):
        token = tokens[cursor]
        if token in SPLIT:
            break
        if token in COPY_FLAGS:
            cursor += 2
            continue
        if token.startswith("-"):
            cursor += 1
            continue
        positionals.append(token)
        cursor += 1
    if len(positionals) < 2:
        return None
    dest = positionals[-1]
    if "/Volumes/" not in dest:
        return None
    at = dest.find("/Volumes/")
    path = dest[at:]
    scope = parse_scope(f"drive:{host}:{path}")
    return Inferred("drive-" + sanitize(volume_name(path)), [scope], choke=True)


def _worker(tokens):
    name = ""
    cursor = 1
    while cursor < len(tokens):
        token = tokens[cursor]
        if token in SPLIT:
            break
        if token in ("--name", "--worktree", "-n") and cursor + 1 < len(tokens):
            name = tokens[cursor + 1]
            break
        if token.startswith("-"):
            cursor += 1
            continue
        name = token
        break
    if not name or name in (".", ".."):
        return None
    return Inferred(
        "worker-" + sanitize(name),
        [parse_scope(f"worktree:{name}")],
        choke=True,
        parent=os.environ.get("COWORK_WORK") or None,
    )


def _launchctl(tokens):
    label = ""
    for token in tokens[1:]:
        if token in SPLIT:
            break
        if token.startswith("-"):
            continue
        if "/" in token or token.startswith("com."):
            label = token.rsplit("/", 1)[-1]
    if not label:
        return None
    return Inferred("job-" + sanitize(label), [parse_scope(f"service:{label}")], choke=True)


def _deploy(tokens):
    base = Path(tokens[0]).name
    return Inferred("job-" + sanitize(base), [parse_scope(f"service:{base}")], choke=True)


def _split(tokens):
    parts = []
    current = []
    for token in tokens:
        if token in SPLIT:
            if current:
                parts.append(current)
            current = []
            continue
        current.append(token)
    if current:
        parts.append(current)
    return parts


def _session(payload):
    for key in ("session_id", "conversation_id", "sessionId"):
        value = payload.get(key)
        if isinstance(value, str) and value and len(value) <= 512:
            return value
    return ""


def _held_by_someone_else(board, scopes, provider, session, host):
    mine = session_key(provider, host, session)
    lines = []
    for scope in scopes:
        if scope["kind"] not in CHOKE_KINDS:
            continue
        # A push records the remote and the mirror lease. Only the mirror lease
        # refuses; the remote name is the record of where it went.
        if scope["kind"] in ("publish", "output") and scope["value"] != "public-mirror":
            continue
        holders, _stale, foreign = board.check(scope)
        for claim in holders:
            if claim.get("session") in (mine, session):
                continue
            lines.append(held_line(claim))
        if foreign:
            mail = f"{foreign['host']}:{foreign['job']}"
            lines.append(
                f'held by {foreign["job"]} (local, {foreign["host"]}) \u2014 ask: agent-mail send {mail}'
            )
    if not lines:
        return False
    for line in dict.fromkeys(lines):
        print(line)
    return True


def _claim_argv(inferred, provider, session, host, name, parent):
    argv = [
        "claim", inferred.work,
        "--provider", provider,
        "--session", session,
        "--host", host,
        "--ttl", str(inferred.ttl),
    ]
    if name:
        argv.extend(["--name", name])
    if parent:
        argv.extend(["--parent", parent])
    for scope in inferred.scopes:
        argv.extend(["--scope", format_scope(scope)])
    return argv
