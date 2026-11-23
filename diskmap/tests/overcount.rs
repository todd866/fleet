//! Totals that grew past what is on disk (21 Nov 2026): a hard-linked tree counted once per
//! link, and a deleted tree left in the map because the rescan that would notice it never ran.

use diskmap::*;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

fn scratch(name: &str) -> PathBuf {
    let n = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let path = std::env::temp_dir().join(format!("diskmap-{name}-{n}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).unwrap();
    path
}

fn walk_all(dir: &Path, db: &Path, now: i64) {
    for _ in 0..50 {
        if map_volume(dir, db, now, 100, EVENT_MIN_BYTES, || false).unwrap().complete {
            return;
        }
    }
    panic!("walk did not finish");
}

fn total_of(db: &Path, path: &str) -> Option<i64> {
    let handle = open_readonly(db).unwrap();
    dir(&handle, path).unwrap().map(|row| row.total_bytes)
}

fn allocated(path: &Path) -> i64 {
    (fs::metadata(path).unwrap().blocks() * 512) as i64
}

/// Spotlight's PipelineStorage keeps eight hard links to every journal file, one per
/// sibling folder; `du` counts the file once, the map counted it eight times.
#[test]
fn a_hard_linked_file_is_counted_once_across_its_links() {
    let dir = scratch("hardlink");
    let root = dir.join("vol");
    for sub in ["a", "b", "c", "d"] {
        fs::create_dir_all(root.join(sub)).unwrap();
    }
    fs::write(root.join("a/journal"), vec![3u8; 200_000]).unwrap();
    for sub in ["b", "c", "d"] {
        fs::hard_link(root.join("a/journal"), root.join(sub).join("journal")).unwrap();
    }
    fs::write(root.join("d/own"), vec![4u8; 10_000]).unwrap();
    let once = allocated(&root.join("a/journal"));
    let own = allocated(&root.join("d/own"));
    let db = dir.join("v.db");
    walk_all(&root, &db, 1_700_000_000);
    assert_eq!(total_of(&db, ""), Some(once + own), "one file, four links, counted once");
    // Each link carries an equal share, so any subtree is at most what it holds.
    assert_eq!(total_of(&db, "a"), Some(once / 4));
    assert_eq!(total_of(&db, "d"), Some(once / 4 + own));
    let _ = fs::remove_dir_all(&dir);
}

/// A deleted tree under `.cursor/worktrees` kept its 30 GB because the rescan of its parent
/// sat at the bottom of a last-in-first-out queue that newer rescans never let drain.
#[test]
fn an_old_queued_rescan_is_not_starved_by_a_stream_of_newer_ones() {
    let dir = scratch("starve");
    let root = dir.join("vol");
    fs::create_dir_all(root.join("old/gone")).unwrap();
    fs::create_dir_all(root.join("busy")).unwrap();
    fs::write(root.join("old/gone/x.bin"), vec![1u8; 60_000]).unwrap();
    let db = dir.join("v.db");
    walk_all(&root, &db, 1_700_000_000);
    let gone = total_of(&db, "old/gone").unwrap();
    let before = total_of(&db, "").unwrap();
    assert!(gone >= 60_000);

    fs::remove_dir_all(root.join("old/gone")).unwrap();
    // The parent's rescan is queued first...
    enqueue(&db, "old", false).unwrap();
    // ...then every slice brings as many new rescans as it can scan.
    let mut n = 0;
    for round in 0..20 {
        for _ in 0..8 {
            let name = format!("busy/d{n}");
            fs::create_dir_all(root.join(&name)).unwrap();
            enqueue(&db, &name, false).unwrap();
            n += 1;
        }
        map_volume(&root, &db, 1_700_000_100 + round, 8, EVENT_MIN_BYTES, || false).unwrap();
    }
    assert_eq!(total_of(&db, "old/gone"), None, "the old rescan never ran");
    assert_eq!(total_of(&db, ""), Some(before - gone));
    let _ = fs::remove_dir_all(&dir);
}

/// A dropped-events flag on a high directory used to swallow every precise rescan in the
/// same replay; the precise ones now run at once while the whole-subtree one waits.
#[test]
fn precise_rescans_survive_a_whole_subtree_rescan_in_the_same_replay() {
    let events = [
        RawEvent { path: "".into(), flags: MUST_SCAN_SUBDIRS, id: 1 },
        RawEvent { path: "Users/me/work/gone".into(), flags: 0, id: 2 },
        RawEvent { path: "Users/me/work/file".into(), flags: ITEM_IS_FILE, id: 3 },
    ];
    assert_eq!(
        coalesce(&events),
        vec![
            ScanReq::Subtree("".into()),
            ScanReq::Dir("Users/me/work".into()),
            ScanReq::Dir("Users/me/work/gone".into()),
        ]
    );
}

/// The missing-directory repair is no longer a one-off: a map that went stale after it ran
/// is repaired again a day later without anyone running a command.
#[test]
fn the_missing_directory_repair_runs_again_after_a_day() {
    let dir = scratch("reprune");
    let root = dir.join("vol");
    fs::create_dir_all(root.join("a/first")).unwrap();
    fs::create_dir_all(root.join("a/second")).unwrap();
    fs::write(root.join("a/first/x.bin"), vec![1u8; 20_000]).unwrap();
    fs::write(root.join("a/second/y.bin"), vec![1u8; 20_000]).unwrap();
    let db = dir.join("v.db");
    let t0 = 1_700_000_000;
    walk_all(&root, &db, t0);
    fs::remove_dir_all(root.join("a/first")).unwrap();
    assert_eq!(prune_missing(&root, &db, t0 + 100, EVENT_MIN_BYTES, 1).unwrap(), 1);
    fs::remove_dir_all(root.join("a/second")).unwrap();
    assert_eq!(prune_missing(&root, &db, t0 + 200, EVENT_MIN_BYTES, 1).unwrap(), 0, "not twice in a day");
    assert_eq!(prune_missing(&root, &db, t0 + 100 + 86_400, EVENT_MIN_BYTES, 1).unwrap(), 1);
    assert_eq!(total_of(&db, "a/second"), None);
    assert_eq!(total_of(&db, ""), Some(0));
    let _ = fs::remove_dir_all(&dir);
}
