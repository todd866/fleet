"""`fleet morning` writes the digest, files work, and can notify. It does not fix anything."""
import io
import json
import os
import subprocess
import sys
from contextlib import redirect_stderr, redirect_stdout
from datetime import datetime
from pathlib import Path

from fleet.morning.collect import collect, collect_local, local_host_id
from fleet.morning.config import load_config, site_label_prefixes
from fleet.morning.evaluate import _zone, evaluate
from fleet.morning.render import notification_text, render
from fleet.morning.snapshot import snapshot_dir, write_snapshot
from fleet.morning.work import file_items, work_path


def main(argv=None, notify=None, gather=None, run=None):
    argv = list(sys.argv[1:] if argv is None else argv)
    if argv and argv[0] == "usage":
        from fleet.ai_usage import main as usage_main
        return usage_main(argv[1:])
    if argv and argv[0] == "upstream":
        from fleet.upstream import main as upstream_main
        return upstream_main(argv[1:])
    if argv and argv[0] == "morning":
        argv = argv[1:]
    if argv and argv[0] == "follow":
        return follow(argv[1:])
    if argv and argv[0] == "collect":
        return _collect(argv[1:], gather=gather, run=run)
    args = _parse(argv)
    config = load_config(args.config) if args.config else _default_config()
    now = int(args.now if args.now is not None else datetime.now().timestamp())
    base = Path(args.home).expanduser() if args.home else Path.home()
    if not (config.get("jobs") or {}).get("label_prefixes"):
        config.setdefault("jobs", {})["label_prefixes"] = site_label_prefixes(base)
    here = args.here or local_host_id(base)
    notify_host = args.notify_host or config.get("notify_host") or here
    state = Path(args.state).expanduser() if args.state else base / ".local" / "state" / "fleet"
    if args.snapshot:
        snapshot = json.loads(Path(args.snapshot).read_text())
    else:
        snapshot = collect(
            config, here=here, now=now, state=state, home=base, gather=gather, run=run,
            snapshots=Path(args.snapshots).expanduser() if args.snapshots else None,
        )
    findings = evaluate(snapshot, config, now)
    date = datetime.fromtimestamp(now, _zone(config.get("tz"))).date().isoformat()
    text, document = render(findings, now, date)
    document["hosts"] = _host_meta(snapshot, now)
    cadence = _cadence(base, now, os.environ)
    if cadence:
        # The digest is as old as its schedule allows, not stale: say what the schedule is.
        document["cadence"] = cadence
    if args.dry_run:
        sys.stdout.write(text)
        return 0
    morning = state / "morning"
    morning.mkdir(parents=True, exist_ok=True)
    _write(morning / f"{date}.md", text)
    body = json.dumps(document, indent=2) + "\n"
    _write(morning / f"{date}.json", body)
    _write(morning / "summary.json", body)
    # Only the hub runs the digest. The other Macs read it from the synced folder the
    # snapshots already travel in; without this their copy is whatever they last made by hand.
    # A run pointed at another state directory (a test, a rehearsal) never publishes.
    shared = base / ".local" / "state" / "mac-sync"
    if not args.state and shared.is_dir():
        _write(shared / "fleet-morning" / "summary.json", body)
    board = Path(args.board).expanduser() if args.board else work_path(base / ".local" / "state")
    _write_items(board, findings, now)
    _sync_cowork(findings)
    note = notification_text(findings)
    receipt = {"status": "not-needed", "assistant_woken": False}
    if note:
        if notify is not None:
            notify(notify_host, note)
            receipt = {"status": "callback-returned", "assistant_woken": False}
        else:
            receipt = _deliver(notify_host, here, note)
    receipt.update(generated=now, host=here, target=notify_host,
                   actionable_keys=[row.key for row in findings if row.status in ("warn", "fail")])
    _write(morning / "delivery.json", json.dumps(receipt, indent=2) + "\n")
    if receipt["status"] in ("failed", "wrong-host"):
        print(f"fleet morning notification {receipt['status']}; see {morning / 'delivery.json'}", file=sys.stderr)
    return 0


