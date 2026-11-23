//! Per-volume directory map. Hot rows live here; the archive keeps the rest.

use crate::sqlite::Db;
use std::path::Path;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirRow {
    pub path: String,
    pub parent: String,
    pub own_bytes: i64,
    pub total_bytes: i64,
    pub own_dataless: i64,
    pub dataless_bytes: i64,
    pub files: i64,
    pub newest_mtime: i64,
    pub scanned_at: i64,
    /// Lower bound: a descendant has not been scanned yet.
    pub partial: bool,
    /// Total at the moment this directory first became complete.
    pub snap_bytes: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventRow {
    pub id: i64,
    pub t: i64,
    pub path: String,
    pub delta_bytes: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistRow {
    pub day: String,
    pub path: String,
    pub total_bytes: i64,
}

pub fn open_readonly(path: &Path) -> Result<Db, String> {
    if !path.is_file() {
        return Err(format!("sqlite open: {}", path.display()));
    }
    Db::open_readonly(path)
}

pub fn open(path: &Path) -> Result<Db, String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let db = Db::open(path)?;
    db.exec(
        "CREATE TABLE IF NOT EXISTS dirs (
            path TEXT PRIMARY KEY,
            parent TEXT,
            own_bytes INTEGER NOT NULL,
            total_bytes INTEGER NOT NULL,
            own_dataless INTEGER NOT NULL DEFAULT 0,
            dataless_bytes INTEGER NOT NULL DEFAULT 0,
            files INTEGER NOT NULL,
            newest_mtime INTEGER NOT NULL,
            scanned_at INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS dirs_parent ON dirs(parent);
        CREATE TABLE IF NOT EXISTS events (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            t INTEGER NOT NULL,
            path TEXT NOT NULL,
            delta_bytes INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS history (
            day TEXT NOT NULL,
            path TEXT NOT NULL,
            total_bytes INTEGER NOT NULL,
            PRIMARY KEY (day, path)
        );
        CREATE TABLE IF NOT EXISTS meta (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS pending (
            path TEXT PRIMARY KEY,
            deep INTEGER NOT NULL,
            seq INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS pending_seq ON pending(seq);",
    )?;
    if !column_exists(&db, "partial")? {
        db.exec("ALTER TABLE dirs ADD COLUMN partial INTEGER NOT NULL DEFAULT 0")?;
    }
    if !column_exists(&db, "snap_bytes")? {
        db.exec("ALTER TABLE dirs ADD COLUMN snap_bytes INTEGER")?;
    }
    // `child_unfinished` runs for every ancestor of every scanned directory.
    // Through `dirs_parent` alone it reads every child of a wide parent (47k
    // under one image folder): about 30 ms CPU per scan. This index holds only
    // the unfinished rows, so the check is one lookup.
    db.exec(
        "CREATE INDEX IF NOT EXISTS dirs_unfinished ON dirs(parent) WHERE scanned_at = 0 OR partial != 0;
        CREATE TABLE IF NOT EXISTS hold (
            root TEXT PRIMARY KEY,
            kind INTEGER NOT NULL,
            first_event INTEGER NOT NULL,
            last_event INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS deferred (
            path TEXT PRIMARY KEY,
            deep INTEGER NOT NULL,
            root TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS deferred_root ON deferred(root);
        CREATE TABLE IF NOT EXISTS opaque (
            path TEXT PRIMARY KEY
        );",
    )?;
    Ok(db)
}

/// Park a rescan under a hold, opening the hold or extending it to `now`.
pub fn hold_push(db: &Db, root: &str, kind: i64, path: &str, deep: bool, now: i64) -> Result<(), String> {
    let stmt = db.prepare(
        "INSERT INTO hold(root, kind, first_event, last_event) VALUES(?1, ?2, ?3, ?3)
         ON CONFLICT(root) DO UPDATE SET last_event = MAX(hold.last_event, excluded.last_event)",
    )?;
    stmt.bind_text(1, root)?;
    stmt.bind_i64(2, kind)?;
    stmt.bind_i64(3, now)?;
    stmt.step()?;
    let stmt = db.prepare(
        "INSERT INTO deferred(path, deep, root) VALUES(?1, ?2, ?3)
         ON CONFLICT(path) DO UPDATE SET deep = MAX(deferred.deep, excluded.deep)",
    )?;
    stmt.bind_text(1, path)?;
    stmt.bind_i64(2, i64::from(deep))?;
    stmt.bind_text(3, root)?;
    stmt.step()?;
    Ok(())
}

/// Open holds: (root, kind, first_event, last_event).
pub fn holds(db: &Db) -> Result<Vec<(String, i64, i64, i64)>, String> {
    let stmt = db.prepare("SELECT root, kind, first_event, last_event FROM hold ORDER BY root")?;
    let mut out = Vec::new();
    while stmt.step()? {
        out.push((stmt.text(0), stmt.i64(1), stmt.i64(2), stmt.i64(3)));
    }
    Ok(out)
}

/// Close a hold and return its parked rescans (path, deep).
pub fn hold_take(db: &Db, root: &str) -> Result<Vec<(String, bool)>, String> {
    let stmt = db.prepare("SELECT path, deep FROM deferred WHERE root = ?1")?;
    stmt.bind_text(1, root)?;
    let mut out = Vec::new();
    while stmt.step()? {
        out.push((stmt.text(0), stmt.i64(1) != 0));
    }
    for sql in ["DELETE FROM deferred WHERE root = ?1", "DELETE FROM hold WHERE root = ?1"] {
        let stmt = db.prepare(sql)?;
        stmt.bind_text(1, root)?;
        stmt.step()?;
    }
    Ok(out)
}

pub fn opaque_push(db: &Db, path: &str) -> Result<(), String> {
    let stmt = db.prepare("INSERT INTO opaque(path) VALUES(?1) ON CONFLICT(path) DO NOTHING")?;
    stmt.bind_text(1, path)?;
    stmt.step()?;
    Ok(())
}

pub fn opaque_next(db: &Db) -> Result<Option<String>, String> {
    let stmt = db.prepare("SELECT path FROM opaque ORDER BY path LIMIT 1")?;
    Ok(if stmt.step()? { Some(stmt.text(0)) } else { None })
}

pub fn opaque_done(db: &Db, path: &str) -> Result<(), String> {
    let stmt = db.prepare("DELETE FROM opaque WHERE path = ?1")?;
    stmt.bind_text(1, path)?;
    stmt.step()?;
    Ok(())
}

pub fn opaque_len(db: &Db) -> Result<i64, String> {
    let stmt = db.prepare("SELECT COUNT(*) FROM opaque")?;
    stmt.step()?;
    Ok(stmt.i64(0))
}

pub fn pending_all(db: &Db) -> Result<Vec<(String, bool)>, String> {
    let stmt = db.prepare("SELECT path, deep FROM pending")?;
    let mut out = Vec::new();
    while stmt.step()? {
        out.push((stmt.text(0), stmt.i64(1) != 0));
    }
    Ok(out)
}

pub fn pending_remove(db: &Db, path: &str) -> Result<(), String> {
    let stmt = db.prepare("DELETE FROM pending WHERE path = ?1")?;
    stmt.bind_text(1, path)?;
    stmt.step()?;
    Ok(())
}

fn column_exists(db: &Db, name: &str) -> Result<bool, String> {
    let stmt = db.prepare("PRAGMA table_info(dirs)")?;
    while stmt.step()? {
        if stmt.text(1) == name {
            return Ok(true);
        }
    }
    Ok(false)
}

pub fn meta_get(db: &Db, key: &str) -> Result<Option<String>, String> {
    let stmt = db.prepare("SELECT value FROM meta WHERE key = ?1")?;
    stmt.bind_text(1, key)?;
    if stmt.step()? {
        Ok(Some(stmt.text(0)))
    } else {
        Ok(None)
    }
}

pub fn meta_set(db: &Db, key: &str, value: &str) -> Result<(), String> {
    let stmt = db.prepare(
        "INSERT INTO meta(key, value) VALUES(?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )?;
    stmt.bind_text(1, key)?;
    stmt.bind_text(2, value)?;
    stmt.step()?;
    Ok(())
}

fn map_dir(stmt: &crate::sqlite::Stmt) -> DirRow {
    DirRow {
        path: stmt.text(0),
        parent: stmt.text(1),
        own_bytes: stmt.i64(2),
        total_bytes: stmt.i64(3),
        own_dataless: stmt.i64(4),
        dataless_bytes: stmt.i64(5),
        files: stmt.i64(6),
        newest_mtime: stmt.i64(7),
        scanned_at: stmt.i64(8),
        partial: stmt.i64(9) != 0,
        snap_bytes: if stmt.is_null(10) { None } else { Some(stmt.i64(10)) },
    }
}

const DIR_COLS: &str =
    "path, parent, own_bytes, total_bytes, own_dataless, dataless_bytes, files, newest_mtime, scanned_at, partial, snap_bytes";

pub fn dir(db: &Db, path: &str) -> Result<Option<DirRow>, String> {
    let stmt = db.prepare(&format!("SELECT {DIR_COLS} FROM dirs WHERE path = ?1"))?;
    stmt.bind_text(1, path)?;
    if stmt.step()? {
        Ok(Some(map_dir(&stmt)))
    } else {
        Ok(None)
    }
}

/// Remove a directory's row and every row beneath it, with anything queued for them.
/// The caller has already taken the directory's totals out of its ancestors.
pub fn delete_subtree(db: &Db, path: &str) -> Result<(), String> {
    if path.is_empty() {
        return Err("refusing to delete the volume root".into());
    }
    // Everything beneath `path` sorts between "path/" and "path0" ('0' follows '/').
    let lo = format!("{path}/");
    let hi = format!("{path}0");
    for table in ["dirs", "pending"] {
        let stmt = db.prepare(&format!(
            "DELETE FROM {table} WHERE path = ?1 OR (path >= ?2 AND path < ?3)"
        ))?;
        stmt.bind_text(1, path)?;
        stmt.bind_text(2, &lo)?;
        stmt.bind_text(3, &hi)?;
        stmt.step()?;
    }
    Ok(())
}

pub fn children(db: &Db, parent: &str) -> Result<Vec<DirRow>, String> {
    // The volume root is stored as path '' with parent ''. Exclude it from its
    // own child list.
    let sql = if parent.is_empty() {
        format!("SELECT {DIR_COLS} FROM dirs WHERE parent = '' AND path != '' ORDER BY total_bytes DESC, path")
    } else {
        format!("SELECT {DIR_COLS} FROM dirs WHERE parent = ?1 ORDER BY total_bytes DESC, path")
    };
    let stmt = db.prepare(&sql)?;
    if !parent.is_empty() {
        stmt.bind_text(1, parent)?;
    }
    let mut out = Vec::new();
    while stmt.step()? {
        out.push(map_dir(&stmt));
    }
    Ok(out)
}

pub fn upsert_own(
    db: &Db,
    path: &str,
    parent: &str,
    own_bytes: i64,
    own_dataless: i64,
    files: i64,
    newest: i64,
    scanned_at: i64,
) -> Result<(), String> {
    let stmt = db.prepare(
        "INSERT INTO dirs(path, parent, own_bytes, total_bytes, own_dataless, dataless_bytes, files, newest_mtime, scanned_at)
         VALUES(?1, ?2, ?3, ?3, ?4, ?4, ?5, ?6, ?7)
         ON CONFLICT(path) DO UPDATE SET
            parent = excluded.parent,
            own_bytes = excluded.own_bytes,
            own_dataless = excluded.own_dataless,
            files = excluded.files,
            newest_mtime = excluded.newest_mtime,
            scanned_at = excluded.scanned_at",
    )?;
    stmt.bind_text(1, path)?;
    stmt.bind_text(2, parent)?;
    stmt.bind_i64(3, own_bytes)?;
    stmt.bind_i64(4, own_dataless)?;
    stmt.bind_i64(5, files)?;
    stmt.bind_i64(6, newest)?;
    stmt.bind_i64(7, scanned_at)?;
    stmt.step()?;
    Ok(())
}

pub fn ensure_dir(db: &Db, path: &str, parent: &str) -> Result<(), String> {
    let stmt = db.prepare(
        "INSERT INTO dirs(path, parent, own_bytes, total_bytes, own_dataless, dataless_bytes, files, newest_mtime, scanned_at)
         VALUES(?1, ?2, 0, 0, 0, 0, 0, 0, 0)
         ON CONFLICT(path) DO NOTHING",
    )?;
    stmt.bind_text(1, path)?;
    stmt.bind_text(2, parent)?;
    stmt.step()?;
    Ok(())
}

pub fn set_totals(
    db: &Db,
    path: &str,
    total_bytes: i64,
    dataless_bytes: i64,
    newest: i64,
) -> Result<(), String> {
    let stmt = db.prepare(
        "UPDATE dirs SET total_bytes = ?2, dataless_bytes = ?3, newest_mtime = MAX(newest_mtime, ?4) WHERE path = ?1",
    )?;
    stmt.bind_text(1, path)?;
    stmt.bind_i64(2, total_bytes)?;
    stmt.bind_i64(3, dataless_bytes)?;
    stmt.bind_i64(4, newest)?;
    stmt.step()?;
    Ok(())
}

/// Recompute `total_bytes` from `own_bytes` plus children, then walk up.
/// Returns (path, old_total, new_total, scanned_at) for each directory touched
/// so the caller can record a change. `scanned_at` is the value from before
/// this call.
pub fn propagate(db: &Db, start: &str) -> Result<Vec<(String, i64, i64, i64)>, String> {
    let mut path = start.to_string();
    let mut touched = Vec::new();
    loop {
        let row = dir(db, &path)?.ok_or_else(|| format!("missing dir {path}"))?;
        let kids = children(db, &path)?;
        let child_bytes: i64 = kids.iter().map(|k| k.total_bytes).sum();
        let child_dataless: i64 = kids.iter().map(|k| k.dataless_bytes).sum();
        let child_newest = kids.iter().map(|k| k.newest_mtime).max().unwrap_or(0);
        let total = row.own_bytes.saturating_add(child_bytes);
        let dataless = row.own_dataless.saturating_add(child_dataless);
        let newest = row.newest_mtime.max(child_newest);
        set_totals(db, &path, total, dataless, newest)?;
        touched.push((path.clone(), row.total_bytes, total, row.scanned_at));
        if path.is_empty() {
            break;
        }
        path = row.parent;
    }
    Ok(touched)
}

pub fn insert_event(db: &Db, t: i64, path: &str, delta: i64) -> Result<i64, String> {
    let stmt = db.prepare("INSERT INTO events(t, path, delta_bytes) VALUES(?1, ?2, ?3)")?;
    stmt.bind_i64(1, t)?;
    stmt.bind_text(2, path)?;
    stmt.bind_i64(3, delta)?;
    stmt.step()?;
    let q = db.prepare("SELECT last_insert_rowid()")?;
    q.step()?;
    Ok(q.i64(0))
}

pub fn events_since(db: &Db, t: i64) -> Result<Vec<EventRow>, String> {
    let stmt = db.prepare(
        "SELECT id, t, path, delta_bytes FROM events WHERE t >= ?1 ORDER BY id",
    )?;
    stmt.bind_i64(1, t)?;
    read_events(&stmt)
}

pub fn events_after_id(db: &Db, id: i64) -> Result<Vec<EventRow>, String> {
    let stmt = db.prepare(
        "SELECT id, t, path, delta_bytes FROM events WHERE id > ?1 ORDER BY id",
    )?;
    stmt.bind_i64(1, id)?;
    read_events(&stmt)
}

fn read_events(stmt: &crate::sqlite::Stmt) -> Result<Vec<EventRow>, String> {
    let mut out = Vec::new();
    while stmt.step()? {
        out.push(EventRow {
            id: stmt.i64(0),
            t: stmt.i64(1),
            path: stmt.text(2),
            delta_bytes: stmt.i64(3),
        });
    }
    Ok(out)
}

pub fn upsert_history(db: &Db, day: &str, path: &str, total: i64) -> Result<(), String> {
    let stmt = db.prepare(
        "INSERT INTO history(day, path, total_bytes) VALUES(?1, ?2, ?3)
         ON CONFLICT(day, path) DO UPDATE SET total_bytes = excluded.total_bytes",
    )?;
    stmt.bind_text(1, day)?;
    stmt.bind_text(2, path)?;
    stmt.bind_i64(3, total)?;
    stmt.step()?;
    Ok(())
}

pub fn snapshot_history(db: &Db, day: &str, min_bytes: i64) -> Result<usize, String> {
    let stmt = db.prepare("SELECT path, total_bytes FROM dirs WHERE total_bytes >= ?1")?;
    stmt.bind_i64(1, min_bytes)?;
    let mut rows = Vec::new();
    while stmt.step()? {
        rows.push((stmt.text(0), stmt.i64(1)));
    }
    let n = rows.len();
    for (path, total) in rows {
        upsert_history(db, day, &path, total)?;
    }
    Ok(n)
}

pub fn history_for_path(db: &Db, path: &str) -> Result<Vec<HistRow>, String> {
    let stmt = db.prepare(
        "SELECT day, path, total_bytes FROM history WHERE path = ?1 ORDER BY day",
    )?;
    stmt.bind_text(1, path)?;
    let mut out = Vec::new();
    while stmt.step()? {
        out.push(HistRow {
            day: stmt.text(0),
            path: stmt.text(1),
            total_bytes: stmt.i64(2),
        });
    }
    Ok(out)
}

pub fn history_all(db: &Db) -> Result<Vec<HistRow>, String> {
    let stmt = db.prepare("SELECT day, path, total_bytes FROM history ORDER BY day, path")?;
    let mut out = Vec::new();
    while stmt.step()? {
        out.push(HistRow {
            day: stmt.text(0),
            path: stmt.text(1),
            total_bytes: stmt.i64(2),
        });
    }
    Ok(out)
}

pub fn photo_dirs(db: &Db, library: &str) -> Result<Vec<DirRow>, String> {
    let stmt = db.prepare(&format!(
        "SELECT {DIR_COLS} FROM dirs WHERE path = ?1 OR (
            path LIKE ?2 AND (
                path LIKE '%/originals' OR path LIKE '%/Masters' OR path LIKE '%/masters'
                OR path LIKE '%/derivatives' OR path LIKE '%/renders' OR path LIKE '%/caches'
            )
        )"
    ))?;
    stmt.bind_text(1, library)?;
    stmt.bind_text(2, &format!("{library}/%"))?;
    let mut out = Vec::new();
    while stmt.step()? {
        out.push(map_dir(&stmt));
    }
    Ok(out)
}

pub fn paths_with_own_at_least(db: &Db, min_bytes: i64) -> Result<Vec<String>, String> {
    let stmt = db.prepare("SELECT path FROM dirs WHERE own_bytes >= ?1 AND scanned_at > 0")?;
    stmt.bind_i64(1, min_bytes)?;
    let mut out = Vec::new();
    while stmt.step()? {
        out.push(stmt.text(0));
    }
    Ok(out)
}

pub fn dirs_at_least(db: &Db, min_bytes: i64) -> Result<Vec<DirRow>, String> {
    let stmt = db.prepare(&format!(
        "SELECT {DIR_COLS} FROM dirs WHERE total_bytes >= ?1 ORDER BY total_bytes DESC"
    ))?;
    stmt.bind_i64(1, min_bytes)?;
    let mut out = Vec::new();
    while stmt.step()? {
        out.push(map_dir(&stmt));
    }
    Ok(out)
}

pub fn top_children(db: &Db, parent: &str, limit: i64) -> Result<Vec<DirRow>, String> {
    let sql = if parent.is_empty() {
        format!("SELECT {DIR_COLS} FROM dirs WHERE parent = '' AND path != '' ORDER BY total_bytes DESC LIMIT ?1")
    } else {
        format!("SELECT {DIR_COLS} FROM dirs WHERE parent = ?1 ORDER BY total_bytes DESC LIMIT ?2")
    };
    let stmt = db.prepare(&sql)?;
    if parent.is_empty() {
        stmt.bind_i64(1, limit)?;
    } else {
        stmt.bind_text(1, parent)?;
        stmt.bind_i64(2, limit)?;
    }
    let mut out = Vec::new();
    while stmt.step()? {
        out.push(map_dir(&stmt));
    }
    Ok(out)
}

/// Drop hot rows only when the archive has confirmed them.
pub fn prune_confirmed(
    db: &Db,
    event_before_unix: i64,
    confirmed_event_id: i64,
    history_before_day: &str,
    confirmed_days: &[String],
) -> Result<(i64, i64), String> {
    // A zero watermark means nothing has been confirmed, including history days.
    if confirmed_event_id <= 0 {
        return Ok((0, 0));
    }
    let stmt = db.prepare("DELETE FROM events WHERE t < ?1 AND id <= ?2")?;
    stmt.bind_i64(1, event_before_unix)?;
    stmt.bind_i64(2, confirmed_event_id)?;
    stmt.step()?;
    let events = db.changes();
    let mut days = 0i64;
    for day in confirmed_days {
        if day.as_str() >= history_before_day {
            continue;
        }
        let stmt = db.prepare("DELETE FROM history WHERE day = ?1")?;
        stmt.bind_text(1, day)?;
        stmt.step()?;
        days += db.changes();
    }
    Ok((events, days))
}

pub fn note_change(
    db: &Db,
    baseline_done: bool,
    touched: &[(String, i64, i64, i64)],
    now: i64,
    event_min: i64,
) -> Result<(), String> {
    if !baseline_done {
        return Ok(());
    }
    for (path, old, new, scanned_at) in touched {
        if *scanned_at <= 0 {
            continue;
        }
        let delta = new - old;
        if delta.abs() >= event_min {
            insert_event(db, now, path, delta)?;
        }
    }
    Ok(())
}

pub fn pending_len(db: &Db) -> Result<i64, String> {
    let stmt = db.prepare("SELECT COUNT(*) FROM pending")?;
    stmt.step()?;
    Ok(stmt.i64(0))
}

pub fn pending_push(db: &Db, path: &str, deep: bool) -> Result<(), String> {
    let seq = {
        let stmt = db.prepare("SELECT COALESCE(MAX(seq), 0) + 1 FROM pending")?;
        stmt.step()?;
        stmt.i64(0)
    };
    let stmt = db.prepare(
        "INSERT INTO pending(path, deep, seq) VALUES(?1, ?2, ?3)
         ON CONFLICT(path) DO UPDATE SET deep = MAX(pending.deep, excluded.deep)",
    )?;
    stmt.bind_text(1, path)?;
    stmt.bind_i64(2, i64::from(deep))?;
    stmt.bind_i64(3, seq)?;
    stmt.step()?;
    Ok(())
}

/// Take the newest rescan (depth first), or with `oldest` the one that has waited
/// longest. Newest-first alone starves the bottom of the queue for good once
/// rescans arrive as fast as they are done.
pub fn pending_pop(db: &Db, oldest: bool) -> Result<Option<(String, bool)>, String> {
    let (path, deep) = {
        let sql = if oldest {
            "SELECT path, deep FROM pending ORDER BY seq ASC LIMIT 1"
        } else {
            "SELECT path, deep FROM pending ORDER BY seq DESC LIMIT 1"
        };
        let stmt = db.prepare(sql)?;
        if !stmt.step()? {
            return Ok(None);
        }
        (stmt.text(0), stmt.i64(1) != 0)
    };
    let del = db.prepare("DELETE FROM pending WHERE path = ?1")?;
    del.bind_text(1, &path)?;
    del.step()?;
    Ok(Some((path, deep)))
}

pub fn scanned_count(db: &Db) -> Result<i64, String> {
    let stmt = db.prepare("SELECT COUNT(*) FROM dirs WHERE scanned_at > 0")?;
    stmt.step()?;
    Ok(stmt.i64(0))
}

pub fn has_history(db: &Db) -> Result<bool, String> {
    let stmt = db.prepare("SELECT 1 FROM history LIMIT 1")?;
    stmt.step()
}

pub fn snap_deltas(db: &Db) -> Result<Vec<(String, i64)>, String> {
    let stmt = db.prepare(
        "SELECT path, total_bytes - snap_bytes FROM dirs
         WHERE snap_bytes IS NOT NULL AND total_bytes != snap_bytes",
    )?;
    let mut out = Vec::new();
    while stmt.step()? {
        out.push((stmt.text(0), stmt.i64(1)));
    }
    Ok(out)
}

pub fn child_unfinished(db: &Db, parent: &str) -> Result<bool, String> {
    let stmt = db.prepare(
        "SELECT 1 FROM dirs WHERE parent = ?1 AND path != '' AND (scanned_at = 0 OR partial != 0) LIMIT 1",
    )?;
    stmt.bind_text(1, parent)?;
    stmt.step()
}

pub fn set_partial(db: &Db, path: &str, partial: bool) -> Result<(), String> {
    let stmt = db.prepare("UPDATE dirs SET partial = ?2 WHERE path = ?1")?;
    stmt.bind_text(1, path)?;
    stmt.bind_i64(2, i64::from(partial))?;
    stmt.step()?;
    Ok(())
}

pub fn snap_if_absent(db: &Db, path: &str) -> Result<(), String> {
    let stmt = db.prepare(
        "UPDATE dirs SET snap_bytes = total_bytes WHERE path = ?1 AND snap_bytes IS NULL AND partial = 0",
    )?;
    stmt.bind_text(1, path)?;
    stmt.step()?;
    Ok(())
}

/// A directory is partial while any descendant is still unscanned. The first
/// time it becomes complete, its total is the walk-time snapshot.
pub fn refresh_partial(db: &Db, start: &str) -> Result<(), String> {
    let mut path = start.to_string();
    for _ in 0..256 {
        let unfinished = child_unfinished(db, &path)?;
        set_partial(db, &path, unfinished)?;
        if !unfinished {
            snap_if_absent(db, &path)?;
        }
        if path.is_empty() {
            return Ok(());
        }
        let parent = dir(db, &path)?.map(|row| row.parent).unwrap_or_default();
        if parent == path {
            return Ok(());
        }
        path = parent;
    }
    Err(format!("dir cycle at {start}"))
}

/// Mark directories that were stored before partial totals existed.
pub fn backfill_partial(db: &Db) -> Result<(), String> {
    if meta_get(db, "partial_ready")?.as_deref() == Some("1") {
        return Ok(());
    }
    db.exec("UPDATE dirs SET partial = 1 WHERE scanned_at = 0")?;
    for _ in 0..64 {
        db.exec(
            "UPDATE dirs SET partial = 1 WHERE partial = 0 AND path IN (SELECT parent FROM dirs WHERE partial != 0)",
        )?;
        if db.changes() == 0 {
            break;
        }
    }
    db.exec(
        "UPDATE dirs SET snap_bytes = total_bytes WHERE snap_bytes IS NULL AND partial = 0 AND scanned_at > 0",
    )?;
    meta_set(db, "partial_ready", "1")?;
    Ok(())
}
