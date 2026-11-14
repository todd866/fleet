# Exact-session Cursor pickup

The `cursor-hook` subcommand is a JSON adapter for `sessionStart` and
`postToolUse`. It selects only valid exact envelopes addressed to provider
`cursor` and the event's `conversation_id`. It never falls back to `session_id`,
generation ID, cwd, environment identity, shared broadcasts, or another session.
Role-targeted messages are withheld. Subagent/parent-tool context is withheld.
This is same-OS-user routing, not authentication: hook JSON and local files can
be spoofed by the same user. "Verified identity" here means the documented
Cursor field with valid syntax, not independent proof of a running process.

## Protocol evidence (checked 2026-11-09)

Official source: https://cursor.com/docs/hooks

Cursor documents JSON stdin/stdout, stable `conversation_id`, `additional_context`
on session start and after tool use, timeout in seconds, and stop continuations.
A stop continuation becomes a user message; this adapter deliberately does not
emit one. It does not implement an idle wake, mid-tool interruption, scheduler,
process restart, or keypress injection. New mail is considered at the next
supported hook boundary, only if that Cursor process loads and executes the hook.

Installed CLI inspected read-only: `2026.11.05-86047df` under
`~/.local/share/cursor-agent/versions/`. `index.js` forwards postToolUse's
`conversation_id` into the hook and maps `additional_context` into its response.
`1706.index.js` consumes sessionStart additional context and separately implements
stop followups as userMessageAction. `6139.index.js` lists supported additional
context event types. These are implementation observations, not a paid live model probe. Further
inspection established that the standard CLI snapshots user hook configuration
at run/session initialization, not at each tool boundary or user turn.
`1706.index.js` loads hooks in runAgent startup; `6715.index.js` performs that
loader read; `6139.index.js` retains the result as this.config and selects hooks
from it at each event. The normal CLI updateConfig call merges fetched team
hooks, not local hook-file edits. Its settings watchFile is not a hooks watcher.
A fresh run, including a resumed chat, is an evidenced configuration-loading
boundary. Do not apply the general website reload claim to existing CLI runs.

## Installation boundary

`examples/cursor-hooks.json` is a proposed merge fragment, not a replacement for
existing hooks. Review and authorize an installation separately; use a binary
built with this command, preserve existing hook entries, and verify actual
boundary execution afterward. No live settings are changed by these commands.
Do not run `agent-mail install` to deploy this adapter: the older install path
still emits `unread --hook --provider cursor`, which is a legacy plaintext,
mark-on-read path and is NOT this protocol adapter. No `beforeSubmitPrompt` or
`stop` entry is required here. An explicit two-second hook timeout bounds stalls;
failures do not block the user's normal work. No network or child process is used.

## State and retries

- Stored: a valid exact envelope exists in inbox.jsonl; this alone does not wake
  or deliver to a model.
- `cursor-presentations.jsonl`: `hook_output_emitted` means JSON was flushed to
  stdout. It cannot prove Cursor accepted it, that the model saw it, or that work
  completed. A crash can omit the audit record even after emission.
- Acknowledged: after actually considering the message and recording a
  disposition, the receiving agent explicitly runs:
  `agent-mail cursor-ack --session CONVERSATION_ID MESSAGE_ID 'disposition'`.
  This validates the pending exact target, appends the disposition to
  `cursor-acknowledgements.jsonl`, and writes that exact reader's receipt.
  A disposition audit without a receipt is an incomplete acknowledgement and
  remains retryable. Acknowledgement is not task completion or permission to reply.

Presentation records defer repeat output for 60 seconds per message, conversation,
and configured host. Pending messages retry on a later natural boundary once the
cooldown expires, including after dropped output. A crash before the audit write
allows immediate retry. Future timestamps (clock rollback), invalid records, and
unavailable/oversized audit files do not suppress retry. No cooldown creates a
read receipt or acknowledgement. Concurrent hook invocations can emit duplicates;
the cooldown is best-effort, not an exactly-once delivery guarantee. There is no
background retry loop. Each invocation emits at most eight complete
messages and 10,000 UTF-8 bytes of complete framed context (including punctuation and prefix); larger or excess messages remain in the
inbox and are reported on stderr, not truncated or marked. Inputs are capped at
1 MiB; inbox and receipt reads at 8 MiB each. Exceeding bounds fails pickup closed
without consuming mail. These conservative bounds require operator attention
for oversized/old mailboxes. Repeated unacknowledged messages can consume context;
acknowledge dispositions promptly rather than treating presentation as receipt.

