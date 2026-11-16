# fleet morning

One scheduled pass that reads telemetry the fleet already records, reads what each project's own check last reported, and writes a compact digest. It does not repair anything. A later agent run, on whichever CLI has capacity, works the queue.

`bin/fleet` is the command. Install it as `~/.local/bin/fleet`. The implementation is the `fleet/morning` package (Python ≥ 3.11, stdlib only). Config and `.fleet/check` are TOML, read with `tomllib`, which the standard library added in 3.11. `bin/fleet` re-execs `/opt/homebrew/bin/python3` when that binary exists, so a launchd job does not stay on the older `/usr/bin/python3`.

```bash
fleet morning collect --local
fleet morning
fleet morning --dry-run
fleet morning follow
```

Exit 0 means the check finished. Warn and fail are in the digest, not in the exit code. `follow` exits 75 when `ai-pick` cannot choose an engine.

## Where it runs

Each Mac pushes its own readings. The digest does not SSH, and it does not ask another Mac to answer.

`fleet morning collect --local` reads this Mac and atomically replaces `~/.local/state/mac-sync/fleet-snapshots/<host-id>.json`. That directory sits inside the Syncthing share (`mac-sync`), so the other two Macs receive the file without anyone holding a key into the primary. The file is this Mac's disk and diskmap summary, backups, launchd jobs, Syncthing, Glancebar update status, agent-mail, heavy-slot, and git/CI for repos checked out here. A crash mid-write leaves the previous file.

`fleet morning` (the digest) reads every snapshot in that folder and replaces this Mac's file with a live reading taken in the same run. A host with no file yet is absent, not a failure. `--snapshot path.json` still loads one digest document and skips collection; the tests use it.

A snapshot older than 3 h is one host row, value `stale`, datum `since HH:MM` (or `since 6 Nov` on an earlier day). That row is info: symbol `?`, the digest stays ok, and no work item is filed. The laptop is asleep, and its last disk and backup rows are still shown. After 48 h the same row is a warn (`~`). `last_ok` in the summary is that snapshot's `collected_at`.

An explicit `ok: false` host (an old `--snapshot` fixture, or a reading that never arrived) is still `away`, and its disk and backup rows are omitted.

Host ids come from diskmap (`summary.json` `host_id`, else `~/.config/diskmap/config.json`, else the built-in LocalHostName map). In the examples, `primary` is the travelling laptop, `hub` the always-on home Mac and `shared` a laptop someone else also uses; the real names and roles come from `~/.config/fleet/site.json` (`hub`, `primary`). This Mac is recorded as its own id, not as the role name `hub`.

The local probe reads known state files and, for Syncthing only, localhost REST. It does not run `du` or `find`. The Syncthing API key is read from this Mac's `config.xml` and is not written to the snapshot.

## What it reads

