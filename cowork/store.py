"""File-backed board. State stays in COWORK_STATE, never in the repo."""
import fcntl
import json
import os
from contextlib import contextmanager
from pathlib import Path, PurePosixPath

from cowork.hosts import resolve_host
from cowork.model import (
    canonical_provider,
    infer_mail,
    live,
    parse_writer,
    presence_state,
    same_party,
    sanitize,
    scopes_overlap,
    session_key,
    session_label,
    visible_state,
    volume_name,
    writer_line,
    writer_live_raw,
)

EMPTY = {"version": 1, "claims": [], "presence": [], "seen_event_ids": [], "items": []}


class ClaimError(Exception):
    def __init__(self, message):
        super().__init__(message)
        self.message = message


def default_state_dir():
    override = os.environ.get("COWORK_STATE")
    if override:
        return Path(override)
    configured = load_config().get("state_dir")
    if configured:
        return Path(configured).expanduser()
    return Path.home() / ".local" / "state" / "cowork"


def load_config():
    raw = os.environ.get("COWORK_CONFIG")
    if raw:
        path = Path(raw)
        if not path.is_file():
            return {}
        return json.loads(path.read_text())
    path = Path.home() / ".config" / "cowork" / "config.json"
    if path.is_file():
        return json.loads(path.read_text())
    return {}


