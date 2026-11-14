use diskmap::*;
use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

fn scratch(name: &str) -> PathBuf {
    let n = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let path = std::env::temp_dir().join(format!("diskmap-{name}-{n}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).unwrap();
    path
}

#[test]
fn walker_reads_logical_size() {
    let dir = scratch("walk");
    fs::write(dir.join("hello.txt"), vec![7u8; 1000]).unwrap();
    let entries = read_dir_entries(&dir).unwrap();
    let file = entries.iter().find(|e| e.name == "hello.txt").unwrap();
    assert!(!file.is_dir);
    assert_eq!(file.logical, 1000);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn resume_walks_one_directory_at_a_time() {
    let dir = scratch("resume");
    fs::create_dir_all(dir.join("a/b")).unwrap();
    fs::write(dir.join("a/b/c.txt"), b"hi").unwrap();
    let db = dir.join("v.db");
    let now = 1_700_000_000;
    let first = map_volume(&dir, &db, now, 1, EVENT_MIN_BYTES, || false).unwrap();
    assert_eq!(first.scanned, 1);
    assert!(!first.complete);
    let mut guard = 0;
    loop {
        guard += 1;
        assert!(guard < 20);
        let stats = map_volume(&dir, &db, now, 1, EVENT_MIN_BYTES, || false).unwrap();
        if stats.complete {
            break;
        }
    }
    let rows = growth_since(&db, 0).unwrap();
    assert!(rows.is_empty(), "the baseline walk is not growth");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn yield_pauses_between_directories() {
    let dir = scratch("yield");
    fs::create_dir_all(dir.join("a")).unwrap();
    let db = dir.join("v.db");
    let mut calls = 0;
    let stats = map_volume(&dir, &db, 10, 10, EVENT_MIN_BYTES, || {
        calls += 1;
        calls > 1
    })
    .unwrap();
    assert!(stats.yielded);
    assert!(stats.scanned >= 1);
    assert!(!stats.complete);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn grew_finds_a_new_300mb_file() {
    let dir = scratch("grew");
    fs::create_dir_all(dir.join("keep/noise")).unwrap();
    fs::write(dir.join("keep/noise/a.txt"), vec![1u8; 4096]).unwrap();
    fs::create_dir_all(dir.join("keep/media/films")).unwrap();
    let db = dir.join("v.db");
    let t0 = 1_700_000_000;
    map_volume(&dir, &db, t0, usize::MAX, EVENT_MIN_BYTES, || false).unwrap();
    let big = dir.join("keep/media/films/big.bin");
    let mut file = File::create(&big).unwrap();
    let mut buf = vec![0u8; 1024 * 1024];
    let mut state = 0x1234_5678u64;
    for _ in 0..300 {
        for chunk in buf.chunks_mut(8) {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1);
            chunk.copy_from_slice(&state.to_ne_bytes());
        }
        file.write_all(&buf).unwrap();
    }
    file.sync_all().unwrap();
    let created = RawEvent {
        path: "keep/media/films/big.bin".into(),
        flags: ITEM_IS_FILE,
        id: 9,
    };
    let scans = coalesce(&[created]);
    assert_eq!(scans, vec![ScanReq::Dir("keep/media/films".into())]);
    apply_scans(&db, &scans).unwrap();
    map_volume(&dir, &db, t0 + 10, usize::MAX, EVENT_MIN_BYTES, || false).unwrap();
    let growth = growth_since(&db, t0).unwrap();
    let grew = collapse_grew(&growth).expect("growth");
    assert_eq!(grew.path, "keep/media/films");
    assert!(
        grew.delta >= 280 * 1024 * 1024,
        "delta {} did not explain the 300 MB file",
        grew.delta
    );
    let _ = fs::remove_dir_all(&dir);
}

fn walk_all(dir: &std::path::Path, db: &std::path::Path, now: i64) {
    for _ in 0..50 {
        if map_volume(dir, db, now, 100, EVENT_MIN_BYTES, || false).unwrap().complete {
            return;
        }
    }
    panic!("walk did not finish");
}

fn total_of(db: &std::path::Path, path: &str) -> Option<i64> {
    let handle = open_readonly(db).unwrap();
    dir(&handle, path).unwrap().map(|row| row.total_bytes)
}

#[test]
fn a_deleted_directory_leaves_the_map_and_its_ancestors_totals() {
    let dir = scratch("deleted");
    let root = dir.join("vol");
    fs::create_dir_all(root.join("keep")).unwrap();
    fs::create_dir_all(root.join("gone/deep/deeper")).unwrap();
    fs::write(root.join("keep/a.bin"), vec![1u8; 4_000]).unwrap();
    fs::write(root.join("gone/b.bin"), vec![1u8; 30_000]).unwrap();
    fs::write(root.join("gone/deep/deeper/c.bin"), vec![1u8; 50_000]).unwrap();
    // a sibling whose name starts with the deleted one must survive
    fs::create_dir_all(root.join("gone2")).unwrap();
    fs::write(root.join("gone2/d.bin"), vec![1u8; 700]).unwrap();
    let db = dir.join("v.db");
    walk_all(&root, &db, 1_700_000_000);
    // sizes are what the file system allocated, so compare totals with each other
    let root_before = total_of(&db, "").unwrap();
    let gone_before = total_of(&db, "gone").unwrap();
    let keep = total_of(&db, "keep").unwrap();
    let gone2 = total_of(&db, "gone2").unwrap();
    assert!(gone_before >= 80_000 && root_before == gone_before + keep + gone2);

    fs::remove_dir_all(root.join("gone")).unwrap();
    // the parent is what the file system reports as changed
    enqueue(&db, "", false).unwrap();
    walk_all(&root, &db, 1_700_000_100);
    assert_eq!(total_of(&db, "gone"), None);
    assert_eq!(total_of(&db, "gone/deep/deeper"), None);
    assert_eq!(total_of(&db, "gone2"), Some(gone2));
    assert_eq!(total_of(&db, "keep"), Some(keep));
    assert_eq!(total_of(&db, ""), Some(root_before - gone_before));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn prune_repairs_a_map_that_still_holds_deleted_directories_once() {
    let dir = scratch("prune");
    let root = dir.join("vol");
    fs::create_dir_all(root.join("a/old/inner")).unwrap();
    fs::create_dir_all(root.join("a/live")).unwrap();
    fs::write(root.join("a/old/inner/x.bin"), vec![1u8; 60_000]).unwrap();
    fs::write(root.join("a/live/y.bin"), vec![1u8; 5_000]).unwrap();
    let db = dir.join("v.db");
    walk_all(&root, &db, 1_700_000_000);
    let a_before = total_of(&db, "a").unwrap();
    let old_before = total_of(&db, "a/old").unwrap();
    let live = total_of(&db, "a/live").unwrap();
    assert!(old_before >= 60_000 && a_before == old_before + live);
    // deleted with no rescan of the parent: the state older maps are in
    fs::remove_dir_all(root.join("a/old")).unwrap();
    assert_eq!(prune_missing(&root, &db, 1_700_000_200, EVENT_MIN_BYTES, 1).unwrap(), 1);
    assert_eq!(total_of(&db, "a/old"), None);
    assert_eq!(total_of(&db, "a/old/inner"), None);
    assert_eq!(total_of(&db, "a"), Some(live));
    assert_eq!(total_of(&db, ""), Some(live));
    // the repair runs once per map
    fs::remove_dir_all(root.join("a/live")).unwrap();
    assert_eq!(prune_missing(&root, &db, 1_700_000_300, EVENT_MIN_BYTES, 1).unwrap(), 0);
    let _ = fs::remove_dir_all(&dir);
}
