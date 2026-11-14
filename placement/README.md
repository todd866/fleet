# placement

Decides where each data set lives, and keeps it there. Rules live in `~/.config/fleet/placement.toml`. Sizes and last access come from diskmap (`summary.json`, `where.json`, `access.json`). Placement does not run `du`, does not open SSH, and does not take a heavy-slot: a copy is disk I/O, not a CPU-bound job.

Rust, as a crate next to diskmap and heavy-slot. The risky part is the move (copy, checksum, Trash, and the personal-data guard). That belongs in a typed core with tests, not in a shell script that can `rm` on a bad branch. Orchestration is thin and stays in the same binary. The evaluator is generic. Host names and volume names appear only in site config. `placement init` can write that config; the file it writes is not part of this repo.

Policy: [docs/storage-policy.md](../docs/storage-policy.md). Example: [docs/examples/placement.toml](../docs/examples/placement.toml). What went wrong, and why the program is shaped this way: [docs/lessons/2026-11-storage.md](../docs/lessons/2026-11-storage.md).

Install: `cargo install --path placement` (binary name `placement`). Put it on `PATH` (`~/.local/bin/placement`).

## Commands

| Command | Purpose |
|---|---|
| `placement plan` | Dry run. One row per action: host, keep / move / sync / ignore / clear / refuse, bytes, reason. |
| `placement apply` | This Mac only. Copy, verify size and SHA-256, move the source to Trash, write `name.OFFLOADED.txt` and a ledger row. Exit 75 if focus-guard is active or a volume `.writer` lock is live. |
| `placement status` | One row per host (free, floor, band) and one row per data gap. Writes `~/.local/state/placement/status.json`. |
| `placement init --from-drive-registry` | Write site config from `drives.json` and `datasets.json`. |

```text
placement plan [--config FILE] [--diskmap DIR] [--replicas FILE] [--json]
placement apply [--config FILE] [--host ID] [--home DIR] [--trash DIR] [--ledger FILE] [--volumes DIR]
placement status [--config FILE] [--diskmap DIR] [--out FILE] [--json]
placement init --from-drive-registry [--drives FILE] [--datasets FILE] [--out FILE]
```

| Path | Default |
|---|---|
| Config | `~/.config/fleet/placement.toml` (`PLACEMENT_CONFIG`) |
| Disk map | `~/.local/state/diskmap` (`DISKMAP_STATE`) |
| Status | `~/.local/state/placement/status.json` |
| Verified replicas | `~/.local/state/placement/replicas.json` |
| Ledger | `~/.config/fleet/placement-ledger.md` (`PLACEMENT_LEDGER`) |
| Drives file | `~/.config/fleet/drives.json` |

`--host` selects the id in the policy. Without it, apply reads `~/.config/diskmap/config.json` and uses the id for this Mac's `LocalHostName`. `PLACEMENT_HOST` overrides that. `PLACEMENT_FOCUS=active` defers apply; `PLACEMENT_FORCE=1` skips the focus check. `PLACEMENT_METERED` points at a metered-link marker; otherwise `~/.local/state/METERED_LINK`.

`status` is safe to schedule. `apply` is not. A template for the status job is `docs/launchd/com.example.placement-status.plist`. Run apply when the plan looks right and focus-guard is idle. It will refuse personal data unless `replicas.json` already records two verified encrypted copies with bytes.

## Bundled init preset

`placement init --from-drive-registry` writes a starting policy from your registry files. The evaluator never mentions preset ids; they exist only in the generated site config.

| Host | Role | Floor | Band | Bulk |
|---|---|---|---|---|
| m5 | travelling primary | 150 GB free | 200–400 GB free | working set |
| m2 | home hub | 40 GB free on the internal disk | — | masters on externals |
| m1 | shared laptop | 30 GB free | — | no bulk unless pinned or opened in 30 days |

- Re-obtainable masters go to **OTV Mirror**, the volume that stays on the hub. **Stash/Media** is the alternate, not the master: that disk does not enumerate on the hub's port.
- Personal data targets **Archive_Encrypted2** and **Vault-T**, and only while the drive file says the volume is encrypted. A volume marked unencrypted is dropped from the replica list. Naming a volume does not mark it verified.
- `personal` and `work` data sets become `personal-unique`. An unknown class does too, so a typo cannot make unique files eligible to leave.
- Desktop and Documents are marked `icloud`. Apply will not read them while a metered link is set.
- Games: `Saves` syncs to every host. Any other child is an install, kept on m5 until use is known, and on m1 only if it was opened there in 30 days. The m2 copy is the external master, not an internal mirror. `ROMs` is an archive: master on OTV Mirror only.

## Rollout

On a machine that can afford the compile:

```bash
cargo install --path placement
```

Copy `~/.cargo/bin/placement` to `~/.local/bin/placement` on each Mac. Do not schedule `apply`.

1. On one Mac, with `~/.config/fleet/drives.json` and a data-set file present: `placement init --from-drive-registry --out ~/.config/fleet/placement.toml`. Read it. Copy that file to the other Macs (it is site config, not a secret, and it does not belong in git).
2. diskmap should already be writing `summary.json`. Last access is `access.json` in the same directory, or `newest_mtime` on a `where.json` row.
3. `placement plan` on each Mac (`--host m5`, `m2`, or `m1`). The rows are the change. Nothing moves.
4. `placement status` writes the JSON fleet-health and mac-registry read. Optional: the launchd template, which runs `status` only.
5. When the plan is acceptable and the machine is idle: `placement apply --host <id>`. Personal rows stay put until two encrypted copies are verified. Syncthing then honours the `.stignore` apply wrote into each sync folder. Do not add `(?d)` by hand.
6. Confirmed replicas land in `replicas.json`. The next status stops calling an empty directory a copy.

## Safety

- Never unlinks a source until a verified copy is in Trash. Same-filesystem moves are a rename into Trash.
- Refuses `personal-unique` without two verified encrypted copies that contain bytes. Refuses a clear (Trash with no destination) of that class outright.
- Honours a live per-volume `.writer` lock. The line format is `<job>@<host> <hostname> <pid> <epoch>`, the same one drive-mirror writes. Placement's own live claim does not block itself.
- `placement apply` asks `cowork check "drive:<host>:/Volumes/<name>"` before a move. A holder prints `held by …` and apply exits 75. If `cowork` is not on `PATH`, the `.writer` lock is still the gate. `PLACEMENT_SKIP_COWORK=1` skips the ask.
- Does not call heavy-slot.
- Does not follow symlinks out of the tree.
