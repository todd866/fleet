use diskmap::*;
use std::fs;
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
fn totals_propagate_from_child_to_parent() {
    let dir = scratch("prop");
    let db = open_db(&dir.join("v.db")).unwrap();
    upsert_own(&db, "", "", 10, 0, 1, 1, 1).unwrap();
    upsert_own(&db, "a", "", 100, 0, 2, 1, 1).unwrap();
    upsert_own(&db, "a/b", "a", 50, 5, 3, 1, 1).unwrap();
    propagate(&db, "a/b").unwrap();
    assert_eq!(diskmap::dir(&db, "a/b").unwrap().unwrap().total_bytes, 50);
    assert_eq!(diskmap::dir(&db, "a").unwrap().unwrap().total_bytes, 150);
    assert_eq!(diskmap::dir(&db, "a").unwrap().unwrap().dataless_bytes, 5);
    assert_eq!(diskmap::dir(&db, "").unwrap().unwrap().total_bytes, 160);
    upsert_own(&db, "a/b", "a", 80, 5, 3, 1, 1).unwrap();
    propagate(&db, "a/b").unwrap();
    assert_eq!(diskmap::dir(&db, "a").unwrap().unwrap().total_bytes, 180);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn archive_ship_is_idempotent_and_prune_waits_for_confirm() {
    let dir = scratch("ship");
    let archive = dir.join("vault");
    let db = open_db(&dir.join("v.db")).unwrap();
    insert_event(&db, 1_000, "keep/media", 80_000_000).unwrap();
    insert_event(&db, 2_000, "keep/media/films", 300_000_000).unwrap();
    upsert_history(&db, "2026-01-01", "keep/media/films", 100).unwrap();
    upsert_history(&db, "2026-11-07", "keep/media/films", 400).unwrap();
    let ev = events_after_id(&db, 0).unwrap();
    let hist = history_all(&db).unwrap();
    let first = ship(&archive, "m5", "VOL", &ev, &hist).unwrap();
    let bytes = fs::read(archive.join("m5/VOL/events.jsonl")).unwrap();
    let second = ship(&archive, "m5", "VOL", &ev, &hist).unwrap();
    assert_eq!(first, second);
    assert_eq!(bytes, fs::read(archive.join("m5/VOL/events.jsonl")).unwrap());
    assert_eq!(String::from_utf8(bytes.clone()).unwrap().lines().count(), 2);

    let spool = dir.join("spool");
    spool_merge(&spool, "m5", "VOL", &ev, &hist).unwrap();
    spool_merge(&spool, "m5", "VOL", &ev, &hist).unwrap();
    let (spooled, _) = spool_take(&spool, "m5", "VOL").unwrap();
    assert_eq!(spooled.len(), 2);
    ship(&archive, "m5", "VOL", &spooled, &[]).unwrap();
    assert_eq!(
        fs::read_to_string(archive.join("m5/VOL/events.jsonl")).unwrap().lines().count(),
        2
    );

    let (kept_e, kept_h) = prune_confirmed(&db, 50_000, 0, "2026-10-01", &first.history_days).unwrap();
    assert_eq!((kept_e, kept_h), (0, 0));
    assert_eq!(events_after_id(&db, 0).unwrap().len(), 2);
    let (dropped_e, dropped_h) = prune_confirmed(
        &db,
        50_000,
        first.events_through_id,
        "2026-10-01",
        &first.history_days,
    )
    .unwrap();
    assert_eq!(dropped_e, 2);
    assert_eq!(dropped_h, 1);
    assert!(events_after_id(&db, 0).unwrap().is_empty());
    let left = history_all(&db).unwrap();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].day, "2026-11-07");
    let _ = fs::remove_dir_all(&dir);
}
