"""Site values for the fleet's Python tools: which Mac is the hub, which is the primary laptop,
which launchd labels are ours, where the drive registry lives, where findings are mailed.

Read from ~/.config/fleet/site.json (FLEET_SITE overrides the path). Every key is optional; the
defaults are generic, so the code carries no host or project names of its own.

  {"hub": "home-hub", "primary": "laptop", "job_label_prefixes": ["com.example"],
   "drives_project": "~/Projects/drives", "mail_to": "laptop:fleet"}
"""
import json
import os
from pathlib import Path

DEFAULTS = {
    "hub": "hub",
    "primary": "primary",
    "job_label_prefixes": ["example"],
    "drives_project": "~/Projects/drives",
    "mail_to": "primary:fleet",
}


def load(path=None):
    path = Path(path or os.environ.get("FLEET_SITE") or Path.home() / ".config/fleet/site.json")
    site = dict(DEFAULTS)
    try:
        data = json.loads(path.read_text())
    except (OSError, ValueError):
        data = {}
    if isinstance(data, dict):
        site.update({k: v for k, v in data.items() if v not in (None, "")})
    return site


SITE = load()


def hub():
    return SITE["hub"]


def primary():
    return SITE["primary"]


def drives_project(home=None):
    raw = str(SITE["drives_project"])
    if raw.startswith("~/") and home is not None:
        return Path(home) / raw[2:]
    return Path(os.path.expanduser(raw))