class Board:
    def __init__(self, root=None, now=None, volumes=None, host=None, hostname=None, pid=None):
        config = load_config()
        self.root = Path(root) if root else default_state_dir()
        self.now = int(now if now is not None else _now())
        volume = volumes or os.environ.get("COWORK_VOLUME_ROOT") or config.get("volume_root") or "/Volumes"
        self.volumes = Path(volume)
        self.host = host or resolve_host(config)
        self.hostname = hostname or os.environ.get("COWORK_HOSTNAME") or config.get("hostname") or "localhost"
        self.pid = int(pid if pid is not None else os.environ.get("COWORK_PID") or os.getpid())

    def claim(self, work, scopes, provider, session, host, ttl, mail=None, takeover=None, parent=None, note=None, label=None):
        provider = canonical_provider(provider)
        host = host or self.host
        key = session_key(provider, host, session)
        mail = mail or infer_mail(host, scopes)
        label = _label(label, session)
        with self._edit() as state:
            self._expire(state)
            state.setdefault("items", [])
            item = _find_item(state, work)
            if item and not scopes:
                scopes = [dict(scope) for scope in item.get("scope") or [] if isinstance(scope, dict)]
            current = _find(state, work)
            if current and not _matches(current, session, provider, host):
                if current.get("status") in ("active", "handed-off"):
                    if not takeover:
                        raise ClaimError("takeover needs a note")
                    current["provider"] = provider
                    current["session"] = key
                    current["host"] = host
                    current["mail"] = mail
                    current["ttl"] = int(ttl)
                    current["heartbeat"] = self.now
                    current["status"] = "active"
                    current["scope"] = scopes
                    current["label"] = label
                    current["lock_released"] = False
                    _note(current, takeover, key, self.now)
                    self._write_leases(state, current)
                    _take_item(item, self.now)
                    return _warnings(state, current, self)
            if current and current.get("status") in ("done", "released"):
                current["status"] = "active"
            if current is None:
                current = {
                    "work": work,
                    "scope": scopes,
                    "provider": provider,
                    "session": key,
                    "host": host,
                    "mail": mail,
                    "ttl": int(ttl),
                    "heartbeat": self.now,
                    "claimed_at": self.now,
                    "status": "active",
                    "notes": [],
                    "parent": parent,
                    "label": label,
                    "lock_released": False,
                }
                state["claims"].append(current)
            else:
                current["scope"] = scopes
                current["provider"] = provider
                current["session"] = key
                current["host"] = host
                current["mail"] = mail
                current["ttl"] = int(ttl)
                current["heartbeat"] = self.now
                current["status"] = "active"
                current["label"] = label
                current["lock_released"] = False
                if parent:
                    current["parent"] = parent
            if note:
                _note(current, note, key, self.now)
            self._write_leases(state, current)
            _take_item(item, self.now)
            return _warnings(state, current, self)

    def add_item(self, key, title="", detail="", status="warn", scopes=None, source=""):
        key = (key or "").strip()
        if not key:
            raise ClaimError("item needs a key")
        scopes = [scope for scope in (scopes or []) if isinstance(scope, dict)]
        with self._edit() as state:
            state.setdefault("items", [])
            current = _find_item(state, key)
            if current is None:
                state["items"].append({
                    "key": key,
                    "title": title or "",
                    "detail": detail or "",
                    "status": status or "warn",
                    "state": "open",
                    "scope": scopes,
                    "opened": self.now,
                    "updated": self.now,
                    "source": source or "",
                })
                return
            opened = int(current.get("opened") or self.now)
            current["title"] = title or ""
            current["detail"] = detail or ""
            current["status"] = status or current.get("status") or "warn"
            if scopes:
                current["scope"] = scopes
            current["opened"] = opened
            current["updated"] = self.now
            if source:
                current["source"] = source
            if current.get("state") == "taken" and _live_claim(state, key):
                current["state"] = "taken"
            else:
                current["state"] = "open"

    def close_item(self, key):
        key = (key or "").strip()
        if not key:
            raise ClaimError("item needs a key")
        with self._edit() as state:
            state.setdefault("items", [])
            current = _find_item(state, key)
            if current is None:
                return
            current["state"] = "closed"
            current["updated"] = self.now

    def open_items(self):
        items = [item for item in self.snapshot().get("items") or [] if item.get("state") == "open"]
        items.sort(key=lambda item: (int(item.get("opened") or 0), item.get("key") or ""))
        return items

    def heartbeat(self, work, session, provider=None, host=None):
        with self._edit() as state:
            self._expire(state)
            current = _require(state, work)
            if not _matches(current, session, provider, host):
                raise ClaimError("not the owner")
            current["heartbeat"] = self.now
            current["lock_released"] = False
            if current.get("status") == "handed-off":
                current["status"] = "active"
            self._write_leases(state, current)

    def note(self, work, text, session, provider=None, host=None):
        with self._edit() as state:
            self._expire(state)
            current = _require(state, work)
            if not _matches(current, session, provider, host):
                raise ClaimError("not the owner")
            _note(current, text, current["session"], self.now)

    def release(self, work, session, provider=None, host=None):
        with self._edit() as state:
            self._expire(state)
            current = _require(state, work)
            if not _matches(current, session, provider, host):
                raise ClaimError("not the owner")
            self._release_claim(current)

    def handoff(self, work, session, to_session, note, provider=None, host=None, mail=None):
        if not note or not str(note).strip():
            raise ClaimError("handoff needs a note")
        with self._edit() as state:
            self._expire(state)
            current = _require(state, work)
            if not _matches(current, session, None, None):
                raise ClaimError("not the owner")
            provider = canonical_provider(provider or current["provider"])
            host = host or current["host"]
            current["provider"] = provider
            current["host"] = host
            current["session"] = session_key(provider, host, to_session)
            current["mail"] = mail or infer_mail(host, current["scope"])
            current["status"] = "handed-off"
            current["heartbeat"] = self.now
            current["lock_released"] = False
            _note(current, note, current["session"], self.now)
            self._write_leases(state, current)

    def done(self, work, session, provider=None, host=None):
        with self._edit() as state:
            self._expire(state)
            current = _require(state, work)
            if not _matches(current, session, provider, host):
                raise ClaimError("not the owner")
            self._release_files(current)
            current["status"] = "done"
            current["lock_released"] = True
            current["heartbeat"] = self.now

    def child(self, parent, work, scopes, provider, session, host, ttl, mail=None, label=None):
        with self._edit() as state:
            self._expire(state)
            if _find(state, parent) is None:
                raise ClaimError("no such work")
        return self.claim(
            work, scopes, provider, session, host, ttl, mail=mail, parent=parent, label=label)

    def join(self, provider, session, host, scopes, work=None, ttl=3600, mail=None, label=None):
        provider = canonical_provider(provider)
        host = host or self.host
        key = session_key(provider, host, session)
        project = "work"
        for scope in scopes:
            if scope["kind"] == "repo":
                project = PurePosixPath(scope["value"]).name
                break
        self.apply_events([{
            "id": key,
            "event_id": f"join-{key}-{self.now}",
            "engine": provider,
            "host": host,
            "project": project,
            "event": "start",
            "at": self.now,
            "source": "cowork join",
        }])
        if work:
            self.claim(work, scopes, provider, session, host, ttl, mail=mail, label=label)

    def leave(self, provider, session, host):
        provider = canonical_provider(provider)
        host = host or self.host
        self.release_session(session_key(provider, host, session))

    def release_session(self, key):
        with self._edit() as state:
            self._expire(state)
            parents = [c["work"] for c in state["claims"] if c.get("session") == key]
            for claim in state["claims"]:
                if claim.get("session") == key or claim.get("parent") in parents:
                    if claim.get("status") in ("active", "handed-off"):
                        self._release_claim(claim)
            for row in state["presence"]:
                if row.get("id") == key:
                    row["event"] = "end"
                    row["at"] = self.now

    def heartbeat_session(self, key):
        with self._edit() as state:
            self._expire(state)
            for claim in state["claims"]:
                if claim.get("session") == key and claim.get("status") in ("active", "handed-off"):
                    claim["heartbeat"] = self.now
                    claim["lock_released"] = False
                    if claim.get("status") == "handed-off":
                        claim["status"] = "active"
                    self._write_leases(state, claim)
            for row in state["presence"]:
                if row.get("id") == key and row.get("event") != "end":
                    row["at"] = self.now
                    row["event"] = row.get("event") if row.get("event") not in ("end",) else "activity"

    def check(self, scope):
        state = self._prepare()
        holders = []
        stale = []
        for claim in state["claims"]:
            if not scopes_overlap(claim["scope"], [scope]):
                continue
            if scope["kind"] == "drive" and not self._drive_holds(claim, scope):
                if visible_state(claim, self.now) == "stale":
                    stale.append(claim)
                continue
            if live(claim, self.now):
                holders.append(claim)
            elif visible_state(claim, self.now) == "stale":
                stale.append(claim)
        foreign = self._foreign_writer(scope, state)
        return holders, stale, foreign

    def snapshot(self):
        state = self._prepare()
        return project(state, self.now, self)

    def migrate(self, payload):
        if isinstance(payload, dict) and "claims" in payload and "sessions" not in payload:
            with self._edit() as state:
                state["claims"] = payload.get("claims") or []
                state["presence"] = payload.get("presence") or []
                if payload.get("seen_event_ids"):
                    state["seen_event_ids"] = payload["seen_event_ids"]
            return
        self.apply_events(_legacy_events(payload))

    def apply_events(self, events):
        with self._edit() as state:
            seen = state.setdefault("seen_event_ids", [])
            for event in events:
                if not isinstance(event, dict) or "id" not in event:
                    continue
                event_id = str(event.get("event_id") or f"{event.get('id')}|{event.get('event')}|{event.get('at')}")
                if event_id in seen:
                    continue
                seen.append(event_id)
                if len(seen) > 4000:
                    del seen[: len(seen) - 4000]
                row = _presence_from(event)
                current = next((item for item in state["presence"] if item["id"] == row["id"]), None)
                if current is None:
                    state["presence"].append(row)
                elif int(row["at"]) >= int(current.get("at") or 0):
                    current.update(row)

    def _prepare(self):
        with self._edit() as state:
            self._expire(state)
            return json.loads(json.dumps(state))

    def _expire(self, state):
        for claim in state["claims"]:
            if claim.get("status") in ("active", "handed-off") and visible_state(claim, self.now) == "stale":
                if not claim.get("lock_released"):
                    self._release_files(claim)
                    claim["lock_released"] = True

    def _write_leases(self, state, claim):
        if not live(claim, self.now):
            return
        line = writer_line(claim["work"], claim["host"], self.hostname, self.pid, claim["heartbeat"])
        for scope in claim["scope"]:
            path = self._lease_path(scope)
            if path is None:
                continue
            # An absolute drive path is a real volume. Do not create one to hold the lock.
            if scope["kind"] == "drive" and str(scope.get("value") or "").startswith("/") and not path.parent.is_dir():
                continue
            if _blocked(path, sanitize(claim["work"]), self.now, self.hostname, state, self):
                continue
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(line)

    def _release_files(self, claim):
        job = sanitize(claim["work"])
        for scope in claim["scope"]:
            path = self._lease_path(scope)
            if path is None or not path.exists():
                continue
            text = path.read_text()
            info = parse_writer(text)
            if info and info["job"] != job:
                continue
            if scope["kind"] == "drive":
                released = path.with_name(".writer.released")
                path.replace(released)
            else:
                path.unlink()

    def _release_claim(self, claim):
        self._release_files(claim)
        claim["status"] = "released"
        claim["lock_released"] = True
        claim["heartbeat"] = self.now

    def _lease_path(self, scope):
        if scope["kind"] == "drive":
            # The volume's host writes the lock. A remote host is not a local path.
            if scope.get("host") and scope["host"] != self.host:
                return None
            name = volume_name(scope["value"])
            if not name or name in (".", "..") or "/" in name:
                return None
            return self.volumes / name / ".writer"
        name = PurePosixPath(scope["value"]).name
        if not name or name in (".", ".."):
            return None
        if scope["kind"] in ("publish", "output"):
            kind = "publish"
        elif scope["kind"] in ("repo", "worktree"):
            kind = scope["kind"]
        else:
            return None
        return self.root / "leases" / kind / sanitize(name)

    def _drive_holds(self, claim, query):
        """A remote drive is held by the board row. A local drive is held by its .writer."""
        if not live(claim, self.now):
            return False
        for scope in claim.get("scope") or []:
            if scope.get("kind") != "drive":
                continue
            if volume_name(scope["value"]) != volume_name(query["value"]):
                continue
            host = scope.get("host") or query.get("host")
            if host and host != self.host:
                return True
        return self._owns_drive(claim, query["value"])

    def _owns_drive(self, claim, value):
        path = self._lease_path({"kind": "drive", "value": value})
        if path is None or not path.is_file():
            return False
        info = parse_writer(path.read_text())
        return bool(info and info["job"] == sanitize(claim["work"]) and live(claim, self.now))

    def _foreign_writer(self, scope, state):
        if scope["kind"] != "drive":
            return None
        if scope.get("host") and scope["host"] != self.host:
            return None
        path = self._lease_path({"kind": "drive", "value": scope["value"]})
        if path is None or not path.is_file():
            return None
        info = parse_writer(path.read_text())
        if not info:
            return None
        for claim in state["claims"]:
            if sanitize(claim["work"]) == info["job"] and self._owns_drive(claim, scope["value"]):
                return None
        if writer_live_raw(info, self.now, self.hostname):
            return info
        return None

    @contextmanager
    def _edit(self):
        self.root.mkdir(parents=True, exist_ok=True)
        try:
            os.chmod(self.root, 0o700)
        except OSError:
            pass
        lock_path = self.root / ".lock"
        fd = os.open(lock_path, os.O_RDWR | os.O_CREAT, 0o600)
        try:
            fcntl.flock(fd, fcntl.LOCK_EX)
            state = _read(self.root / "state.json")
            yield state
            _write(self.root, state, project(state, self.now, self))
        finally:
            fcntl.flock(fd, fcntl.LOCK_UN)
            os.close(fd)


