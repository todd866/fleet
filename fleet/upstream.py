"""fleet upstream: watch other projects' releases for ideas worth taking.

A standing link to a project we learn from (not a dependency: its code may be under a
licence we do not copy from). `check` asks GitHub for each repo's releases and records
which ones nobody here has reviewed yet; `fleet morning` turns an unreviewed release into
a work item. The agent that works it reads `show`, writes what it took or skipped into the
repo's notes file, then runs `ack`.

Config: ~/.config/fleet/upstream.toml (example: docs/examples/upstream.toml).
State:  ~/.local/state/fleet/upstream/<name>.json, one file per repo.
Network: one unauthenticated GitHub API call per repo per check (GITHUB_TOKEN is used
when set). Stdlib only.
"""
import argparse
import json
import os
import sys
import time
import tomllib
import urllib.request
from pathlib import Path

API = "https://api.github.com"


def config_path(home):
    return Path(home) / ".config/fleet/upstream.toml"


def state_dir(home):
    return Path(home) / ".local/state/fleet/upstream"


def load_repos(home):
    path = config_path(home)
    try:
        data = tomllib.loads(path.read_text())
    except FileNotFoundError:
        return []
    repos = []
    for r in data.get("repo") or []:
        if isinstance(r, dict) and r.get("name") and r.get("github"):
            repos.append(r)
    return repos


def fetch_releases(github, opener=None):
    """Newest first: [{tag, name, published_at, url, body}]."""
    req = urllib.request.Request(f"{API}/repos/{github}/releases?per_page=30",
                                 headers={"Accept": "application/vnd.github+json",
                                          "User-Agent": "fleet-upstream"})
    token = os.environ.get("GITHUB_TOKEN")
    if token:
        req.add_header("Authorization", f"Bearer {token}")
    with (opener or urllib.request.urlopen)(req, timeout=30) as resp:
        raw = json.load(resp)
    out = []
    for r in raw if isinstance(raw, list) else []:
        if r.get("draft") or not r.get("tag_name"):
            continue
        out.append({"tag": r["tag_name"], "name": r.get("name") or r["tag_name"],
                    "published_at": r.get("published_at"), "url": r.get("html_url"),
                    "body": r.get("body") or ""})
    return out


def _read(path):
    try:
        return json.loads(path.read_text())
    except (OSError, ValueError):
        return None


def _write(path, data):
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(".tmp")
    tmp.write_text(json.dumps(data, indent=2) + "\n")
    tmp.replace(path)


def check(home, now=None, fetch=fetch_releases):
    """Update every repo's state. A first check is a baseline: what exists is taken as seen,
    since the decision to watch a repo is made after reading it."""
    now = int(now if now is not None else time.time())
    results = []
    for repo in load_repos(home):
        path = state_dir(home) / f"{repo['name']}.json"
        prev = _read(path) or {}
        state = {"name": repo["name"], "github": repo["github"], "notes": repo.get("notes"),
                 "seen": list(prev.get("seen") or []), "checked_at": now,
                 "last_ok": prev.get("last_ok"), "error": None, "new": []}
        try:
            releases = fetch(repo["github"])
        except Exception as exc:  # network, rate limit, JSON: recorded, never raised
            state["error"] = f"{type(exc).__name__}: {exc}"[:300]
            state["new"] = prev.get("new") or []
        else:
            state["last_ok"] = now
            if "seen" not in prev:
                state["seen"] = [r["tag"] for r in releases]
            seen = set(state["seen"])
            state["new"] = [{k: r[k] for k in ("tag", "name", "published_at", "url")}
                            for r in releases if r["tag"] not in seen]
        _write(path, state)
        results.append(state)
    return results


def ack(home, name, tags=None):
    path = state_dir(home) / f"{name}.json"
    state = _read(path)
    if not state:
        raise SystemExit(f"fleet upstream: no state for {name}; run 'fleet upstream check'")
    take = set(tags) if tags else {r["tag"] for r in state.get("new") or []}
    state["seen"] = list(dict.fromkeys([*state.get("seen", []), *sorted(take)]))
    state["new"] = [r for r in state.get("new") or [] if r["tag"] not in take]
    _write(path, state)
    return sorted(take)


def statuses(home):
    """What fleet morning reads: one dict per configured repo (missing state is None)."""
    out = []
    for repo in load_repos(home):
        out.append({"name": repo["name"], "state": _read(state_dir(home) / f"{repo['name']}.json")})
    return out


def main(argv=None, home=None, fetch=fetch_releases):
    home = Path(home or Path.home())
    p = argparse.ArgumentParser(prog="fleet upstream", description=__doc__.split("\n\n")[0])
    sub = p.add_subparsers(dest="cmd", required=True)
    sub.add_parser("check", help="ask GitHub for new releases of every watched repo")
    sub.add_parser("status", help="print what is unreviewed")
    s = sub.add_parser("show", help="print the unreviewed release notes of one repo")
    s.add_argument("name")
    a = sub.add_parser("ack", help="mark releases reviewed (all new ones by default)")
    a.add_argument("name")
    a.add_argument("tags", nargs="*")
    args = p.parse_args(argv)
    if args.cmd == "check":
        for s in check(home, fetch=fetch):
            print(f"{s['name']}: {len(s['new'])} new" + (f" (error: {s['error']})" if s["error"] else ""))
        return 0
    if args.cmd == "status":
        for s in statuses(home):
            st = s["state"]
            if not st:
                print(f"{s['name']}: never checked")
                continue
            tags = " ".join(r["tag"] for r in st.get("new") or [])
            print(f"{s['name']}: {len(st.get('new') or [])} new {tags}".rstrip()
                  + (f"; notes {st['notes']}" if st.get("notes") else ""))
        return 0
    if args.cmd == "show":
        repo = next((r for r in load_repos(home) if r["name"] == args.name), None)
        st = _read(state_dir(home) / f"{args.name}.json")
        if not repo or not st:
            print(f"fleet upstream: unknown repo {args.name}", file=sys.stderr)
            return 2
        new = {r["tag"] for r in st.get("new") or []}
        if not new:
            print(f"{args.name}: nothing unreviewed")
            return 0
        if repo.get("why"):
            print(f"Watching {repo['github']} for: {repo['why']}")
        if st.get("notes"):
            print(f"Record what was taken or skipped in: {st['notes']}\n")
        for r in reversed(fetch(repo["github"])):
            if r["tag"] in new:
                print(f"## {r['name']} ({r['published_at'] or 'unpublished'})\n{r['url']}\n\n{r['body'].strip()}\n")
        print(f"Then: fleet upstream ack {args.name}")
        return 0
    if args.cmd == "ack":
        taken = ack(home, args.name, args.tags)
        print(f"{args.name}: reviewed {' '.join(taken) or 'nothing'}")
        return 0
    return 2
