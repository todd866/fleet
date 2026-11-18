# diskmap

A read-only map of what is on each Mac and each external drive: directory sizes, day-by-day history, and a free-space alert when a disk is falling or already through its floor. It does not copy, move, or delete anything. The first map of a volume is a continuous background walk. Steady state, after that, is a short throttled pass once a minute, well under one core.

`diskmap` does not publish its own report. Glancebar and fleet-health style readers consume `~/.local/state/diskmap/summary.json`. Free-space *rates* come from `machine-log`'s existing `diskFreeGB` field, not from a second sampler.

Install: `cargo install --path diskmap` (binary name `diskmap`). Put it on `PATH` (`~/.local/bin/diskmap`). Nothing site-specific is required in code. Host names, floors, peers, and the drive registry come from config.

## Commands

| Command | Purpose |
|---|---|
| `diskmap fleet` | One row per Mac disk, physical drive, iCloud, and IDrive: total, free, 1h/24h/7d change in free space, top grower, last seen. |
| `diskmap grew [--host H\|all] [--since 24h]` | The deepest directory that explains at least 80% of the growth since `30m`, `24h`, `7d`, or similar. |
| `diskmap top [path]` | Largest directories directly inside `path` (volume root when omitted). |
| `diskmap history <path> [--host H]` | Daily totals for that directory, including days that have already shipped to the archive. |
| `diskmap where <name\|glob>` | Which Macs, drives, or cloud locations hold a matching directory of at least 100 MB. `--json` prints the rows. |
| `diskmap peers` | Refresh this Mac's cache of every peer's summary and `where` rows (`peers/<peer>.json`, `peers/<peer>.where.json`). A peer that does not answer within 60 s keeps its last cache; the line prints its age. Run hourly before `placement status`. |
| `diskmap json` | Print the summary document. Also refreshes `summary.json`. |
| `diskmap tick` | One launchd pass: keep the initial-walk worker alive, and once that walk has finished fold in FSEvents, write the summary, alert, ship history. |

`fleet` asks each peer in the config for `diskmap json` over SSH (`BatchMode`, 5s). If a peer is down, the last cached summary is shown as of that time. `json`, `grew`, and `history` on the machine you SSH to do not SSH again.

An unplugged drive stays in the map. Its row is `o` and `seen` is the age of the last scan. `*` is present. `!` is an alert (free space through the floor, or free space falling faster than 15 GB/h for 30 minutes).

```
  host  where      total          free         1h      24h       7d  grower             seen   map
! desk  internal   494 GB    149 GB free   -16 GB  -20 GB   -40 GB  Movies +30 GB        4s   37%
* desk  iCloud     40 GB local  80 GB cloud       —       —        —  —                   4s     —
* desk  IDrive     ok 2d           6 paths        —       —        —  Desktop             2d     —
o lab   usb-2tb    2.0 TB     400 GB free        —       —        —  —                   3d  done
```

Column heads are the only labels. `1h`/`24h`/`7d` are the change in free space (negative means the disk got fuller). `map` is the initial walk: `37%` while it is running (mapped bytes against space used on the volume) and `done` after. `iCloud` uses `local` and `cloud` because those are not free space. `IDrive` uses the age of the last successful local backup, not a capacity.

## What a pass does

`diskmap tick` (launchd, every minute):

