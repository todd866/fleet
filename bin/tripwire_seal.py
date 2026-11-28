"""Tripwire's seal: proof that tripwire itself wrote a piece of its own state.

Tripwire acts on what it saved earlier: the baseline, the kept copy of a login-key file, an
incident's list of holds. All of it sits in files the owner's own programs can write, so without
proof of origin anything able to write a file could have tripwire put bytes of its choosing into
`authorized_keys`. Every such file therefore carries an HMAC-SHA256 made with a key that only
tripwire reads, and what fails the check is treated as evidence, never as an instruction.

The key is 32 random bytes in a file that is created once, exclusively, and used only while it is
a plain file of this user's with mode 0600 in a folder nobody else can write (the same care as
agentkit's ledger key). It lives in agentkit's state folder because the command guard already
keeps agents from writing there; the guard must also refuse to let them read it (docs/tripwire.md).
The seal is as strong as that refusal: a program running as the owner that reads the key can
forge anything here.
"""
import hashlib
import hmac
import json
import os
import pathlib
import pwd
import stat

FIELD = "seal"
FLAGS = os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC
_KEYS = {}


def real_home():
    """The account's home from the password database: `$HOME` is whatever the caller says."""
    try:
        return pathlib.Path(pwd.getpwuid(os.getuid()).pw_dir)
    except KeyError:
        return pathlib.Path(os.path.expanduser("~"))


def key_path(home, state, config):
    """Where the key is. The real key signs only the real state: run with any override
    (TRIPWIRE_STATE, TRIPWIRE_CONFIG, HOME) and the key is one inside that state folder, so a
    run pointed at other files cannot produce anything the scheduled check would accept."""
    real = real_home()
    usual = ((home, real), (state, real / ".local/state/fleet/tripwire"), (config, real / ".config/fleet/tripwire.json"))
    if all(os.path.realpath(given) == os.path.realpath(expected) for given, expected in usual):
        return real / ".local/state/agentkit/tripwire.key"
    return pathlib.Path(state) / "seal.key"


def _folder(path, create):
    """An open descriptor for the key's folder, only if it is ours and nobody else can write it."""
    try:
        if create:
            os.makedirs(path, mode=0o700, exist_ok=True)
        fd = os.open(path, os.O_RDONLY | os.O_DIRECTORY | FLAGS)
    except OSError:
        return None
    info = os.fstat(fd)
    if stat.S_ISDIR(info.st_mode) and info.st_uid == os.geteuid() and not info.st_mode & 0o022:
        return fd
    os.close(fd)
    return None


def _read(folder, name):
    try:
        fd = os.open(name, os.O_RDONLY | FLAGS, dir_fd=folder)
    except OSError:
        return None
    try:
        info = os.fstat(fd)
        if not (stat.S_ISREG(info.st_mode) and info.st_uid == os.geteuid() and stat.S_IMODE(info.st_mode) == 0o600):
            return None
        data = os.read(fd, 257)
    except OSError:
        return None
    finally:
        os.close(fd)
    return data if 16 <= len(data) <= 256 else None


def load_key(path, create=False):
    """(key, created). The key is None when there is none or it cannot be trusted; `created` is
    True only for the call that made it, which is how tripwire knows nothing can be sealed yet."""
    path = pathlib.Path(path)
    if str(path) in _KEYS:
        return _KEYS[str(path)], False
    folder = _folder(path.parent, create)
    if folder is None:
        return None, False
    try:
        key, created = _read(folder, path.name), False
        if key is None and create:
            fresh = os.urandom(32)
            try:
                # O_EXCL: fails when anything (a link, a FIFO, a stranger's file) is already there.
                fd = os.open(path.name, os.O_WRONLY | os.O_CREAT | os.O_EXCL | FLAGS, 0o600, dir_fd=folder)
            except FileExistsError:
                key = _read(folder, path.name)  # another run made it first, or it is not ours to use
            except OSError:
                key = None
            else:
                try:
                    os.fchmod(fd, 0o600)
                    os.write(fd, fresh)
                finally:
                    os.close(fd)
                key, created = _read(folder, path.name), True
                if key != fresh:
                    key, created = None, False
    finally:
        os.close(folder)
    if key is not None:
        _KEYS[str(path)] = key
    return key, created


def sha(data):
    return hashlib.sha256(data).hexdigest()


def mac(key, *parts):
    """HMAC-SHA256 over the parts, each prefixed with its length so that no two lists of parts
    collide. An empty string with no key: nothing can then be sealed or checked."""
    if not key:
        return ""
    code = hmac.new(key, digestmod=hashlib.sha256)
    for part in parts:
        data = part if isinstance(part, bytes) else str(part).encode()
        code.update(len(data).to_bytes(8, "big") + data)
    return code.hexdigest()


def same(found, expected):
    return bool(expected) and isinstance(found, str) and hmac.compare_digest(found.encode(), expected.encode())


def _body(value):
    return json.dumps({k: v for k, v in value.items() if k != FIELD}, sort_keys=True, separators=(",", ":"))


def sealed(key, kind, name, value):
    """A copy of the JSON object with its seal. `kind` and `name` tie it to one place, so a
    sealed record cannot be replayed as another incident's or as the baseline."""
    value = json.loads(json.dumps({k: v for k, v in value.items() if k != FIELD}))  # as it will be read back
    value[FIELD] = mac(key, kind, name, _body(value))
    return value


def check(key, kind, name, value):
    return isinstance(value, dict) and same(value.get(FIELD), mac(key, kind, name, _body(value)))
