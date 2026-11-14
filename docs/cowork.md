# cowork

Agents on different CLIs do not see each other's jobs. Two of them will build the same public export, or one will start a drive migration while another still holds the volume. **cowork** is the board they share: a claim has an owner, a scope, a heartbeat, and an agent-mail address. `taskhub` is the same program under the fleet-board client name.

The board state stays on the hub (`~/.local/state/cowork/` by default, or `state_dir` in config). It does not belong in this repo. Nothing here opens a network connection unless you pass `--host` later; this tree does not ship the old private dashboard.

## Model

A **claim** is one piece of work. The owner is a provider session on a host. Scope is one or more of:

| Kind | Example | Conflict |
|---|---|---|
| `repo` | `repo:fleet` | Same repo name, or a path whose components include that name |
| `path` | `path:/work/fleet` | One path is a prefix of the other, on a boundary (`/work/fleet` overlaps `/work/fleet/cowork`, not `/work/fleet-board`) |
| `worktree` | `worktree:cowork` | Same worktree name |
| `drive` | `drive:hub:/Volumes/Media Mirror/Music` | Same volume on that host. A legacy `drive:Media` is the volume name on this host. The path is one value and may contain spaces. |
| `output`, `publish` | `publish:public-mirror` | Same target. These two kinds share one namespace: who may push the public mirror |
| `machine`, `service` | `machine:hub` | Recorded, not a conflict by themselves |

`claim` records the work even when the scope overlaps. Overlap prints one warning line on stderr, with the other owner and `agent-mail send <address>`, and exits 0. `cowork check <scope>` exits 1 when a live claim or a live `.writer` holds that scope, and exits 0 when the only holder is stale. A stale row stays on the board. Taking the same work name needs `--takeover` and a note. A heartbeat from anyone else does not move the owner.

The clock is the last heartbeat plus `ttl` seconds (default 3600). `start` registers a session, a prompt heartbeats it, and session end releases that session's claims and their child claims. A released or finished claim drops its leases. `done` keeps the row with state `✓`.

A **child claim** is how `cursor-worker` and `cursor-remote` show up under the session that launched them. Overlap with the parent is not a conflict.

Session ids stored on the board are `sha256(provider|host|session)`. The raw id and the prompt text are not written.

## Commands

```text
cowork claim WORK --provider P --session S --host H --ttl 8h|30m|2d|SEC
    --scope KIND:VALUE [--scope KIND:HOST:PATH ...] [--name SESSION] [--mail HOST:PROJECT] [--takeover NOTE]
cowork heartbeat WORK --session S
cowork note WORK TEXT --session S
cowork handoff WORK --to SESSION --note TEXT --session S [--provider P --host H]
cowork release WORK --session S
cowork done WORK --session S
cowork child PARENT --work WORK --provider P --session S --scope KIND:VALUE
cowork check SCOPE                 # held by <owner> (<provider>, <host>) — ask: cowork ask <work> "…"
cowork observe                      # stdin is the tool-call JSON; pre-action hooks
cowork context                      # one line for the prompt hook
cowork doctor [--json]              # sessions, missing hooks, quiet claims
cowork flush                        # send the local queue to the hub
cowork board [--json]
cowork who SCOPE
cowork mine --session S --provider P [--host H] [--json]
cowork ask WORK "message"
cowork join --provider P --session S --host H [--scope ...] [--work WORK]
cowork leave --session S --provider P --host H
cowork hooks PROVIDER
cowork hook PROVIDER EVENT          # stdin is the provider payload; fail-open
cowork update FILE                  # migrate a fleet-board dump, or replace a board document
cowork open                         # refuses unless config names a hub; no dashboard in this tree
```

`taskhub` with no arguments prints the board (`status`). `taskhub json` is `cowork board --json`. `taskhub update` and `taskhub report` are the old client verbs. `bin/taskhub` is the alias to install later; this build does not put it on `PATH`.

### Board

One header, then one row per claim. A session with no claim of its own gets a row too. The same session is not listed twice.

```text
work               owner            host       scope                    state heartbeat  conflict
public-export      cur/660e2e       worker-a   repo fleet +1            *             0s ! hub:fleet
drive-migrate      cld/7060c3       hub        drive Media              ~            11d ·
```

| Column | Reading |
|---|---|
| owner | Provider and session name, `claude · glancebar-62`. The session hash is only in `--json` (`owner.session`). |
| state | `*` live, `~` stale, `→` handed off, `✓` done, `·` released |
| heartbeat | Age of the last heartbeat |
| conflict | `·` none, or `!` and the other owner's mail |

