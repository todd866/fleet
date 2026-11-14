# Storage, November 2026

A travelling primary lost hundreds of gigabytes of free space overnight, and nobody was told which directory did it. A small shared laptop reached a few gigabytes free because one sync folder mirrored every install onto every machine. A second copy of personal data turned out to be the right folders with nothing in them, and it had been counted anyway. Each session invented its own scan and its own move. The scans walked the disk again instead of reading a map.

Those are five different failures, and they need five different answers.

**Silent fill.** Free space is a reading, like temperature. One map records it, compares it with a floor and a rate, and names the directory that grew. Alerts come from that reading. A job that needs a size reads the map. It does not start another walk.

**Sync everything.** A shared folder is not a decision about where bytes live. The decision is per child: small state that every host needs, a working install on the hosts that use it, an archive on the hub's external and nowhere else. The ignore file is generated from that decision. Ignore means "do not transfer". It does not mean "delete".

**The empty second copy.** A replica counts when its bytes have been checked, not when its directory exists and not when a config file names the volume. Personal data stays where it is until two encrypted copies have been verified. Off-site is a third requirement, reported on its own, and it is not satisfied by a sync service that only holds placeholders.

**A tool per session.** Placement, the disk map, and the registry are three programs with three jobs. Sessions do not grow a fourth scanner or a one-off mover. The registry says where a data set is supposed to live. The map says what is there. Placement says what would change, and is the only one that changes it.

**An ad-hoc scan, and a delete with no way back.** Moves go copy, then verify size and checksum, then Trash, with a stub and a ledger row. Trash is recoverable. Unlink is not a step. Two jobs do not write the same volume: a live `.writer` lock defers the later one. None of this is a heavy-slot job. The slot is for heat. A copy is I/O, and it waits when the owner is at the machine and when the link is metered and the files are only in the cloud.