1. Drops to background QoS and `IOPOL_THROTTLE`, and turns off materialising dataless iCloud files, so a scan cannot download a placeholder.
2. When a volume is unmapped or has queued rescans, `tick` (as its last step) starts `diskmap walk` unless `worker.lock` is held. The worker sets its own priority (nice 20, `PRIO_DARWIN_BG`, background QoS, `IOPOL_THROTTLE`), so it stays in the background however it is started. It works in slices of at most 400 directories or 0.4 s, each a committed transaction under `walk.lock`, then lets the lock go and rests to keep a CPU budget: its own and its children's CPU over wall time, averaged over a minute. Rescans after the baseline get 10% of one core, focus or not; the first walk of a volume gets 25% and stops while `focus-guard active` exits 0. Either stops when the newest `machine-log` line (under 3 minutes old) has `thermal` of `serious` or `critical`. `DISKMAP_FORCE=1` lifts the budget. Focus and heat are checked every 20 s. The worker exits when nothing is queued, or after 30 minutes (the next tick starts a fresh one). A directory read that has not returned after 30 s ends the worker: a launchd-started process that opens a privacy-protected folder its binary has no grant for waits in open(2) indefinitely (`Library/Caches/com.apple.TV` and `com.apple.Music`, 18 Nov 2026). The folder goes into `stuck.tsv` and is skipped for a week; its row keeps its last size. A tick that runs over 150 s exits and logs the stage it was in (one sat 24 minutes opening the archive drive); the summary is already written by then. The cursor is the pending table in the volume database, so sleep and restart continue from the last commit.
3. `tick` itself no longer slices the initial walk. If `focus-guard active` exits 0, or the newest `machine-log` line is thermally hot (`thermal` not `nominal`, or `chipC` above 60), the FSEvents pass waits. The summary is still refreshed from what is already mapped.
4. After the first full walk, FSEvents since the saved event id (device UUID from `FSEventsCopyUUIDForDevice`, so the cursor survives sleep and moving a drive) are coalesced, then routed:
   - **Churn trees** (temp, cache and build output: `private/var/folders/*/*`, `private/tmp`, `private/var/tmp`, `Library/Caches`, `Users/*/Library/Caches`, `Users/*/.cursor/projects`, `Users/*/.Trash`, `Users/*/.npm`, and at any depth `.next`, `node_modules`, `_cacache`, `.git/objects`, `DerivedData`, plus `churn_trees` in the config) are one opaque row holding the whole tree's size, with no rows beneath it. The worker measures it a 0.4 s slice at a time with no lock held, alternating with slices of the queue, and rescans it at most hourly. Rescans already queued inside churn trees collapse to one per tree when a worker starts.
   - **Bursty trees**: 256 or more rescans in one replay under one directory at least three levels deep (the deepest such) are held until that tree has been quiet for 10 minutes, or at most 2 hours. Later rescans inside it join the hold. Then each named directory is rescanned once.
   - **`MustScanSubDirs`** subtree rescans are held the same way, so repeated ones run once.
   - Everything else is queued at once. A size change of at least 50 MB is an `events` row. Totals are summed up to the volume root as directories complete; a directory that still has an unscanned descendant is a partial lower bound (`≥ 12 GB (walking)` in `top`).
5. Once a day, after the initial walk has finished, directories of at least 100 MB are written to `history`. A partial map is not stored as that day's history.
6. iCloud Drive, Desktop & Documents, and the Photos library are summarised as local bytes versus dataless bytes. Photos splits originals (`originals/` or `Masters/`) from derivatives. IDrive is read from its local logs under `~/Library/Application Support/IDriveforMac` (last success, selected paths). No login and no network call.
7. Alerts: free space from `machine-log` falling more than 15 GB/h across 30 minutes, or under the host floor. A notification names the top grower and the rate, at most once an hour per cause. The same record is appended to `alerts.jsonl` and copied into `summary.json` for fleet-health.
8. New events and history are shipped to the archive, or spooled if that volume is away.

Volumes in one APFS container (the registry flag `shared_container`) are one physical drive: one capacity, not the sum. A sparsebundle volume is part of its host drive and its size is not added again. Volumes are identified by volume UUID from your drive registry file (default `~/.config/fleet/drives.json`, or override with `drives_json` / `DISKMAP_DRIVES`).

## Config

`~/.config/diskmap/config.json`. Missing file means the defaults below. A file overlays the defaults; you do not have to repeat every host.

