"""Fleet host id. LocalHostName maps through diskmap, then the built-in fleet table."""
import json
import os
import subprocess
from pathlib import Path

# Same names diskmap ships. A config file replaces or extends these.
DEFAULT_HOSTS = {
    "Coordinator-Mac": "desk",
    "Worker-One": "lab",
    "Worker-Two": "studio",
}


def local_host_name():
    forced = os.environ.get("COWORK_LOCAL_HOST_NAME")
    if forced is not None:
        return forced.strip()
    try:
        out = subprocess.run(
            ["scutil", "--get", "LocalHostName"],
            capture_output=True,
            text=True,
            timeout=0.15,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired):
        return ""
    if out.returncode != 0:
        return ""
    return out.stdout.strip()


def host_map():
    mapping = dict(DEFAULT_HOSTS)
    raw = os.environ.get("COWORK_DISKMAP_CONFIG")
    path = Path(raw) if raw else Path.home() / ".config" / "diskmap" / "config.json"
    if not path.is_file():
        return mapping
    try:
        data = json.loads(path.read_text())
    except (OSError, ValueError):
        return mapping
    hosts = data.get("hosts") if isinstance(data, dict) else None
    if not isinstance(hosts, dict):
        return mapping
    for name, profile in hosts.items():
        if isinstance(profile, dict) and profile.get("id"):
            mapping[str(name)] = str(profile["id"])
    return mapping


def resolve_host(config=None):
    config = config or {}
    env = os.environ.get("COWORK_HOST")
    if env:
        return env
    configured = config.get("host")
    if configured:
        return str(configured)
    name = local_host_name()
    mapping = host_map()
    if name and name in mapping:
        return mapping[name]
    if name:
        return name
    return "localhost"