| Check | Source already on disk | Rule |
|---|---|---|
| Disk free and 24 h trend | `~/.local/state/diskmap/summary.json` (or mounted volumes matched to `drives.json` by UUID when that file is missing) | One row per physical drive, on the host where it is mounted. A drive mounted nowhere is `away`, not a floor breach. The internal disk of a Mac with a row in the targets file (`disk.targets_file`, by default `~/.local/state/mac-sync/storage-audit/free-targets.tsv`) is judged by that row: under its target is warn, under its floor is fail. Any other disk: under the floor is warn, under `floor * fail_ratio` is fail. Recovery, Preboot and the other system volumes share the internal disk's space and are not rows. A 24 h free-space change at or below `trend_warn_gb` / `trend_fail_gb` is warn / fail. Bytes are decimal (1e9), same as diskmap. |
| Placement | `~/.local/state/placement/status.json` | A data-set gap is a fail row. A host already shown on a disk row is not repeated. |
| IDrive age | Latest bounded local `SessionLogsNew/Backup/*.summary`, completion marker, failed-file count and mtime | Incomplete/unreadable/future-dated summaries and failed files are failures, never replaced by cached health. Age against `idrive_warn_h` / `idrive_fail_h`. No stamp on an `idrive_hosts` Mac is a fail. |
| drive-mirror | mtime of `~/.local/state/drive-mirror/last-ok` on the hub | `drive_mirror_warn_h` / `drive_mirror_fail_h`. |
| hdd-backup | newest `~/.local/state/hdd-backup/**/last-success*` or health `backup_stamps` | `hdd_backup_warn_h` / `hdd_backup_fail_h`. |
| iCloud stalls | `brctl status` pending uploads, else health `cloud_live` | Pending uploads with no sync time, or a sync older than `icloud_stall_h`, is a warn. |
| Coverage | `<drives_project>/coverage.json` (`drives_project` in `site.json`) | Older than 24 h is a warn. |
| Syncthing | REST on `127.0.0.1:8384`, key from `config.xml` | Errors or receive-only local changes are a fail. Folders with `needBytes` are a warn. No config file means that Mac is not running Syncthing; it is skipped. |
| Glancebar updates | `~/.local/state/glancebar-update/status.json` per Mac | `state: ok` with `behind` 0 is current. `behind` > 0 is a warn. A missing file is a warn. Fields: `state`, `behind`, `installed`, `origin` (or `release` / `latest` / `version`). |
| launchd | mac-registry `hosts/<alias>.json` plus `launchctl print` (`runs`, last exit, state) and the job's state/log mtime | Not loaded, or last exit not 0, is a fail; last exit 75 (the job deferred itself: a busy drive, a metered link) is a warn, `deferred`. `daily HH:MM` that has not run by the slot plus `overdue_h` is a fail. `every Nm` is a fail when launchd has not started it, or the only clock is older than two intervals. A quiet log is not a miss when `runs` > 0 and the last exit is 0. The PID column of `launchctl list` is not a clock. |
| Thermal and slots | `~/.local/state/machine-analytics/<date>.jsonl`, `~/.local/state/heavy-slot/jobs.jsonl` | A job exit other than 0 or 75 in the last day is a fail. Exit 75, or a wait of `wait_warn_s`, is a warn. Chip at or above that Mac's own heavy-slot cool target (`~/.local/state/heavy-slot/cool`, sent in its snapshot as `cool_c`), or `thermal` other than `nominal`, counts toward hot hours. `thermal.hot_c` is the line only for a Mac that sends no target; with neither, only thermal pressure counts (`hot_hours_warn` / `hot_hours_fail`). Gaps longer than `gap_s` are not treated as hot the whole way. |
| AI capacity | `ai-capacity --json` on the hub | `services.<engine>.pools[]` with `remainingPct`, optional `jobsLeft`, `reset`, `asOf`. At or under `near_empty_pct` or `near_empty_jobs`, with the reset still ahead, is a warn; 0 is a fail. A reset that has already passed, or a reading older than `stale_h`, is `stale` or `unread`, not a live percentage. A missing `ai-capacity` binary is skipped. |
| Agents | `~/.local/state/cowork/board.json` if present, else `taskhub json` | Stale claims are a warn. Conflicts are a fail. A presence id that is not a 64-hex cowork session, and is not an ended session, is an unregistered session (warn). |
| Mail | `~/.local/state/agent-mail/inbox.jsonl` and `read.txt` | Unread older than `unread_mail_h` (12 by default) is a warn. |
| agentkit | `agentkit doctor --json` | Missing binary is skipped. A reported issue is a fail. |
| Projects | mac-registry `descriptions.tsv` and `hosts/*.json` | See below. |
| Upstream | `~/.local/state/fleet/upstream/<name>.json`, written by `fleet upstream check` (daily 22:00Z on the hub) for each repo in `~/.config/fleet/upstream.toml` | A release nobody has reviewed is a warn and a work item: `fleet upstream show NAME`, record what was taken or skipped in the repo's notes file, `fleet upstream ack NAME`. Never checked, or a check failing for over `upstream.stale_h` (72), is a warn. |
| Job reviews | mac-registry `jobs.tsv`, columns 4 (project) and 5 (review date) | A job whose review date is today or earlier, or that has no date, is a warn on the `reviews` row. A job with no project is a warn on the `owners` row. Neither is ever a fail. |