Colour follows the symbol. `COWORK_PLAIN=1` or `NO_COLOR` or a pipe drops the colour. Columns are as wide as their text: a scope is never ellipsised. Notes and the takeover text are in the JSON, not a second line. The scope column shows every scope in full.

### `board.json`

Every command rewrites `$COWORK_STATE/board.json`. fleet-health and Glancebar read that file. They do not need to shell out, and they should not treat `stale` as free to take: the owner is still the owner until a takeover note or an explicit release.

```json
{
  "version": 1,
  "generated": 1700000000,
  "claims": [
    {
      "work": "public-export",
      "state": "active",
      "owner": {"provider": "cursor", "session": "<sha256>", "host": "worker-a", "mail": "worker-a:fleet"},
      "scope": [{"kind": "publish", "value": "public-mirror"}],
      "heartbeat": 1700000000,
      "ttl": 3600,
      "expires": 1700003600,
      "conflicts": [{"work": "public-export-2", "mail": "hub:fleet", "state": "active"}],
      "parent": null,
      "notes": [{"at": 1700000000, "text": "take the mirror", "by": "<sha256>"}],
      "leases": [{"kind": "publish", "name": "public-mirror"}]
    }
  ],
  "presence": [
    {"id": "<sha256 or legacy id>", "provider": "claude", "host": "hub", "project": "fleet",
     "event": "activity", "at": 1700000000, "state": "stale", "source": "local lifecycle hook"}
  ]
}
```

`state` is `active`, `stale`, `handed-off`, `done`, or `released`. `mail` is an agent-mail target, `HOST:PROJECT`.

## Leases and `.writer`

A drive claim writes `<volume>/.writer` only on the host that owns the volume. `drive:hub:/Volumes/Media Mirror/Music` is volume `Media Mirror` on `hub`; the Mac that is not `hub` does not create a local path from the last component. The owning host writes the file when that volume directory is already mounted, and does not create the volume. A repo, worktree, or publish claim writes the same line under `$COWORK_STATE/leases/<kind>/<name>`. The line is the one placement already understands:

```text
<job>@<host> <hostname> <pid> <epoch>
```

`epoch` is the heartbeat, so a living claim stays inside placement's 12 hour window. When the heartbeat ttl passes, the next cowork read renames `.writer` to `.writer.released` and deletes the other lease files. The claim row remains, marked `~`.

A `.writer` that cowork did not create is held when it is younger than 12 hours. If its hostname is this machine, the pid must still be alive. Cowork will not replace a live foreign file. `cowork check` is how a job asks before it starts. A live holder prints one line and exits 1:

```text
held by claude · glancebar-62 (claude, desk) — ask: cowork ask music-offload "…"
```

## Presence

`cowork hooks <provider>` prints a snippet and does not edit settings. Providers: `claude`, `codex`, `cursor`, `agentkit`, `local`, `cloud`.

| Provider | What the snippet calls |
|---|---|
| claude, codex | `cowork hook <provider> SessionStart` (and prompt, tool, stop, end) |
| cursor | The same events under Cursor's names (`sessionStart`, `beforeSubmitPrompt`, `postToolUse`, `stop`, `sessionEnd`). The `worker` object is the line for `cursor-worker` and `cursor-remote`: `cowork child "$COWORK_WORK" ...` |
| agentkit | `session.start`, `prompt`, `session.end` on the context hook |
| local, cloud | `cowork join` at start, `cowork heartbeat` on a prompt, `cowork leave` on exit |

The hook reads the provider's JSON from stdin, keeps `session_id`, `cwd`, and the event name, and drops everything else. It exits 0 on bad input. `cowork.lifecycle` is the same hook under the old `lifecycle_hook.py ENGINE EVENT` arguments, for a line that already points at that file.

Set `COWORK_WORK` to the parent claim before launching a Cursor worker, and `COWORK_CHILD` / `COWORK_WORKTREE` for the child row.

## Messaging

Each claim stores `mail`. The overlap warning and `cowork ask WORK "text"` use it. `ask` runs `agent-mail send HOST:PROJECT "text"` (`COWORK_AGENT_MAIL` overrides the binary). It does not invent a second mailbox.

## Migrating the fleet-board dump

The old client posted session events and accepted `status`, `json`, `update`, and `report`. `cowork update FILE` (and `taskhub update`) still rejects a payload over 64 KiB. It accepts:

