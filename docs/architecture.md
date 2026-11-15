# Architecture: an AI OS on several Macs

The fleet treats multiple Macs and multiple AI subscriptions (Claude Code, Codex, Cursor, optional alternate API backends) as one system: jobs respect machine capacity, messages cross hosts, quota-aware tools pick engines, and policy lives in hooks and skills rather than in project code. This document maps layers to components. See the private fleet inventory for a full survey and publishing order.

## Scheduling

**Purpose:** Limit concurrent heavy work, offload when the active Mac should stay cool or on battery, mirror project trees to workers, and (planned) order waiters by what the owner is likely to want next.

| Component | Implementation today |
|---|---|
| **heavy-slot** | Rust in this repo; live drop-in for bash `spec/heavy-slot.sh`. Machine-wide slot pool, cool gate, offload exit 75, `jobs.jsonl`; `--now` for the owner's job, `workers` / `$HEAVY_SLOT_WORKERS` to size a pool, lost-job records. |
| **queue-steward** | Spec only ([queue-steward.md](queue-steward.md)); planned crate + small heavy-slot wait-loop hook. |
| **Cloud lane** | Modal workspace, enabled 14 Nov 2026 ($30 credit a month); used through per-project scripts, no fleet backend yet. State, measurements and open decisions are in the private tree. |
| **git-stale-locks** | Python in `bin/`; launchd at login and every 30 minutes. Trashes git lock files no git process can still own. |
| **offload-target** | Bash in `~/.local/bin`; reads Glancebar `thermal` / `chipC`, battery, idle. Not in fleet-tools yet. |
| **remote-run** | Bash; rsync worktree, run under worker `heavy-slot` with `REMOTE_RUN=1`. Not in fleet-tools yet. |
| **focus-guard**, **power-pause** | Python/bash; QoS and battery helpers. Planned publish as a group. |
| **worker-governor** | Referenced by comments and `limit`/`mode` files; **no script found in the reference survey** — gap before worker side is reproducible. |

## Messaging

**Purpose:** Reliable async notes between agent sessions on different machines without assuming both sides are online at once.

| Component | Implementation today |
|---|---|
| **agent-mail** | Rust in this repo. Inbox JSONL, outbox + coordinator **relay**. Addressed by host and project (optional role), never by provider. Claude `unread --hook` unchanged; Codex and Cursor adapters install beside it; any other agent uses `unread --provider other --json` or `wait`. A receipt is per reader (`provider:session@host`); the project has read a message once any agent working there has seen it. |
| **relay** / **relay-hook** / **franken** | Bash; slash-command frontends to other backends — separate from agent-mail. |
| **cursor-peer** | Python; in-session message queue for Cursor — complementary. |
| **cowork** (`taskhub`) | Python in this repo. Claims, scope conflicts, drive `.writer` leases, and presence for every provider. `board.json` is what fleet-health and Glancebar read. The private dashboard server is not in this tree. |

## Capacity

**Purpose:** Know remaining quota per provider pool and pick an engine or defer work instead of burning the last few percent blindly.

| Component | Implementation today |
|---|---|
| **ai-capacity** | Python wrapper exec’ing `ai-budget.mjs` — not in fleet-tools. |
| **ai-budget.mjs** | Node; reads Claude/Codex/Cursor caches, Glancebar AI reader state, Keychain for refresh. Provider-specific; neutral reader interface planned. |
| **ai-pick** | Python; chooses engine with headroom using `ai-capacity --json` and `shared.json` on workers. |
| **ai-capacity-share** | Zsh; pushes JSON snapshot to workers on a timer. |
| **cursor-bridge / capacity.py** | Separate git tree; admission control for Cursor helper jobs (global cap 3). |

Hooks inject capacity context at session start; they do not automatically re-run on a timer mid-session (documented gap in inventory).

## Policy

**Purpose:** Encode operating rules (offload heavy Bash, no destructive deletes, shared capacity, metered link, alternate backend fallback) so every CLI behaves consistently.

| Component | Implementation today |
|---|---|
| Claude **settings.json** / Codex **hooks.json** / Cursor **hooks.json** | Templates ship incrementally; agent-mail `install` is the first automated piece in this repo. |
| **Skills** (budget-aware-allocation, chatgpt-fleet, adversarial-review, …) | Markdown in user skill dirs; scrubbed templates planned from inventory §6. |
| **offload-heavy.py**, **battery_guard.py**, **no-hard-delete.py** | Python hooks; reference copies outside fleet-tools. The portable copies are the `agentkit` guard binaries. |
| **agentkit** | Rust in this repo. Canonical rules, skills, guards, context, and notebook. `sync` generates Claude, Codex, and Cursor files. `doctor` writes `~/.local/state/agentkit/doctor.json` for fleet-health and mac-registry. |
| **placement** | Rust in this repo. Decides where each data set lives (`plan`, `apply`, `status`). Status JSON is read by fleet-health and by mac-registry's Needs-attention block. Does not take a heavy-slot. See [storage-policy.md](storage-policy.md). |