def project(state, now, board):
    claims = []
    for claim in state["claims"]:
        conflicts = []
        for other in state["claims"]:
            if other is claim or other.get("work") == claim.get("work"):
                continue
            if same_party(claim, other):
                continue
            if other.get("status") in ("released", "done"):
                continue
            if not scopes_overlap(claim["scope"], other["scope"]):
                continue
            if any(s["kind"] == "drive" for s in claim["scope"]) and not _drive_conflict(claim, other, board):
                # A drive declared but not held still conflicts when the other claim holds it
                # or shares a non-drive scope. Keep non-drive overlaps; drop a drive-only miss.
                shared = _without_drives(claim["scope"]), _without_drives(other["scope"])
                if not scopes_overlap(shared[0], other["scope"]) and not scopes_overlap(claim["scope"], shared[1]):
                    if not board._owns_drive(other, _drive_name(other)):
                        continue
            conflicts.append({
                "work": other["work"],
                "mail": other.get("mail") or "",
                "state": visible_state(other, now),
            })
        claims.append({
            "work": claim["work"],
            "state": visible_state(claim, now),
            "owner": {
                "provider": claim.get("provider"),
                "session": claim.get("session"),
                "host": claim.get("host"),
                "mail": claim.get("mail"),
                "label": claim.get("label") or "",
            },
            "scope": claim.get("scope") or [],
            "heartbeat": int(claim.get("heartbeat") or 0),
            "ttl": int(claim.get("ttl") or 0),
            "expires": int(claim.get("heartbeat") or 0) + int(claim.get("ttl") or 0),
            "conflicts": conflicts,
            "parent": claim.get("parent"),
            "notes": claim.get("notes") or [],
            "leases": _lease_names(claim),
        })
    items = []
    for item in state.get("items") or []:
        items.append({
            "key": item.get("key") or "",
            "title": item.get("title") or "",
            "detail": item.get("detail") or "",
            "status": item.get("status") or "",
            "state": item.get("state") or "open",
            "scope": item.get("scope") or [],
            "opened": int(item.get("opened") or 0),
            "updated": int(item.get("updated") or 0),
            "source": item.get("source") or "",
        })
    presence = []
    for row in state["presence"]:
        presence.append({
            "id": row.get("id"),
            "provider": row.get("provider"),
            "host": row.get("host"),
            "project": row.get("project"),
            "label": row.get("label") or "",
            "event": row.get("event"),
            "at": int(row.get("at") or 0),
            "state": presence_state(row, now),
            "source": row.get("source") or "",
        })
    return {"version": 1, "generated": int(now), "claims": claims, "presence": presence, "items": items}


