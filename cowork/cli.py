"""cowork command line. taskhub is the same program under the old name."""
import shlex
import argparse
import io
import json
import os
import subprocess
import sys
from contextlib import redirect_stderr, redirect_stdout

from cowork.hooks import PROVIDERS, clean_event, snippet
from cowork.hosts import resolve_host
from cowork.model import canonical_provider, held_line, parse_scope, parse_ttl, scopes_overlap
from cowork.store import Board, ClaimError, load_config
from cowork.transport import (
    MUTATIONS,
    effective_config,
    enqueue,
    flush,
    forward,
    should_forward,
    spawn_flush,
    with_host,
)
from cowork.view import table_from_snapshot


def _ttl_arg(text):
    try:
        return parse_ttl(text)
    except ValueError as exc:
        raise argparse.ArgumentTypeError(str(exc)) from exc


def main(argv=None):
    argv = list(sys.argv[1:] if argv is None else argv)
    host_at = _leading_host(argv)
    if host_at is not None:
        return _remote(argv, host_at)
    parser = _parser()
    args = parser.parse_args(argv)
    if args.cmd in ("observe", "hook", "hooks", "flush") or os.environ.get("COWORK_LOCAL") == "1":
        return _dispatch(args)
    config = effective_config()
    host = resolve_host(config)
    if should_forward(config, host):
        sent = with_host(argv, host, args.cmd)
        delivered, result = forward(sent, config)
        if delivered:
            sys.stdout.write(result.stdout)
            sys.stderr.write(result.stderr)
            if result.returncode == 0 and args.cmd in MUTATIONS:
                _replay(sent)
            return result.returncode
        enqueue(sent)
        spawn_flush()
        return _dispatch(parser.parse_args(sent))
    return _dispatch(args)


def _parser():
    parser = argparse.ArgumentParser(prog="cowork", description="Who holds which job across agent providers.")
    parser.add_argument("--color", action="store_true", help=argparse.SUPPRESS)
    sub = parser.add_subparsers(dest="cmd", required=True)

    claim = sub.add_parser("claim")
    claim.add_argument("work")
    claim.add_argument("--scope", action="append", default=[])
    claim.add_argument("--provider", required=True)
    claim.add_argument("--session", required=True)
    claim.add_argument("--host")
    claim.add_argument("--ttl", type=_ttl_arg, default=3600)
    claim.add_argument("--mail")
    claim.add_argument("--takeover")
    claim.add_argument("--parent")
    claim.add_argument("--note")
    claim.add_argument("--name")

    heart = sub.add_parser("heartbeat")
    heart.add_argument("work")
    heart.add_argument("--session", required=True)
    heart.add_argument("--provider")
    heart.add_argument("--host")

    note = sub.add_parser("note")
    note.add_argument("work")
    note.add_argument("text")
    note.add_argument("--session", required=True)
    note.add_argument("--provider")
    note.add_argument("--host")

    release = sub.add_parser("release")
    release.add_argument("work")
    release.add_argument("--session", required=True)
    release.add_argument("--provider")
    release.add_argument("--host")

    hand = sub.add_parser("handoff")
    hand.add_argument("work")
    hand.add_argument("--to", required=True)
    hand.add_argument("--note")
    hand.add_argument("--session", required=True)
    hand.add_argument("--provider")
    hand.add_argument("--host")
    hand.add_argument("--mail")

    done = sub.add_parser("done")
    done.add_argument("work")
    done.add_argument("--session", required=True)
    done.add_argument("--provider")
    done.add_argument("--host")

    check = sub.add_parser("check")
    check.add_argument("scope")

    for name in ("board", "who", "mine"):
        view = sub.add_parser(name)
        view.add_argument("--json", action="store_true")
        if name == "who":
            view.add_argument("query")
        if name == "mine":
            view.add_argument("--session", required=True)
            view.add_argument("--provider", required=True)
            view.add_argument("--host")

    ask = sub.add_parser("ask")
    ask.add_argument("work")
    ask.add_argument("message")

    join = sub.add_parser("join")
    join.add_argument("--provider", required=True)
    join.add_argument("--session", required=True)
    join.add_argument("--host")
    join.add_argument("--scope", action="append", default=[])
    join.add_argument("--work")
    join.add_argument("--ttl", type=_ttl_arg, default=3600)
    join.add_argument("--mail")
    join.add_argument("--name")

    leave = sub.add_parser("leave")
    leave.add_argument("--session", required=True)
    leave.add_argument("--provider", required=True)
    leave.add_argument("--host")

    child = sub.add_parser("child")
    child.add_argument("parent")
    child.add_argument("--work", required=True)
    child.add_argument("--provider", required=True)
    child.add_argument("--session", required=True)
    child.add_argument("--host")
    child.add_argument("--scope", action="append", default=[])
    child.add_argument("--ttl", type=_ttl_arg, default=3600)
    child.add_argument("--mail")
    child.add_argument("--name")

    hooks = sub.add_parser("hooks")
    hooks.add_argument("provider")

    hook = sub.add_parser("hook")
    hook.add_argument("provider")
    hook.add_argument("event")

    update = sub.add_parser("update")
    update.add_argument("file")

    observe = sub.add_parser("observe")
    observe.add_argument("--provider", default="local")
    observe.add_argument("--host")

    context = sub.add_parser("context")
    context.add_argument("--provider")
    context.add_argument("--session")
    context.add_argument("--host")
    context.add_argument("--scope")

    doctor = sub.add_parser("doctor")
    doctor.add_argument("--json", action="store_true")

    item = sub.add_parser("item")
    item_cmd = item.add_subparsers(dest="item_cmd", required=True)
    add = item_cmd.add_parser("add")
    add.add_argument("key")
    add.add_argument("--title", default="")
    add.add_argument("--detail", default="")
    add.add_argument("--status", default="warn")
    add.add_argument("--scope", action="append", default=[])
    add.add_argument("--source", default="")
    add.add_argument("--host")
    close = item_cmd.add_parser("close")
    close.add_argument("key")
    close.add_argument("--host")
    listing = item_cmd.add_parser("list")
    listing.add_argument("--json", action="store_true")
    listing.add_argument("--host")

    sub.add_parser("flush")
    sub.add_parser("open")
    sub.add_parser("status")
    json_cmd = sub.add_parser("json")
    json_cmd.add_argument("--json", action="store_true")
    sub.add_parser("report")

    return parser