def _collect(argv, gather=None, run=None):
    """`fleet morning collect --local` writes this Mac's snapshot and returns."""
    import argparse
    parser = argparse.ArgumentParser(prog="fleet morning collect")
    parser.add_argument("--local", action="store_true")
    parser.add_argument("--home", default=None)
    parser.add_argument("--here", default=None)
    parser.add_argument("--now", type=int)
    parser.add_argument("--config", default=None)
    args = parser.parse_args(argv)
    if not args.local:
        parser.error("collect reads this Mac only; pass --local")
    base = Path(args.home).expanduser() if args.home else Path.home()
    config = load_config(args.config) if args.config else _default_config()
    now = int(args.now if args.now is not None else datetime.now().timestamp())
    here = args.here or local_host_id(base)
    try:
        document = collect_local(config, here=here, now=now, home=base, run=run, gather=gather)
        write_snapshot(snapshot_dir(base), document)
    except ValueError as exc:
        # Say why: a silent exit 2 left the hub's snapshot stale unnoticed (15 Nov 2026).
        print(f"fleet morning collect: {type(exc).__name__}: {exc}", file=sys.stderr)
        return 2
    return 0


def follow(argv):
    """Ask ai-pick which engine has room. Record the assignment. Do not start the engine."""
    import argparse
    parser = argparse.ArgumentParser(prog="fleet morning follow")
    parser.add_argument("--state", default=None)
    parser.add_argument("--engines", default="claude,codex,cursor")
    args = parser.parse_args(argv)
    state = Path(args.state).expanduser() if args.state else Path.home() / ".local" / "state" / "fleet"
    summary_path = state / "morning" / "summary.json"
    if not summary_path.is_file():
        return 0
    summary = json.loads(summary_path.read_text())
    if summary.get("ok", True):
        return 0
    try:
        picked = subprocess.run(
            ["ai-pick", "--engines", args.engines],
            capture_output=True, text=True, timeout=30,
        )
    except (OSError, subprocess.TimeoutExpired):
        return 75
    if picked.returncode != 0:
        return picked.returncode
    engine = (picked.stdout or "").strip().splitlines()
    engine = engine[-1] if engine else ""
    rows = [row for row in summary.get("rows") or [] if row.get("status") in ("warn", "fail")]
    assignment = {"engine": engine, "digest": str(summary_path), "rows": rows}
    _write(state / "morning" / "follow.json", json.dumps(assignment, indent=2) + "\n")
    if engine:
        print(engine)
    return 0


def _parse(argv):
    import argparse
    parser = argparse.ArgumentParser(prog="fleet morning")
    parser.add_argument("--snapshot")
    parser.add_argument("--state")
    parser.add_argument("--board")
    parser.add_argument("--here")
    parser.add_argument("--home")
    parser.add_argument("--snapshots")
    parser.add_argument("--notify-host")
    parser.add_argument("--now", type=int)
    parser.add_argument("--config")
    parser.add_argument("--dry-run", action="store_true")
    return parser.parse_args(argv)


def _default_config():
    path = Path.home() / ".config" / "fleet" / "morning.toml"
    if path.is_file():
        return load_config(path)
    return load_config(None)


def _cadence(home, now, environ):
    """How often this digest is written and when the next one is due, from its own launchd job.

    The job is the one launchd says is running us (XPC_SERVICE_NAME), otherwise the plist in
    ~/Library/LaunchAgents whose command is `fleet morning`. None when there is no such job.
    """
    import plistlib
    from datetime import timezone
    from fleet.morning.probe import _utc_daily, _when

    agents = Path(home) / "Library" / "LaunchAgents"
    found = {}
    for path in sorted(agents.glob("*.plist")) if agents.is_dir() else []:
        try:
            with path.open("rb") as handle:
                plist = plistlib.load(handle)
        except (OSError, ValueError, plistlib.InvalidFileException):
            continue
        args = [str(part) for part in plist.get("ProgramArguments") or []]
        for index, part in enumerate(args[:-1]):
            if Path(part).name == "fleet" and args[index + 1] == "morning" and args[index + 2:index + 3] not in (["follow"], ["collect"]):
                found[str(plist.get("Label") or path.stem)] = plist
                break
    label = str(environ.get("XPC_SERVICE_NAME") or "")
    if label not in found:
        label = next(iter(found), "")
    if not label:
        return None
    plist = found[label]
    wrapped = _utc_daily(plist)
    if wrapped and wrapped["weekday"] is None:
        moment = datetime.fromtimestamp(now, timezone.utc)
        starts = []
        for slot in wrapped["slots"]:
            hour, minute = (int(part) for part in slot.split(":"))
            start = int(moment.replace(hour=hour, minute=minute, second=0, microsecond=0).timestamp())
            starts.append(start if start > now else start + 86400)
        upcoming = min(starts)
        clock = datetime.fromtimestamp(upcoming, timezone.utc).strftime("%H:%MZ")
        return {"every": "daily", "at": [f"{slot}Z" for slot in wrapped["slots"]], "next": upcoming,
                "text": f"daily, next at {clock}", "label": label}
    every = _when(plist)
    cadence = {"every": every, "text": every, "label": label}
    if "StartInterval" in plist and not wrapped:
        cadence["next"] = now + int(plist["StartInterval"])
    return cadence


