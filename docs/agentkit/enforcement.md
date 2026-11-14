# Agent enforcement inventory

Measured on the primary Mac (10 Nov 2026). **Today** is what is on disk before `agentkit sync --apply`.
**After agentkit** is what this branch generates (hooks merged, never replacing battery, patient-data,
lifecycle, peer, or capacity hooks).

| Rule | Claude | Codex | Cursor | After agentkit (all providers) |
|---|---|---|---|---|
| No hard-delete outside temp (`trash`) | PreToolUse `Bash` → `no-hard-delete.py` | text only | text only | `agentkit hook <p> hard-delete` |
| No iCloud/Desktop/Documents wipe or empty Trash | PreToolUse → `icloud-guard.py` | text only | text only | `agentkit hook <p> icloud` |
| Heavy compute via `heavy-slot` / `remote-run` | PreToolUse → `offload-heavy.py` | text only (battery guard only) | text only | `agentkit hook <p> offload-heavy` |
| Disk work on `/Volumes/*` via `disk-slot` | text only | text only | text only | `agentkit hook <p> disk-slot` |
| No large transfer on metered link | text only | text only | text only | `agentkit hook <p> metered-link` |
| `cd`/writes outside allowed roots | PreToolUse → `no-prompt-patterns.py` (`cd-outside`) | text only | text only | `agentkit hook <p> cd-outside` |
| `git push` in gated projects / force-push | project hook / markers | text only | text only | `agentkit hook <p> push` (allow plain push by default; `push_gate_projects` ask; deny `-f`; ask `--force-with-lease`) |
| No raw `agent -p --force/--yolo` | text in rules | text in rules | text in rules | `agentkit hook <p> agent-yolo` |
| No audible playback in tests | text in rules | text in rules | text in rules | `agentkit hook <p> test-audio` |
| Session context (capacity, mail, fleet) | SessionStart / UserPromptSubmit hooks | same + `ai-capacity` | lifecycle/capacity only | `agent-context` (merged) |

## Mechanism by provider

| Provider | Pre-tool hook channel | Can deny? | Notes |
|---|---|---|---|
| Claude Code | `~/.claude/settings.json` → `PreToolUse` matcher `Bash` | yes | Existing Python hooks stay until owner removes them after a clean doctor week. |
| Codex | `~/.codex/hooks.json` → `PreToolUse` | yes | New `agentkit hook codex …` entries require owner `/hooks` trust (`[hooks.state.*].trusted_hash` in `config.toml`). |
| Cursor | `~/.cursor/hooks.json` → `beforeShellExecution` | yes (`permission`: deny/allow) | User hooks bind CLI sessions; IDE surfaces that skip `hooks.json` remain text-only. |

## `cursor-worker` vs Claude session

`cursor-worker` prepends a worktree-only brief and runs `agent` with `--sandbox=enabled` and an isolated
worktree. It does **not** install PreToolUse guards in the worker process.

| Capability | Claude (hooked) | Cursor worker today | Minimal close |
|---|---|---|---|
| Hard-delete outside temp | denied | prompt only inside worker | `beforeShellExecution` via synced `~/.cursor/hooks.json` (post-apply) |
| Heavy local `cargo test` | denied when `offload-target` names worker | allowed in worker sandbox | same hooks + brief already says `remote-run` |
| Writes outside worktree | `cd-outside` + project bounds | sandbox limits; still wrote fixtures under `$HOME` in incident | hooks + `agent -p` sandbox; sweep + deny `~/` writes without marker |
| Raw `agent --force` | n/a | blocked by brief, not hook | `agent-yolo` guard on host shell that launches worker |

## Conformance

```bash
agentkit doctor --conformance   # table: installed / trusted / firing per provider×guard
agentkit doctor --json          # includes `"conformance"` for fleet morning (`<host>:fleet` section `other`)
```

Fixtures: `agentkit/tests/fixtures/guards/<guard>.json` (tool-call JSON per provider shape).

## Rule file size budget (generated always-loaded)

| Provider | Before (hand-maintained) | After (core-always + appendix only) | Moved to on-demand |
|---|---|---|---|
| Claude `CLAUDE.md` | 24 892 B | ~11 264 B (core + appendix-claude) | `rules/core-ondemand.md` (+ skills) |
| Codex `AGENTS.md` | 20 073 B | ~8 153 B | same on-demand file |
| Cursor always rules | ~6 414 B in four `.mdc` | `agentkit-global.mdc` + retired stubs | folded overlap into global; stubs `alwaysApply: false` |

Overlapping Cursor rules named in `retired_cursor_rules` (site config) are retired to stubs pointing at `agentkit-global.mdc`.

## Human steps after merge

1. Install `agentkit` + `agent-context` to `~/.local/bin`.
2. `agentkit sync` (dry run), then `agentkit sync --apply` with backups.
3. **Codex:** run `/hooks` and trust each new `agentkit hook codex …` line.
4. Confirm `agentkit doctor --conformance` is all `*` on the primary Mac, then repeat on the other Macs.
5. Remove legacy Claude Python guard commands only after a week of clean doctor reports.