Thresholds live in `~/.config/fleet/morning.toml`. The shipped numbers are [examples/morning.toml](examples/morning.toml). A missing key keeps the default in `fleet/morning/config.py`. `tz` is an IANA name (`UTC` in the example). Per-disk floors are `[disk.floors.<host>]` tables, in GB, and win over the targets file.

Rows that follow other files:

- **Jobs.** A job on the `utc-daily` wrapper is judged in UTC from the wrapper's own record (`~/.local/state/utc-daily/<label>/last` and `running`), not from launchd's last exit status. A run ended by SIGTERM is not a failure while the job is running again or is owed its one retry. A job with no plist on a Mac, which `jobs.tsv` in the registry does not place there, is retired and has no row. A project row does not fail for a job that already has a job row.
- **Queue.** An attempt in the job queue whose ownership is uncertain stops every dispatch on that Mac. It is a `queue:<host>:<job>` failure naming the attempt, when it began, and the `fleet-queue` command that clears it.
- **Cadence.** `summary.json` carries `cadence` (`every`, `at`, `next`, `text`, for example "daily, next at 22:30Z"), read from the launchd job that writes the digest, so a reader can tell a daily report from a stale one.

Listing a host under `idrive_hosts`, `mirror_hosts`, `hdd_hosts`, or `icloud_hosts` means that backup is expected there. An empty list does not invent a failure.

## Job reviews

Every launchd job in `jobs.tsv` names the project it belongs to and carries a review date:
the day someone asks whether the job should still exist. A temporary job (a logger, a
one-off migration) gets a date two to four weeks out when it is installed. A keeper gets
about 90 days.

When a date comes due the digest shows one `reviews` row listing the jobs, and it is filed
as a work item like any other warn. Working that item means, for each job listed:

- still needed: move its review date on and say why in the description if that changed;
- no longer needed: propose removing it. Removal is the owner's call. With his yes, boot
  the job out, `trash` its plist, and delete its `jobs.tsv` line in the same piece of work.

A job is never removed just because its date passed, and a date is never pushed forward
without looking at what the job did since the last review (its log, its state folder, its
last exit).

A job installed on a Mac with no `jobs.tsv` line has no project and no review date, so the
review rows cannot see it. The digest names these in one `unregistered` row (warn). Working
that item means adding the line, with a project and a review date, or proposing removal.

## Project checks

`mac-registry` descriptions name the projects (`name`, category, status, text). Status `active` or `unknown` is checked. `archive` and `dormant` are skipped unless the repo has a check file.

A repo may declare `.fleet/check` (TOML):

```toml
command = "tools/ops/menubar-morning-check.sh --quick"
max_runtime_s = 180
where = "hub"
```

`where` is `any`, `hub`, `primary`, or the hub's or primary's own name from `site.json`. Copy [examples/fleet-check.toml](examples/fleet-check.toml). Examples:

| Project | Command | Where |
|---|---|---|
| glancebar | `tools/ops/menubar-morning-check.sh --quick` | `hub` |
| selector | the deck test, silent | `hub` |
| a web app | `gh run list --limit 1 --json conclusion,status` | `any` |

`max_runtime_s` and `where` describe where a check is allowed to run. The push collector does not start that command: placing it on another Mac was `remote-run` or SSH, and workers do not hold keys into the primary. `plan_check` still reports that placement for callers that schedule a check themselves.

### The result file

A repo's own check reaches the digest through a file. The repo runs its check on its own
schedule (a launchd job, or the end of a job it already has) and writes the result; the
collector only reads it. Name the file in `.fleet/check`:

```toml
command = "tools/media-sync/media_sync.py check"
max_runtime_s = 30
where = "any"
result = "~/.local/state/media-sync/check.json"
max_age_h = 30
```

`result` is `~/…` (under the home folder), an absolute path, or a path inside the repo.
`command` may be left out when only the result matters. The file is JSON:

```json
{"version": 1, "generated": 1700000000, "status": "warn",
 "value": "1 fail 2 warn", "datum": "upload",
 "detail": "upload: failed 36 h ago; scout: taste step timed out",
 "rows": [{"key": "media:upload", "status": "fail", "name": "upload", "value": "failed", "datum": "36 h", "detail": "…"}]}
```

`status` is `pass`, `warn` or `fail`. `value`, `datum` and `detail` are the digest row's
columns and its detail line. `generated` is epoch seconds; without it the file's own
modification time is the clock. `rows` is the repo's business: the digest shows one row per
repo and does not read them.

| The file | The row |
|---|---|
| read, within `max_age_h` | `check <repo>` with the file's status, value and datum |
| older than `max_age_h` (the repo's own, else `projects.check_max_age_h`, 30) | warn, `stale`, `since 14 Nov 03:10`; the detail keeps the last status |
| not JSON, not an object, or no status of pass/warn/fail | warn, `unreadable` |
| `.fleet/check` itself does not parse | warn, `unreadable`, `.fleet/check` |
| not on this Mac | nothing from this Mac: the check may run on another one |
| named, and on no Mac | warn, `missing`, `no result` |

The check is its own row, key `check:<repo>`, area `check`. It never replaces the repo's
`project:<repo>` row, so a passing check cannot hide a dirty checkout or a failed CI run,
and a failing check is filed as its own work item. When two Macs hold a result for the
same repo the digest shows the worse one, and of two equally bad the newer.

What the snapshot records, for a repo checked out on this Mac, is the cheap reading:

- git dirty, ahead, behind, or unpushed commits → warn
- `gh run list` when `origin` is a GitHub remote → fail on `failure` or `cancelled` (no `gh`, or no remote, is not a failure)
- a launchd job whose label or command names the project, last exit non-zero → fail

The same project on two Macs is one row: dirty if either checkout is dirty, ahead/behind the larger count, CI failure if either side failed. A repo that is not checked out here is not invented from another Mac's registry.

`gh` runs during collection when a checkout's `origin` is GitHub. The unit tests use a local repo with no remote, so they do not call it. The `.fleet/check` command is not started; what the check found arrives through its result file (above).

## Digest

Written on the hub:

| Path | What |
|---|---|
| `~/.local/state/fleet/morning/<date>.md` | Instrument rows, then one detail line per warn/fail |
| `~/.local/state/fleet/morning/<date>.json` | The same document |
| `~/.local/state/fleet/morning/summary.json` | Stable copy for Glancebar |

The text is one row per instrument, no section headers:

```text
! backup   primary idrive                    1272 h  floor 72 h
~ backup   hub idrive                    39.5 h  floor 36 h
? disk     t9-1                         away  unseen
* 12 checks passed

! backup:primary:idrive  age 1272 h; warn 36 h; fail 72 h
```

Columns are symbol, area, name, value, datum. Symbols are `*` pass, `~` warn, `!` fail, `?` unreachable or away. The value keeps the unit (`213 GB`, `40 h`, `4%`). Datum says what the number is (`24h -2.1 GB`, `floor 36 h`, `resets 14:00`). Names are not clipped. Rows are ordered fail, then warn, then away, then one `N checks passed` line. Passing instruments stay in the JSON. Explanations sit on the detail line, keyed, and in the JSON.

### Glancebar JSON

`summary.json` version 1:

```json
{
  "version": 1,
  "generated": 1700000000,
  "date": "2026-11-07",
  "ok": false,
  "counts": {"pass": 4, "warn": 1, "fail": 0},
  "rows": [
    {
      "key": "disk:hub:internal",
      "status": "pass",
      "symbol": "*",
      "area": "disk",
      "name": "hub internal",
      "value": "213 GB",
      "datum": "24h -2 GB",
      "detail": "free 213 GB; floor 40 GB; 24h -2 GB"
    }
  ],
  "hosts": {
    "hub": {"ok": true, "last_ok": 1700000000},
    "shared": {"ok": false, "away_since": 1699900000}
  }
}
```

Glancebar should read `rows` and tint by `symbol` (do not rely on colour alone). `ok` is true only when every row is `pass`. `detail` is the tooltip. `hosts` is how the next morning knows `away since`.

## Notification

When any row is warn or fail, one notification is posted on this Mac, and only when `notify_host` is this Mac. The body is a single line, for example `1 fail 1 warn · hub internal 19 GB`. The digest does not SSH to another Mac to show it. A clean morning, including one whose only extra row is an asleep laptop (`stale`, info), sends nothing. Set `notify_host` to the Mac that runs the digest, or run the digest on the Mac that should see the banner.

## Work items

Every warn and fail is one open item, deduplicated by `key`. A later morning updates that row (`opened` stays, `updated` moves). A pass closes it (`state: done`). Passing rows are not filed.

If `~/.local/state/cowork/board.json` exists (or `COWORK_STATE` points at a directory that has one), items are written to `inbox.json` beside it. Otherwise they go to `~/.local/state/taskhub/items.json`. `taskhub json` is only the fallback read for the live board when cowork's file is absent.

```json
{
  "version": 1,
  "generated": 1700000000,
  "source": "fleet-morning",
  "items": [
    {
      "key": "backup:idrive",
      "work": "morning-backup-idrive",
      "status": "fail",
      "state": "open",
      "title": "idrive 80 h",
      "detail": "age 80 h; warn 36 h; fail 72 h",
      "scope": ["service:backup"],
      "source": "fleet-morning",
      "opened": 1700000000,
      "updated": 1700000000
    }
  ]
}
```

`work` is the claim name. Morning does not call `cowork claim`; that would take the work for itself.

## How any agent consumes this

The fixing step is not part of `fleet morning`. Any provider does the same thing:

1. Pick an engine that has fresh headroom: `ai-pick --engines claude,codex,cursor`. Exit 75 means defer. Pools are separate; a stale or unknown reading is not spare capacity.
2. Read `~/.local/state/fleet/morning/summary.json` and the inbox file above. `fleet morning follow` does the pick and writes `~/.local/state/fleet/morning/follow.json` (`engine`, `digest`, warn/fail `rows`) without starting a CLI.
3. For each open item, claim it if cowork is installed: `cowork claim morning-backup-idrive --provider <engine> --session <id> --host <hub> --scope service:backup --mail <hub>:<project>`.
4. Fix the cause the `detail` describes. Do not change a threshold to silence the row.
5. `cowork done morning-backup-idrive --session <id>`.
6. Leave the inbox row. The next morning marks it done if the reading is healthy, or updates it if it is not. The key does not grow a second item.

## launchd

After `fleet` is on `PATH` (the script selects Homebrew Python when it is present):

| Template | Where | When | Command |
|---|---|---|---|
| [launchd/com.example.fleet-snapshot.plist](launchd/com.example.fleet-snapshot.plist) | every Mac | every hour, and at load | `fleet morning collect --local` |
| [launchd/com.example.fleet-morning.plist](launchd/com.example.fleet-morning.plist) | the hub | 22:30Z daily, through `utc-daily` | `fleet morning` |
| [launchd/com.example.fleet-morning-follow.plist](launchd/com.example.fleet-morning-follow.plist) | the hub | 22:45Z daily, through `utc-daily` | `fleet morning follow` |

The snapshot job's `StartInterval` is 3600 and `RunAtLoad` is true, so the first file is written as soon as the agent loads, then hourly. Replace `REPLACE_HOME`. Load the follow template only when an operator will actually start the engine `follow.json` names. The follow job does not launch Claude, Codex, or Cursor itself.

```bash
launchctl bootstrap "gui/$(id -u)" ~/Library/LaunchAgents/com.example.fleet-morning.plist
```

## Tests

```bash
python3 -m unittest tests.test_morning
```

No network. They cover threshold sides, an absent host, digest width, work-item dedup, `.fleet/check` discovery, a check result file that is absent, old, broken or held by two Macs, the snapshot write/read round-trip, the 3 h info and 48 h warn staleness lines, and a digest that merges three host files without SSH.

## Rollout

Python ≥ 3.11 on each Mac. Homebrew's `/opt/homebrew/bin/python3` is the one `fleet` will re-exec. Do not point the plists at `/usr/bin/python3`.

1. On each Mac, copy `docs/examples/morning.toml` to `~/.config/fleet/morning.toml`. Set `notify_host` to the Mac that runs the digest (the hub, unless the banner should appear somewhere else), `hub`, and `tz`. `hosts` is no longer an SSH target list. Clear any backup host list you do not use. Also write `~/.config/fleet/site.json` on each Mac: `{"hub": ..., "primary": ..., "job_label_prefixes": ["com.you"], "drives_project": "~/Projects/drives", "mail_to": "primary:fleet"}` (see `fleet/site.py`; every key optional).
2. Symlink `bin/fleet` to `~/.local/bin/fleet` on every Mac (a copy still works if it lives in this repo tree; the script follows its own path to the package). Confirm `fleet morning collect --local` writes `~/.local/state/mac-sync/fleet-snapshots/<host-id>.json` and leaves no `.tmp` beside it.
3. Confirm Syncthing is already sharing `~/.local/state/mac-sync`. The new `fleet-snapshots` directory rides along. If the share is an explicit file list, add that directory. Wait until each Mac's file is visible on the other two.
4. On the hub, `fleet morning --dry-run`. The rows come from the snapshot files plus the hub's live reading. A laptop with a snapshot older than 3 h is `? host <id> stale since HH:MM`, info, and its last disk row is still there. Nothing in the run opens SSH.
5. Load [com.example.fleet-snapshot.plist](launchd/com.example.fleet-snapshot.plist) on every Mac (`StartInterval` 3600, `RunAtLoad`). Replace `REPLACE_HOME`.
6. Load the 22:30Z digest plist on the hub only. A second run must not duplicate inbox keys. A snapshot older than 48 h is a warn and files one `host:<id>` item.
7. When cowork's `board.json` is on the hub, the inbox is written beside it. Until then the same items are under `~/.local/state/taskhub/items.json`.
8. Load the 06:45 follow plist on the hub only after you have a way to start the engine named in `follow.json`. The morning command itself never calls a model. `.fleet/check` commands are not started by this job; git and CI for repos checked out on each Mac are what the snapshot carries.

## Delivery evidence and current limits

`morning/delivery.json` records every digest's notification outcome, generation time,
source/target host, actionable finding keys, and `assistant_woken: false`.
`wrong-host` means no attempt was made; `failed` means the OS command failed or
could not run; `submitted` means only that osascript returned zero. It does not
prove the banner was displayed or read. Failures also appear on stderr, while
check exit semantics remain unchanged. The inbox and `follow.json` are work
records, not assistant wake mechanisms. No credentials, webhook or scheduler
is installed by this patch.

For scheduled assistant handling, use an existing supported app automation in
the owning conversation, and have it inspect fresh snapshots plus delivery
receipts. If its only connected computer is offline, report observation as
unavailable; never turn the previous green reading into a new success.
A local notification cannot reach an absent owner on another device.

The local probe always measures internal physical free bytes at collection time
(`shutil.disk_usage`, no purgeable estimate). If that read fails, old health data
cannot replace it with a green value. External disk trends still come from
diskmap; their `last_seen` values must be respected by consumers.