def _leading_host(argv):
    """--host before the subcommand is the hub. Later --host is the claim's machine."""
    index = 0
    while index < len(argv):
        if argv[index] == "--color":
            index += 1
            continue
        if argv[index] == "--host":
            return index
        return None
    return None


def _remote(argv, idx):
    """Run the same command on the hub. The host must be listed in config."""
    if idx + 1 >= len(argv):
        print("cowork: --host needs a name", file=sys.stderr)
        return 2
    host = argv[idx + 1]
    rest = argv[:idx] + argv[idx + 2 :]
    config = effective_config()
    allowed = set(config.get("hub_aliases") or [])
    if config.get("hub"):
        allowed.add(config["hub"])
    if host not in allowed:
        print("cowork: host is not in hub_aliases", file=sys.stderr)
        return 2
    remote = [
        "ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=8",
        host, "env", "COWORK_LOCAL=1", "PATH=$HOME/.local/bin:/opt/homebrew/bin:/usr/bin:/bin", "$HOME/.local/bin/cowork", *[shlex.quote(a) for a in rest],
    ]
    result = subprocess.run(remote, check=False)
    return result.returncode


def _dispatch(args):
    try:
        if args.cmd == "hook":
            return _hook(args)
        if args.cmd == "hooks":
            return _hooks(args)
        if args.cmd == "open":
            return _open(args)
        if args.cmd == "observe":
            return _observe(args)
        if args.cmd == "flush":
            return flush()
        if args.cmd == "doctor":
            return _doctor(args)
        board = Board()
        if args.cmd == "context":
            return _context(board, args)
        if args.cmd == "item":
            return _item(board, args)
        plain = _plain(args)
        if args.cmd == "claim":
            warnings = board.claim(
                args.work, _scopes(args.scope), args.provider, args.session, args.host,
                args.ttl, mail=args.mail, takeover=args.takeover, parent=args.parent,
                note=args.note, label=args.name)
            _print_warnings(warnings)
            return 0
        if args.cmd == "heartbeat":
            board.heartbeat(args.work, args.session, args.provider, args.host)
            return 0
        if args.cmd == "note":
            board.note(args.work, args.text, args.session, args.provider, args.host)
            return 0
        if args.cmd == "release":
            board.release(args.work, args.session, args.provider, args.host)
            return 0
        if args.cmd == "handoff":
            board.handoff(
                args.work, args.session, args.to, args.note, args.provider, args.host, args.mail)
            return 0
        if args.cmd == "done":
            board.done(args.work, args.session, args.provider, args.host)
            return 0
        if args.cmd == "check":
            return _check(board, parse_scope(args.scope), plain)
        if args.cmd in ("board", "status", "report", "json"):
            return _show(board, plain, json_out=args.cmd == "json" or getattr(args, "json", False))
        if args.cmd == "who":
            return _who(board, parse_scope(args.query) if _scoped(args.query) else args.query, plain, args.json)
        if args.cmd == "mine":
            return _mine(board, args, plain)
        if args.cmd == "ask":
            return _ask(board, args.work, args.message)
        if args.cmd == "join":
            board.join(
                args.provider, args.session, args.host, _scopes(args.scope),
                work=args.work, ttl=args.ttl, mail=args.mail, label=args.name)
            return 0
        if args.cmd == "leave":
            board.leave(args.provider, args.session, args.host)
            return 0
        if args.cmd == "child":
            warnings = board.child(
                args.parent, args.work, _scopes(args.scope), args.provider, args.session,
                args.host, args.ttl, mail=args.mail, label=args.name)
            _print_warnings(warnings)
            return 0
        if args.cmd == "update":
            return _update(board, args.file)
    except ClaimError as exc:
        print(exc.message, file=sys.stderr)
        return 1
    except ValueError as exc:
        print(str(exc), file=sys.stderr)
        return 2
    return 2