```json
{
  "hosts": {
    "Coordinator-Mac": {"id": "desk", "floor_gb": 150},
    "Worker-One": {"id": "lab", "floor_gb": 40},
    "Worker-Two": {"id": "studio", "floor_gb": 25}
  },
  "peers": [],
  "archive_volume": "SecureArchive",
  "archive_rel": "diskmap",
  "drives_json": "",
  "event_min_bytes": 52428800,
  "history_min_bytes": 104857600,
  "hot_days": 30,
  "alert_gb_per_hour": 15,
  "alert_sustain_secs": 1800,
  "alert_cooldown_secs": 3600,
  "churn_trees": []
}
```

`hosts` keys are `LocalHostName` (`scutil --get LocalHostName`). A name that is not listed uses that name as its id and has no free-space floor (the rate alert still applies). `peers` are SSH host aliases, the same ones as `~/.ssh/config`. Empty means this Mac only.

`drives_json` defaults to `~/.config/fleet/drives.json` when that file exists.

`churn_trees` adds volume-relative patterns to the built-in churn trees (`*` is one path component; a leading `**/` matches at any depth), for example `"Users/*/Downloads/torrents"`.

## State

All under `~/.local/state/diskmap/` unless `DISKMAP_STATE` is set.

| File | Contents |
|---|---|
| `<volume-uuid>.db` | SQLite (system `libsqlite3`). `dirs(path, parent, own_bytes, total_bytes, files, newest_mtime, scanned_at)` plus `own_dataless` / `dataless_bytes`. `events(t, path, delta_bytes)` for changes of at least 50 MB. `history(day, path, total_bytes)` for directories of at least 100 MB. |
| `summary.json` | Fleet summary, rewritten every tick. Glancebar and fleet-health read this. |
| `where.json` | Directories of at least 100 MB, for `diskmap where` and `coverage.py`. |
| `alerts.jsonl` | One JSON alert per line. |
| `spool/` | Events and history waiting while the archive volume is unplugged. |
| `worker.lock` | Held by the walk worker for its life: one per Mac. A dead worker releases it; the next tick starts another. |
| `walk.lock` | Held for one database slice at a time (the worker's, or the tick's writes; the tick waits up to 5 s for it). |
| `walk.log` | The worker's log: its priority at start, the queue length each minute, any stuck directory. Kept under 1 MB. |
| `stuck.tsv` | Directories that blocked a worker (unix time, tab, path). Skipped for a week. |
| `peers/<host>.json` | Last summary fetched from a peer. |
| `peers/<host>.where.json` | Last `where` rows fetched from a peer: `generated` (newest row, measured there), `fetched` (copied here), `matches`. Replaced only by a good answer. |

`total_bytes` is allocated local bytes (a directory plus its children). Until the walk finishes, a partial directory's total is a lower bound and `summary.json` carries `map` (`dirs`, `percent`, `done`) on that volume. Dataless bytes are the logical size that is not on disk, including the undownloaded part of a partial iCloud file. The first walk records a baseline and a snapshot of each directory's total when that directory first becomes complete. It does not emit growth events. Later rescans do. Before any `history` row exists, `grew` compares the current map with that snapshot and, when nothing has changed, prints `no history yet (map since HH:MM)`.

Hot rows are kept for 30 days (`hot_days`). Older events and history days are deleted only after the archive manifest lists them.

## Archive

Default destination: `/Volumes/SecureArchive/diskmap/`, laid out as `<host>/<volume-uuid>/events.jsonl`, `history/<day>.jsonl`, and `manifest.json`.

Shipping is a merge on event id and on `(day, path)`. Running it twice does not duplicate rows. The copy is incremental: only ids above `shipped_event_id` are new, and the manifest is rewritten atomically.

The archive path is refused unless `drives.json` marks that `/Volumes/<name>` encrypted. Only volumes you mark encrypted are accepted; unencrypted volumes such as a reobtain mirror or a staging disk are refused. Paths in the archive can be personal, which is why the default targets an encrypted volume. While that volume is away, rows sit in `spool/` and ship on a later tick.

## Summary document

`summary.json` (and `diskmap json`):

