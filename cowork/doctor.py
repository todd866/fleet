"""What the morning report and Glancebar read. Local files only."""
from pathlib import Path

PRESENCE_WINDOW = 3600


def report(board, home=None):
    home = Path(home) if home is not None else Path.home()
    snapshot = board.snapshot()
    return {
        "version": 1,
        "generated": int(board.now),
        "sessions": _sessions(snapshot, board.now),
        "hooks_missing": hooks_missing(home),
        "no_heartbeat": _quiet(snapshot),
    }


def format_report(data):
    lines = []
    for row in data["sessions"]:
        mark = "*" if row["seen"] else "·"
        lines.append(
            f"{row['provider']}  {row['host']}  seen {row['seen']}  reg {row['registered']}  {mark}"
        )
    for name in data["hooks_missing"]:
        lines.append(f"{name}  hooks  !")
    for claim in data["no_heartbeat"]:
        who = claim.get("label") or claim.get("provider") or "·"
        lines.append(f"~  {claim['work']}  {who}  {claim['host']}")
    return "\n".join(lines) + ("\n" if lines else "")


def hooks_missing(home):
    home = Path(home)
    checks = {
        "claude": [home / ".claude" / "settings.json"],
        "codex": [home / ".codex" / "hooks.json", home / ".codex" / "config.toml"],
        "cursor": [home / ".cursor" / "hooks.json"],
    }
    missing = []
    for name, paths in checks.items():
        if not _installed(paths):
            missing.append(name)
    agent = home / ".agentkit"
    if agent.is_dir() and not _installed([agent / "config.json", agent / "hooks.json"]):
        missing.append("agentkit")
    return missing


def _installed(paths):
    for path in paths:
        if not path.is_file():
            continue
        try:
            text = path.read_text(errors="ignore")
        except OSError:
            continue
        if "cowork hook" in text or "cowork observe" in text or "cowork context" in text:
            return True
    return False


def _sessions(snapshot, now):
    groups = {}
    for row in snapshot.get("presence") or []:
        provider = row.get("provider") or "local"
        host = row.get("host") or "·"
        slot = groups.setdefault((provider, host), {"provider": provider, "host": host, "seen": 0, "registered": 0})
        slot["registered"] += 1
        age = int(now) - int(row.get("at") or 0)
        if age <= PRESENCE_WINDOW and row.get("event") != "end" and row.get("state") != "released":
            slot["seen"] += 1
    return [groups[key] for key in sorted(groups)]


def _quiet(snapshot):
    rows = []
    for claim in snapshot.get("claims") or []:
        if claim.get("state") in ("done", "released"):
            continue
        if int(claim.get("heartbeat") or 0) != 0 and claim.get("state") != "stale":
            continue
        owner = claim.get("owner") or {}
        rows.append({
            "work": claim.get("work"),
            "provider": owner.get("provider") or "",
            "host": owner.get("host") or "",
            "label": owner.get("label") or "",
            "state": claim.get("state") or "",
        })
    return rows


def context_line(snapshot, provider, session, host, project):
    """One line: project, other holders, your claims, conflict mark."""
    from cowork.model import display_owner, scopes_overlap, session_key

    key = session_key(provider, host, session) if session else ""
    wanted = [{"kind": "repo", "value": project}]
    held = []
    mine = []
    conflict = "·"
    for claim in snapshot.get("claims") or []:
        if claim.get("state") not in ("active", "handed-off"):
            continue
        if not scopes_overlap(claim.get("scope") or [], wanted):
            continue
        owner = claim.get("owner") or {}
        mine_it = bool(session) and owner.get("session") in (key, session)
        if mine_it:
            mine.append(claim.get("work") or "·")
            if claim.get("conflicts"):
                other = claim["conflicts"][0].get("work") or "·"
                conflict = f"! {other}"
        else:
            held.append(
                f"{display_owner(owner.get('provider'), owner.get('label'), owner.get('mail'))} {owner.get('host') or '·'}"
            )
    if conflict == "·" and held and mine:
        conflict = "! " + held[0].split(" ")[0]
    held_text = ", ".join(held) if held else "·"
    mine_text = " ".join(mine) if mine else "·"
    items = [item for item in snapshot.get("items") or [] if item.get("state") == "open"]
    items.sort(key=lambda item: (int(item.get("opened") or 0), item.get("key") or ""))
    top = " ".join(item.get("key") or "·" for item in items[:3])
    opened = f"  open {len(items)}" + (f"  {top}" if top else "")
    return f"{project}  held {held_text}  mine {mine_text}  {conflict}{opened}"
