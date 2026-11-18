//! 18 Nov 2026: the walk worker ran 27 h at ~80% CPU and its queue grew
//! faster than it drained (temp folders, editor transcripts, an image download
//! committing thousands of folders). These pin the CPU budget, the churn-tree
//! exclusions and the coalescing of bursty trees.

use diskmap::*;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The skip list is process-wide; tests that set it take turns.
static SKIP_LIST: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn scratch(name: &str) -> PathBuf {
    let n = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let path = std::env::temp_dir().join(format!("diskmap-{name}-{n}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).unwrap();
    path
}

fn baseline(root: &Path, db: &Path, now: i64) {
    loop {
        if walk_one_batch(root, db, now, 0, 10_000, |_| false).unwrap().complete {
            break;
        }
    }
}

fn dirs(n: usize, prefix: &str) -> Vec<ScanReq> {
    (0..n).map(|i| ScanReq::Dir(format!("{prefix}/{i}"))).collect()
}

#[test]
fn budget_sleep_pays_for_work_at_the_budget_rate() {
    let s = Duration::from_secs;
    assert_eq!(budget_sleep(s(1), s(2), 10), s(8));
    assert_eq!(budget_sleep(s(1), s(10), 10), Duration::ZERO);
    assert_eq!(budget_sleep(s(1), s(20), 10), Duration::ZERO);
    assert_eq!(budget_sleep(s(1), s(1), 25), s(3));
    assert_eq!(budget_sleep(s(5), s(0), 0), Duration::ZERO);
    assert_eq!(budget_sleep(s(5), s(0), 100), Duration::ZERO);
}

/// A worker that does 50 ms of CPU per slice and sleeps as told averages the
/// budget, never more, however long it runs: no hot loop.
#[test]
fn a_paced_loop_averages_ten_percent_of_one_core() {
    let slice = Duration::from_millis(50);
    let (mut cpu, mut wall) = (Duration::ZERO, Duration::ZERO);
    for _ in 0..10_000 {
        cpu += slice;
        wall += slice;
        wall += budget_sleep(cpu, wall, DRAIN_BUDGET_PCT);
        let used = cpu.as_secs_f64() / wall.as_secs_f64();
        assert!(used <= 0.100_001, "averaged {used}");
    }
    let used = cpu.as_secs_f64() / wall.as_secs_f64();
    assert!(used > 0.099, "budget left unused: {used}");
}

#[test]
fn temp_cache_and_build_trees_are_churn_roots() {
    let none: Vec<String> = Vec::new();
    let cases = [
        ("private/var/folders/qb/abc123/T/tmp.x/y", Some("private/var/folders/qb/abc123")),
        ("private/var/folders/qb/abc123", Some("private/var/folders/qb/abc123")),
        ("private/var/folders/qb", None),
        ("private/tmp/scratch/a", Some("private/tmp")),
        ("Users/someone/.cursor/projects/p1/agent-transcripts/t", Some("Users/someone/.cursor/projects")),
        ("Users/someone/.cursor/chats", None),
        ("Users/someone/Library/Caches/com.example/x", Some("Users/someone/Library/Caches")),
        ("Users/someone/code/app/.next/static/chunks", Some("Users/someone/code/app/.next")),
        ("Users/someone/code/app/node_modules/.cache/babel", Some("Users/someone/code/app/node_modules")),
        ("Users/someone/code/app/node_modules/react", Some("Users/someone/code/app/node_modules")),
        ("Users/someone/.Trash/node_modules 14-18-54/next", Some("Users/someone/.Trash")),
        ("Users/someone/.npm/_cacache/index-v5/ab", Some("Users/someone/.npm")),
        ("Users/someone/.local/state/ci/npm-cache/_cacache/index-v5", Some("Users/someone/.local/state/ci/npm-cache/_cacache")),
        ("Users/someone/code/app/.git/objects/ab", Some("Users/someone/code/app/.git/objects")),
        ("Users/someone/code/app/.git/refs", None),
        ("Users/someone/Library/Developer/Xcode/DerivedData/App-x/Build", Some("Users/someone/Library/Developer/Xcode/DerivedData")),
        // The shallowest tree wins: a repository inside the temp folder.
        ("private/tmp/work/.git/objects/ab", Some("private/tmp")),
        ("Users/someone/code/app/src", None),
        ("", None),
    ];
    for (path, want) in cases {
        assert_eq!(churn_root(path, &none).as_deref(), want, "{path}");
    }
    let extra = vec!["Users/*/Downloads/torrents".to_string()];
    assert_eq!(
        churn_root("Users/someone/Downloads/torrents/a/b", &extra).as_deref(),
        Some("Users/someone/Downloads/torrents")
    );
}

#[test]
fn thousands_of_temp_events_become_one_churn_hold() {
    let none: Vec<String> = Vec::new();
    let mut reqs = dirs(5000, "private/var/folders/qb/abc/T");
    reqs.push(ScanReq::Subtree("private/var/folders/qb/abc/T/x".into()));
    reqs.push(ScanReq::Dir("Users/someone/code/app/src".into()));
    let plan = plan_scans(&reqs, &[], &none);
    assert_eq!(plan.now, vec![ScanReq::Dir("Users/someone/code/app/src".into())]);
    assert_eq!(
        plan.held,
        vec![(
            "private/var/folders/qb/abc".to_string(),
            HoldKind::Churn,
            ScanReq::Dir("private/var/folders/qb/abc".into())
        )]
    );
}

#[test]
fn a_burst_under_one_folder_waits_for_quiet_and_small_changes_do_not() {
    let none: Vec<String> = Vec::new();
    let small = dirs(BURST_MIN - 1, "Users/someone/data/images/articles");
    let plan = plan_scans(&small, &[], &none);
    assert_eq!(plan.now.len(), BURST_MIN - 1);
    assert!(plan.held.is_empty());

    let mut big = dirs(BURST_MIN + 10, "Users/someone/data/images/articles");
    big.push(ScanReq::Dir("Users/someone/notes".into()));
    let plan = plan_scans(&big, &[], &none);
    assert_eq!(plan.now, vec![ScanReq::Dir("Users/someone/notes".into())]);
    assert_eq!(plan.held.len(), BURST_MIN + 10);
    assert!(plan.held.iter().all(|(root, kind, _)| root == "Users/someone/data/images/articles" && *kind == HoldKind::Burst));

    // Once a tree is held, later rescans inside it join the hold.
    let later = vec![ScanReq::Dir("Users/someone/data/images/articles/9/x".into())];
    let plan = plan_scans(&later, &["Users/someone/data/images/articles".to_string()], &none);
    assert!(plan.now.is_empty());
    assert_eq!(plan.held[0].0, "Users/someone/data/images/articles");
}

#[test]
fn a_burst_spread_across_a_home_folder_is_not_held_as_a_whole() {
    let none: Vec<String> = Vec::new();
    // 400 rescans, but under 400 different top-level folders of one home:
    // the only ancestor with enough of them is too shallow to hold.
    let reqs: Vec<ScanReq> = (0..400).map(|i| ScanReq::Dir(format!("Users/someone/p{i}/src"))).collect();
    let plan = plan_scans(&reqs, &[], &none);
    assert_eq!(plan.now.len(), 400);
    assert!(plan.held.is_empty());
}

#[test]
fn whole_subtree_rescans_coalesce_in_a_deep_hold() {
    let none: Vec<String> = Vec::new();
    let reqs = vec![ScanReq::Subtree("Users/someone/data".into())];
    let plan = plan_scans(&reqs, &[], &none);
    assert!(plan.now.is_empty());
    assert_eq!(plan.held[0].1, HoldKind::Deep);
}

#[test]
fn holds_release_on_quiet_after_the_maximum_or_hourly_for_churn() {
    let t = 1_700_000_000;
    assert!(!hold_due(&HoldKind::Burst, t, t, 0, t + QUIET_SECS - 1));
    assert!(hold_due(&HoldKind::Burst, t, t, 0, t + QUIET_SECS));
    // Never quiet, still released by the maximum hold.
    let now = t + MAX_HOLD_SECS;
    assert!(hold_due(&HoldKind::Burst, t, now - 1, 0, now));
    assert!(hold_due(&HoldKind::Deep, t, t, 0, t + QUIET_SECS));
    assert!(hold_due(&HoldKind::Churn, t, t, 0, t), "a never-scanned churn tree goes at once");
    assert!(!hold_due(&HoldKind::Churn, t, t, t - 60, t));
    assert!(hold_due(&HoldKind::Churn, t, t, t - CHURN_EVERY_SECS, t));
}

/// End to end: a repository's object store is one opaque row with the whole
/// size, nothing beneath it, and the totals above it stay exact.
#[test]
fn a_churn_tree_is_mapped_as_one_opaque_total() {
    let tmp = scratch("opaque");
    let root = tmp.join("tree");
    let db = tmp.join("state/vol.db");
    let objects = root.join("app/.git/objects");
    for i in 0..40 {
        let d = objects.join(format!("{i:02x}"));
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("obj"), vec![7u8; 4096]).unwrap();
    }
    fs::create_dir_all(root.join("app/src")).unwrap();
    fs::write(root.join("app/src/main.rs"), vec![1u8; 4096]).unwrap();
    let now = 1_700_000_000;
    baseline(&root, &db, now);
    assert_eq!(opaque_next(&db).unwrap().as_deref(), Some("app/.git/objects"));
    let conn = open_db(&db).unwrap();
    assert!(dir_rows_under(&conn, "app/.git/objects") == 0, "no rows beneath a churn tree");
    drop(conn);

    let mut rests = 0;
    let total = opaque_measure(&objects, || rests += 1);
    assert_eq!(total.dirs, 41);
    assert!(total.files >= 40);
    apply_opaque(&db, "app/.git/objects", &total, now + 1, 0).unwrap();
    assert_eq!(opaque_len(&db).unwrap(), 0);
    let conn = open_db(&db).unwrap();
    let row = dir(&conn, "app/.git/objects").unwrap().unwrap();
    assert_eq!(row.total_bytes, total.own_bytes as i64);
    assert!(row.total_bytes >= 40 * 4096);
    assert_eq!(dir_rows_under(&conn, "app/.git/objects"), 0);
    let src = dir(&conn, "app/src").unwrap().unwrap().total_bytes;
    let app = dir(&conn, "app").unwrap().unwrap();
    let git = dir(&conn, "app/.git").unwrap().unwrap().total_bytes;
    assert_eq!(app.total_bytes, app.own_bytes + src + git);
    assert!(!app.partial, "the opaque tree no longer leaves its parents partial");
    // A second measurement of the same tree changes nothing.
    drop(conn);
    apply_opaque(&db, "app/.git/objects", &total, now + 2, 0).unwrap();
    let conn = open_db(&db).unwrap();
    assert_eq!(dir(&conn, "app").unwrap().unwrap().total_bytes, app.total_bytes);
    let _ = fs::remove_dir_all(&tmp);
}

fn dir_rows_under(conn: &Db, path: &str) -> i64 {
    let stmt = conn.prepare("SELECT COUNT(*) FROM dirs WHERE path > ?1 AND path < ?2").unwrap();
    stmt.bind_text(1, &format!("{path}/")).unwrap();
    stmt.bind_text(2, &format!("{path}0")).unwrap();
    assert!(stmt.step().unwrap());
    stmt.i64(0)
}

/// End to end: a burst of rescans is parked, not queued, and reaches the
/// queue once the tree has been quiet; each folder once, however many events.
#[test]
fn a_bursty_tree_reaches_the_queue_once_after_it_goes_quiet() {
    let tmp = scratch("burst");
    let root = tmp.join("tree");
    let db = tmp.join("state/vol.db");
    fs::create_dir_all(root.join("lake/src/images/articles")).unwrap();
    let t = 1_700_000_000;
    baseline(&root, &db, t);
    let none: Vec<String> = Vec::new();
    let burst = dirs(BURST_MIN * 2, "lake/src/images/articles");
    let stats = apply_plan(&db, &burst, t + 10, &none).unwrap();
    assert_eq!(stats.queued, 0);
    assert_eq!(queue_len(&db).unwrap(), 0);
    // The same folders again a minute later, plus a few more: still held.
    let again = dirs(20, "lake/src/images/articles");
    apply_plan(&db, &again, t + 70, &none).unwrap();
    assert_eq!(queue_len(&db).unwrap(), 0);
    assert_eq!(hold_count(&db).unwrap(), 1);
    // Quiet: released on the next fold, with no new events.
    let stats = apply_plan(&db, &[], t + 70 + QUIET_SECS, &none).unwrap();
    assert_eq!(stats.released, BURST_MIN * 2);
    assert_eq!(queue_len(&db).unwrap(), (BURST_MIN * 2) as i64);
    assert_eq!(hold_count(&db).unwrap(), 0);
    let _ = fs::remove_dir_all(&tmp);
}

/// Maps from before this change carry tens of thousands of queued rescans
/// inside churn trees; they collapse to one request per tree.
#[test]
fn a_churn_backlog_collapses_to_one_request_per_tree() {
    let tmp = scratch("migrate");
    let root = tmp.join("tree");
    let db = tmp.join("state/vol.db");
    fs::create_dir_all(root.join("keep")).unwrap();
    let t = 1_700_000_000;
    baseline(&root, &db, t);
    for i in 0..500 {
        enqueue(&db, &format!("private/var/folders/qb/abc/T/t{i}"), true).unwrap();
        enqueue(&db, &format!("Users/someone/.cursor/projects/p{i}"), true).unwrap();
    }
    enqueue(&db, "keep", false).unwrap();
    assert_eq!(queue_len(&db).unwrap(), 1001);
    walk_slice(&root, &db, t + 1, 0, 0, Duration::from_millis(1), &[], |_| false).unwrap();
    assert_eq!(queue_len(&db).unwrap(), 1, "only the ordinary rescan stays queued");
    assert_eq!(opaque_len(&db).unwrap(), 2);
    let _ = fs::remove_dir_all(&tmp);
}

/// The worker sets nice 20 and the Darwin background band on the whole
/// process itself, so a relaunch by any route keeps them. (This lowers the
/// test process too, which is harmless.)
#[test]
fn the_worker_sets_its_own_background_priority() {
    let (nice, bg) = std::thread::spawn(worker_priority).join().unwrap();
    assert_eq!((nice, bg), (20, true));
    assert_eq!(read_priority(), (20, true), "process-wide, not just the calling thread");
}

/// Every scan asks whether each ancestor still has unfinished children. On a
/// parent with 47k children that read every child (about 30 ms CPU a scan);
/// the partial index makes it one lookup. Checked on a fresh database, where
/// the index is created after the `partial` column migration.
#[test]
fn the_unfinished_child_check_uses_the_partial_index() {
    let tmp = scratch("index");
    let conn = open_db(&tmp.join("v.db")).unwrap();
    let stmt = conn
        .prepare("EXPLAIN QUERY PLAN SELECT 1 FROM dirs WHERE parent = ?1 AND path != '' AND (scanned_at = 0 OR partial != 0) LIMIT 1")
        .unwrap();
    let mut plan = String::new();
    while stmt.step().unwrap() {
        plan.push_str(&stmt.text(3));
    }
    assert!(plan.contains("dirs_unfinished"), "{plan}");
    let _ = fs::remove_dir_all(&tmp);
}

/// A directory that blocked a worker (a privacy-protected folder opened by a
/// launchd process) is never opened again, and its row keeps its last size
/// rather than dropping to zero.
#[test]
fn a_directory_that_blocked_a_worker_is_skipped_and_keeps_its_size() {
    let _turn = SKIP_LIST.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = scratch("skip");
    let root = tmp.join("tree");
    let db = tmp.join("state/vol.db");
    fs::create_dir_all(root.join("held")).unwrap();
    fs::write(root.join("held/f"), vec![9u8; 8192]).unwrap();
    let t = 1_700_000_000;
    baseline(&root, &db, t);
    let before = dir(&open_db(&db).unwrap(), "held").unwrap().unwrap().total_bytes;
    assert!(before >= 8192);
    set_skip(vec![root.join("held")]);
    assert!(skipped(&root.join("held")));
    assert!(read_dir_entries(&root.join("held")).is_err());
    fs::write(root.join("held/g"), vec![9u8; 8192]).unwrap();
    enqueue(&db, "held", false).unwrap();
    walk_one_batch(&root, &db, t + 10, 0, 10, |_| false).unwrap();
    set_skip(Vec::new());
    assert_eq!(queue_len(&db).unwrap(), 0);
    assert_eq!(dir(&open_db(&db).unwrap(), "held").unwrap().unwrap().total_bytes, before);
    let _ = fs::remove_dir_all(&tmp);
}

/// The Photos library blocks every open from a binary without a grant; the
/// walker never opens it, and a folder recorded as stuck covers its subtree.
#[test]
fn protected_trees_are_never_opened_and_stuck_folders_cover_their_subtree() {
    let _turn = SKIP_LIST.lock().unwrap_or_else(|e| e.into_inner());
    assert_eq!(
        protected_root("Users/someone/Pictures/Photos Library.photoslibrary/resources/derivatives/C").as_deref(),
        Some("Users/someone/Pictures/Photos Library.photoslibrary")
    );
    assert_eq!(protected_root("Users/someone/Pictures/holiday"), None);
    let tmp = scratch("subtree-skip");
    fs::create_dir_all(tmp.join("a/b/c")).unwrap();
    set_skip(vec![tmp.join("a")]);
    assert!(skipped(&tmp.join("a/b/c")));
    assert!(!skipped(&tmp.join("ab")));
    assert!(read_dir_entries(&tmp.join("a/b")).is_err());
    set_skip(Vec::new());
    let _ = fs::remove_dir_all(&tmp);
}

/// A churn tree whose root cannot be read keeps its recorded size instead of
/// dropping to zero.
#[test]
fn an_unreadable_churn_root_keeps_its_size() {
    let _turn = SKIP_LIST.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = scratch("opaque-skip");
    let root = tmp.join("tree");
    let db = tmp.join("state/vol.db");
    let objects = root.join("app/.git/objects/aa");
    fs::create_dir_all(&objects).unwrap();
    fs::write(objects.join("o"), vec![1u8; 8192]).unwrap();
    let t = 1_700_000_000;
    baseline(&root, &db, t);
    let first = opaque_measure(&root.join("app/.git/objects"), || {});
    apply_opaque(&db, "app/.git/objects", &first, t + 1, 0).unwrap();
    let size = dir(&open_db(&db).unwrap(), "app/.git/objects").unwrap().unwrap().total_bytes;
    assert!(size >= 8192);
    set_skip(vec![root.join("app/.git/objects")]);
    let blocked = opaque_measure(&root.join("app/.git/objects"), || {});
    set_skip(Vec::new());
    assert_eq!(blocked.dirs, 0);
    apply_opaque(&db, "app/.git/objects", &blocked, t + 2, 0).unwrap();
    assert_eq!(dir(&open_db(&db).unwrap(), "app/.git/objects").unwrap().unwrap().total_bytes, size);
    let _ = fs::remove_dir_all(&tmp);
}
