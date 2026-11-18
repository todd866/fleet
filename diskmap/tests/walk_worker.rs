use diskmap::*;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

fn scratch(name: &str) -> PathBuf {
    let n = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let path = std::env::temp_dir().join(format!("diskmap-{name}-{n}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).unwrap();
    path
}

fn tree(dir: &Path) -> (PathBuf, PathBuf) {
    let tree = dir.join("tree");
    let state = dir.join("state");
    fs::create_dir_all(&tree).unwrap();
    fs::create_dir_all(&state).unwrap();
    (tree, state.join("vol.db"))
}

#[test]
fn walk_resumes_from_the_committed_checkpoint() {
    let scratch_dir = scratch("resume-batch");
    let (tree, db) = tree(&scratch_dir);
    fs::write(tree.join("root.bin"), vec![1u8; 4096]).unwrap();
    let mut deep = tree.clone();
    for name in ["a", "b", "c", "d"] {
        deep.push(name);
        fs::create_dir(&deep).unwrap();
    }
    fs::write(deep.join("f.txt"), vec![2u8; 8192]).unwrap();
    let now = 1_700_000_000;
    let first = walk_one_batch(&tree, &db, now, EVENT_MIN_BYTES, 100, |n| n >= 1).unwrap();
    assert!(first.yielded);
    assert_eq!(first.scanned, 1);
    assert!(!first.complete);
    assert!(queue_len(&db).unwrap() > 0);
    assert!(scanned_dirs(&db).unwrap() >= 1);

    let second = walk_one_batch(&tree, &db, now, EVENT_MIN_BYTES, 100, |_| false).unwrap();
    assert!(second.complete);
    assert_eq!(queue_len(&db).unwrap(), 0);
    let conn = open_db(&db).unwrap();
    let before = dir(&conn, "").unwrap().unwrap().total_bytes;
    propagate(&conn, "a/b/c/d").unwrap();
    assert_eq!(dir(&conn, "").unwrap().unwrap().total_bytes, before);
    assert!(before > 0);
    let _ = fs::remove_dir_all(&scratch_dir);
}

#[test]
fn committed_batch_is_visible_to_another_connection() {
    let scratch_dir = scratch("visible");
    let (tree, db) = tree(&scratch_dir);
    fs::write(tree.join("root.bin"), vec![3u8; 4096]).unwrap();
    let mut deep = tree.clone();
    for name in ["a", "b", "c", "d"] {
        deep.push(name);
        fs::create_dir(&deep).unwrap();
    }
    let now = 1_700_000_100;
    let first = walk_one_batch(&tree, &db, now, EVENT_MIN_BYTES, 2, |_| false).unwrap();
    assert!(!first.complete, "a batch of 2 must leave the rest of the tree queued");
    assert_eq!(first.scanned, 2);
    let db_path = db.clone();
    let seen = std::thread::spawn(move || {
        let n = scanned_dirs(&db_path).unwrap();
        let q = queue_len(&db_path).unwrap();
        let conn = open_db(&db_path).unwrap();
        let total = dir(&conn, "").unwrap().unwrap().total_bytes;
        (n, q, total)
    })
    .join()
    .unwrap();
    assert!(seen.0 >= 2);
    assert!(seen.1 > 0);
    assert!(seen.2 > 0);
    let rest = walk_one_batch(&tree, &db, now, EVENT_MIN_BYTES, WALK_BATCH, |_| false).unwrap();
    assert!(rest.complete);
    let _ = fs::remove_dir_all(&scratch_dir);
}

#[test]
fn partial_parent_keeps_a_lower_bound_until_children_finish() {
    let scratch_dir = scratch("partial");
    let (tree, db) = tree(&scratch_dir);
    fs::write(tree.join("now.bin"), vec![4u8; 4096]).unwrap();
    fs::create_dir(tree.join("later")).unwrap();
    fs::write(tree.join("later/x.bin"), vec![5u8; 16384]).unwrap();
    let now = 1_700_000_200;
    map_volume(&tree, &db, now, 1, EVENT_MIN_BYTES, || false).unwrap();
    let conn = open_db(&db).unwrap();
    let root = dir(&conn, "").unwrap().unwrap();
    assert!(root.partial);
    assert!(root.total_bytes > 0);
    assert!(root.snap_bytes.is_none());
    let later = dir(&conn, "later").unwrap().unwrap();
    assert_eq!(later.scanned_at, 0);
    let lower = root.total_bytes;
    drop(conn);

    map_volume(&tree, &db, now, 100, EVENT_MIN_BYTES, || false).unwrap();
    let conn = open_db(&db).unwrap();
    let root = dir(&conn, "").unwrap().unwrap();
    assert!(!root.partial);
    assert!(root.total_bytes > lower);
    assert_eq!(root.snap_bytes, Some(root.total_bytes));
    let _ = fs::remove_dir_all(&scratch_dir);
}

#[test]
fn top_lists_partial_children_by_descending_total() {
    assert_eq!(format_bytes_cell(1_000_000_000, true), "≥ 1.0 GB (walking)");
    assert_eq!(format_bytes_cell(1_000_000_000, false), "1.0 GB");
    let text = format_top(
        &[TopMap {
            volume: "internal".into(),
            dirs: 1438,
            percent: 37,
            done: false,
        }],
        &[
            TopEntry {
                volume: "internal".into(),
                path: "docs".into(),
                bytes: 2_000_000_000,
                partial: false,
            },
            TopEntry {
                volume: "internal".into(),
                path: "movies".into(),
                bytes: 9_000_000_000,
                partial: true,
            },
        ],
    );
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 3);
    let width = lines[0].chars().count();
    assert!(lines.iter().all(|line| line.chars().count() == width));
    assert!(lines[0].contains("map 1438 dirs"));
    assert!(lines[0].ends_with("37%"));
    assert!(lines[1].contains("movies"));
    assert!(lines[1].contains("≥ 9.0 GB (walking)"));
    assert!(lines[2].contains("docs"));
    assert!(!lines[2].contains("walking"));
    let done = format_top(
        &[TopMap {
            volume: "internal".into(),
            dirs: 5,
            percent: 100,
            done: true,
        }],
        &[],
    );
    assert!(done.contains("done"));
    assert!(!done.contains('%'));
}

#[test]
fn progress_percent_compares_mapped_bytes_with_used_bytes() {
    assert_eq!(WALK_BATCH, 10_000);
    assert_eq!(map_percent(370, 1_000, false), 37);
    assert_eq!(map_percent(1_000, 1_000, false), 99);
    assert_eq!(map_percent(10, 0, false), 0);
    assert_eq!(map_percent(10, 1, true), 100);

    let scratch_dir = scratch("progress");
    let (tree, db) = tree(&scratch_dir);
    fs::write(tree.join("now.bin"), vec![6u8; 8192]).unwrap();
    fs::create_dir(tree.join("later")).unwrap();
    let now = 1_700_000_300;
    map_volume(&tree, &db, now, 1, EVENT_MIN_BYTES, || false).unwrap();
    remember_volume(&db, "U", "internal", &tree.to_string_lossy(), 1_000_000, 0, now).unwrap();
    let conn = open_db(&db).unwrap();
    let progress = map_progress(&conn).unwrap();
    assert!(!progress.done);
    assert!(progress.dirs >= 1);
    assert_eq!(
        progress.percent,
        map_percent(progress.mapped_bytes, 1_000_000, false)
    );
    drop(conn);
    map_volume(&tree, &db, now, 100, EVENT_MIN_BYTES, || false).unwrap();
    let progress = map_progress(&open_db(&db).unwrap()).unwrap();
    assert!(progress.done);
    assert_eq!(progress.percent, 100);
    let _ = fs::remove_dir_all(&scratch_dir);
}

#[test]
fn grew_without_history_names_the_map_time() {
    assert_eq!(
        grew_status_line(false, false, Some("21:00")).as_deref(),
        Some("no history yet (map since 21:00)")
    );
    assert_eq!(grew_status_line(true, false, Some("21:00")), None);
    assert_eq!(grew_status_line(false, true, Some("21:00")), None);
    assert_eq!(grew_status_line(false, false, None).as_deref(), Some("no history yet"));

    let scratch_dir = scratch("grew-map");
    let (tree, db) = tree(&scratch_dir);
    fs::create_dir(tree.join("later")).unwrap();
    fs::write(tree.join("later/a.txt"), vec![7u8; 1024]).unwrap();
    let now = 1_700_000_400;
    map_volume(&tree, &db, now, usize::MAX, i64::MAX, || false).unwrap();
    assert!(history_all(&open_db(&db).unwrap()).unwrap().is_empty());
    let roots = Roots {
        home: scratch_dir.clone(),
        state: scratch_dir.join("state"),
        config: scratch_dir.join("config.json"),
        analytics: scratch_dir.join("analytics"),
    };
    let before = grew_report(&roots, 0, "24h");
    assert!(before.hits.is_empty());
    let banner = before.banner.expect("banner");
    assert!(banner.starts_with("no history yet (map since "));
    assert!(banner.ends_with(')'));

    fs::write(tree.join("later/more.bin"), vec![8u8; 128 * 1024]).unwrap();
    enqueue(&db, "later", false).unwrap();
    map_volume(&tree, &db, now + 10, usize::MAX, i64::MAX, || false).unwrap();
    assert!(growth_since(&db, 0).unwrap().is_empty());
    let growth = growth_against_snapshot(&db).unwrap();
    assert!(growth.iter().any(|row| row.delta > 0 && row.path == "later"));
    let after = grew_report(&roots, 0, "24h");
    assert!(after.banner.is_none());
    assert!(after.hits.iter().any(|hit| hit.delta > 0 && hit.since_label == "map"));
    let _ = fs::remove_dir_all(&scratch_dir);
}

#[test]
fn walk_pauses_for_focus_or_serious_heat() {
    let now = 1_700_000_000i64;
    assert!(thermal_blocks_walk(r#"{"t":1700000000,"thermal":"Serious"}"#, now));
    assert!(thermal_blocks_walk(r#"{"t":1700000000,"thermal":"critical"}"#, now));
    assert!(!thermal_blocks_walk(r#"{"t":1700000000,"thermal":"nominal","chipC":95}"#, now));
    assert!(!thermal_blocks_walk(r#"{"t":1700000000,"thermal":"fair"}"#, now));
    assert!(!thermal_blocks_walk(r#"{"t":1600000000,"thermal":"critical"}"#, now));
    assert!(pause_walk(true, false, false));
    assert!(pause_walk(false, true, false));
    assert!(!pause_walk(true, true, true));
    assert!(!pause_walk(false, false, false));
}

// 14 Nov 2026: the map was five days stale because rescans drained only while
// nobody was at the Mac. 18 Nov 2026: the opposite fault, a drain at full
// speed for 27 hours. Rescans now always run on a 10% budget, focus or not;
// only serious heat stops them, and only DISKMAP_FORCE lifts the budget.
#[test]
fn rescan_queue_keeps_a_budget_and_stops_only_for_heat() {
    assert_eq!(drain_pace(false, false), DrainPace::Budget(10));
    assert_eq!(drain_pace(true, false), DrainPace::Paused);
    assert_eq!(drain_pace(true, true), DrainPace::Full);
    assert_eq!(drain_pace(false, true), DrainPace::Full);
    assert_eq!(baseline_pace(false, false, false), DrainPace::Budget(BASELINE_BUDGET_PCT));
    assert_eq!(baseline_pace(true, false, false), DrainPace::Paused);
    assert_eq!(baseline_pace(false, true, false), DrainPace::Paused);
    assert_eq!(baseline_pace(true, true, true), DrainPace::Full);
}

#[test]
fn a_worker_runs_while_any_volume_is_unwalked_or_has_queued_rescans() {
    assert!(!needs_worker(&[(true, 0), (true, 0)]));
    assert!(needs_worker(&[(true, 0), (false, 0)]));
    assert!(needs_worker(&[(true, 351)]));
    assert!(!needs_worker(&[]));
}

#[test]
fn queued_rescans_drain_in_worker_batches_after_the_baseline() {
    let scratch_dir = scratch("drain");
    let (root, db_path) = tree(&scratch_dir);
    fs::create_dir_all(root.join("a")).unwrap();
    let now = 1_700_000_000i64;
    // Finish the baseline.
    loop {
        let stats = walk_one_batch(&root, &db_path, now, 0, 10_000, |_| false).unwrap();
        if stats.complete {
            break;
        }
    }
    // A new folder appears and its parent is queued for a deep rescan.
    fs::create_dir_all(root.join("a/new.noindex/deep")).unwrap();
    fs::write(root.join("a/new.noindex/deep/blob"), vec![0u8; 4096]).unwrap();
    enqueue(&db_path, "a", true).unwrap();
    assert!(queue_len(&db_path).unwrap() > 0);
    loop {
        let stats = walk_one_batch(&root, &db_path, now + 60, 0, 1, |_| false).unwrap();
        if queue_len(&db_path).unwrap() == 0 {
            break;
        }
        assert!(stats.scanned <= 1);
    }
    let db = open_db(&db_path).unwrap();
    let row = dir(&db, "a/new.noindex").unwrap().expect("new .noindex folder is mapped");
    assert!(row.total_bytes >= 4096);
    let _ = fs::remove_dir_all(&scratch_dir);
}

#[test]
fn summary_without_a_map_field_still_loads() {
    let text = r#"{
        "generated": 1,
        "host_id": "h1",
        "local_host_name": "MacBook-Air",
        "locations": [{
            "kind": "volume",
            "id": "h1:internal",
            "host": "h1",
            "name": "internal",
            "volume_uuid": null,
            "physical_drive": "internal",
            "mount": null,
            "mounted": true,
            "encrypted": true,
            "total_bytes": 10,
            "free_bytes": 4,
            "local_bytes": 6,
            "dataless_bytes": 0,
            "trend_bytes": {"h1": null, "h24": null, "d7": null},
            "top_grower": null,
            "last_seen": 1,
            "detail": "",
            "folders": []
        }],
        "drives": [],
        "alerts": []
    }"#;
    let summary: Summary = serde_json::from_str(text).unwrap();
    assert!(summary.locations[0].map.is_none());
    let with = Summary {
        locations: vec![Location {
            map: Some(MapStatus { dirs: 12, percent: 37, done: false }),
            ..summary.locations[0].clone()
        }],
        ..summary
    };
    let json = serde_json::to_string(&with).unwrap();
    assert!(json.contains("\"percent\":37"));
    assert!(json.contains("\"dirs\":12"));
}
