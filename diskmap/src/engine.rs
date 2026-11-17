//! Resumable directory walk over one volume. Read-only.
//!
//! The initial map is a batch of directory scans inside one transaction so a
//! crash resumes from the last commit and other connections see growing totals.

use crate::darwin::{self, Entry};
use crate::logic::{self, FileAttr, Growth, ScanReq};
use crate::sqlite::Db;
use crate::store;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Directories committed together by the long-lived initial walk.
pub const WALK_BATCH: usize = 10_000;

const BATCH_MAX: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Pending {
    path: String,
    deep: bool,
}

#[derive(Clone, Debug, Default)]
pub struct MapStats {
    pub scanned: usize,
    pub complete: bool,
    pub yielded: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MapProgress {
    pub dirs: u64,
    pub mapped_bytes: u64,
    pub percent: u8,
    pub done: bool,
}

enum Pace {
    Go,
    Yield,
    Limit,
}

pub fn map_volume(
    root: &Path,
    db_path: &Path,
    now: i64,
    budget: usize,
    event_min: i64,
    mut yield_now: impl FnMut() -> bool,
) -> Result<MapStats, String> {
    let db = store::open(db_path)?;
    in_txn(&db, || {
        run_walk(&db, root, now, event_min, |n| {
            if n >= budget {
                Pace::Limit
            } else if yield_now() {
                Pace::Yield
            } else {
                Pace::Go
            }
        })
    })
}

/// One committed batch of the initial walk. `pause` is checked before each
/// directory and receives how many directories this batch has already scanned.
pub fn walk_one_batch(
    root: &Path,
    db_path: &Path,
    now: i64,
    event_min: i64,
    batch: usize,
    mut pause: impl FnMut(usize) -> bool,
) -> Result<MapStats, String> {
    if pause(0) {
        return Ok(MapStats {
            yielded: true,
            ..MapStats::default()
        });
    }
    let db = store::open(db_path)?;
    let started = Instant::now();
    in_txn(&db, || {
        run_walk(&db, root, now, event_min, |n| {
            if pause(n) {
                Pace::Yield
            } else if n >= batch || (n > 0 && started.elapsed() >= BATCH_MAX) {
                Pace::Limit
            } else {
                Pace::Go
            }
        })
    })
}

pub fn queue_len(db_path: &Path) -> Result<i64, String> {
    let db = store::open(db_path)?;
    store::pending_len(&db)
}

pub fn scanned_dirs(db_path: &Path) -> Result<i64, String> {
    let db = store::open(db_path)?;
    store::scanned_count(&db)
}

pub fn map_progress(db: &Db) -> Result<MapProgress, String> {
    let done = store::meta_get(db, "baseline")?.as_deref() == Some("1");
    let dirs = store::scanned_count(db)?.max(0) as u64;
    let mapped = store::dir(db, "")?
        .map(|row| row.total_bytes.max(0) as u64)
        .unwrap_or(0);
    let total = meta_u64(db, "total_bytes")?;
    let free = meta_u64(db, "free_bytes")?;
    let used = total.saturating_sub(free);
    Ok(MapProgress {
        dirs,
        mapped_bytes: mapped,
        percent: logic::map_percent(mapped, used, done),
        done,
    })
}

fn meta_u64(db: &Db, key: &str) -> Result<u64, String> {
    Ok(store::meta_get(db, key)?
        .and_then(|s| s.parse().ok())
        .unwrap_or(0))
}

fn in_txn<T>(db: &Db, body: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    db.exec("BEGIN IMMEDIATE")?;
    match body() {
        Ok(value) => {
            db.exec("COMMIT")?;
            Ok(value)
        }
        Err(err) => {
            let _ = db.exec("ROLLBACK");
            Err(err)
        }
    }
}

fn run_walk(
    db: &Db,
    root: &Path,
    now: i64,
    event_min: i64,
    mut pace: impl FnMut(usize) -> Pace,
) -> Result<MapStats, String> {
    let baseline = prepare_queue(db, now)?;
    let mut scanned = 0usize;
    let mut yielded = false;
    while store::pending_len(db)? > 0 {
        match pace(scanned) {
            Pace::Go => {}
            Pace::Yield => {
                yielded = true;
                break;
            }
            Pace::Limit => break,
        }
        let Some(item) = store::pending_pop(db)? else {
            break;
        };
        scan_one(root, db, &item.0, item.1, now, event_min, baseline)?;
        scanned += 1;
    }
    let complete = close_batch(db, baseline)?;
    Ok(MapStats {
        scanned,
        complete,
        yielded,
    })
}

fn prepare_queue(db: &Db, now: i64) -> Result<bool, String> {
    import_legacy(db)?;
    store::backfill_partial(db)?;
    let baseline = store::meta_get(db, "baseline")?.as_deref() == Some("1");
    if store::pending_len(db)? == 0 && !baseline {
        store::pending_push(db, "", true)?;
        if store::meta_get(db, "walk_started")?.is_none() {
            store::meta_set(db, "walk_started", &now.to_string())?;
        }
    }
    Ok(baseline)
}

fn import_legacy(db: &Db) -> Result<(), String> {
    if store::pending_len(db)? > 0 {
        if store::meta_get(db, "pending")?
            .as_deref()
            .is_some_and(|text| !text.is_empty() && text != "[]")
        {
            store::meta_set(db, "pending", "[]")?;
        }
        return Ok(());
    }
    let Some(text) = store::meta_get(db, "pending")? else {
        return Ok(());
    };
    if text.trim().is_empty() || text.trim() == "[]" {
        return Ok(());
    }
    let items: Vec<Pending> = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    for item in items {
        store::pending_push(db, &item.path, item.deep)?;
    }
    store::meta_set(db, "pending", "[]")?;
    Ok(())
}

fn close_batch(db: &Db, was_baseline: bool) -> Result<bool, String> {
    let empty = store::pending_len(db)? == 0;
    let baseline = if empty && !was_baseline {
        store::meta_set(db, "baseline", "1")?;
        true
    } else {
        was_baseline
    };
    Ok(empty && baseline)
}

fn scan_one(
    root: &Path,
    db: &Db,
    path: &str,
    deep: bool,
    now: i64,
    event_min: i64,
    baseline: bool,
) -> Result<(), String> {
    let parent = parent_of(path);
    // A replayed event may name a deep directory created since the baseline.
    // Discover its missing parents from the nearest known ancestor first;
    // propagating totals through absent parent rows would abort every retry.
    let mut ancestor = parent.clone();
    while !ancestor.is_empty() && store::dir(db, &ancestor)?.is_none() {
        ancestor = parent_of(&ancestor);
    }
    if ancestor != parent {
        store::pending_push(db, &ancestor, true)?;
        return Ok(());
    }
    let abs = abs_path(root, path);
    let entries = match darwin::read_dir_entries(&abs) {
        Ok(v) => v,
        Err(_) => {
            let mut touched = apply_own(db, path, &parent, 0, 0, 0, 0, now)?;
            // Gone, not merely unreadable: nothing beneath it exists either.
            if is_missing(&abs) {
                touched.extend(drop_missing_children(db, path, &[])?);
            }
            store::note_change(db, baseline, &touched, now, event_min)?;
            store::refresh_partial(db, path)?;
            return Ok(());
        }
    };
    let pairs: Vec<(String, FileAttr)> = entries.iter().map(|e| (e.name.clone(), file_attr(e))).collect();
    let refs: Vec<(&str, FileAttr)> = pairs.iter().map(|(n, a)| (n.as_str(), a.clone())).collect();
    let account = logic::account_entries(&refs);
    let mut touched = apply_own(
        db,
        path,
        &parent,
        account.own_bytes as i64,
        account.own_dataless as i64,
        account.files as i64,
        account.newest,
        now,
    )?;
    touched.extend(drop_missing_children(db, path, &account.subdirs)?);
    for name in &account.subdirs {
        let child = join_rel(path, name);
        store::ensure_dir(db, &child, path)?;
        let scanned = store::dir(db, &child)?.map(|d| d.scanned_at).unwrap_or(0);
        if deep || scanned == 0 {
            store::pending_push(db, &child, deep)?;
        }
    }
    store::note_change(db, baseline, &touched, now, event_min)?;
    store::refresh_partial(db, path)?;
    Ok(())
}

const PRUNED_KEY: &str = "pruned_missing";

/// True only when the path is known not to exist. A folder this process may not look into
/// (another user's home, a privacy-protected folder) is not missing.
fn is_missing(abs: &Path) -> bool {
    matches!(std::fs::symlink_metadata(abs), Err(e) if e.kind() == std::io::ErrorKind::NotFound)
}

/// A subdirectory that was deleted, moved or renamed leaves its row behind, and every
/// ancestor's total still counts it. `present` is the sorted list of subdirectory names found
/// on disk just now; each child row not in it is taken out of the totals and dropped with
/// everything beneath it.
fn drop_missing_children(
    db: &Db,
    path: &str,
    present: &[String],
) -> Result<Vec<(String, i64, i64, i64)>, String> {
    let mut touched = Vec::new();
    for child in store::children(db, path)? {
        let name = child.path.rsplit('/').next().unwrap_or("");
        if present.binary_search_by(|p| p.as_str().cmp(name)).is_ok() {
            continue;
        }
        touched.extend(bubble(db, path, -child.total_bytes, -child.dataless_bytes, 0)?);
        store::delete_subtree(db, &child.path)?;
    }
    Ok(touched)
}

/// One-off repair for maps made before missing children were dropped: remove every row whose
/// directory no longer exists. Returns how many top-level stale directories were removed.
/// Only rows of at least `min_bytes` are checked; smaller leftovers go when their parent is
/// next scanned.
pub fn prune_missing(root: &Path, db_path: &Path, now: i64, event_min: i64, min_bytes: i64) -> Result<usize, String> {
    let db = store::open(db_path)?;
    if store::meta_get(&db, PRUNED_KEY)?.is_some() {
        return Ok(0);
    }
    let mut rows = store::dirs_at_least(&db, min_bytes)?;
    rows.sort_by(|a, b| a.path.cmp(&b.path));
    let baseline = store::meta_get(&db, "baseline")?.as_deref() == Some("1");
    in_txn(&db, || {
        let mut removed = 0usize;
        let mut last: Option<String> = None;
        for row in &rows {
            if row.path.is_empty() {
                continue;
            }
            if let Some(prefix) = &last {
                if row.path.starts_with(prefix.as_str()) && row.path.as_bytes().get(prefix.len()) == Some(&b'/') {
                    continue;
                }
            }
            if !is_missing(&abs_path(root, &row.path)) {
                continue;
            }
            // Read the row again: an ancestor's removal or an earlier bubble may have changed it.
            let Some(current) = store::dir(&db, &row.path)? else { continue };
            let touched = bubble(&db, &current.parent, -current.total_bytes, -current.dataless_bytes, 0)?;
            store::delete_subtree(&db, &row.path)?;
            store::note_change(&db, baseline, &touched, now, event_min)?;
            last = Some(row.path.clone());
            removed += 1;
        }
        store::meta_set(&db, PRUNED_KEY, &now.to_string())?;
        Ok(removed)
    })
}

fn apply_own(
    db: &Db,
    path: &str,
    parent: &str,
    own: i64,
    dataless: i64,
    files: i64,
    newest: i64,
    now: i64,
) -> Result<Vec<(String, i64, i64, i64)>, String> {
    store::ensure_dir(db, path, parent)?;
    let before = store::dir(db, path)?.ok_or_else(|| format!("missing dir {path}"))?;
    let d_bytes = own - before.own_bytes;
    let d_dataless = dataless - before.own_dataless;
    store::upsert_own(db, path, parent, own, dataless, files, newest, now)?;
    bubble(db, path, d_bytes, d_dataless, newest)
}

fn bubble(
    db: &Db,
    start: &str,
    d_bytes: i64,
    d_dataless: i64,
    newest: i64,
) -> Result<Vec<(String, i64, i64, i64)>, String> {
    let mut path = start.to_string();
    let mut touched = Vec::new();
    for _ in 0..256 {
        let row = store::dir(db, &path)?.ok_or_else(|| format!("missing dir {path}"))?;
        let new_bytes = row.total_bytes.saturating_add(d_bytes);
        let new_dataless = row.dataless_bytes.saturating_add(d_dataless);
        store::set_totals(db, &path, new_bytes, new_dataless, newest)?;
        touched.push((path.clone(), row.total_bytes, new_bytes, row.scanned_at));
        if path.is_empty() {
            return Ok(touched);
        }
        if row.parent == path {
            return Ok(touched);
        }
        path = row.parent;
    }
    Err(format!("dir cycle at {start}"))
}

fn file_attr(e: &Entry) -> FileAttr {
    FileAttr {
        allocated: e.allocated,
        logical: e.logical,
        flags: e.flags,
        mtime: e.mtime,
        is_dir: e.is_dir,
        mount_point: e.mount_point,
        symlink: e.symlink,
    }
}

pub fn enqueue(db_path: &Path, path: &str, deep: bool) -> Result<(), String> {
    let db = store::open(db_path)?;
    import_legacy(&db)?;
    let path = logic::norm_rel(path);
    store::pending_push(&db, &path, deep)?;
    Ok(())
}

pub fn apply_scans(db_path: &Path, scans: &[ScanReq]) -> Result<(), String> {
    for scan in scans {
        match scan {
            ScanReq::Dir(path) => enqueue(db_path, path, false)?,
            ScanReq::Subtree(path) => enqueue(db_path, path, true)?,
        }
    }
    Ok(())
}

pub fn parent_of(path: &str) -> String {
    if path.is_empty() {
        return String::new();
    }
    match path.rfind('/') {
        Some(i) => path[..i].to_string(),
        None => String::new(),
    }
}

pub fn join_rel(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        name.to_string()
    } else {
        format!("{parent}/{name}")
    }
}

