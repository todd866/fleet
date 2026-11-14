//! A walker that holds the database must not make tick fail, and every
//! connection waits a short time instead of returning "database is locked"
//! at once.

use diskmap::{open_db, open_readonly};
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn scratch(name: &str) -> PathBuf {
    let n = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let path = std::env::temp_dir().join(format!("diskmap-{name}-{n}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).unwrap();
    path
}

fn pragma_text(db: &diskmap::Db, sql: &str) -> String {
    let stmt = db.prepare(sql).unwrap();
    assert!(stmt.step().unwrap(), "{sql}");
    stmt.text(0)
}

#[test]
fn every_connection_waits_five_seconds_and_uses_wal() {
    let dir = scratch("busy");
    let path = dir.join("v.db");
    let db = open_db(&path).unwrap();
    assert_eq!(pragma_text(&db, "PRAGMA journal_mode").to_lowercase(), "wal");
    drop(db);

    let ro = open_readonly(&path).unwrap();
    assert_eq!(pragma_text(&ro, "PRAGMA journal_mode").to_lowercase(), "wal");
    drop(ro);

    let holder = open_db(&path).unwrap();
    holder.exec("BEGIN IMMEDIATE").unwrap();
    let second = open_db(&path).unwrap();
    let started = Instant::now();
    let err = match second.exec("BEGIN IMMEDIATE") {
        Err(err) => err,
        Ok(()) => panic!("a second writer should wait, then report the lock"),
    };
    let waited = started.elapsed();
    assert!(err.contains("database is locked"), "{err}");
    assert!(
        waited >= Duration::from_millis(4500) && waited < Duration::from_secs(7),
        "busy wait was {waited:?}, want about 5s"
    );
    let read_at = Instant::now();
    let reader = open_readonly(&path).unwrap();
    let count = reader.prepare("SELECT COUNT(*) FROM meta").unwrap();
    assert!(count.step().unwrap());
    assert!(
        read_at.elapsed() < Duration::from_secs(1),
        "readonly summary read waited {:?}",
        read_at.elapsed()
    );
    assert_eq!(pragma_text(&reader, "PRAGMA journal_mode").to_lowercase(), "wal");
    let _ = fs::remove_dir_all(&dir);
}
