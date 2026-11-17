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
(reading one makes the cloud download it), and replaced files are kept beside the copy. The
morning check shows `backup:<laptop>:home-mirror`: amber after two days away, red after a week.

## Telling placement what is held

`status.json` keeps `held`: for each source, when the newest successful run that included it
finished. After each good pass home-mirror writes `~/.local/state/placement/replicas.d/home-mirror.json`
on the laptop: one row per top-level home folder (encrypted when the copy sits in an encrypted
image, with the folder's excludes listed as `except`), and a second, off-site row once `held`
for its source (`offsite.source` in its config) is later than its first pass. `placement status`
reads every file in `replicas.d/` beside `replicas.json`, so its copy and off-site checks report
what really exists.

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

One lock serializes ticks; phase state is persisted before changing mounts so restart resumes
the same plan. Priority runs complete before bulk resumes. Existing uploaded paths remain in
the account, and completed files are available to the client's incremental backup logic.
Archive Cleanup is never part of this cycle.
