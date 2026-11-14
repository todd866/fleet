# Fleet

Rust tools and docs for running several AI coding agents across several Macs without them fighting over thermals, slots, or subscription quota. Host names, drive layouts, and limits live in `~/.config/<tool>/`, not in this repository.

**License:** MIT (see [LICENSE](LICENSE)).

## What ships here today

| Tool | Role |
| --- | --- |
| [heavy-slot](heavy-slot/README.md) | Machine-wide pool for heavy jobs (tests, builds). Cool/offload hooks optional. |
| [agent-mail](agent-mail/README.md) | Cross-Mac inbox for agent sessions; coordinator relay when a worker cannot SSH back. |
| [queue-steward](docs/queue-steward.md) | Orders heavy-slot waiters by score (live crate; wiring to your wait loop may still be manual). |
| [diskmap](diskmap/README.md) | Volume map, growth history, archive shipping; summary JSON for health UIs. |
| [placement](placement/README.md) | Data-set placement planner (`plan` / `apply` / `status`). |
| [agentkit](agentkit/README.md) | Canonical rules, guards, and provider sync (`sync`, `doctor`). |
| [utc-daily](bin/utc-daily), [utc-schedule](bin/utc-schedule) | Daily launchd jobs on a UTC schedule: launchd's calendar is local time in the zone the Mac booted in, so a laptop that travels fires jobs hours off. `utc-schedule` converts existing plists. |
| [cowork](docs/cowork.md) | Work claims, leases, and board JSON for multi-agent coordination. |

Bash originals under `spec/` are the behavioural spec for heavy-slot and agent-mail while you validate Rust drop-ins.

**Not in this tree (yet):** `remote-run`, `offload-target`, `ai-capacity`, Glancebar integration scripts, and fleet-morning digests. See [docs/architecture.md](docs/architecture.md) for the full layer map and gaps.

**Tested where:** Continuous integration and the author's Macs. Multi-machine behaviour is integration-tested with fixture hosts and fake SSH; treat first install on your own fleet as production validation.

## Five-minute quick start (two Macs, two tools)

Assumptions: macOS, Rust toolchain (`rustup`), SSH between a **coordinator** and at least one **worker**, and both machines can reach `~/.local/bin` on your PATH.

### 1. Build

```bash
git clone <your-fork-url> fleet
cd fleet
cargo install --path heavy-slot
cargo install --path agent-mail
```

### 2. heavy-slot on each Mac

```bash
heavy-slot status
heavy-slot -- true   # should exit 0 and log a line under ~/.local/state/heavy-slot/jobs.jsonl
```

Optional per machine: `~/.local/state/heavy-slot/limit` (one line, max slots) and `cool` (target °C when Glancebar is installed). Details: [heavy-slot/README.md](heavy-slot/README.md).

### 3. agent-mail identity and relay

On every Mac:

```bash
agent-mail install desk    # replace desk with this Mac's short name
```

Coordinator only (polls workers listed in `~/.config/agent-mail/hosts`):

```bash
# ~/.config/agent-mail/hosts — one SSH alias per line, e.g. lab
agent-mail relay
```

LaunchAgent template: [docs/launchd/com.example.agent-mail-relay.plist](docs/launchd/com.example.agent-mail-relay.plist).

Send a note from the worker when the coordinator is reachable:

```bash
agent-mail send desk:myproject "hello from the worker"
agent-mail unread --mark
```

Full command reference: [agent-mail/README.md](agent-mail/README.md).

### 4. Verify like a stranger

```bash
./scripts/smoke.sh
```

## Configuration map

| Tool | Config directory |
| --- | --- |
| heavy-slot | `~/.local/state/heavy-slot/` (limits, mode, cool) |
| agent-mail | `~/.config/agent-mail/` (`name`, `hosts`) |
| diskmap | `~/.config/diskmap/config.json` |
| placement | `~/.config/fleet/placement.toml` (from `placement init`) |
| agentkit | `~/.config/agentkit/` (see [agentkit/example](agentkit/example/)) |
| cowork | `~/.config/cowork/config.json` |

SSH host aliases for relay and remote work: `~/.ssh/config` plus `~/.config/remote-run/hosts` when you add offload scripts. Walkthrough: [docs/setup.md](docs/setup.md).

## Documentation

| Doc | Contents |
| --- | --- |
| [docs/setup.md](docs/setup.md) | SSH, host lists, launchd templates, checklist |
| [docs/architecture.md](docs/architecture.md) | Scheduling, messaging, capacity, policy, monitoring |
| [docs/storage-policy.md](docs/storage-policy.md) | Data classes and placement overview |

## The rest of the setup

Fleet is the Rust-and-docs core for multi-Mac agent work. These companion projects share menubar UX, telemetry, and ops patterns; install what matches your fleet.

| Project | Role |
| --- | --- |
| [Glancebar](https://github.com/todd866/glancebar) | Menu-bar machine vitals and fleet rows. `heavy-slot`'s cool gate reads Glancebar thermal/`chipC`; Glancebar fleet rows read heavy-slot and diskmap summary state; a morning digest can surface as a Glancebar row. |
| [Energybar](https://github.com/todd866/energybar) | Same instrument-style menubar for home energy (solar, EV charger). Uses the shared morning-check pattern with Glancebar; not required to run Fleet scheduling or agent-mail. |

More of the stack (offload to a worker Mac, capacity-aware engine picking, the morning digest) is in use on the author's Macs and will be added here as each piece is made config-driven.

## Contributing

This repository is intended as a public face of a larger private fleet tree. Export tooling in the private repo runs `tools/public-export/export.py` and `leakcheck.py` before a candidate publish; do not commit site-specific host tables or absolute home paths.