Peer text is framed as untrusted coordination evidence, never user permission.
The adapter does not execute it, send replies, or authorize additional tasks.
Legacy read.txt and other providers' receipts are never changed. Existing manual
`unread` behavior is unchanged; it can still list shared mail when explicitly used.

## Verification

Synthetic integration tests use private temporary inbox/config/home directories:
exact identity versus separate lifecycle ID, shared/other-session exclusion,
cooldown and retry after expiry until explicit acknowledgement,
clock rollback/future timestamps, cross-host and cross-conversation isolation, wrong-session ack rejection, invalid or
missing identity, unsupported hooks, subagent context rejection, oversized message
preservation, and emitted-versus-acknowledged state. No real agent was messaged,
no live hooks changed, and no model request made.

Run `cargo test -p agent-mail --offline` and `cargo fmt -p agent-mail -- --check`.

## Natural invocation verification

Each successful hook flush also atomically replaces a small JSON file under
`cursor-invocations/`, keyed by a filename hash of the conversation ID. Inspect
its `conversation_id`, `hook_event_name`, `at`, and `outcome` fields to verify a
natural invocation, including `empty_output_emitted` when no mail is eligible.
No prompt, tool input, or mail content is recorded. This proves the adapter ran,
not that Cursor accepted context or a model read mail. There is one last-status
file per conversation; concurrent calls may replace one another. The hash is a
filename convenience only; always verify the conversation_id inside the record.

Installed CLI `6139.index.js` rejects additional context over 10,000 JavaScript
characters. This adapter conservatively caps the full framed context at 10,000
UTF-8 bytes, which also bounds UTF-16 units, including all prefix overhead.

## Existing-session lifecycle bridge

Source-controlled proposal: `../integration/cursor-lifecycle/`. It contains the
baseline Python lifecycle hook, reviewed candidate snapshot, ten synthetic tests,
and deployment/rollback guidance. Actual installation and verification are
tracked separately from the candidate in the deployment record.

Existing standard CLI runs which already loaded the lifecycle command execute
that Python file afresh at each event. Replacing its implementation after review
therefore permits pickup at their next natural sessionStart/postToolUse boundary
without reloading configuration or restarting a run. It does not wake idle runs.
The bridge forwards only a constructed identity/event whitelist to the Rust
adapter and returns only bounded additional_context, never a stop followup or an
implicit read/acknowledgement receipt. Lifecycle metadata reporting is preserved.

After deploying the bridge, remove only our exact standalone agent-mail
cursor-hook entries from sessionStart/postToolUse, preserving every original
handler. This avoids duplicate entries for future runs. A previously loaded pair
can race once; cooldown is best-effort, not exactly-once delivery.

The bridge calls the pinned, reviewed Rust adapter with a 0.5-second subprocess
timeout, inside the existing two-second hook timeout. That adapter launches no
child processes. Its input/output bounds are part of the guarantee: this is not
containment for an arbitrary same-UID replacement binary or descendants created
by one. Output capture is validated after the reviewed adapter returns.

The M2 canonical `~/Projects/fleet-board/lifecycle_hook.py` must be reconciled by
an explicitly approved, atomic replacement with a fresh baseline hash check and
backup. The installer copies that source; an unreconciled M5-only installation
could be reverted later. Never overwrite concurrent owner changes.

Deployment observation (sanitized example): fresh headless sessions can produce
empty-output adapter calls. That verifies direct adapter invocation in those
sessions only, not model receipt, nonempty pickup, old-session configuration
reload, or installation of an optional lifecycle bridge.

A coordinated rollout may record bridge SHA256
`54b9aafac5a27e2e58fde561bccfd84b3a4183216dfc49c3610e44327d4f1620`
on M5 at 10:36:22 UTC, 2026-11-09. Only the two newly added standalone entries
were removed; the hook configuration is byte-identical to the original pre-pickup
configuration, preserving all existing handlers. M2 canonical source was
hash-checked, backed up, and atomically replaced at 10:36:44 UTC with the same
bridge candidate; M2's installed runtime was not changed. Deployment evidence:
`~/.local/state/cursor-lifecycle-bridge-20261109/deployment.json` on M5.
Subsequent natural `postToolUse` invocations were verified in all three existing
main conversations by 10:40:38 UTC, without restarting them or sending test mail.
All returned empty output. This proves the bridge ran at their tool boundaries,
not nonempty message delivery, model receipt, or acknowledgement.

The later reviewed spool-recovery version is now installed on M5 and reconciled
to the M2 canonical source, SHA256
`b9287ad5b60800c40aa363ba1db2bafdb7d34a826760af974bd75d49c37f43ce`.
It leaves exact-mail pickup unchanged. See
`../integration/cursor-lifecycle/SPOOL-RECOVERY.md` for live recovery evidence.
