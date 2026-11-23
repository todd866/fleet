use diskmap::*;

fn file(allocated: u64, logical: u64, flags: u32) -> FileAttr {
    FileAttr {
        allocated,
        logical,
        flags,
        mtime: 10,
        is_dir: false,
        mount_point: false,
        symlink: false,
        links: 1,
    }
}

#[test]
fn dataless_bytes_are_not_counted_as_local() {
    let entries = [
        ("local.txt", file(500, 500, 0)),
        ("cloud.txt", file(0, 1_000, SF_DATALESS)),
        ("partial.txt", file(100, 400, SF_DATALESS)),
    ];
    let acc = account_entries(&entries.iter().map(|(n, a)| (*n, a.clone())).collect::<Vec<_>>());
    assert_eq!(acc.own_bytes, 600);
    assert_eq!(acc.own_dataless, 1_300);
    assert_eq!(acc.files, 3);
}

#[test]
fn mount_points_are_not_walked_as_children() {
    let dir = FileAttr {
        allocated: 0,
        logical: 0,
        flags: 0,
        mtime: 1,
        is_dir: true,
        mount_point: true,
        symlink: false,
        links: 1,
    };
    let acc = account_entries(&[("Volumes", dir)]);
    assert!(acc.subdirs.is_empty());
}

#[test]
fn photos_originals_and_derivatives() {
    assert_eq!(
        photos_kind("Pictures/Photos Library.photoslibrary/originals/0/a.jpg"),
        PhotoKind::Originals
    );
    assert_eq!(
        photos_kind("Pictures/Photos Library.photoslibrary/resources/derivatives/a.jpg"),
        PhotoKind::Derivatives
    );
    assert_eq!(
        photos_kind("Pictures/Photos Library.photoslibrary/database/Photos.sqlite"),
        PhotoKind::Other
    );
}

#[test]
fn config_defaults_follow_local_host_name() {
    let cfg = default_config();
    let desk = resolve_host(&cfg, "Coordinator-Mac");
    assert_eq!(desk.id, "desk");
    assert_eq!(desk.floor_gb, Some(150.0));
    let lab = resolve_host(&cfg, "Worker-One");
    assert_eq!(lab.id, "lab");
    assert_eq!(lab.floor_gb, Some(40.0));
    let studio = resolve_host(&cfg, "Worker-Two");
    assert_eq!(studio.id, "studio");
    assert_eq!(studio.floor_gb, Some(25.0));
    let other = resolve_host(&cfg, "studio");
    assert_eq!(other.id, "studio");
    assert_eq!(other.floor_gb, None);
}

#[test]
fn config_file_overrides_one_floor_and_keeps_the_rest() {
    let cfg = load_config(Some(
        r#"{"hosts":{"Worker-Two":{"id":"studio","floor_gb":10}},"peers":["lab"]}"#,
    ))
    .unwrap();
    assert_eq!(resolve_host(&cfg, "Worker-Two").floor_gb, Some(10.0));
    assert_eq!(resolve_host(&cfg, "Coordinator-Mac").floor_gb, Some(150.0));
    assert_eq!(cfg.peers, vec!["lab".to_string()]);
    assert_eq!(cfg.event_min_bytes, EVENT_MIN_BYTES);
    assert_eq!(cfg.hot_days, 30);
}

#[test]
fn fsevents_coalesce_to_one_subtree_named_dirs_and_file_parents() {
    let events = [
        RawEvent { path: "a/b/c".into(), flags: 0, id: 1 },
        RawEvent { path: "a/b/c".into(), flags: 0, id: 2 },
        RawEvent { path: "a/b".into(), flags: MUST_SCAN_SUBDIRS, id: 4 },
        RawEvent { path: "a/b/d".into(), flags: 0, id: 3 },
        RawEvent { path: "z/file".into(), flags: ITEM_IS_FILE, id: 5 },
        RawEvent { path: "z/file".into(), flags: ITEM_IS_FILE, id: 6 },
    ];
    let scans = coalesce(&events);
    assert_eq!(
        scans,
        vec![
            ScanReq::Subtree("a/b".into()),
            ScanReq::Dir("a/b/c".into()),
            ScanReq::Dir("a/b/d".into()),
            ScanReq::Dir("z".into()),
        ]
    );
}

#[test]
fn grew_collapses_to_the_dir_that_explains_eighty_percent() {
    let rows = [
        Growth { path: "Users/a/big/data".into(), delta: 300 },
        Growth { path: "Users/a/big/other".into(), delta: 10 },
        Growth { path: "Users/a/small".into(), delta: 5 },
    ];
    let grew = collapse_grew(&rows).unwrap();
    assert_eq!(grew.path, "Users/a/big/data");
    assert_eq!(grew.delta, 300);
    assert!(grew.share >= 0.8);

    let split = [
        Growth { path: "Users/a/x".into(), delta: 100 },
        Growth { path: "Users/a/y".into(), delta: 100 },
    ];
    let grew = collapse_grew(&split).unwrap();
    assert_eq!(grew.path, "Users/a");
    assert_eq!(grew.delta, 200);
}

