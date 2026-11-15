# Fleet

The working setup of one person learning and building in something as complex as medicine, made so that someone else can copy it: several Macs and AI coding agents that share the machines without fighting over heat, slots or quota; guards and tripwires so agents can work without prompts; data placed once and backed up off-site once; a morning check that says what broke; and the learning, media and instrument apps that run on top. [docs/stack.md](docs/stack.md) maps every layer and how far each has got.

Host names, drive layouts and limits live in `~/.config/<tool>/`, not in this repository.

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
| [git-stale-locks](bin/git-stale-locks) | Moves git lock files that no git process can still own (left by a restart or a killed agent) to the Trash, so the next agent is not blocked by `index.lock`. |
| [cowork](docs/cowork.md) | Work claims, leases, and board JSON for multi-agent coordination. |
| [tripwire](bin/tripwire) | Watches login keys, startup files, launchd jobs, listeners and accounts; pauses what changed, reversibly, and starts an agent to find out why ([docs](docs/tripwire.md)). |
| [idrive-ingest](bin/idrive-ingest) | Off-site backup of a Mac's drives and home through IDrive's Linux client in a small VM: one copy of each thing, daily, reported to the morning check ([docs](docs/idrive-ingest.md)). |

Bash originals under `spec/` are the behavioural spec for heavy-slot and agent-mail while you validate Rust drop-ins.

**Not in this tree (yet):** `remote-run`, `offload-target`, `ai-capacity`, the fleet morning check (in daily use; published once its site values are in config), and the one-step "join a new Mac" restore. See [docs/architecture.md](docs/architecture.md) for the full layer map and gaps.

**Tested where:** every push runs `cargo test --workspace` on a GitHub macOS runner ([![ci](https://github.com/todd866/fleet/actions/workflows/ci.yml/badge.svg)](https://github.com/todd866/fleet/actions/workflows/ci.yml)), and the author's Macs run the same tests plus the Python tests of the private tree. Multi-machine behaviour is integration-tested with fixture hosts and fake SSH; treat first install on your own fleet as production validation.

## Five-minute quick start (two Macs, two tools)

Assumptions: macOS, Rust toolchain (`rustup`), SSH between a **coordinator** and at least one **worker**, and both machines can reach `~/.local/bin` on your PATH.

### 1. Build

```bash
git clone https://github.com/todd866/fleet.git
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

Fleet is the core. These projects run on the same machines, share the slots, drives and backup, and report into the same morning check. Install what you need.

| Project | Role |
| --- | --- |
| [Cohort](https://github.com/todd866/cohort) | Open-source spaced repetition for medicine, scheduled over an embedding of concepts. |
| [anki-harness](https://github.com/todd866/anki-harness) | Scripted scenarios run inside a real Anki, across released versions. |
| [Carousel](https://github.com/todd866/carousel), [DeckKit](https://github.com/todd866/DeckKit) | Teaching decks an agent can write and revise. |
| [Glancebar](https://github.com/todd866/glancebar) | Menu-bar vitals and fleet rows, and the music player: lossless hot set on the laptop, cold tier in the cloud, a new-music scout. heavy-slot's cool gate reads its thermal state. |
| [Onto the TV](https://github.com/todd866/onto-the-tv) | Offline family shuffle and local-network TV playback from the home hub's media drives. |
| [omnifeed](https://github.com/todd866/omnifeed) | Personal media library and headphone audio client. |
| [Energybar](https://github.com/todd866/energybar), [Isobar](https://github.com/todd866/isobar) | Home energy and weather/aviation instruments in the same style. |

## Contributing

This repository is exported from a larger private tree: an allowlist chooses the files, and a leak check plus gitleaks must pass before each sync. Issues and pull requests are welcome; a change accepted here is applied to the private tree and comes back with the next sync. Keep site values (host names, volume names, home paths) in config, with generic examples in code.
