# agent-mail

Sessions on different Macs do not share memory. SSH is already there for sync and remote runs, but ad-hoc messages get lost when a machine sleeps or a session never checks. **agent-mail** is a per-Mac inbox file plus a delivery adapter for each agent CLI. Shared notes are addressed by host and project (and an optional role). Exact-session notes also name a provider and stable session ID. A coordinator **relay** delivers when a worker cannot SSH back.

Install: `cargo install --path agent-mail` (binary `agent-mail` on `PATH`, typically `~/.local/bin/agent-mail`).

## Commands

| Command | Arguments | Purpose |
|---|---|---|
| `send` | `HOST[:PROJECT[:ROLE]] "text" [--to-provider P --to-session ID]` | Append one message to `HOST`’s inbox (or local). Prints message `id` on stdout. |
| `unread` | `[--mark \| --hook] [--provider claude\|codex\|cursor\|other] [--json] [--role ROLE] [--session ID]` | List unread messages for the current git project. `--mark` / `--hook` record a receipt for that reader. |
| `wait` | `[--timeout SEC] [--provider P] [--role ROLE] [--session ID]` | Block until a new inbox line for this project arrives. For a background watcher; replaces polling `inbox.jsonl`. |
| `status` | `HOST [ID]` | For messages this Mac sent to `HOST`, print inbox `read` / `unread`, proven `queued`, `sent` / `relayed` with unknown read status, or `unknown`, plus reader identity (last 10 IDs if `ID` omitted). |
| `log` | `[N]` | Print last `N` lines of the local inbox (default 10). |
| `snippet` | — | Print the project instruction block for `AGENTS.md`, `CLAUDE.md`, or a Cursor rule. |
| `relay` | — | Coordinator only: drain workers’ outboxes, deliver, sync the legacy `read.txt` receipt. |
| `install` | `[--provider claude\|codex\|cursor\|all] [NAME]` | Write `name` if `NAME` given. Install that provider’s adapter. Default provider is `claude`. Idempotent. |
| `uninstall` | `[--provider claude\|codex\|cursor\|all]` | Reverse `install` for that provider. Default is `claude`. |

Unknown commands or no arguments print a short usage block on stdout and exit `2`.

### `send` targets

| Target | Behavior |
|---|---|
| `local` or this Mac’s configured `name` | Append to local `inbox.jsonl`. |
| Other `HOST` | SSH append to `~/.local/state/agent-mail/inbox.jsonl` on `HOST`. |
| Unreachable `HOST` | Append to local `outbox.jsonl` with a `"to"` field; stderr warns that the relay will deliver. |

`HOST:PROJECT` sets the JSON `"project"` field. `HOST:PROJECT:ROLE` also sets `"role"` (omitted when empty, so older lines stay the same shape). For shared mail, only sessions whose git toplevel basename matches `project` see the message. An empty `project` matches every project. A non-empty `role` is shown only to a reader that passes the same `--role` (or `AGENT_MAIL_ROLE`).

### Exact-session delivery

```sh
agent-mail send desk:fleet "review this change" --to-provider cursor --to-session SESSION_ID
agent-mail unread --provider cursor --session SESSION_ID --json
agent-mail unread --provider cursor --session SESSION_ID --mark
```

The two send flags must appear **after the text**, together, exactly once. They add
`"to_provider"` and `"to_session"` to the message. The envelope `"role"` is a reserved
`__agent_mail_session_v1__:PROVIDER:SESSION` guard; an optional user role is stored
separately in `"to_role"`. Updated readers require the matching guard and enforce
`to_role` when present. Never pass the guard as a reader's `--role`. Supported providers are `claude`,
`codex`, `cursor`, and `other`. IDs contain letters, digits, `.`, `_`, or `-`.
Malformed flags, incomplete pairs, and invalid IDs fail without sending anything.
Legacy `send HOST[:PROJECT[:ROLE]] "text"` keeps its existing JSON shape.

