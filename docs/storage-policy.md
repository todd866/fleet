# Storage policy

Where a data set lives is a decision, made once, in config. `placement` reads that decision and diskmap's measurements. It does not discover policy by scanning, and it does not run `du`.

Site config is `~/.config/fleet/placement.toml`. A commented skeleton is [examples/placement.toml](examples/placement.toml). `placement init --from-drive-registry` can write a first draft from a drive registry and a data-set list. The draft is site config: do not commit it.

## Classes

| Class | Copies | On a laptop |
|---|---|---|
| `personal-unique` | Two verified encrypted copies, and one verified off-site copy | Stays until those two encrypted copies exist. A folder that is empty, or a volume that is only named in config, is not a copy |
| `re-obtainable` | One copy on a home-hub external volume | A working set: what that host uses. The master is the external, not a second full mirror |
| `scratch` | None | Local only. First thing cleared when free space is under the band |
| `synced-small` | Every host | Configs, saves, notes. Never cleared to make room |

## Hosts

Each host has a role, a free-space floor, an optional target band, and a bulk rule.

| Role | Bulk | What it keeps |
|---|---|---|
| `travelling-primary` | `working-set` | Items listed for it, until use is known. A known-stale item leaves when free space is under the band. An item with no access record leaves only when free space is through the floor |
| `home-hub` | `masters-on-external` | Internal disk holds small sync and the live system. Bulk masters are on named volumes |
| `shared-laptop` | `no-bulk` | Nothing large unless a pin names it, or it was opened inside the played-within window |

The floor is the hard line (`below floor`). The band is the target (`below band`, `in band`, `above band`). Above the band is spare room, not a fault. A missing diskmap summary is `no reading`, not a guess.

## Placement

A data set, and each child of a sync folder, has:

- which hosts keep a local copy (`keep`, or `*` for all of them)
- pins that override the bulk rule
- where the master lives (`master_host`, `master_volume`)
- how it moves between hosts (`none`, `syncthing`, `copy`)

Who uses it where comes from two signals. Diskmap records last access (FSEvents / `newest_mtime`, or `access.json`). A pin is the explicit signal. No access record means "not known yet", which is not the same as "unused".

## Sync folders

`placement` writes a Syncthing `.stignore` per host. The first matching pattern wins, so includes are listed before the final `*`. Saves and other synced-small children are included everywhere. An install is included only on a host that keeps it. An archive is included nowhere in the share: it lives on the hub volume.

The file must not contain `(?d)`. An ignore line stops a future transfer. It must not tell Syncthing to delete the local files. Removal is `placement apply`, which keeps a Trash copy.

## Commands

| Command | Effect |
|---|---|
| `placement plan` | Dry run. One row per host and path: keep, move, sync, ignore, clear, or refuse, with bytes and a short reason. Changes nothing |
| `placement apply` | On this Mac only. Copy to the master, verify size and checksum, move the source to Trash, write a stub and a ledger row. No network and no SSH |
| `placement status` | Floors, bands, and data-set gaps. Writes `~/.local/state/placement/status.json` for fleet-health and mac-registry |
| `placement init --from-drive-registry` | Write site config from a drives file and a data-set file |

`apply` exits 75 when it defers: focus-guard reports the owner is active, or a volume `.writer` lock is live (claim younger than 12 hours, and the pid still alive when the claim is from this machine). A stale lock does not block. `PLACEMENT_FORCE=1` skips the focus check. I/O is not a heavy-slot job; apply does not take a slot.

A metered link (`~/.local/state/METERED_LINK`, or `PLACEMENT_METERED`) skips Desktop, Documents, and any data set marked `icloud`. Other local copies still run. Reading an iCloud placeholder on a metered link would download it.

## Apply

1. Copy the tree to a staging directory on the destination volume. Symlinks are not followed.
2. Every source file must exist there at the same size and SHA-256. Extra files already on the destination are allowed. A destination that already matches is reused. A destination that differs is left alone and the source stays.
3. Move the source into Trash (`rename`, or, across devices, copy to Trash, verify that copy, and only then remove the source).
4. Write `name.OFFLOADED.txt` beside the old path, and append a row to the placement ledger (`PLACEMENT_LEDGER`, default `~/.config/fleet/placement-ledger.md`): date, `trash`, source, destination, size, why.
5. Record the verified replica. The record is what a later plan trusts. The folder name is not.

Scratch is the exception to the copy: it has no master, so apply moves it straight to Trash. Personal-unique is never cleared that way, and a move of it is refused unless two verified encrypted copies with bytes already exist. The check uses the observation, not the action's own claim.

Verification that fails leaves the source in place. The incomplete staging copy is moved to Trash rather than unlinked.

## What status reports

One row per host: mark, free, floor, band, state. Then one row per data gap: too few encrypted copies, no off-site copy, a master volume with none of the bytes that exist elsewhere, or bulk sitting on a host that should not have it. Column heads are the only labels. fleet-health copies `below-floor`, `below-band`, and each gap into its issue list. mac-registry counts them in Needs attention. Both read the JSON; they do not re-measure the disk.