#[test]
fn grew_uses_inclusive_parent_deltas_without_double_counting() {
    let rows = [
        Growth { path: "keep".into(), delta: 300 },
        Growth { path: "keep/media".into(), delta: 300 },
        Growth { path: "keep/media/films".into(), delta: 300 },
    ];
    let grew = collapse_grew(&rows).unwrap();
    assert_eq!(grew.path, "keep/media/films");
    assert_eq!(grew.delta, 300);
}

#[test]
fn free_space_rate_needs_thirty_minutes() {
    let now = 10_000;
    let steep = [(now - 1800, 200.0), (now, 190.0)];
    let rate = sustained_drop(&steep, now, 1800, 15.0).unwrap();
    assert!((rate - 20.0).abs() < 0.1);
    let short = [(now - 600, 200.0), (now, 100.0)];
    assert!(sustained_drop(&short, now, 1800, 15.0).is_none());
    let slow = [(now - 1800, 200.0), (now, 195.0)];
    assert!(sustained_drop(&slow, now, 1800, 15.0).is_none());
}

#[test]
fn trend_is_the_change_in_free_gb() {
    let now = 100_000;
    let samples = [
        (now - 7 * 86400, 100.0),
        (now - 86400, 90.0),
        (now - 3600, 80.0),
        (now, 70.0),
    ];
    let trend = trend(&samples, now);
    assert_eq!(trend.h1, Some(-10.0));
    assert_eq!(trend.h24, Some(-20.0));
    assert_eq!(trend.d7, Some(-30.0));
    let text = "{\"t\":1,\"diskFreeGB\":12.5,\"host\":\"x\"}\n";
    assert_eq!(samples_from_jsonl(text), vec![(1, 12.5)]);
}

#[test]
fn alerts_are_throttled_per_cause() {
    assert!(floor_breach(100.0, Some(150.0)));
    assert!(!floor_breach(200.0, Some(150.0)));
    assert!(!floor_breach(10.0, None));
    let first = Alert {
        t: 1_000,
        cause: "floor:h1:internal".into(),
        host: "h1".into(),
        location: "internal".into(),
        free_gb: 100.0,
        rate_gb_h: None,
        grower: "Movies".into(),
        message: "Movies, 100 GB free".into(),
    };
    let again = Alert { t: 2_000, ..first.clone() };
    let later = Alert { t: 5_000, ..first.clone() };
    assert!(gate_alerts(vec![again], &[first.clone()], 3600).is_empty());
    assert_eq!(gate_alerts(vec![later], &[first], 3600).len(), 1);
}

#[test]
fn shared_container_is_one_drive_and_bundles_are_not_added() {
    let defs = parse_drives(
        r#"{"drives":[
            {"name":"h2-ssd-2tb","volumes":{
                "Old Mirror":{"uuid":"AAA","encrypted":false,"shared_container":true,"size_gb":2000},
                "Vault-B":{"uuid":"BBB","encrypted":true,"shared_container":true,"size_gb":2000}
            }},
            {"name":"t9-1","volumes":{
                "Research":{"uuid":"CCC","encrypted":false,"size_gb":700},
                "Archive_Encrypted":{"uuid":"DDD","encrypted":true,"bundle":"/Volumes/Research/x.sparsebundle","size_gb":400}
            }}
        ]}"#,
    )
    .unwrap();
    let obs = vec![
        VolumeObs { uuid: "AAA".into(), name: "Old Mirror".into(), total_bytes: 2_000, free_bytes: 500, mounted: true, last_seen: 10, encrypted: false },
        VolumeObs { uuid: "BBB".into(), name: "Vault-B".into(), total_bytes: 2_000, free_bytes: 500, mounted: true, last_seen: 10, encrypted: true },
        VolumeObs { uuid: "CCC".into(), name: "Research".into(), total_bytes: 700, free_bytes: 100, mounted: true, last_seen: 10, encrypted: false },
        VolumeObs { uuid: "DDD".into(), name: "Archive_Encrypted".into(), total_bytes: 400, free_bytes: 50, mounted: true, last_seen: 10, encrypted: true },
    ];
    let rows = group_drives(&defs, &obs);
    let ssd = rows.iter().find(|r| r.name == "h2-ssd-2tb").unwrap();
    assert_eq!(ssd.total_bytes, 2_000);
    assert_eq!(ssd.free_bytes, 500);
    assert!(ssd.encrypted_mounted);
    let t9 = rows.iter().find(|r| r.name == "t9-1").unwrap();
    assert_eq!(t9.total_bytes, 700);
    assert_eq!(t9.free_bytes, 100);
}

