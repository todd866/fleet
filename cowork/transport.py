"""The board of record is the hub. This Mac queues when the hub is away.

`observe` and `hook` never call this on the request path. `flush` does,
in another process.
"""
import shlex
import json
import os
import subprocess
import sys
from pathlib import Path

from cowork.store import default_state_dir, load_config

# With no config file the hub is the ssh alias `hub` (and `hub-lan`): example names. A site
# names its own in ~/.config/cowork/config.json: {"hub": "...", "hub_aliases": ["...", "..."]}.
FLEET_HUB = {"hub": "hub", "hub_aliases": ["hub", "hub-lan"]}
MUTATIONS = {
    "claim", "heartbeat", "note", "release", "handoff", "done",
    "join", "leave", "child", "update", "item",
}
HOST_COMMANDS = MUTATIONS | {"mine", "context", "observe"}


def config_is_explicit():
    if os.environ.get("COWORK_CONFIG"):
        return True
    return (Path.home() / ".config" / "cowork" / "config.json").is_file()


def effective_config():
    cfg = dict(load_config())
    if config_is_explicit():
        return cfg
    if cfg.get("hub"):
        return cfg
    merged = {"hub": FLEET_HUB["hub"], "hub_aliases": list(FLEET_HUB["hub_aliases"])}
    merged.update(cfg)
    return merged


def hub_targets(config):
    ordered = []
    for name in [config.get("hub"), *(config.get("hub_aliases") or [])]:
        if name and name not in ordered:
            ordered.append(name)
    return ordered


def should_forward(config, host_id):
    if os.environ.get("COWORK_LOCAL") == "1":
        return False
    targets = hub_targets(config)
    if not targets or host_id in targets:
        return False
    return True


def with_host(argv, host, command):
    if command not in HOST_COMMANDS or "--host" in argv or not host:
        return list(argv)
    return [*argv, "--host", host]


def ssh_cowork(host, argv, stdin="", timeout=2):
    cmd = [
        "ssh", "-o", "BatchMode=yes", "-o", f"ConnectTimeout={timeout}",
        host, "env", "COWORK_LOCAL=1", "PATH=$HOME/.local/bin:/opt/homebrew/bin:/usr/bin:/bin", "$HOME/.local/bin/cowork", *[shlex.quote(a) for a in argv],
    ]
    try:
        return subprocess.run(
            cmd, input=stdin or None, capture_output=True, text=True, timeout=timeout + 15,
        )
    except (OSError, subprocess.TimeoutExpired):
        return None


def forward(argv, config, stdin=""):
    """(delivered, result). delivered is false when every alias is away."""
    for host in hub_targets(config):
        result = ssh_cowork(host, argv, stdin=stdin)
        if result is None or result.returncode == 255:
            continue
        return True, result
    return False, None


def queue_path(root=None):
    return (Path(root) if root else default_state_dir()) / "queue.jsonl"


def enqueue(argv, root=None, stdin=""):
    path = queue_path(root)
    path.parent.mkdir(parents=True, exist_ok=True)
    item = {"argv": list(argv)}
    if stdin:
        item["stdin"] = stdin
    with path.open("a", encoding="utf-8") as handle:
        handle.write(json.dumps(item, sort_keys=True) + "\n")


def spawn_flush():
    if os.environ.get("COWORK_FLUSH") == "0":
        return
    env = os.environ.copy()
    env["COWORK_LOCAL"] = "1"
    env["COWORK_FLUSH"] = "0"
    subprocess.Popen(
        [sys.executable, "-m", "cowork", "flush"],
        env=env,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        start_new_session=True,
    )


def flush(config=None, root=None):
    path = queue_path(root)
    if not path.is_file():
        return 0
    config = config if config is not None else effective_config()
    keep = []
    away = False
    for line in path.read_text().splitlines():
        if not line.strip():
            continue
        if away:
            keep.append(line)
            continue
        try:
            item = json.loads(line)
        except ValueError:
            continue
        delivered, _result = forward(item.get("argv") or [], config, stdin=item.get("stdin") or "")
        if not delivered:
            away = True
            keep.append(line)
    if keep:
        path.write_text("\n".join(keep) + "\n")
    else:
        path.unlink()
    return 0