fn abs_path(root: &Path, rel: &str) -> PathBuf {
    if rel.is_empty() {
        root.to_path_buf()
    } else {
        root.join(rel)
    }
}

pub fn growth_since(db_path: &Path, since: i64) -> Result<Vec<Growth>, String> {
    let db = store::open_readonly(db_path)?;
    let events = store::events_since(&db, since)?;
    Ok(events
        .into_iter()
        .map(|e| Growth {
            path: e.path,
            delta: e.delta_bytes,
        })
        .collect())
}

pub fn growth_against_snapshot(db_path: &Path) -> Result<Vec<Growth>, String> {
    let db = store::open_readonly(db_path)?;
    Ok(store::snap_deltas(&db)?
        .into_iter()
        .map(|(path, delta)| Growth { path, delta })
        .collect())
}

pub fn remember_volume(
    db_path: &Path,
    uuid: &str,
    name: &str,
    mount: &str,
    total: u64,
    free: u64,
    seen: i64,
) -> Result<(), String> {
    let db = store::open(db_path)?;
    store::meta_set(&db, "uuid", uuid)?;
    store::meta_set(&db, "name", name)?;
    store::meta_set(&db, "mount", mount)?;
    store::meta_set(&db, "total_bytes", &total.to_string())?;
    store::meta_set(&db, "free_bytes", &free.to_string())?;
    store::meta_set(&db, "last_seen", &seen.to_string())?;
    Ok(())
}

pub fn meta(db_path: &Path, key: &str) -> Result<Option<String>, String> {
    if !db_path.is_file() {
        return Ok(None);
    }
    let db = store::open_readonly(db_path)?;
    store::meta_get(&db, key)
}