- `{"sessions": [ ...events ]}`
- `{"events": [ ... ]}` or a bare list
- one event object
- a board document that already has `claims` (replaces the stored claims)

An event has `id`, `event_id`, `engine`, `host`, `project`, `event` (`start`, `activity`, `turn_end`, `end`, `interrupted`, `error`), `at`, and `source`. The same `event_id` is applied once. `end` is released. Anything else older than one hour is shown stale, not deleted. Engines `Claude CLI`, `Codex`, and `Cursor` map to `claude`, `codex`, and `cursor`.

## Config and environment

`~/.config/cowork/config.json` (or `COWORK_CONFIG`):

```json
{
  "hub": "hub",
  "hub_aliases": ["hub", "hub-lan"],
  "host": "worker-a",
  "hostname": "worker-a.local",
  "volume_root": "/Volumes",
  "state_dir": "~/.local/state/cowork"
}
```

| Variable | Effect |
|---|---|
| `COWORK_STATE` | Board directory. Overrides `state_dir`. |
| `COWORK_HOST`, `COWORK_HOSTNAME`, `COWORK_PID` | Identity written into leases. Unset `COWORK_HOST` uses `scutil --get LocalHostName` and the diskmap host map (`Coordinator-Mac` → `desk`, `Worker-One` → `lab`, `Worker-Two` → `studio`). Override the map in `~/.config/diskmap/config.json`. |
| `COWORK_SESSION_NAME` | Owner column. A worktree directory name is used when this is unset. |
| `COWORK_LOCAL` | `1` writes this machine's store and does not forward. The hub side of an ssh hop sets this. |
| `COWORK_FLUSH` | `0` leaves the queue for a later `cowork flush`. |
| `COWORK_VOLUME_ROOT` | Parent of volume directories. Default `/Volumes`. |
| `COWORK_AGENT_MAIL` | `agent-mail` binary for `ask`. |
| `COWORK_NOW` | Clock override, for tests. |
| `COWORK_PLAIN`, `NO_COLOR` | No ANSI colour. |

With no config file, the hub is the ssh alias `hub` and the aliases are `hub`, `hub-lan`: example names. Name your own in `~/.config/cowork/config.json`: `{"hub": "<alias>", "hub_aliases": ["<alias>", "<alias>-lan"]}`. A config file that names a hub uses that hub; a config file with no hub stays local. `cowork claim` and `cowork check` try the aliases in order (`BatchMode`, 2 second connect). The remote command is `env COWORK_LOCAL=1 cowork …`, so the hub does not ssh to itself. If every alias is away (ssh status 255 or no route), the command is appended to `$COWORK_STATE/queue.jsonl`, applied on this Mac, and `cowork flush` retries in the background.

`cowork observe` and `cowork hook` never wait on that ssh. They write locally and queue the same claim; flush delivers it. `cowork --host hub board` is the explicit form (`ConnectTimeout` 8 seconds) and still refuses a name that is not the hub or an alias. `--host` after the subcommand is the machine that owns the claim, not an SSH target.

`--ttl` accepts seconds or `30m`, `8h`, `2d`, `90s`.

## Exit codes

| Code | When |
|---|---|
| 0 | Claimed, released, board printed, or `check` found nothing live |
| 1 | `check` found a holder; takeover or handoff lacked a note; not the owner; `ask` could not send |
| 2 | Usage, a bad scope, an unknown `hooks` provider, or `open` with no hub configured |

## Lessons

**Two public exports.** Two agents each produced the public mirror. Neither row existed on the board, so neither warning fired. A `publish:public-mirror` claim is the lease for that output. The second `cowork check output:public-mirror` exits 1 and names the mail address of the first.

**A migration against a live drive.** A copy started while another job still held the volume. The holder had a `.writer` line (`job@host hostname pid epoch`). Cowork writes that same line for a drive claim and will not replace a live one. `cowork check drive:NAME` is the question to ask before the copy. When the heartbeat goes stale the row stays visible and the file becomes `.writer.released`, which is the signal placement already treats as free.

## Inferred claims

`cowork observe` reads one tool-call JSON from stdin (Claude `tool_name` / `tool_input`, or Cursor `tool` / `arguments`). It exits 0 on input it does not understand. A recognised call records a claim for the current session:

