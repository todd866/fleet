# Session identity and message pickup

`agent-mail-sessions --json` inventories live local Cursor CLI sessions without
contacting them. Use a full UUID with `--session UUID` for a single verified
selection. It emits JSON only, preserving full titles and paths without clipping.
The title is a label, never an address. Two agents may share a title, project,
provider or Terminal window.

The directory binds a same-user process to its open Cursor session database and
rechecks the open session binding and process identity after collecting metadata.
It reads only the separate
`meta.json` display fields. It does not inspect conversation blobs, environment
variables, authentication files or the database metadata containing encryption
keys. It neither writes a registry nor installs a watcher.

`complete: false` and exit 2 mean incomplete or changing evidence. Missing or
ambiguous database bindings, process exits/reuse, failed observations and duplicate
live UUIDs are visible. Explicit selection fails closed on any incomplete census.
Metadata timestamps are conversation-file timestamps, not model heartbeats.
`model_activity: unknown` is intentional: a live CLI can be idle, waiting on the owner,
running tools or receiving a model response. Ownership still requires current
task evidence or an authorized coordination reply.

This is a point-in-time inventory, not an atomic process lease. Processes can
change after the last sample, and start times have `ps` second-level resolution.
It is local-only; run it on a separately authorized host to inspect that host.

## Delivery states

- Queued: a durable local inbox/outbox record exists.
- Read: the intended provider/session produced a matching receipt.
- Replied: an identified reply exists. A read receipt alone is not agreement or
  task completion.
- Notified: requires separately verified notification delivery. Writing a queue
  file or observing a live process does not establish this state.

Exact session-addressed messages must not fall back to a project/provider
broadcast when a target is absent or ambiguous. Host/project/role messages retain
their legacy semantics; they are not unique session addresses.

Current running Cursor sessions do not automatically learn of a newly installed
mail binary or a new inbox record. A supported existing read hook can pick up mail
at a turn boundary if it supplies the actual session ID. Installing/changing hooks
or interrupting live work is separate from this read-only directory and is not
done here. No unattended UI keystroke/paste loop is provided. An authorized exact
session can be asked to read its mail through its existing interface, preserving
user input and verifying the subsequent receipt. Otherwise report waiting for
pickup, rather than claiming delivery to the agent.
