# heavy-slot

Every agent on one Mac shares a small pool of concurrent “heavy” jobs (test suites, builds, long compiles). Without a limit, several sessions can start heavy work at once and overload CPU, memory, and thermals. **heavy-slot** waits for a free slot, runs your command while holding that slot, logs the run, and releases the slot when the command exits.

Install from this repo: `cargo install --path heavy-slot` (binary name `heavy-slot`). Put it on `PATH` (for example `~/.local/bin/heavy-slot`). The bash reference implementation lives in `spec/heavy-slot.sh`; the Rust binary uses the same lock file paths and `flock`/`lockf`-compatible advisory locks so either implementation can share slots with the other.

## Commands

| Command | Purpose |
|---|---|
| `heavy-slot status` | Print each slot: `free`, `off (limit N)`, or a busy line from `slotN.info`. |
| `heavy-slot -- COMMAND …` | Wait for a slot, run `COMMAND` with its arguments, exit with the command’s status. |
| `heavy-slot COMMAND …` | Same as above; the `--` separator is optional. |
| `heavy-slot release PID` | Ask holder `PID` (as shown by `status`) to give its slot back now; its job keeps running. Exit 0 once the slot is free, 1 if `PID` holds no slot or is not this binary. |

There are no command-line flags.

### Idle jobs give their slot back

A slot is for compute. While a job runs, heavy-slot samples the CPU time of the job's process tree;
once it has used under 1% of one core across the last `HEAVY_SLOT_IDLE_SECS` (default 600 s), the
holder releases the slot itself and logs `gave slot N back (idle …)` to stderr. The job runs on
unslotted to its normal end and still writes its `jobs.jsonl` line. This catches I/O jobs (backups,
copies) and hung reads that would otherwise queue compute work for hours. Do not SIGKILL a holder to
free its slot: under launchd, the job's process group dies with it (5 Nov 2026, an hdd backup).

## Environment variables

| Variable | Default | Effect |
|---|---|---|
| `HEAVY_SLOTS` | `3` | Total slot count (`slot1` … `slotN`). |
| `HEAVY_SLOT_DIR` | `~/.local/state/heavy-slot` | State directory (locks, info files, log). |
| `HEAVY_SLOT_LABEL` | Basename of current working directory | Shown in `status` and `jobs.jsonl`. |
| `HEAVY_SLOT_LOCAL` | unset | If `1`, skip the offload check (run here even when `offload-target` would send work away). |
| `HEAVY_SLOT_IDLE_SECS` | `600` | A running job under 1% CPU for this long gives its slot back (`0` disables). |
| `HEAVY_SLOT_COOL_MAX` | `1200` | Max seconds to wait for thermal/chip readings before starting anyway (see `cool` file). |
| `REMOTE_RUN` | unset | If `1`, skip the offload check (job is already running on a worker via `remote-run`). |
| `XPC_SERVICE_NAME` | unset | Launchd jobs (`XPC_SERVICE_NAME` set and not matching `application.*`) skip the offload check. |

Offload behavior calls `~/.local/bin/offload-target` when the checks above allow it. That script is not part of this crate; see `docs/setup.md` and `docs/architecture.md`.

## State and config files

All paths are under `HEAVY_SLOT_DIR` unless overridden.

| File | Default / format | Effect |
|---|---|---|
| `limit` | absent → use `HEAVY_SLOTS` | One line, digits only: max slots to use. Slots above `limit` show as `off (limit N)` and are not acquired. |
| `cool` | absent → no cool gate | First line: target chip temperature in °C (integer). New jobs wait until [Glancebar](#glancebar-optional) reports `thermal` = `nominal` and `chipC` ≤ target, polling every 15 s, up to `HEAVY_SLOT_COOL_MAX`. Missing or bad readings do not block. |
| `mode` | absent → normal priority | If contents trim to `gentle`, the job runs under `/usr/sbin/taskpolicy -b` (background QoS). |
| `slot{i}.lock` | created on use | Advisory lock file; holder keeps the file open until the job finishes. |
| `slot{i}.info` | one line while busy | `{label} · pid {pid} · {HH:MM:SS} · {command…}` truncated to 200 bytes UTF-8. |
| `jobs.jsonl` | append-only JSON lines | One object per completed job (see below). |

### `jobs.jsonl` line shape

```json
{"start":UNIX,"end":UNIX,"waited":SECONDS,"slot":N,"label":"…","rc":INT,"maxrss_mb":INT|null,"cmd":"… "}
```

`cmd` is the command line plus a trailing space, truncated to 300 bytes. `maxrss_mb` is the child process peak RSS from `wait4` (may differ slightly from the bash version’s `/usr/bin/time -l` measurement).

## Exit codes

| Code | Meaning |
|---|---|
| `0` | `status`, or job exited 0. |
| `2` | Usage error (no command). |
| `75` | Offload recommended: stderr prints a suggested `remote-run -- …` line; job did not run. |
| `127` | Could not execute the command (spawn error). |
| `128+N` | Job died from signal `N` (for example `143` for SIGTERM). |
| other | Job’s normal exit code. |

## Glancebar (optional)

If `/Applications/Glancebar.app/.../Glancebar` or `~/Applications/Glancebar.app/.../Glancebar` exists and is executable, **heavy-slot** runs `Glancebar --sample` once per cool-gate iteration and parses `"chipC"` and `"thermal"` from the JSON line. See [Glancebar](https://github.com/todd866/glancebar) and `docs/architecture.md` for the full sample contract.

## Worked example

On a Mac with default three slots:

```bash
heavy-slot status
# slot 1: free
# slot 2: free
# slot 3: free

HEAVY_SLOT_LABEL=sim-tests heavy-slot -- npm test

heavy-slot status   # while running: slot 1: sim-tests · pid … · …
```

After the job finishes, inspect the log:

```bash
tail -1 ~/.local/state/heavy-slot/jobs.jsonl
```

## How it interoperates

- **Bash original**: Lock files are `slot{i}.lock` under the same directory. The Rust binary uses BSD `flock(2)` on those paths; the bash script uses `/usr/bin/lockf -k`. Integration tests confirm both see the same busy slots.
- **offload-target / remote-run**: Interactive agent sessions may get exit `75` instead of running locally; launchd and `REMOTE_RUN=1` jobs are exempt.
- **queue-steward** (planned): Will register waiters under `HEAVY_SLOT_DIR/queue/`; see `docs/queue-steward.md`.
- **machine-report** (script, not in this repo): Reads `jobs.jsonl` for history.

Signals: SIGINT and SIGTERM to the **heavy-slot** process are forwarded to the child job.
