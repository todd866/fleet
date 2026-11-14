"""Pure claim, scope, and lease rules. No files and no clock of its own."""
import hashlib
import os
import re
from pathlib import PurePosixPath

KINDS = ("repo", "path", "machine", "drive", "service", "worktree", "output", "publish")
WRITER_TTL = 12 * 60 * 60
PRESENCE_TTL = 3600

ABBREV = {
    "claude": "cld",
    "codex": "cdx",
    "cursor": "cur",
    "local": "loc",
    "cloud": "rmt",
    "agentkit": "kit",
}

# Fleet-board lifecycle names. Claude, Codex, and Cursor each spell the same
# moment differently; the board stores one verb.
EVENT_MAP = {
    "SessionStart": "start",
    "sessionStart": "start",
    "UserPromptSubmit": "activity",
    "beforeSubmitPrompt": "activity",
    "PostToolUse": "activity",
    "postToolUse": "activity",
    "Stop": "turn_end",
    "stop": "turn_end",
    "SessionEnd": "end",
    "sessionEnd": "end",
    "Interrupt": "interrupted",
    "StopFailure": "error",
}


def session_key(provider, host, session):
    """Stable id. The raw session string is not stored."""
    raw = f"{canonical_provider(provider)}|{host}|{session}"
    return hashlib.sha256(raw.encode()).hexdigest()


def canonical_provider(name):
    text = (name or "").strip().lower()
    if "claude" in text:
        return "claude"
    if "codex" in text:
        return "codex"
    if "cursor" in text:
        return "cursor"
    if "agentkit" in text:
        return "agentkit"
    if "cloud" in text or "remote" in text:
        return "cloud"
    if "local" in text:
        return "local"
    return text or "local"


def abbrev(provider):
    return ABBREV.get(canonical_provider(provider), canonical_provider(provider)[:3])


def sanitize(text):
    cleaned = re.sub(r"[^A-Za-z0-9._+-]", "-", str(text or ""))
    cleaned = cleaned.strip(".-")
    return cleaned or "work"


# A host token in kind:host:path. The path after it is one value, spaces included.
HOST_RE = re.compile(r"[A-Za-z0-9][A-Za-z0-9._-]{0,63}")


def parse_ttl(text, default=3600):
    """Seconds, or a count with s/m/h/d. `8h`, `30m`, `2d`, and `45` are all one ttl."""
    if text is None or text == "":
        return int(default)
    raw = str(text).strip().lower()
    if raw.isdigit():
        return int(raw)
    match = re.fullmatch(r"(\d+)([smhd])", raw)
    if not match:
        raise ValueError(f"bad ttl {text}")
    number = int(match.group(1))
    scale = {"s": 1, "m": 60, "h": 3600, "d": 86400}[match.group(2)]
    return number * scale


def _split_host(tail):
    """kind:host:path → (host, path). A value with no host stays one string."""
    if ":" not in tail:
        return None, tail.strip()
    host, path = tail.split(":", 1)
    host, path = host.strip(), path.strip()
    if HOST_RE.fullmatch(host) and path and path not in (".", ".."):
        return host, path
    return None, tail.strip()


def parse_scope(spec):
    """One scope. Never split the value on spaces. Form is kind:value or kind:host:path."""
    text = (spec or "").strip()
    kind = None
    value = None
    host = None
    if ":" in text:
        head, tail = text.split(":", 1)
        if head in KINDS:
            kind = head
            host, value = _split_host(tail)
    if kind is None and (text.startswith("/") or text.startswith(".")):
        kind, value = "path", text
    if kind is None or not value or value in (".", ".."):
        raise ValueError(f"bad scope {spec}")
    if ".." in value.split("/"):
        raise ValueError(f"bad scope {spec}")
    scope = {"kind": kind, "value": value}
    if host:
        scope["host"] = host
    return scope


def format_scope(scope):
    if scope.get("host"):
        return f"{scope['kind']}:{scope['host']}:{scope['value']}"
    return f"{scope['kind']}:{scope['value']}"


def volume_name(value):
    """The volume, not the last path component. `/Volumes/Media Mirror/Music` is `Media Mirror`."""
    text = (value or "").strip()
    if text.startswith("/"):
        parts = [part for part in text.split("/") if part]
        if len(parts) >= 2 and parts[0] == "Volumes":
            return parts[1]
        return parts[-1] if parts else text
    if "/" in text:
        return text.rstrip("/").split("/")[-1]
    return text


def session_label(cwd):
    if not cwd:
        return ""
    name = PurePosixPath(str(cwd)).name
    if name in ("", ".", ".."):
        return ""
    return name


def display_owner(provider, label, mail=""):
    """`claude · glancebar-62`. The session hash is not part of the label."""
    name = (label or "").strip()
    if not name and mail and ":" in str(mail):
        name = str(mail).split(":", 1)[1]
    if not name:
        name = "·"
    return f"{canonical_provider(provider)} \u00b7 {name}"


def held_line(claim):
    provider = canonical_provider(claim.get("provider"))
    label = display_owner(provider, claim.get("label"), claim.get("mail"))
    host = claim.get("host") or "·"
    work = claim.get("work") or "·"
    return f'held by {label} ({provider}, {host}) \u2014 ask: cowork ask {work} "\u2026"'


