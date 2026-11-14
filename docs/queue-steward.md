# queue-steward: who gets the next slot

The owner, 5 Nov 2026: "maybe we can have a subagent that looks at the queue and tries to guess what I
wanna prioritize". Today every slot pool (heavy-slot on each Mac, the Cursor helper pool) is first
come first served, and a waiting job is a waiting agent. The steward orders waiters by what the owner is
most likely to want, shows its guess with a one-line reason, and learns from his overrides.

## Shape
A crate `queue-steward` (binary of the same name) plus a small change to `heavy-slot`.

Registry: `$HEAVY_SLOT_DIR/queue/<pid>.json`, written by a waiter when it first fails to get a slot:
`{"pid":N,"label":L,"project":P,"origin":O,"cmd":C,"expected_s":E,"since":UNIX,"host":H}`.
Removed by the waiter when it gets a slot or exits. The steward ignores (and deletes) entries whose
pid is gone (`kill(pid, 0)` fails).

`project`: basename of the git toplevel of the waiter's cwd, else cwd basename.
`origin`: `interactive` when the waiter's stdin or stderr is a tty, or `CLAUDE_SESSION_INTERACTIVE=1`;
`scheduled` when XPC_SERVICE_NAME is set and not `application.*`, or `NIGHT_RUN=1`; else `agent`.
`expected_s`: median run time of this label in jobs.jsonl (last 30 days), null when unseen.

## Commands
- `queue-steward pick` → stdout: the pid that should go next (nothing, exit 1, when no waiters);
  stderr: `steward: <label> first — <reason>`.
- `queue-steward status` → one line per waiter in order: `  <rank>. <label> · <project> · <origin> ·
  waiting <Nm> · score <S> — <reason>`. heavy-slot status appends this under its slot lines when the
  queue is non-empty.
- `queue-steward bump <label|pid> [--for 2h]` → writes an override to `$DIR/queue/overrides.jsonl`
  `{"at":UNIX,"label":L,"project":P,"until":UNIX,"hour":H,"weekday":D}`; the next pick honours it.
- `queue-steward explain` → the score table for every waiter, every term shown.

## Score (deterministic; higher first; ties by `since`, oldest first)
| term | value |
|---|---|
| origin | interactive +3 · agent +2 · scheduled 0 |
| focus | project equals the active project +3. Active project: the `~/Projects/*` repo with the newest `.git/index` or `.git/HEAD` mtime in the last 60 min; none when nothing moved. +1 more when `focus-guard active` exits 0 (the owner is at the keyboard), because then daytime work matters most. |
| short | expected_s < 120 → +1; > 1800 → −1; unknown → 0. Only applied when there are 2+ waiters. |
| age | +0.5 per 5 min waited, cap +3 (starvation guard) |
| override | an unexpired override for this label or project → +10 |
| learned | +1 per override in the last 30 days for this project in the same weekday class (weekday/weekend) and 3-hour bucket of the day, cap +3 |

No LLM in v1. A v2 may let an engine chosen by `ai-pick` order the ambiguous remainder once per
wake-up, never per job; the hook for it is a single function `tie_break(waiters) -> order` that v1
implements as the deterministic sort.

## heavy-slot change
In the wait loop: on the first `all slots busy` wait, register. Each loop, before trying locks, run
the pick; when the pick is another live pid, sleep 5 and continue without trying the locks (so the
winner takes the slot). Deregister on acquisition and on exit paths (signal handler too). A waiter
whose steward call fails behaves as today (FIFO), so a steward bug can never block the fleet.
`heavy-slot status` prints the steward's status lines after the slots when any waiter exists.

## Later (not in this crate)
cursor-bridge's helper pool (python, ~/Projects/.cursor-bridge) consults `queue-steward pick` the
same way, with `label` = the worker name and `project` = the repo. agent-mail and remote-run
register their remote jobs on the worker, so the hub's queue is visible from the primary Mac's fleet row.

## Slot semantics (owner, 5 Nov 2026)
"A slot isn't full unless it's actively processing; if it's doing data transfer ahead of processing
it's not yet full." So a registry entry carries `state`: `transferring` (remote-run mirroring, a
worker fetching inputs) or `processing`. Readiness and `status` count only `processing` holders
toward "slots full"; a transferring job may still be preferred as the next slot holder but does not
block another job from starting its own transfer. remote-run already mirrors before it takes the
worker's slot; cursor-bridge's helper count should follow the same rule.