def _drive_conflict(claim, other, board):
    names = {_drive_name(claim), _drive_name(other)}
    if None in names or len(names) != 1:
        return False
    return board._owns_drive(claim, names.pop()) or board._owns_drive(other, _drive_name(other))


def _drive_name(claim):
    for scope in claim.get("scope") or []:
        if scope["kind"] == "drive":
            return scope["value"]
    return None


def _without_drives(scopes):
    return [scope for scope in scopes if scope["kind"] != "drive"]


def _lease_names(claim):
    names = []
    for scope in claim.get("scope") or []:
        if scope["kind"] in ("drive", "repo", "worktree", "publish", "output"):
            kind = "publish" if scope["kind"] in ("publish", "output") else scope["kind"]
            name = volume_name(scope["value"]) if scope["kind"] == "drive" else PurePosixPath(scope["value"]).name
            names.append({"kind": kind, "name": name})
    return names


def _warnings(state, claim, board):
    lines = []
    snap = project(state, board.now, board)
    current = next(item for item in snap["claims"] if item["work"] == claim["work"])
    for conflict in current["conflicts"]:
        other = _find(state, conflict["work"])
        mark = "~" if conflict["state"] == "stale" else "!"
        from cowork.model import display_owner
        owner = display_owner(other.get("provider"), other.get("label"), other.get("mail"))
        lines.append(
            f"{mark} {other['work']} {owner} {other.get('host')} "
            f"agent-mail send {other.get('mail')}"
        )
    return lines


