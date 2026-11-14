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