| Call | Scope |
|---|---|
| Edit, Write, and the other file tools | `repo:<project>` and `path:<cwd>` |
| `git push [remote]` | `publish:<remote>` and `publish:public-mirror` |
| `cp`, `mv`, `rsync` onto `/Volumes/…` | `drive:<this-host>:<dest>` |
| `cursor-worker`, `cursor-remote` | child claim, `worktree:<name>` |
| `launchctl` or a `*deploy` / `*release` command | `service:<job>` |
| `heavy-slot` | `machine:<this-host>` (recorded, not a refusal) |

The recording itself stays under 20 ms and does not open a socket. A second session whose tool hits a held publish mirror, drive, worktree, or service exits 2 and prints the `held by` line, which is the Claude and Cursor deny code. The session that already holds the scope is not refused.

## Context and doctor

`cowork context` prints one line for the project: other holders, this session's claims, and `!` when they overlap. The prompt hooks run it. `cowork doctor` prints one row per provider and host (`seen` in the last hour, `registered`), one row per provider whose hook file does not mention cowork, and one row per claim with no live heartbeat. `cowork doctor --json` is the document fleet morning and Glancebar read:

```json
{"version": 1, "generated": 1700000000, "sessions": [{"provider": "claude", "host": "desk", "seen": 1, "registered": 1}], "hooks_missing": ["codex"], "no_heartbeat": []}
```

## Enforcement

These are the one-line checks. Tools in this repo call them. Tools in other repos get the same line; the caller applies it.

```bash
# git push of the public mirror
cowork check "publish:public-mirror" || exit $?

# cursor-worker / cursor-remote, before the worktree name is taken
cowork check "worktree:${name}" || exit $?

# glancebar tools/ops — glancebar-deploy and glancebar-release
cowork check "publish:public-mirror" || exit $?
cowork check "service:glancebar-deploy" || exit $?

# drive-registry scripts — mirror, offload, any write under /Volumes
cowork check "drive:${host}:/Volumes/${volume}" || exit $?
```

`placement apply` runs the drive check itself and exits 75 when the line comes back. A missing `cowork` binary does not block placement; the `.writer` file still does.

## Rollout

Do this by hand, one provider at a time. Do not point a live hook at this tree until a throwaway `COWORK_STATE` on that Mac has shown a row. Nothing here installs itself.

1. On the hub, keep the state directory out of git. On every Mac, name the hub and its aliases in `~/.config/cowork/config.json`; with no config file the example names `hub` / `hub-lan` are used. `COWORK_LOCAL=1` belongs only on the command the ssh hop runs.
2. `cowork update` a copy of the old session dump. `cowork board` should show the stale rows with full scopes and `claude · <name>`, not a hash. `cowork doctor --json` is the file fleet morning and Glancebar can start reading.
3. Claim a scratch item with `--ttl 30m`, a `publish:public-mirror` scope, and `drive:<hub>:/Volumes/<a mounted volume>` from another Mac. That Mac must not grow a local `.writer` for that volume. On the hub, the same claim writes `<volume>/.writer` only when the volume is mounted.

**Claude.** Paste `cowork hooks claude` into `~/.claude/settings.json`. That adds `PreToolUse` → `cowork observe` and `UserPromptSubmit` → `cowork context`, beside the lifecycle events. Leave the old `lifecycle_hook.py` line until a new row appears, then remove it.

**Codex.** Paste `cowork hooks codex` into the Codex hooks file. Same events as Claude, including Interrupt.

**Cursor.** Paste `cowork hooks cursor` into `~/.cursor/hooks.json`. `preToolUse` observes; `beforeSubmitPrompt` prints the context line. Export `COWORK_WORK` in the parent session so `cursor-worker` and `cursor-remote` use the `worker` command and show up as child claims. In the cursor-bridge repo, refuse a taken worktree with `cowork check "worktree:${name}"`.

**agentkit.** Paste the `context_hook` from `cowork hooks agentkit`, including `context` and `observe`.

**local / cloud.** The shell snippet is `cowork join` at start and `cowork leave` on exit. Run `cowork context` where the script prints a status line.

**Other repos, applied by the caller.** glancebar `tools/ops` (`glancebar-deploy`, `glancebar-release`) and your drive-registry scripts (`drive-mirror`, `offload.sh`) get the one-line checks in Enforcement. `placement apply` in this repo already calls the drive check.

4. When `cowork doctor` shows a seen session for the provider you just wired, and `hooks_missing` no longer names it, point `taskhub` at `bin/taskhub`. Glancebar reads `board.json` and `cowork doctor --json`; it shows `owner.label`, not the hash, and it does not take over a stale row.