def _blocked(path, job, now, hostname, state, board):
    if not path.exists():
        return False
    info = parse_writer(path.read_text())
    if not info or info["job"] == job:
        return False
    for claim in state["claims"]:
        if sanitize(claim["work"]) == info["job"] and live(claim, now):
            return True
    return writer_live_raw(info, now, hostname)


def _find(state, work):
    return next((claim for claim in state["claims"] if claim.get("work") == work), None)


def _find_item(state, key):
    return next((item for item in state.get("items") or [] if item.get("key") == key), None)


def _live_claim(state, work):
    claim = _find(state, work)
    return bool(claim and claim.get("status") in ("active", "handed-off"))


def _take_item(item, now):
    if item and item.get("state") == "open":
        item["state"] = "taken"
        item["updated"] = int(now)


def _require(state, work):
    current = _find(state, work)
    if current is None:
        raise ClaimError("no such work")
    return current


def _matches(claim, session, provider, host):
    if not session:
        return False
    if session == claim.get("session"):
        return True
    prov = canonical_provider(provider or claim.get("provider"))
    where = host or claim.get("host")
    return session_key(prov, where, session) == claim.get("session")


def _note(claim, text, by, now):
    claim.setdefault("notes", []).append({"at": int(now), "text": text, "by": by})


def _presence_from(event):
    engine = event.get("engine") or event.get("provider") or "local"
    return {
        "id": event.get("id"),
        "provider": canonical_provider(engine),
        "engine": engine,
        "host": event.get("host") or "",
        "project": event.get("project") or "unmapped",
        "event": event.get("event") or "activity",
        "at": int(float(event.get("at") or 0)),
        "source": event.get("source") or "",
        "label": event.get("label") or "",
        "ttl": int(event.get("ttl") or 3600),
    }