Exact-session mail is visible only to the named provider and stable session.
Missing session identity does not match, even if the fallback PID happens to equal
the target. Invalid or partial target fields in an inbox line never fall back to
shared delivery. The exact session can change directories: `project` is context
for exact mail, but remains a filter for shared mail. An optional role still filters.

`--session ID` overrides hook JSON, which overrides `AGENT_MAIL_SESSION`.
Hook keys are `session_id`, `sessionId`, `conversation_id`, and `conversationId`.
Do not invent a session ID or reuse another session's identity.

**Rolling upgrades:** old ordinary `agent-mail unread` readers do not match the
reserved role, so they cannot show or mark exact mail. They must upgrade to receive
it. Tests freeze the old matcher and validate a baseline binary against synthetic
mail. Tools that read raw inbox files or ignore role filters may still expose it. This is cooperative routing,
not authentication or hostile same-UID isolation: caller-supplied flags, environment,
and hook identities can be spoofed, and all same-user sessions can read the files.
No hook, watcher, UI input, or notification is installed by `send`, `unread`, or `wait`.
An inbox append proves only that the note is stored. `read` proves that a matching
tool claimed that reader identity and recorded a receipt, not that the model was
awakened, understood it, or acted. Stronger pickup evidence needs independent
process/session or UI matching.

### Readers and receipts

A reader is `<provider>:<session-or-pid>@<host>`. Session selection is described above. Without a stable ID, explicit-provider readers fall back to the process ID and legacy readers to `manual`, for shared mail only. `host` is this Mac’s configured name. The following table applies to **shared mail**:

| Path | What counts as already seen | What a mark writes |
|---|---|---|
| `unread` / `--mark` / `--hook` with no `--provider` | Legacy `read.txt`, `read-<session>.txt`, and Claude receipts | `read.txt`, `read-<session>.txt`, and a `claude:…` receipt. This is the existing Claude hook. |
| `--provider claude` | `read.txt` and Claude receipts | A Claude receipt only |
| `--provider codex`, `cursor`, or `other` | That provider’s receipts only | That provider’s receipt only |

One provider marking shared mail does not hide it from another. Within a provider, the next session does not replay shared mail. Exact mail requires a receipt matching the target provider, session, and host; another session’s receipt and legacy `read.txt` never consume it. Exact mail never writes legacy marks. `status` counts only a matching receipt for exact mail; any reader counts for shared mail, including an id only in legacy `read.txt`. A legacy-only id is reported as `legacy@<host>`. Both `status local` and `status <configured-name>` inspect local state without SSH, including each other’s sent IDs. `queued` requires an actual matching outbox entry; missing inbox/route evidence is never treated as proof of delivery or queueing.

`unread --json` prints one object, `{"project","reader","unread"}`, and does not mark unless `--mark` or `--hook` is also set.

`wait` applies the same project/role/session routing and returns when a matching inbox line appears that was not in the file when the command started. It does not mark the message. `--timeout SEC` exits 0 with no output if nothing arrives.

### `unread` modes

| Mode | stdin | Marking |
|---|---|---|
| (none) | — | Lists unread for the legacy Claude reader. |
| `--mark` | — | Lists unread and records the receipt described above. |
| `--hook` | Hook JSON | Uses `session_id` and `cwd` from the event, then marks. The Claude Code hook is still `unread --hook` with no `--provider`. |
| `--provider P` | — | Lists unread for that provider. Combine with `--mark` or `--hook`. |
| `--json` | — | Same selection, as one JSON object and no banner. |

Shared-mail project filter: basename of `git rev-parse --show-toplevel` for `cwd`, else basename of `cwd`. Empty `project` in the message matches all projects.

## Environment variables