def _plain(args):
    if getattr(args, "color", False) and os.environ.get("COWORK_PLAIN") != "1":
        return False
    if os.environ.get("COWORK_PLAIN") == "1" or os.environ.get("NO_COLOR") is not None:
        return True
    return not sys.stdout.isatty()


def _scopes(items):
    return [parse_scope(item) for item in items or []]


def _scoped(text):
    head = text.split(":", 1)[0]
    return head in ("repo", "path", "machine", "drive", "service", "worktree", "output", "publish") or text.startswith("/") or text.startswith(".")


def _print_warnings(lines):
    for line in lines or []:
        print(line, file=sys.stderr)


def _show(board, plain, json_out=False, snapshot=None):
    snapshot = snapshot if snapshot is not None else board.snapshot()
    if json_out:
        print(json.dumps(snapshot, indent=2, sort_keys=True))
        return 0
    sys.stdout.write(table_from_snapshot(snapshot, board.now, plain=plain))
    return 0


def _who(board, query, plain, json_out):
    snapshot = board.snapshot()
    if isinstance(query, str):
        # A bare name matches a repo, a drive, or a worktree of that name.
        matched = []
        for claim in snapshot["claims"]:
            if any(scope["kind"] in ("repo", "drive", "worktree") and scope["value"].rstrip("/").split("/")[-1] == query
                   for scope in claim["scope"]):
                matched.append(claim)
        presence = [row for row in snapshot["presence"] if row.get("project") == query]
    else:
        matched = [claim for claim in snapshot["claims"] if scopes_overlap(claim["scope"], [query])]
        presence = []
    filtered = {"version": 1, "generated": snapshot["generated"], "claims": matched, "presence": presence}
    return _show(board, plain, json_out, filtered)


def _mine(board, args, plain):
    from cowork.model import session_key
    snapshot = board.snapshot()
    host = args.host or board.host
    key = session_key(args.provider, host, args.session)
    matched = []
    for claim in snapshot["claims"]:
        owner = claim["owner"]["session"]
        if owner == key or owner == args.session:
            matched.append(claim)
    presence = [row for row in snapshot["presence"] if row.get("id") in (key, args.session)]
    filtered = {"version": 1, "generated": snapshot["generated"], "claims": matched, "presence": presence}
    return _show(board, plain, args.json, filtered)


def _replay(argv):
    args = _parser().parse_args(argv)
    sink = io.StringIO()
    with redirect_stdout(sink), redirect_stderr(sink):
        return _dispatch(args)


def _observe(args):
    try:
        raw_text = sys.stdin.read(1048577)
        if len(raw_text.encode()) > 1048576:
            return 0
        payload = json.loads(raw_text) if raw_text.strip() else {}
    except Exception:
        return 0
    try:
        from cowork.observe import record_observation
        config = effective_config()
        host = args.host or resolve_host(config)
        board = Board(host=host)
        return record_observation(
            payload, args.provider, board.root, board.volumes, host, board.hostname,
            board.now, forward=should_forward(config, host), pid=board.pid,
        )
    except Exception:
        return 0


