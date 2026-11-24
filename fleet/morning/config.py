"""Thresholds live in ~/.config/fleet/morning.toml. Missing keys keep these defaults."""
import copy
import tomllib
from pathlib import Path

DEFAULTS = {
    "hosts": [],
    "hub": "hub",
    "notify_host": "primary",
    "tz": "local",
    "probe_s": 20,
    "disk": {
        "floor_gb": 40,
        "fail_ratio": 0.5,
        "trend_warn_gb": -20,
        "trend_fail_gb": -80,
        "floors": {},
        # Per-Mac targets for the internal disk: "" reads the shared storage-audit file.
        "targets_file": "",
    },
    "backups": {
        "idrive_warn_h": 36,
        "idrive_fail_h": 72,
        "drive_mirror_warn_h": 26,
        "drive_mirror_fail_h": 48,
        "hdd_backup_warn_h": 36,
        "hdd_backup_fail_h": 168,
        "icloud_stall_h": 6,
        "idrive_hosts": [],
        "mirror_hosts": [],
        "hdd_hosts": [],
        "icloud_hosts": [],
        "syncthing_hosts": [],
    },
    # label_prefixes: this site's launchd label prefixes, stripped from job names in the digest.
    # Empty reads ~/.config/fleet/utc-prefixes, the list the UTC scheduler already keeps.
    "jobs": {"overdue_h": 2, "label_prefixes": []},
    "thermal": {
        # No hot_c here: each Mac is judged by its own heavy-slot `cool` target, which travels
        # in its snapshot. Set hot_c only as the line for a Mac that sends none.
        "hot_hours_warn": 2,
        "hot_hours_fail": 6,
        "wait_warn_s": 600,
        "gap_s": 900,
        "window_h": 24,
    },
    "capacity": {"near_empty_pct": 10, "near_empty_jobs": 5, "stale_h": 3},
    "agents": {"stale_claim_h": 6, "unread_mail_h": 12},
    "projects": {"heavy_runtime_s": 60, "active_host": "primary", "check_max_age_h": 30},
    "surface_probes": {"logs": [], "window_h": 24, "ip_warn": 20, "ip_fail": 100, "total_warn": 50, "total_fail": 300},
}


def _merge(base, override):
    # A copy all the way down: a caller that edits its config must not edit DEFAULTS.
    merged = copy.deepcopy(base)
    for key, value in (override or {}).items():
        if isinstance(value, dict) and isinstance(merged.get(key), dict):
            merged[key] = _merge(merged[key], value)
        else:
            merged[key] = value
    return merged


def site_label_prefixes(home=None):
    """This site's launchd label prefixes, one per line in ~/.config/fleet/utc-prefixes."""
    path = Path(home or Path.home()) / ".config" / "fleet" / "utc-prefixes"
    try:
        lines = path.read_text().splitlines()
    except OSError:
        return []
    return [line.strip() for line in lines if line.strip() and not line.startswith("#")]


def load_config(path=None):
    """Return the threshold dict. `path` is the toml file; a missing optional file uses defaults."""
    data = {}
    if path:
        text = Path(path).read_text()
        data = tomllib.loads(text) if text.strip() else {}
    return _merge(DEFAULTS, data)