Nothing in this layer replaces the owner’s project-specific hooks; fleet-tools documents patterns, not private project guards.

## Monitoring

**Purpose:** One glance at machine health, fleet readiness, and (via Glancebar) AI subscription headroom.

| Component | Implementation today |
|---|---|
| **[Glancebar](https://github.com/todd866/glancebar)** | Public Objective-C menu-bar app. **`Glancebar --sample`** prints one JSON line per invocation. Keys used by fleet scripts today include at least **`thermal`**, **`chipC`**, and for machine-log / UI rows the wider set: `cpu`, `memPressure`, `memUsedGB`, `memTotalGB`, `swapGB`, `cellsC`, `pMHz`, `pTopMHz`, `pBusy`, `eMHz`, `eTopMHz`, `eBusy`, `batt`, `battW`, `ac`, `charging`, `diskFreeGB`, `diskTotalGB`, `volumes`. heavy-slot’s cool gate only parses **`thermal`** and **`chipC`**. |
| **[Energybar](https://github.com/todd866/energybar)** | Public; home energy UI, shared instrument style with Glancebar — not fleet-critical. |
| **machine-log** | Bash + Python; minute JSONL samples combining Glancebar, heavy-slot status, top processes. Script only. `diskFreeGB` is the free-space series diskmap uses for rates. |
| **diskmap** | Rust in this repo. Per-volume directory map (sizes, FSEvents, 30-day hot history), iCloud and IDrive as locations, archive on the encrypted volume. Writes `~/.local/state/diskmap/summary.json` for Glancebar and fleet-health. Not a separate report. |
| **machine-report** | Python; aggregates logs and `jobs.jsonl` over SSH. Script only. |
| **fleet morning** | Python package `fleet/morning` (3.11+, `tomllib`), command `bin/fleet morning`. Each Mac writes `~/.local/state/mac-sync/fleet-snapshots/<host>.json`; the digest reads that directory plus this Mac. No SSH. Writes `~/.local/state/fleet/morning/summary.json`. Does not repair anything. See [morning.md](morning.md). |
| **link-meter** | Bash; creates `~/.local/state/METERED_LINK` on metered carriers. Script only. |

Glancebar “other machines” rows SSH to each configured host, run `Glancebar --sample`, read `heavy-slot` mode/status — documented in Glancebar’s repo; fleet-tools depends on that contract, not on a private defaults domain name.

## Backends

**Purpose:** Run agents on Claude Code, Codex CLI, Cursor CLI, or a third-party Anthropic-compatible endpoint without forking the whole orchestration stack.

| Backend | Role in fleet |
|---|---|
| **Claude Code** | Primary interactive agent; agent-mail hooks; ai-budget/statusline; optional third-party launcher pattern for alternate models. |
| **Codex** | Read-only review, relay targets, battery hook; shares capacity rules via synced AGENTS.md templates. |
| **Cursor** | `cursor-agent` via cursor-bridge (worker, opinion, remote LaunchAgent trick for keychain); separate API vs Grok/Composer pools in capacity math. |
| **Alternate backend** | Reference pattern: local proxy, env-isolated Claude Code settings, keychain service name, budget footer — keep URLs and models in site config, not in this repo. |

**claude-sync-setup** (bash) copies budget scripts to a worker and patches Claude hooks; generalised “install on worker” story is still manual for most tools (see inventory §8).

---

## Rust vs scripts vs planned (this repository)

| Crate / doc | Status |
|---|---|
| heavy-slot | Rust, integration-tested, parallel week vs bash |
| agent-mail | Rust, integration-tested; relay `to` validation stricter than bash spec |
| queue-steward | Spec in docs only |
| diskmap | Rust, integration-tested. Monitoring layer: contents and history. fleet-health reads its summary. |
| placement | Rust. Policy layer: classes, floors, per-host placement, generated `.stignore`. `status` feeds fleet-health and mac-registry. |
| cowork | Python. Messaging layer: work claims, leases, and session presence. `taskhub` is the alias. |
| Everything else in inventory | Bash/python/node outside repo or not yet extracted |

For install order and gaps (overnight drivers, per-session suite caps, pool-points-per-job math), see the private fleet inventory § “Turn-key assembly” and § “Gaps”.