def _item(board, args):
    from cowork.model import age

    if args.item_cmd == "add":
        board.add_item(
            args.key, title=args.title, detail=args.detail, status=args.status,
            scopes=_scopes(args.scope), source=args.source)
        return 0
    if args.item_cmd == "close":
        board.close_item(args.key)
        return 0
    if args.item_cmd == "list":
        items = board.open_items()
        if getattr(args, "json", False):
            print(json.dumps({"items": items}, indent=2, sort_keys=True))
            return 0
        for item in items:
            print(f"{item.get('key') or '·'}  age {age(board.now, item.get('opened') or 0)}")
        return 0
    return 2


def _context(board, args):
    from cowork.doctor import context_line
    provider = args.provider or os.environ.get("COWORK_PROVIDER") or "local"
    session = args.session or os.environ.get("COWORK_SESSION") or ""
    host = args.host or board.host
    if args.scope:
        scope = parse_scope(args.scope)
        project = scope["value"].rstrip("/").split("/")[-1]
    else:
        project = os.path.basename(os.getcwd()) or "work"
    sys.stdout.write(context_line(board.snapshot(), provider, session, host, project) + "\n")
    return 0


def _doctor(args):
    from cowork.doctor import format_report, report
    data = report(Board())
    if args.json:
        print(json.dumps(data, indent=2, sort_keys=True))
    else:
        sys.stdout.write(format_report(data))
    return 0


def _check(board, scope, plain):
    del plain
    holders, stale, foreign = board.check(scope)
    if holders:
        for claim in holders:
            print(held_line(claim))
        return 1
    if foreign:
        mail = f"{foreign['host']}:{foreign['job']}"
        print(f'held by {foreign["job"]} (local, {foreign["host"]}) \u2014 ask: agent-mail send {mail}')
        return 1
    if stale:
        for claim in stale:
            from cowork.model import display_owner
            who = display_owner(claim.get("provider"), claim.get("label"), claim.get("mail"))
            print(
                f"~ {claim['work']} {who} {claim.get('host')} agent-mail send {claim.get('mail')}",
                file=sys.stderr)
        return 0
    return 0


def _ask(board, work, message):
    snapshot = board.snapshot()
    claim = next((item for item in snapshot["claims"] if item["work"] == work), None)
    if claim is None:
        print("no such work", file=sys.stderr)
        return 1
    mail = claim["owner"]["mail"]
    program = os.environ.get("COWORK_AGENT_MAIL") or "agent-mail"
    result = subprocess.run([program, "send", mail, message], check=False)
    return result.returncode


def _update(board, path):
    data = sys.stdin.read() if path == "-" else open(path, encoding="utf-8").read()
    if len(data.encode()) > 65536:
        print("Update exceeds 64 KiB", file=sys.stderr)
        return 1
    board.migrate(json.loads(data))
    return 0


def _hooks(args):
    name = canonical_provider(args.provider)
    if args.provider.lower() not in PROVIDERS and name not in PROVIDERS:
        print("usage: cowork hooks claude|codex|cursor|agentkit|local|cloud", file=sys.stderr)
        return 2
    sys.stdout.write(snippet(args.provider))
    return 0


def _hook(args):
    try:
        raw_text = sys.stdin.read(1048577)
        if len(raw_text.encode()) > 1048576:
            return 0
        raw = json.loads(raw_text) if raw_text.strip() else {}
        board = Board()
        event = clean_event(raw, args.provider, board.host, args.event, board.now)
        if not event:
            return 0
        board.apply_events([event])
        if event["event"] == "end":
            board.release_session(event["id"])
        elif event["event"] in ("start", "activity", "turn_end"):
            board.heartbeat_session(event["id"])
        return 0
    except Exception:
        return 0


def _open(_args):
    """Private dashboard launcher. Refuses to open a tunnel until a hub is configured."""
    config = load_config()
    hub = config.get("hub") or ""
    aliases = set(config.get("hub_aliases") or ([] if not hub else [hub]))
    if not hub or not aliases:
        print("cowork: no hub configured for the private dashboard", file=sys.stderr)
        return 2
    print("cowork: dashboard server is not in this tree", file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main())