def _is_hash(text):
    return len(text) >= 32 and all(ch in "0123456789abcdef" for ch in text.lower())


def _label(explicit, session):
    """Owner column. A given name wins; otherwise the session, not the account directory."""
    named = (explicit or os.environ.get("COWORK_SESSION_NAME") or "").strip()
    if named and not _is_hash(named):
        return named
    for candidate in (session or "", os.environ.get("COWORK_SESSION") or ""):
        candidate = str(candidate).strip()
        if candidate and not _is_hash(candidate):
            return candidate
    text = session_label(os.getcwd())
    if not text or text == session or _is_hash(text):
        return ""
    return text


def _legacy_events(payload):
    if isinstance(payload, list):
        return payload
    if isinstance(payload, dict):
        if isinstance(payload.get("sessions"), list):
            return payload["sessions"]
        if isinstance(payload.get("events"), list):
            return payload["events"]
        if "id" in payload and "event" in payload:
            return [payload]
    raise ClaimError("unrecognised fleet-board state")


def _read(path):
    if not path.is_file():
        return json.loads(json.dumps(EMPTY))
    try:
        data = json.loads(path.read_text())
    except (OSError, ValueError):
        return json.loads(json.dumps(EMPTY))
    data.setdefault("version", 1)
    data.setdefault("claims", [])
    data.setdefault("presence", [])
    data.setdefault("seen_event_ids", [])
    data.setdefault("items", [])
    return data


def _write(root, state, snapshot):
    target = root / "state.json"
    temporary = root / ".state.json.tmp"
    temporary.write_text(json.dumps(state, indent=2, sort_keys=True) + "\n")
    os.chmod(temporary, 0o600)
    temporary.replace(target)
    board = root / "board.json"
    board.write_text(json.dumps(snapshot, indent=2, sort_keys=True) + "\n")
    try:
        os.chmod(board, 0o644)
    except OSError:
        pass


def _now():
    raw = os.environ.get("COWORK_NOW")
    if raw:
        return int(float(raw))
    import time
    return int(time.time())
