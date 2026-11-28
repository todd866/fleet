# idrive-ingest

Off-site backup of a Mac's external drives and home folder to IDrive, through IDrive's headless
Linux client running in a small Lima VM on that Mac. Use it on the Mac that holds your drives.

Why not IDrive's macOS app: it cannot select external volumes from its web console, and on a Mac
with spinning drives it read one disk for hours and stalled every other job on it. The Linux
client backs up any path it is given and runs where it cannot touch the host.

## How it works

- A Lima VM (Apple virtualization, virtiofs) mounts `/Volumes` and your home **read-only**.
- A container in the VM holds the client. Each source in the config is mounted read-only at
  `/backup/<name>`; the client's backup set is the single line `/backup`. Restores come back
  under the same names.
- **One copy of each thing.** Folders that are second copies are blanked inside the container
  with an empty tmpfs: a disk image whose mounted volume is also a source, a mirror of another
  source, caches, the VM's own disk, cloud-synced folders that would download on read.
- A source can wait: `after` a file exists (a migration's done stamp) or while a process runs
  (`wait_for`), so the backup never competes with a migration for a disk.
- `idrive-ingest tick` runs every 30 minutes from launchd: it starts the VM and container if
  they are down, starts a backup when none has succeeded in 24 hours or the sources changed,
  never runs two at once, and writes `~/.local/state/idrive-ingest/status.json`. A source marked
  `priority` that becomes available ends the running backup so the next run includes it.
- The client's login and history live in a Docker volume in the VM; the tick saves a copy to
  `login-backup.tar` (mode 600) once a day.
- The fleet morning check reads `status.json` as `backup:<host>:idrive-ingest`: green while the
  last good run is under a day and a half old, amber during a first seed, red when the tick
  stops, the VM is down, or no run has succeeded for three days.

## Setup

1. `brew install lima docker`. Create the VM (once):
   `limactl create --name=idrive-ingest --tty=false --vm-type=vz --mount-type=virtiofs --cpus=2 --memory=3 --disk=40 --mount-only=/Volumes,$HOME template:docker`
   then `limactl start --tty=false idrive-ingest`. Start it from a terminal or ssh session, not
   from launchd, the first time: macOS may ask for access to removable volumes.
2. Write `~/.config/fleet/idrive-ingest.json` (format at the top of `bin/idrive-ingest`).
   `idrive-ingest plan` prints what would be backed up and what is blanked.
3. `idrive-ingest build` fetches IDrive's installer and builds the image in the VM.
4. Run the tick once (`idrive-ingest tick`) to create the container, then sign in:
   `idrive-ingest login` (email, password, two-factor code; set the backup set to `/backup`).
   You type the password, not an agent. A new device can refuse backups for up to an hour after
   it is first registered ("prepare header" timeouts); wait, do not re-diagnose.
5. Load [launchd/com.example.idrive-ingest.plist](launchd/com.example.idrive-ingest.plist)
   (replace REPLACE_HOME with your home folder) and watch `idrive-ingest status`.

Never start a second container with the same login: two clients on one IDrive device collide.
Never run IDrive's Archive Cleanup on a device that backs up drives you also offload to: it
deletes from the backup whatever is no longer at the source.

## Sizing

The client uploads at the line's full rate (63 Mbit/s here, about 0.65 TB a day). A first seed
of a few terabytes takes about a week; the morning check stays amber until it finishes.

## Laptops

A laptop does not run a backup client. [home-mirror](../bin/home-mirror) pushes its home to a
folder on the hub once a day, when it is on an unmetered link and on mains power, under the hub's
disk slot. A home holds keys and personal files, so on a removable drive the copy lives inside an
encrypted disk image that the hub attaches before each pass (`home-mirror hub-attach`, which
creates it on first use); the image's volume is a source, the image file itself is masked, and
the copy goes off-site with everything else. Every
exclude in `~/.config/fleet/home-mirror.json` carries a reason, cloud placeholders are never read
(reading one makes the cloud download it), and replaced files are kept beside the copy.

Each run is two passes, each with its own space check and its own record.

| Pass | What it copies | Space it needs | Record in `status.json` | Morning row |
|---|---|---|---|---|
| Personal, first | The `class = "personal-unique"` data sets in `~/.config/fleet/placement.toml` whose paths exist under this home, one data set at a time | Per data set: its own measured plan, 10 % headroom and `min_free_gb`, against what is free at that moment; never `first_pass_free_gb` | `personal`, one entry per data set in `personal.sets` | `backup:<laptop>:home-mirror-personal` |
| Full | The whole home | As before: `first_pass_free_gb` for a first seed, or the measured plan with `preflight_space` | the top-level fields | `backup:<laptop>:home-mirror` |

The personal pass exists so that what cannot be replaced reaches the hub while the whole home
does not fit: a full pass that is waiting for space never skips, delays or fails it. Its list is
read from placement.toml on every run, so a newly declared personal data set is covered with no
change to home-mirror. A path that is not on this Mac, or that `home-mirror.json` excludes, is
named in `personal.skipped` with the reason.

The data sets are taken in the order placement.toml declares them, which is the order of
importance. An optional integer `priority` on a data set moves it: lower goes first, a data set
without one counts as 0, and equal numbers keep the file's order. Each set is planned alone and
copied when its plan fits; free space is read again after every copy. A set that does not fit is
recorded as waiting, with the room it needs, and the sets after it still go: one large set never
holds back the small ones. A set's waiting answer is kept for `preflight_cache_h` (6 hours) so
it is not planned again to learn the same thing; that never puts off another set.

Each set copies into the same tree the full pass uses, at the same paths, so the full pass sends
none of it again; the excludes and the placeholder rule apply unchanged. Like the full pass a set
gives up the hub's disk slot after `slice_s`. That ends the personal pass for the run and the
full pass does not start; the next run skips the sets already copied in this cycle and resumes at
that set. A copy stopped because the laptop came into use, lost mains or went onto a metered link
is `deferred`, in either pass, not a failure. A Mac with no placement.toml has no personal pass
and no personal row.

`personal` in `status.json` holds, for each set, `name`, `paths`, `state` (`ok`, `waiting for
space: …`, `slice used, continues`, `deferred: …`, `queued: …`, `skipped: …` or `failed: …`),
`planned_bytes`, `required_free_gb` and `last_success_epoch`; and for the pass `sets_total`,
`sets_copied`, `sets_waiting`, `copied_paths`, and a `last_success_epoch` that is the oldest
set's and is present only when every set has been copied.

The full row is amber after two days without a good copy and red after a week. The personal row
is judged set by set:

| Personal row | When | Shows |
|---|---|---|
| Green | Every set has a good copy less than two days old | the age of the oldest set's copy |
| Amber | A set is waiting for space (however long), has never been copied, was stopped part-way, or its copy is two days old | `9 of 14 sets`, and in the detail each waiting set with the free space it needs and the size it has to copy |
| Red | A set failed and has no copy at all, or a set's last copy is a week old | the set and its state |

Neither row colours the other: the personal row is green with every set copied while the full
row stays amber with "waiting for space". A laptop whose home-mirror predates the personal pass
shows the personal row amber until that pass has run.

To see what the personal pass would do without copying anything, run
`home-mirror run --dry-run --personal` on the laptop. It plans every set (an rsync dry run each,
under the hub's disk slot), counts each set that fits against the ones after it, prints the
result and writes neither `status.json` nor the replica rows.

## Telling placement what is held

`status.json` keeps `held`: for each source, when the newest successful run that included it
finished. After each good pass home-mirror writes `~/.local/state/placement/replicas.d/home-mirror.json`
on the laptop: one row per top-level home folder (encrypted when the copy sits in an encrypted
image, with the folder's excludes listed as `except`), and a second, off-site row once `held`
for its source (`offsite.source` in its config) is later than its first pass. `placement status`
reads every file in `replicas.d/` beside `replicas.json`, so its copy and off-site checks report
what really exists.

A copied personal data set counts the same way before any full pass has finished: one row per
path of a set that has been copied (`personal.copied_paths`), so a personal data set has its
encrypted second copy as soon as its own copy succeeds, and a set still waiting has none. It is
off-site only once the hub's backup has started and finished a run after that set's copy; until
then there is no off-site row. A path is never given two rows for one copy (placement counts rows
as copies): where a full-pass row for the top-level folder exists it is the only row, and its
off-site row then waits for a backup run later than the newest copy under that folder. A copied
path that lies inside a set still to be copied earns no row until that set is copied: placement
would read the smaller copy as a copy of the whole set.

## Coverage: nothing misses the backup unnoticed

Every 6 hours the tick lists the volumes the VM can see. Each must be a source, inside one, or a
key of `not_sources` with a reason; every mask on a source needs a `why`. After each successful
run it counts the files under `/backup` and compares them with the files the client considered.
An unplaced volume, a missing source, a mask without a reason or a gap over 0.5 % turns the
morning row amber. The VM does the listing because it can read every drive: on macOS a
background job may not be allowed to open removable volumes.

## Resuming laptop copies safely

`home-mirror` can check `hub_mount` before `hub_prepare`, avoiding a disk-slot wait when the
existing encrypted destination is already mounted. Set `preflight_space: true` to measure the
remaining rsync transfer instead of requiring the original first-seed capacity after a partial
copy. The receiver rechecks that requirement inside its disk slot; the reserve is never waived.
`bwlimit_kib` can limit transfer pressure while cloud ingest runs. Each host has one run lock.
A failed cloud-placeholder scan aborts the copy. Off-site replica claims require a backup run
started after the latest completed home copy; an older seed is not proof of newer files.
The personal pass always measures each set's transfer, whatever `preflight_space` says, and keeps
a plan log and a copy log per set in `~/.local/state/home-mirror/personal/`, a folder home-mirror
itself leaves out of the copy. placement.toml is TOML,
which needs Python 3.11: under macOS's own older `python3` home-mirror has a Homebrew `python3`
read it, and when none can, the personal record says `failed`, never a copy.

The existing mounted home source can hold separate host folders without restarting the IDrive
container. Files added after its initial scan require a later successful ingest before they
count as off-site. Do not change source mounts or restart the running first seed just to add them.

New home-copy attempts may set `hub_priority_lock` to the hub archive batch's existing lock.
A held lock (or an unavailable check) defers the new attempt; an already-running copy is not
interrupted. With `preflight_space`, an impossible plan is reused only to defer for up to
`preflight_cache_h` (default 6 hours). More destination space triggers a fresh plan; `--force`
requests a fresh plan after substantial source reduction. A cached plan never authorizes a
copy. This avoids repeatedly scanning millions of files while the missing archive copies wait.

## Priority batches before bulk

Optional `priority_batches` entries contain `name`, absolute host `paths`, and `every_h`
(default 24). For example, a batch `laptop-offloads` can select `/Volumes/Archive/laptop`.
`preempt_bulk_for_priority: true` permits a graceful stop of a bulk run when a priority
batch is due. No cloud files are deleted. The same persistent client login/history volumes
are retained; narrow read-only mounts preserve `/backup/<original-source>/<relative-path>`.
The configured backup set stays `/backup`. Sources outside the batch return for the bulk run.

A completed, zero-failure batch receives its own path-bound receipt in `cycle.json` and
`status.json`; it does not qualify as a complete backup of all drives. A freshly verified
offload sends `idrive-ingest request PATH VERIFIED_AT`. This makes its priority batch due
again if the previous run began before verification, even if that run finished recently.
The offload queue verifies the source and destination again after qualifying cloud coverage.
A later bulk run does not erase a valid narrow receipt. Changed masks, paths, missing sources,
failed files, and stale observations still prevent reclamation.

Verified requests run oldest first. `request_order` (a list of host path prefixes) puts matching
requests ahead of that, for example a folder that is the only copy of a machine before folders still
held elsewhere. A priority run that ends without a receipt keeps its place and is retried; after three
such runs the scope is set aside for 24 hours with a problem naming the failed files, so it cannot
block the queue. A new verification request brings it back at once.

IDrive reports a run with some failed files as `Success*`. It counts as a success only when the log
names every failed file and each matches a regular expression in `accept_failed` (for example
`/Library/Containers/com\.apple\.news/Data/Library/Caches/` for cache names too long to store, or
`/Icon\r$` for Finder's custom-icon files). The receipt then lists those files as
`accepted_failures`. With no `accept_failed`, a `Success*` run is never a success.

One lock serializes ticks; phase state is persisted before changing mounts so restart resumes
the same plan. Priority runs complete before bulk resumes. Existing uploaded paths remain in
the account, and completed files are available to the client's incremental backup logic.
Archive Cleanup is never part of this cycle.
