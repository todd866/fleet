use diskmap::{read_where, store_summary, store_where, summary_path, where_path};
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

fn tmp(tag: &str) -> PathBuf {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let dir = std::env::temp_dir().join(format!("diskmap-peers-{tag}-{nanos}"));
    fs::create_dir_all(&dir).unwrap();
    dir
}

const ROWS: &str = r#"[
 {"host":"hub","location":"Media","path":"Games/Big","home_path":null,"bytes":50000000000,"as_of":900,"kind":"volume"},
 {"host":"hub","location":"internal","path":"Users/u/Music","home_path":"Music","bytes":200000000,"as_of":950,"kind":"volume"},
 {"host":"laptop","location":"internal","path":"Users/u/Fake","home_path":"Fake","bytes":900000000,"as_of":990,"kind":"volume"},
 {"host":"other","location":"Media","path":"Games/Other","home_path":null,"bytes":900000000,"as_of":990,"kind":"volume"}
]"#;

#[test]
fn a_peers_where_rows_are_cached_with_measured_and_fetched_times() {
    let state = tmp("where");
    let doc = store_where(&state, "hub", "laptop", ROWS.as_bytes(), 1000).unwrap();
    assert_eq!(doc.matches.len(), 2);
    assert_eq!(doc.generated, 950, "measured = newest row");
    assert_eq!(doc.fetched, 1000);
    assert!(where_path(&state, "hub").ends_with("peers/hub.where.json"));
    let back = read_where(&state, "hub").unwrap();
    assert_eq!(back.matches[0].location, "Media");
    let _ = fs::remove_dir_all(&state);
}

#[test]
fn a_bad_or_empty_answer_keeps_the_last_cache() {
    let state = tmp("keep");
    store_where(&state, "hub", "laptop", ROWS.as_bytes(), 1000).unwrap();
    assert!(store_where(&state, "hub", "laptop", b"", 2000).is_err());
    assert!(store_where(&state, "hub", "laptop", b"[]", 2000).is_err());
    assert!(store_where(&state, "hub", "laptop", b"ssh: connect refused", 2000).is_err());
    let kept = read_where(&state, "hub").unwrap();
    assert_eq!(kept.fetched, 1000);
    assert_eq!(kept.matches.len(), 2);
    assert!(store_summary(&state, "hub", "laptop", b"not json").is_err());
    assert!(!summary_path(&state, "hub").exists());
    let _ = fs::remove_dir_all(&state);
}

#[test]
fn a_peer_cannot_speak_for_this_mac_or_a_third_mac() {
    let state = tmp("trust");
    let doc = store_where(&state, "hub", "laptop", ROWS.as_bytes(), 1000).unwrap();
    assert!(doc.matches.iter().all(|r| r.host == "hub"), "rows for laptop and other dropped");
    assert_eq!(doc.matches.len(), 2);
    assert!(store_where(&state, "laptop", "laptop", ROWS.as_bytes(), 1000).is_err(), "never cache this Mac as a peer");
    assert!(!where_path(&state, "laptop").exists());
    let only_others = r#"[{"host":"other","location":"Media","path":"X","home_path":null,"bytes":1,"as_of":1,"kind":"volume"}]"#;
    assert!(store_where(&state, "hub", "laptop", only_others.as_bytes(), 1000).is_err());
    assert!(store_where(&state, "../x", "laptop", ROWS.as_bytes(), 1000).is_err());
    let _ = fs::remove_dir_all(&state);
}
