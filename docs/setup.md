# Multi-Mac setup

Turn-key order for two or more Macs sharing heavy-job limits, remote offload, and agent mail. Replace example hostnames (`desk`, `lab`, `studio`) with your own SSH config names. The **coordinator** is the Mac that runs the agent-mail relay and often hosts the owner’s primary sessions; **workers** run offloaded jobs and may only have SSH out via the outbox pattern.

## 1. Network and SSH

1. Install [Tailscale](https://tailscale.com) (or any VPN/LAN) so each Mac has a stable hostname reachable from the others.
2. Configure `~/.ssh/config` with short `Host` aliases (MagicDNS names or LAN addresses). Use key-based auth; `BatchMode=yes` must work:

   ```bash
   ssh -o BatchMode=yes lab true
   ```

3. From the coordinator, SSH into every worker you will use for `remote-run`, agent-mail relay, and capacity sharing. Workers do not need inbound SSH from other workers if mail uses the relay.

## 2. Build and install fleet binaries

On each Mac (prefer a machine with adequate cooling for release builds):

```bash
cd fleet-tools    # clone or unpack this repository
cargo install --path heavy-slot
cargo install --path agent-mail
cargo install --path diskmap
cargo install --path placement
```

Symlink or copy the binaries to `~/.local/bin/` if `cargo install` did not already place them there. Keep bash drop-ins from `spec/` only during a parallel validation week; the Rust binaries are intended as drop-in replacements.

## 3. Host lists

**Remote run** — worker preference order (first suitable host wins):

`~/.config/remote-run/hosts`

```text
# Workers, best first
lab
studio
```

**Agent mail** — workers whose outboxes the coordinator relay polls (often the same list):

`~/.config/agent-mail/hosts`

```text
lab
studio
```

**Agent mail identity** — on every Mac:

```bash
agent-mail install desk    # use this Mac’s short name
```

Or write `~/.config/agent-mail/name` manually (one line).

## 4. Heavy-slot per machine

Default state directory: `~/.local/state/heavy-slot`.

| Machine role | Suggested files |
|---|---|
| Coordinator (many cores) | Default pool (`HEAVY_SLOTS=3` or unset) |
| Fanless or shared worker | `echo 1 > ~/.local/state/heavy-slot/limit` |
| Hot-climate worker | `echo 65 > ~/.local/state/heavy-slot/cool` (°C; requires Glancebar for readings) |
| Shared desktop when someone is present | `echo gentle > ~/.local/state/heavy-slot/mode` (optional; often set by external governor tooling) |

Details: [heavy-slot/README.md](../heavy-slot/README.md).

## 5. LaunchAgent templates

Copy plists into `~/Library/LaunchAgents/`, replace `REPLACE_HOME` with your home directory (or use `$HOME` expansion when loading), replace `REPLACE_BIN` with the path to the binary if not `~/.local/bin/…`.

Load:

```bash
launchctl bootstrap "gui/$(id -u)" ~/Library/LaunchAgents/com.example.agent-mail-relay.plist
launchctl bootstrap "gui/$(id -u)" ~/Library/LaunchAgents/com.example.machine-log.plist
launchctl bootstrap "gui/$(id -u)" ~/Library/LaunchAgents/com.example.diskmap.plist
```

Unload with `launchctl bootout "gui/$(id -u)" …` when testing.

### Coordinator: agent-mail relay

File: [launchd/com.example.agent-mail-relay.plist](launchd/com.example.agent-mail-relay.plist)

Runs `agent-mail relay` every 60 seconds. Only the coordinator needs this job if workers cannot SSH back to it.

### Optional: machine telemetry

File: [launchd/com.example.machine-log.plist](launchd/com.example.machine-log.plist)

Placeholder for the bash `machine-log` script (not yet a Rust crate). Omit until that tool is installed. diskmap reads this log's `diskFreeGB` for free-space rates, so install machine-log on any Mac whose trend should be real rather than blank.

### Every Mac: disk map

File: [launchd/com.example.diskmap.plist](launchd/com.example.diskmap.plist)

Runs `diskmap tick` every 60 seconds at background priority. Create `~/.config/diskmap/config.json` only to override host floors, archive volume name, or list SSH peers (see [diskmap/README.md](../diskmap/README.md)). The default archive is `/Volumes/SecureArchive/diskmap/` and is written only when that volume is mounted and your drive registry marks it encrypted; other Macs spool until then.

Other launchd jobs from a full fleet (focus-guard, link-meter, ai-budget refresh, ai-capacity-share) are described in the private fleet inventory and [architecture.md](architecture.md); add them when those scripts are installed.

### Home hub: morning check

`fleet morning` reads snapshots each Mac writes for itself. `fleet morning collect --local` runs hourly on every Mac; the daily digest (22:30Z in the example plist) stays on the home hub. Templates, the Python 3.11 requirement, and the threshold file are in [morning.md](morning.md). Put `bin/fleet` on `PATH` as `~/.local/bin/fleet` on each Mac first. The script uses `/opt/homebrew/bin/python3` when it is present.

## 6. Claude Code hooks (agent-mail)

`agent-mail install [NAME]` merges into `~/.claude/settings.json`:

| Event | Command |
|---|---|
| `SessionStart` | `"$HOME/.local/bin/agent-mail" unread --hook` |
| `UserPromptSubmit` | same |

Minimal manual snippet (if you edit JSON yourself):

```json
{
  "hooks": {
    "SessionStart": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "\"$HOME/.local/bin/agent-mail\" unread --hook"
          }
        ]
      }
    ],
    "UserPromptSubmit": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "\"$HOME/.local/bin/agent-mail\" unread --hook"
          }
        ]
      }
    ]
  }
}
```

A full fleet also wires `ai-budget.mjs`, `ai-capacity`, offload guards, and capacity hooks; those live outside this repository today. See [architecture.md](architecture.md).

### Other agent CLIs

`agent-mail install --provider codex` prints a `hooks.json` snippet and does not edit `~/.codex`. Codex runs a new hook only after the owner’s `/hooks` trust review. Until then, paste `agent-mail snippet` into the repo’s `AGENTS.md`.

`agent-mail install --provider cursor` merges `sessionStart`, `beforeSubmitPrompt`, and `stop` into `~/.cursor/hooks.json` (override the path with `AGENT_MAIL_CURSOR_HOOKS`). `agent-mail install --provider all` does Claude, the Codex snippet, and the Cursor hooks. `uninstall` with the same `--provider` reverses that adapter.

`agent-mail unread --provider other --json` is the check for a script or a local model that is not using the Claude hook. `agent-mail wait` blocks until new mail for the current project.

## 7. Glancebar (recommended)

Install [Glancebar](https://github.com/todd866/glancebar) on each Mac that should participate in thermal gating and fleet status rows. Scripts fall back safely when Glancebar is missing. Configure “other machines” in Glancebar defaults with `{name=…; host=…;}` entries matching your SSH aliases.

## 8. Verify it works

| Check | Command | Expect |
|---|---|---|
| Heavy slots | `heavy-slot status` | All configured slots `free` or expected busy lines |
| Disk map | `diskmap json` | A `locations` array; `~/.local/state/diskmap/summary.json` exists |
| Run a job | `heavy-slot -- /usr/bin/true` | Exit 0; one line in `~/.local/state/heavy-slot/jobs.jsonl` |
| Local mail | `agent-mail send local "ping"` then `agent-mail unread --mark` | Message listed once; `read.txt` contains id |
| Remote mail | From coordinator: `agent-mail send lab "hello"` | On lab: `agent-mail unread` shows it (after SSH path works) |
| Relay path | Stop reverse SSH; `agent-mail send desk "via outbox"` from lab; run `agent-mail relay` on desk | Message appears in desk inbox |
| Read receipt | After lab hook marks read: `agent-mail status lab` on desk | `read` for recent ids (or `read-receipts-*` via relay) |

Fix SSH before debugging mail. Use `agent-mail log` and `~/.local/state/agent-mail/undeliverable.jsonl` on the coordinator if relay rejects a `to` host.

## 9. What this repo does not install yet

The following are inventory items still shipped as bash/python on a reference setup: `offload-target`, `remote-run`, `focus-guard`, `ai-capacity`, `ai-pick`, `machine-log`, cursor-bridge helpers, and queue-steward. Document them in [architecture.md](architecture.md); install from future crates or scrubbed scripts when published.