def repo_name(scope):
    return PurePosixPath(norm_path(scope["value"])).name if "/" in scope["value"] else scope["value"]


def norm_path(value):
    return os.path.normpath(value)


def path_parts(value):
    return [part for part in PurePosixPath(norm_path(value)).parts if part not in ("/", "")]


def path_prefix(left, right):
    a, b = norm_path(left), norm_path(right)
    if a == b:
        return True
    short, long = (a, b) if len(a) <= len(b) else (b, a)
    return long.startswith(short.rstrip("/") + "/")


def _path_values(scope):
    if scope["kind"] == "path":
        return [norm_path(scope["value"])]
    if scope["kind"] in ("repo", "worktree") and "/" in scope["value"]:
        return [norm_path(scope["value"])]
    return []


def scopes_overlap(left, right):
    """True when two scope lists share a path prefix, a drive, or an output."""
    for a in left:
        for b in right:
            if a["kind"] == "drive" and b["kind"] == "drive":
                if volume_name(a["value"]) == volume_name(b["value"]):
                    left, right = a.get("host"), b.get("host")
                    if left and right and left != right:
                        return False
                    return True
            if a["kind"] == "service" and b["kind"] == "service" and a["value"] == b["value"]:
                return True
            if a["kind"] in ("output", "publish") and b["kind"] in ("output", "publish"):
                if a["value"] == b["value"]:
                    return True
            if a["kind"] == "worktree" and b["kind"] == "worktree":
                if a["value"] == b["value"]:
                    return True
            if a["kind"] == "repo" and b["kind"] == "repo" and repo_name(a) == repo_name(b):
                return True
            for pa in _path_values(a):
                for pb in _path_values(b):
                    if path_prefix(pa, pb):
                        return True
            for repo, other in ((a, b), (b, a)):
                if repo["kind"] != "repo":
                    continue
                if other["kind"] == "path" or "/" in other["value"]:
                    if repo_name(repo) in path_parts(other["value"]):
                        return True
    return False


def same_party(left, right):
    if left.get("session") and left.get("session") == right.get("session"):
        return True
    if left.get("parent") and left.get("parent") == right.get("work"):
        return True
    if right.get("parent") and right.get("parent") == left.get("work"):
        return True
    return False


def visible_state(claim, now):
    status = claim.get("status") or "active"
    if status in ("active", "handed-off"):
        ttl = int(claim.get("ttl") or PRESENCE_TTL)
        if int(now) > int(claim.get("heartbeat") or 0) + ttl:
            return "stale"
    return status


def presence_state(row, now):
    event = row.get("event") or "activity"
    if event == "end":
        return "released"
    if event in ("interrupted", "error"):
        return "stale"
    ttl = int(row.get("ttl") or PRESENCE_TTL)
    if int(now) > int(row.get("at") or 0) + ttl:
        return "stale"
    return "active"


def live(claim, now):
    return visible_state(claim, now) in ("active", "handed-off")


def infer_mail(host, scopes):
    for scope in scopes:
        if scope["kind"] == "drive":
            return f"{host}:{volume_name(scope['value'])}"
    for kind in ("repo", "output", "publish", "worktree", "service"):
        for scope in scopes:
            if scope["kind"] == kind:
                return f"{host}:{PurePosixPath(scope['value']).name}"
    return f"{host}:work"


def writer_line(work, host, hostname, pid, epoch):
    return f"{sanitize(work)}@{sanitize(host)} {sanitize(hostname)} {int(pid)} {int(epoch)}\n"


def parse_writer(text):
    parts = (text or "").split()
    if len(parts) < 4 or "@" not in parts[0]:
        return None
    job, host = parts[0].split("@", 1)
    try:
        pid = int(parts[2])
        epoch = int(parts[3])
    except ValueError:
        return None
    return {"job": job, "host": host, "hostname": parts[1], "pid": pid, "epoch": epoch}


def pid_alive(pid):
    if pid <= 0:
        return False
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    except OSError:
        return False
    return True


def writer_live_raw(info, now, hostname):
    """Placement's rule for a .writer that cowork does not own: 12h, and a local pid."""
    if info is None:
        return False
    if int(now) - int(info["epoch"]) >= WRITER_TTL:
        return False
    if info["hostname"] == hostname:
        return pid_alive(int(info["pid"]))
    return True


def age(now, then):
    seconds = max(0, int(now) - int(then))
    if seconds < 90:
        return f"{seconds}s"
    if seconds < 90 * 60:
        return f"{seconds // 60}m"
    if seconds < 36 * 3600:
        return f"{seconds // 3600}h"
    return f"{seconds // 86400}d"


def scope_text(scope):
    host = scope.get("host")
    value = scope.get("value") or ""
    if host:
        return f"{scope['kind']} {host}:{value}"
    return f"{scope['kind']} {value}"


def scope_label(scopes):
    """Every scope, full text. Columns widen; nothing is cut."""
    if not scopes:
        return "·"
    return " \u00b7 ".join(scope_text(scope) for scope in scopes)