#[test]
fn archive_refuses_anything_but_an_encrypted_volume() {
    let defs = parse_drives(
        r#"{"drives":[{"name":"h2-ssd-2tb","volumes":{
            "Vault-B":{"uuid":"BBB","encrypted":true},
            "Old Mirror":{"uuid":"AAA","encrypted":false}
        }}]}"#,
    )
    .unwrap();
    assert!(archive_allowed("/Volumes/Vault-B/diskmap", &defs).is_ok());
    assert!(archive_allowed("/Volumes/Old Mirror/diskmap", &defs).is_err());
    assert!(archive_allowed("/tmp/diskmap", &defs).is_err());
    assert!(archive_allowed("/Volumes/Spare/diskmap", &defs).is_err());
}

#[test]
fn idrive_success_requires_zero_failures() {
    let xml = r#"<sessions><record><datetime>10-30-2026 21:00:00</datetime><status>Success</status><logfile>a.log</logfile></record>
        <record><datetime>10-31-2026 23:00:00</datetime><status>Failure</status><logfile>b.log</logfile></record></sessions>"#;
    let records = parse_log_records(xml);
    assert_eq!(records.len(), 2);
    assert!(confirm_success(&records[0].status, Some(0)));
    assert!(!confirm_success(&records[1].status, Some(0)));
    assert!(!confirm_success("Success", Some(3)));
    let summary = parse_backup_summary("End Time: 21:00\nFiles failed to backup: 0\nFiles considered for backup: 12\n");
    assert_eq!(summary.failed, Some(0));
    assert_eq!(summary.considered, Some(12));
    assert!(parse_local_mdy("10-30-2026 21:00:00").is_some());
}

#[test]
fn where_matches_basename_or_glob() {
    assert!(where_match("Films", "Users/a/Movies/Films"));
    assert!(where_match("Movies/*", "Movies/Films"));
    assert!(!where_match("Photos", "Movies/Films"));
}

#[test]
fn fleet_rows_share_one_width() {
    let disk = Location {
        kind: "volume".into(),
        id: "h1:internal".into(),
        host: "h1".into(),
        name: "internal".into(),
        volume_uuid: None,
        physical_drive: Some("internal".into()),
        mount: None,
        mounted: true,
        encrypted: true,
        total_bytes: 494_000_000_000,
        free_bytes: 149_000_000_000,
        local_bytes: 0,
        dataless_bytes: 0,
        trend_bytes: TrendBytes { h1: Some(-400_000_000), h24: Some(-2_000_000_000), d7: Some(-11_000_000_000) },
        top_grower: Some(Grower { path: "Users/a/Movies".into(), delta_bytes: 30_000_000_000 }),
        last_seen: 1_000,
        detail: String::new(),
        folders: Vec::new(),
        map: Some(MapStatus { dirs: 20_000, percent: 100, done: true }),
    };
    let cloud = Location {
        kind: "icloud".into(),
        id: "h1:icloud".into(),
        host: "h1".into(),
        name: "iCloud".into(),
        volume_uuid: None,
        physical_drive: None,
        mount: None,
        mounted: true,
        encrypted: false,
        total_bytes: 120_000_000_000,
        free_bytes: 0,
        local_bytes: 40_000_000_000,
        dataless_bytes: 80_000_000_000,
        trend_bytes: TrendBytes { h1: None, h24: None, d7: None },
        top_grower: None,
        last_seen: 900,
        detail: String::new(),
        folders: Vec::new(),
        map: None,
    };
    let summary = Summary {
        generated: 1_000,
        host_id: "h1".into(),
        local_host_name: "MacBook-Air".into(),
        locations: vec![disk, cloud],
        drives: Vec::new(),
        alerts: vec![Alert {
            t: 1_000,
            cause: "floor:h1:internal".into(),
            host: "h1".into(),
            location: "internal".into(),
            free_gb: 149.0,
            rate_gb_h: None,
            grower: "Movies".into(),
            message: "Movies, 149 GB free".into(),
        }],
        access: None,
    };
    let text = format_fleet(&fleet_lines(&[summary], 1_000));
    let lines: Vec<&str> = text.lines().collect();
    assert!(lines.len() >= 3);
    let width = lines[0].chars().count();
    assert!(lines.iter().all(|l| l.chars().count() == width));
    assert!(lines[1].contains("internal"));
    assert!(lines[1].starts_with('!'));
    assert!(lines[1].contains("done"));
    assert!(lines[0].contains("map"));
    assert!(lines.iter().any(|l| l.contains("iCloud")));
    assert!(!text.contains("Needs attention"));
}

#[test]
fn durations() {
    assert_eq!(parse_since("24h").unwrap(), 86_400);
    assert_eq!(parse_since("7d").unwrap(), 7 * 86_400);
    assert_eq!(parse_since("30m").unwrap(), 1_800);
}