| Variable | Default | Effect |
|---|---|---|
| `AGENT_MAIL_DIR` | `~/.local/state/agent-mail` | Inbox, outbox, read marks, relay artifacts. |
| `AGENT_MAIL_CONFIG` | `~/.config/agent-mail` | `name` and `hosts` files. |
| `AGENT_MAIL_SESSION` | unset | Stable reader session, unless overridden by hook JSON or `--session`. Required for exact mail when neither is supplied. |
| `AGENT_MAIL_ROLE` | empty | Reader role unless `--role` is supplied. |
| `AGENT_MAIL_SETTINGS` | `~/.claude/settings.json` | Target for `install` hook edits. |
| `AGENT_MAIL_SSH` | `ssh` | SSH program (tests use a fake script). |

SSH invocations always use `-o BatchMode=yes -o ConnectTimeout=8` and `-- HOST command` (host validated before send, status, and relay delivery).

## Config files

| Path | Format |
|---|---|
| `~/.config/agent-mail/name` | One line: this Mac’s short name (used in message ids and relay receipts). If missing, `scutil --get LocalHostName`, then `hostname -s`, then `localhost`. |
| `~/.config/agent-mail/hosts` | One hostname per line; `#` comments and blank lines ignored. First token on each line must pass [host validation](#host-names-for-relay). Used by `relay` to poll worker outboxes. |

## State files

| File | Written by | Format |
|---|---|---|
| `inbox.jsonl` | `send`, `relay` | One JSON object per line (see below). |
| `outbox.jsonl` | `send` when SSH fails | Same fields as inbox plus `"to":"HOST"`. |
| `outbox.sent.jsonl` | worker, after relay | Copy of delivered outbox lines (on the worker). |
| `outbox.jsonl.relaying` | worker relay handoff | Temporary; relay moves and clears via remote shell. |
| `sent.tsv` | `send` | `id<TAB>host<TAB>at`; exact targets add `<TAB>provider<TAB>session`. |
| `read.txt` | legacy `unread --mark` / `--hook` (no `--provider`) | One message id per line. Still honoured for shared mail by the Claude path and `status`. |
| `read-{session_id}.txt` | legacy `unread --hook` / `--mark` | Per-session seen ids for that Claude path. Files older than 14 days are deleted on unread. |
| `receipts.jsonl` | `unread --mark` / `--hook` | One object per reader: `{"id","reader","project"}`. Readers append to the same file; one reader’s line does not replace another’s. |
| `read-receipts-{COORD}.txt` | `relay` on worker | Copy of coordinator’s `read.txt` so `status COORD` works without reverse SSH. |
| `undeliverable.jsonl` | `relay` | Messages whose `"to"` failed validation or delivery. |

### Inbox message JSON

Compact keys, no spaces after colons (stable for tooling):

```json
{"id":"YYYYMMDDHHMMSS-me-NNNN","at":"YYYY-MM-DD HH:MM:SS","from":"me","project":"","text":"…"}
```

Exact-session messages append `"role":"__agent_mail_session_v1__:cursor:SESSION_ID","to_provider":"cursor","to_session":"SESSION_ID"`, plus `"to_role"` only when an explicit role was requested.

`id` uses local time, configured name, and a pseudo-random suffix (`NNNN` 0–32767). Queued outbox lines add `"to":"host"`. After relay from a worker, `"from"` becomes the worker hostname; if the worker forged a different `from`, the original is kept as `"claimed_from"`.

### Host names for relay

Allowed characters: start with lowercase letter or digit; then lowercase letters, digits, `.`, `_`, `-`. Relay delivers only to `local`, this Mac’s name, or hosts listed in `hosts`. Others go to `undeliverable.jsonl`.

## Exit codes

| Code | When |
|---|---|
| `0` | Success (including empty unread, `relay` with missing state dir). |
| `1` | Missing send/status arguments, state/read/append failure. Inbox/outbox and sent-log failures include the message ID on stderr. Partial writes can leave readable mail: inspect the ID and do not resend blindly. |
| `2` | Unknown command, invalid flags/host/session, or incomplete exact target. |

Appends are not transactional and do not use `fsync`. A failed write may leave a
partial record or complete JSON without its trailing newline; an append result is
not a crash-durability guarantee. Existing SSH/relay retries can also duplicate a
message after an uncertain transport failure. Check the stable message ID before
retrying; exact-session routing does not add transport-level deduplication.

## Worked example

Coordinator `desk`, worker `lab` (SSH `desk` → `lab` works; reverse may not).

```bash
# On every Mac once:
agent-mail install desk    # or lab, etc.

# On desk:
echo -e "lab\n" >> ~/.config/agent-mail/hosts

agent-mail send lab "build finished ok"
agent-mail status lab        # unread until lab’s Claude hook runs

# On lab (Claude session starts; hook runs unread --hook):
agent-mail unread            # manual check
agent-mail send desk "acknowledged"

# If lab cannot SSH to desk, send still queues on lab:
agent-mail send desk "queued note"   # stderr: queued for the relay
# On desk, launchd runs every minute:
agent-mail relay
```

## How it interoperates

- **SSH**: Direct delivery uses the same non-interactive SSH assumptions as the rest of the fleet (`BatchMode`, short timeout).
- **Relay (launchd)**: On the coordinator, a LaunchAgent should run `agent-mail relay` about every 60 s. It SSHs to each host in `hosts`, atomically moves `outbox.jsonl` → `outbox.jsonl.relaying`, delivers each line, appends to `outbox.sent.jsonl` on the worker, pushes `read.txt` to `read-receipts-{coordinator}.txt` on workers. See `docs/setup.md` for a plist template.
- **Claude Code**: `agent-mail install` (or `--provider claude`) adds the same command to `SessionStart` and `UserPromptSubmit` in `~/.claude/settings.json`:

  `"$HOME/.local/bin/agent-mail" unread --hook`

  Older hooks that used `unread --mark` are removed when install runs. `uninstall` removes the agent-mail unread commands. Claude-compatible local launchers use this same hook; a launcher that must not consume Claude’s read state should call `unread --provider other` instead.
- **Codex CLI** (observed `codex-cli 0.154.0`): hooks live in `~/.codex/hooks.json` with the same nested `SessionStart` / `UserPromptSubmit` / `Stop` shape as Claude. `config.toml` `notify` is a single turn-ended command, not a hook list, and `[hooks.state]` stores a `trusted_hash` per hook. A new hook does not run until the owner reviews it with `/hooks`. `agent-mail install --provider codex` prints the snippet and writes it under `~/.config/agent-mail/snippets/codex-hooks.json`. It does not edit `~/.codex`. The fallback, also printed by `agent-mail snippet`, is the project instruction: at session start and before finishing, run `agent-mail unread --provider codex`.
- **Cursor CLI** (hooks schema version 1, user file `~/.cursor/hooks.json`): `install --provider cursor` adds `sessionStart`, `beforeSubmitPrompt`, and `stop` commands (idempotent; `uninstall --provider cursor` removes them). The path is `AGENT_MAIL_CURSOR_HOOKS` when that is set, otherwise `~/.cursor/hooks.json`. The same `snippet` text is the fallback for `AGENTS.md` or `.cursor/rules` when hooks are not loaded.
- **Any other agent or script**: `agent-mail unread --provider other --json`. A watcher that should block instead of polling runs `agent-mail wait`.
- **Project text**: `agent-mail snippet` prints one block. Put it in `AGENTS.md`, `CLAUDE.md`, or a Cursor rule so every checkout carries the same instruction.
- **Codex reader outside this crate**: `chatgpt-task-mail` can still read the inbox files. New Codex sessions should use `--provider codex` so the receipt stays inside agent-mail.

## Security note

The Rust **relay** validates `"to"` against the configured host list before SSH. Treat `hosts` as the delivery allowlist; do not put untrusted hostnames there.

### Cursor JSON context pickup

The opt-in `cursor-hook` adapter supports exact-session mail at session start and
post-tool boundaries without automatically acknowledging it. See
[Cursor protocol, bounds, receipts and installation limitations](CURSOR-HOOKS.md).
The legacy `unread --hook --provider cursor` install path is not this adapter.
