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