def _host_meta(snapshot, now):
    meta = {}
    for name, host in (snapshot.get("hosts") or {}).items():
        if host.get("ok"):
            meta[name] = {"ok": True, "last_ok": host.get("collected_at") or now}
        else:
            meta[name] = {"ok": False, "away_since": host.get("away_since")}
    return meta


def _write_items(path, findings, now):
    existing = []
    if path.is_file():
        try:
            loaded = json.loads(path.read_text())
            existing = loaded.get("items") if isinstance(loaded, dict) else loaded
        except (OSError, ValueError):
            existing = []
    items = file_items(findings, existing or [], now)
    document = {"version": 1, "generated": now, "source": "fleet-morning", "items": items}
    _write(path, json.dumps(document, indent=2) + "\n")


def _sync_cowork(findings):
    """File warn/fail keys on the hub board. A pass closes an open key. The hub queues when it is away."""
    from cowork.cli import main as cowork_main

    open_keys = _cowork_open_keys(cowork_main)
    seen = set()
    for finding in findings:
        if not finding.key or finding.key in seen:
            continue
        seen.add(finding.key)
        if finding.status in ("warn", "fail"):
            _cowork(cowork_main, [
                "item", "add", finding.key,
                "--title", f"{finding.name} {finding.value}".strip(),
                "--detail", finding.detail or "",
                "--status", finding.status,
                "--scope", f"service:{finding.area}",
                "--source", "fleet-morning",
            ])
        elif finding.status in ("pass", "info") and finding.key in open_keys:
            _cowork(cowork_main, ["item", "close", finding.key])


def _cowork(cowork_main, argv):
    out, err = io.StringIO(), io.StringIO()
    try:
        with redirect_stdout(out), redirect_stderr(err):
            code = cowork_main(list(argv))
    except SystemExit as exc:
        code = exc.code if isinstance(exc.code, int) else 1
    except Exception as exc:
        print(f"cowork: {exc}", file=sys.stderr)
        return None
    if code not in (0, None):
        text = (err.getvalue() or out.getvalue()).strip()
        if text:
            print(text, file=sys.stderr)
    return code, out.getvalue(), err.getvalue()


def _cowork_open_keys(cowork_main):
    result = _cowork(cowork_main, ["item", "list", "--json"])
    if not result or result[0] != 0:
        return set()
    try:
        data = json.loads(result[1] or "")
    except ValueError:
        return set()
    items = data.get("items") if isinstance(data, dict) else None
    return {item.get("key") for item in items or [] if isinstance(item, dict) and item.get("key")}


def _write(path, text):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(path.suffix + ".tmp")
    tmp.write_text(text)
    os.replace(tmp, path)


def _deliver(host, here, body):
    """Record submission evidence, never claim a banner was seen or an agent woke."""
    receipt = {"status": "wrong-host", "assistant_woken": False, "user_seen": None}
    if host != here:
        receipt["reason"] = "notify_host is not this Mac; no remote delivery configured"
        return receipt
    script = f"display notification {json.dumps(body)} with title {json.dumps('fleet')}"
    try:
        result = subprocess.run(["osascript", "-e", script], capture_output=True, text=True, timeout=15)
    except (OSError, subprocess.TimeoutExpired) as exc:
        receipt.update(status="failed", reason=type(exc).__name__)
        return receipt
    receipt.update(status="submitted" if result.returncode == 0 else "failed", returncode=result.returncode)
    return receipt