```json
{
  "generated": 1700000000,
  "host_id": "desk",
  "local_host_name": "Coordinator-Mac",
  "locations": [
    {
      "kind": "volume",
      "host": "desk",
      "name": "internal",
      "physical_drive": "internal",
      "mounted": true,
      "total_bytes": 494000000000,
      "free_bytes": 213000000000,
      "trend_bytes": {"h1": -400000000, "h24": -2100000000, "d7": null},
      "top_grower": {"path": "Users/me/Library", "delta_bytes": 1200000000},
      "last_seen": 1700000000
    }
  ],
  "drives": [],
  "alerts": []
}
```

`kind` is `volume`, `icloud`, or `idrive`. iCloud `folders[]` has `area` (`drive`, `desktop`, `documents`, `photos`), `name`, `local_bytes`, `dataless_bytes`. Photos names are `originals` and `derivatives`. IDrive `detail` is `ok 2d` or `no success`; `folders[]` are the selected paths and the size `diskmap` has for them. `trend_bytes` is the change in free space, in bytes, from `machine-log`. Bytes are decimal (1e9), matching fleet-health's free-GB figure.

`where.json`:

```json
{
  "generated": 1700000000,
  "min_bytes": 104857600,
  "matches": [
    {
      "host": "desk",
      "location": "internal",
      "path": "Users/me/Documents",
      "home_path": "Documents",
      "bytes": 500000000,
      "as_of": 1700000000,
      "kind": "volume"
    }
  ]
}
```

`coverage.py` reads this file. For each data set it attaches `row["diskmap"]` when a `home_path` (or volume `path`) is that data set or a child of it, and only when the file exists. `COVERAGE.md` is unchanged. Sizes are observations, not a second copy of coverage's yes/no logic.

## Alerts

| Cause | When |
|---|---|
| `rate:<host>:internal:<grower>` | Free space fell more than 15 GB/h and the samples span at least 30 minutes, the newest sample is under 5 minutes old. |
| `floor:<host>:internal` | Free space is under that host's `floor_gb`. |

Notification title `diskmap desk`, body like `Movies +18 GB/h, 149 GB free`. Same cause is silent for an hour. `DISKMAP_NO_NOTIFY=1` records the alert and skips the notification (launchd should not set this).

## Environment

| Variable | Effect |
|---|---|
| `DISKMAP_STATE` | State directory. |
| `DISKMAP_CONFIG` | Config file path. |
| `DISKMAP_HOME` | Home used for iCloud/IDrive paths and the default drives file. |
| `DISKMAP_ANALYTICS` | `machine-log` directory (`<day>.jsonl` with `t` and `diskFreeGB`). |
| `DISKMAP_DRIVES` | Drive registry JSON. |
| `DISKMAP_HOST` | Override `LocalHostName` (tests). |
| `DISKMAP_NO_SSH` | Do not SSH to peers. |
| `DISKMAP_NO_NOTIFY` | Do not post a notification. |
| `DISKMAP_FORCE` | Walk even if focus-guard or thermals say wait. |

## Launchd

Template: `docs/launchd/com.example.diskmap.plist`. `StartInterval` 60, `ProcessType` Background, `LowPriorityIO`. One job per Mac. It does not need root.

See `docs/setup.md` for the install lines.

## Safety

The walker opens directories read-only. It never unlinks, renames, or writes outside `~/.local/state/diskmap` and the encrypted archive. Dataless files are measured, not downloaded. No credentials are read; IDrive's plist is parsed only for path strings, and account folder names are not printed. There is no network except the SSH `diskmap` already uses for fleet peers, the same path fleet-health uses.

### Change-log cursor recovery

A missing or invalid FSEvents cursor replays retained history from event zero. A polling
stream cannot initialize with “since now”: it would discard changes between ticks and
never save its first cursor. Rescan requests are queued before the cursor advances, so a
failed queue write is retried on the next pass. After upgrading an affected installation,
verify that database `meta.fsevent_id` becomes populated and changed directories refresh;
a fresh summary timestamp alone does not prove current directory measurements.

Replayed changes below previously unseen parents rescan from the nearest mapped ancestor.
This discovers the missing directory chain before updating totals, rather than stopping
the worker on a missing-parent error.
