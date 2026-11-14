use placement::*;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const NOW: i64 = 1_700_000_000;
const DAY: i64 = 86_400;

static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

struct Tmp(PathBuf);
impl Tmp {
    fn new() -> Self {
        let n = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("placement-{nanos}-{n}"));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}
impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn gb(n: u64) -> u64 {
    n * GB
}

fn host(id: &str, role: Role, floor: f64, low: Option<f64>, high: Option<f64>, bulk: Bulk) -> Host {
    Host {
        id: id.into(),
        role,
        floor_gb: floor,
        band_low_gb: low,
        band_high_gb: high,
        bulk,
    }
}

fn media() -> Volume {
    Volume {
        name: "media".into(),
        host: "hub".into(),
        encrypted: false,
        accepts: vec![Class::Reobtainable],
        subpath: String::new(),
    }
}

fn games_policy() -> Policy {
    Policy {
        hosts: vec![
            host(
                "primary",
                Role::TravellingPrimary,
                150.0,
                Some(200.0),
                Some(400.0),
                Bulk::WorkingSet,
            ),
            host(
                "hub",
                Role::HomeHub,
                40.0,
                None,
                None,
                Bulk::MastersOnExternal,
            ),
            host("shared", Role::SharedLaptop, 30.0, None, None, Bulk::NoBulk),
        ],
        volumes: vec![media()],
        datasets: vec![],
        sync_folders: vec![SyncFolder {
            name: "games".into(),
            root: "Games".into(),
            method: SyncMethod::Syncthing,
            played_within_days: 30,
            parts: vec![
                Part {
                    path: "Saves".into(),
                    class: Class::SyncedSmall,
                    keep: vec!["*".into()],
                    master_host: String::new(),
                    master_volume: String::new(),
                    pins: vec![],
                    archive: false,
                },
                Part {
                    path: "ROMs".into(),
                    class: Class::Reobtainable,
                    keep: vec![],
                    master_host: "hub".into(),
                    master_volume: "media".into(),
                    pins: vec![],
                    archive: true,
                },
                Part {
                    path: "*".into(),
                    class: Class::Reobtainable,
                    keep: vec!["primary".into()],
                    master_host: "hub".into(),
                    master_volume: "media".into(),
                    pins: vec![],
                    archive: false,
                },
            ],
        }],
        pins: vec![],
    }
}

fn present(host: &str, path: &str, bytes: u64) -> Present {
    Present {
        host: host.into(),
        location: "internal".into(),
        path: path.into(),
        bytes,
    }
}

fn free(host: &str, bytes: u64) -> Free {
    Free {
        host: host.into(),
        free_bytes: bytes,
    }
}

fn access(host: &str, path: &str, when: i64) -> Access {
    Access {
        host: host.into(),
        path: path.into(),
        last_access: when,
    }
}

fn obs(
    frees: Vec<Free>,
    present_rows: Vec<Present>,
    access_rows: Vec<Access>,
    replicas: Vec<Replica>,
) -> Observation {
    Observation {
        now: NOW,
        diskmap_present: true,
        frees,
        present: present_rows,
        access: access_rows,
        replicas,
    }
}

fn action<'a>(plan: &'a Plan, host: &str, path: &str) -> &'a Action {
    plan.actions
        .iter()
        .find(|a| a.host == host && a.path == path)
        .unwrap_or_else(|| panic!("no action for {host} {path}"))
}

#[test]
fn floors_bands_and_a_hard_shared_floor() {
    let primary = host(
        "primary",
        Role::TravellingPrimary,
        150.0,
        Some(200.0),
        Some(400.0),
        Bulk::WorkingSet,
    );
    assert_eq!(
        budget_state(&primary, Some(gb(120))),
        BudgetState::BelowFloor
    );
    assert_eq!(
        budget_state(&primary, Some(gb(180))),
        BudgetState::BelowBand
    );
    assert_eq!(budget_state(&primary, Some(gb(250))), BudgetState::InBand);
    assert_eq!(
        budget_state(&primary, Some(gb(450))),
        BudgetState::AboveBand
    );
    let shared = host("shared", Role::SharedLaptop, 30.0, None, None, Bulk::NoBulk);
    assert_eq!(budget_state(&shared, Some(gb(5))), BudgetState::BelowFloor);
    assert_eq!(budget_state(&shared, Some(gb(45))), BudgetState::InBand);
    assert_eq!(budget_state(&shared, None), BudgetState::Unknown);
}

#[test]
fn scratch_is_cleared_before_a_stale_install_when_under_the_band() {
    let mut policy = games_policy();
    policy.datasets.push(Dataset {
        name: "renders".into(),
        class: Class::Scratch,
        paths: vec!["tmp/renders".into()],
        ..Dataset::default()
    });
    let plan = evaluate(
        &policy,
        &obs(
            vec![
                free("primary", gb(180)),
                free("hub", gb(80)),
                free("shared", gb(40)),
            ],
            vec![
                present("primary", "tmp/renders", gb(8)),
                present("primary", "Games/Demo", gb(12)),
            ],
            vec![access("primary", "Games/Demo", NOW - 60 * DAY)],
            vec![],
        ),
    );
    let ops: Vec<_> = plan
        .actions
        .iter()
        .filter(|a| a.host == "primary" && matches!(a.kind, ActionKind::Clear | ActionKind::Move))
        .collect();
    assert_eq!(ops[0].kind, ActionKind::Clear);
    assert_eq!(ops[0].path, "tmp/renders");
    assert!(
        ops.iter()
            .any(|a| a.path == "Games/Demo" && a.kind == ActionKind::Move)
    );
}

#[test]
fn an_empty_replica_is_not_a_verified_copy() {
    let mut policy = games_policy();
    policy.datasets.push(photos());
    let plan = evaluate(
        &policy,
        &obs(
            vec![
                free("primary", gb(100)),
                free("hub", gb(80)),
                free("shared", gb(40)),
            ],
            vec![present("primary", "Pictures/Library", gb(20))],
            vec![],
            vec![
                replica("Pictures/Library", "vault-a", true, true, false, gb(20)),
                replica("Pictures/Library", "vault-b", true, true, false, 0),
            ],
        ),
    );
    let photos = action(&plan, "primary", "Pictures/Library");
    assert_eq!(photos.kind, ActionKind::Refuse);
    assert!(
        plan.violations
            .iter()
            .any(|v| v.code == "personal-copies" && v.detail.contains('1'))
    );
    assert!(
        plan.actions
            .iter()
            .all(|a| a.path != "Pictures/Library" || a.kind != ActionKind::Move)
    );
    assert!(
        plan.actions
            .iter()
            .all(|a| a.path != "Pictures/Library" || a.kind != ActionKind::Clear)
    );
}

#[test]
fn personal_unique_may_leave_a_laptop_once_two_encrypted_copies_are_verified() {
    let mut policy = games_policy();
    policy.datasets.push(photos());
    let plan = evaluate(
        &policy,
        &obs(
            vec![
                free("primary", gb(100)),
                free("hub", gb(80)),
                free("shared", gb(40)),
            ],
            vec![present("primary", "Pictures/Library", gb(20))],
            vec![],
            vec![
                replica("Pictures/Library", "vault-a", true, true, false, gb(20)),
                replica("Pictures/Library", "vault-b", true, true, false, gb(20)),
                replica("Pictures/Library", "offsite", false, true, true, gb(20)),
            ],
        ),
    );
    assert_eq!(
        action(&plan, "primary", "Pictures/Library").kind,
        ActionKind::Move
    );
    assert!(
        !plan
            .violations
            .iter()
            .any(|v| v.code == "personal-copies" || v.code == "offsite")
    );
}

fn photos() -> Dataset {
    Dataset {
        name: "Photos library".into(),
        class: Class::PersonalUnique,
        paths: vec!["Pictures/Library".into()],
        master_host: "hub".into(),
        master_volume: "vault-a".into(),
        keep: vec!["primary".into()],
        replica_volumes: vec!["vault-a".into(), "vault-b".into()],
        offsite: "offsite-backup".into(),
        ..Dataset::default()
    }
}

fn replica(
    path: &str,
    volume: &str,
    encrypted: bool,
    verified: bool,
    offsite: bool,
    bytes: u64,
) -> Replica {
    Replica {
        path: path.into(),
        volume: volume.into(),
        encrypted,
        verified,
        offsite,
        bytes,
    }
}

#[test]
fn saves_sync_everywhere_and_the_shared_laptop_drops_unplayed_installs() {
    let plan = evaluate(
        &games_policy(),
        &obs(
            vec![
                free("primary", gb(250)),
                free("hub", gb(80)),
                free("shared", gb(40)),
            ],
            vec![
                present("primary", "Games/Saves", 1_000),
                present("hub", "Games/Saves", 1_000),
                present("shared", "Games/Saves", 1_000),
                present("primary", "Games/Demo", gb(4)),
                present("shared", "Games/Demo", gb(4)),
                present("hub", "Games/Demo", gb(4)),
            ],
            vec![access("primary", "Games/Demo", NOW - DAY)],
            vec![],
        ),
    );
    for host in ["primary", "hub", "shared"] {
        let kind = action(&plan, host, "Games/Saves").kind;
        assert!(
            matches!(kind, ActionKind::Keep | ActionKind::Sync),
            "{host} {kind:?}"
        );
    }
    assert_eq!(
        action(&plan, "primary", "Games/Demo").kind,
        ActionKind::Keep
    );
    assert_eq!(action(&plan, "shared", "Games/Demo").kind, ActionKind::Move);
    assert_eq!(action(&plan, "hub", "Games/Demo").kind, ActionKind::Move);
}

#[test]
fn primary_keeps_an_install_with_no_access_record_until_the_floor() {
    let above = evaluate(
        &games_policy(),
        &obs(
            vec![
                free("primary", gb(180)),
                free("hub", gb(80)),
                free("shared", gb(40)),
            ],
            vec![present("primary", "Games/Demo", gb(4))],
            vec![],
            vec![],
        ),
    );
    assert_eq!(
        action(&above, "primary", "Games/Demo").kind,
        ActionKind::Keep
    );
    let through = evaluate(
        &games_policy(),
        &obs(
            vec![
                free("primary", gb(100)),
                free("hub", gb(80)),
                free("shared", gb(40)),
            ],
            vec![present("primary", "Games/Demo", gb(4))],
            vec![],
            vec![],
        ),
    );
    assert_eq!(
        action(&through, "primary", "Games/Demo").kind,
        ActionKind::Move
    );
}

#[test]
fn a_game_opened_on_the_shared_laptop_within_30_days_stays() {
    let plan = evaluate(
        &games_policy(),
        &obs(
            vec![
                free("primary", gb(250)),
                free("hub", gb(80)),
                free("shared", gb(40)),
            ],
            vec![present("shared", "Games/Played", gb(2))],
            vec![access("shared", "Games/Played", NOW - 10 * DAY)],
            vec![],
        ),
    );
    assert_eq!(
        action(&plan, "shared", "Games/Played").kind,
        ActionKind::Keep
    );
}

#[test]
fn archives_stay_on_the_hub_external_only() {
    let plan = evaluate(
        &games_policy(),
        &obs(
            vec![
                free("primary", gb(250)),
                free("hub", gb(80)),
                free("shared", gb(40)),
            ],
            vec![
                present("primary", "Games/ROMs", gb(30)),
                present("shared", "Games/ROMs", gb(30)),
                present("hub", "Games/ROMs", gb(30)),
            ],
            vec![],
            vec![],
        ),
    );
    for host in ["primary", "shared", "hub"] {
        assert_eq!(
            action(&plan, host, "Games/ROMs").kind,
            ActionKind::Move,
            "{host}"
        );
    }
}

#[test]
fn stignore_includes_saves_and_played_installs_before_the_catch_all() {
    let policy = games_policy();
    let seen = obs(
        vec![
            free("shared", gb(40)),
            free("primary", gb(250)),
            free("hub", gb(80)),
        ],
        vec![
            present("shared", "Games/Played", gb(2)),
            present("shared", "Games/Demo", gb(4)),
            present("shared", "Games/ROMs", gb(9)),
        ],
        vec![access("shared", "Games/Played", NOW - 10 * DAY)],
        vec![],
    );
    let text = stignore(&policy, &seen, "shared", "games");
    let lines: Vec<_> = text
        .lines()
        .filter(|l| !l.starts_with("//") && !l.is_empty())
        .collect();
    assert!(lines.contains(&"!/Saves"));
    assert!(lines.contains(&"!/Saves/**"));
    assert!(lines.contains(&"!/Played"));
    assert!(lines.contains(&"!/Played/**"));
    assert!(
        !lines
            .iter()
            .any(|l| l.contains("Demo") && l.starts_with('!'))
    );
    assert!(
        !lines
            .iter()
            .any(|l| l.contains("ROMs") && l.starts_with('!'))
    );
    assert_eq!(*lines.last().unwrap(), "*");
    let star = lines.iter().position(|l| *l == "*").unwrap();
    let saves = lines.iter().position(|l| *l == "!/Saves").unwrap();
    assert!(saves < star);
    assert!(!text.contains("(?d)"));
}

#[test]
fn status_is_one_row_per_host_and_one_per_data_issue() {
    let mut policy = games_policy();
    policy.datasets.push(photos());
    let plan = evaluate(
        &policy,
        &obs(
            vec![
                free("primary", gb(120)),
                free("hub", gb(80)),
                free("shared", gb(50)),
            ],
            vec![present("primary", "Pictures/Library", gb(20))],
            vec![],
            vec![
                replica("Pictures/Library", "vault-a", true, true, false, gb(20)),
                replica("Pictures/Library", "offsite", false, true, true, gb(20)),
            ],
        ),
    );
    let text = format_status(&plan);
    let filled: Vec<_> = text.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(filled.len(), 6, "{text}");
    assert!(
        filled[0].contains("free") && filled[0].contains("floor") && filled[0].contains("band")
    );
    assert!(
        filled[1].starts_with('!')
            && filled[1].contains("primary")
            && filled[1].contains("below floor")
    );
    assert!(filled[2].starts_with('*') && filled[2].contains("hub"));
    assert!(filled[3].starts_with('*') && filled[3].contains("shared"));
    assert!(filled[5].starts_with('!') && filled[5].contains("1 encrypted copy"));
    assert!(!text.contains("##"));
    assert!(!text.to_lowercase().contains("violation"));
    assert!(!text.contains("(?d)"));
    let doc = status_file(&plan, NOW);
    assert!(!doc.ok);
    assert_eq!(doc.hosts[0].state, "below-floor");
}

#[test]
fn apply_copies_verifies_trashes_and_leaves_the_other_host_alone() {
    let tmp = Tmp::new();
    let home = tmp.path().join("hosts/primary");
    let hub = tmp.path().join("hosts/hub");
    let src = home.join("Games/Demo");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("readme.txt"), b"hello placement").unwrap();
    fs::create_dir_all(&hub).unwrap();
    fs::write(hub.join("sentinel"), b"leave me").unwrap();
    let report = run_move(&tmp, &home, false, false, None);
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert!(!report.deferred);
    let dest = tmp.path().join("volumes/media/Games/Demo/readme.txt");
    assert_eq!(fs::read(&dest).unwrap(), b"hello placement");
    assert!(!src.exists());
    let trashed = only_child(&tmp.path().join("trash"));
    assert_eq!(
        fs::read(trashed.join("readme.txt")).unwrap(),
        b"hello placement"
    );
    let stub = fs::read_to_string(home.join("Games/Demo.OFFLOADED.txt")).unwrap();
    assert!(stub.contains("Why that is acceptable"));
    assert!(stub.contains("volumes/media/Games/Demo") || stub.contains("media"));
    let ledger = fs::read_to_string(tmp.path().join("OFFLOADS.md")).unwrap();
    assert!(ledger.contains("| trash |"));
    assert!(ledger.contains("`Games/Demo`"));
    assert_eq!(fs::read(hub.join("sentinel")).unwrap(), b"leave me");
    assert!(!hub.join("Games").exists());
}

#[test]
fn apply_copy_fallback_still_leaves_the_bytes_in_trash() {
    let tmp = Tmp::new();
    let home = tmp.path().join("hosts/primary");
    fs::create_dir_all(home.join("Games/Demo")).unwrap();
    fs::write(home.join("Games/Demo/readme.txt"), b"copied").unwrap();
    let report = run_move(&tmp, &home, true, false, None);
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert!(!home.join("Games/Demo").exists());
    let trashed = only_child(&tmp.path().join("trash"));
    assert_eq!(fs::read(trashed.join("readme.txt")).unwrap(), b"copied");
    assert_eq!(
        fs::read(tmp.path().join("volumes/media/Games/Demo/readme.txt")).unwrap(),
        b"copied"
    );
}

#[test]
fn apply_leaves_the_source_when_verification_fails() {
    let tmp = Tmp::new();
    let home = tmp.path().join("hosts/primary");
    fs::create_dir_all(home.join("Games/Demo")).unwrap();
    fs::write(home.join("Games/Demo/readme.txt"), b"keep me").unwrap();
    let report = run_move(&tmp, &home, false, true, None);
    assert!(!report.errors.is_empty());
    assert_eq!(
        fs::read(home.join("Games/Demo/readme.txt")).unwrap(),
        b"keep me"
    );
    assert!(!tmp.path().join("volumes/media/Games/Demo").exists());
    assert!(!home.join("Games/Demo.OFFLOADED.txt").exists());
}

#[test]
fn apply_refuses_personal_unique_with_one_verified_copy_even_if_the_plan_says_move() {
    let tmp = Tmp::new();
    let home = tmp.path().join("home");
    fs::create_dir_all(home.join("Pictures/Library")).unwrap();
    fs::write(home.join("Pictures/Library/a.jpg"), b"only copy").unwrap();
    let policy = Policy {
        volumes: vec![Volume {
            name: "vault-a".into(),
            host: "hub".into(),
            encrypted: true,
            accepts: vec![Class::PersonalUnique],
            subpath: String::new(),
        }],
        ..Policy::default()
    };
    let seen = obs(
        vec![],
        vec![],
        vec![],
        vec![replica(
            "Pictures/Library",
            "vault-a",
            true,
            true,
            false,
            10,
        )],
    );
    let plan = Plan {
        actions: vec![Action {
            host: "primary".into(),
            kind: ActionKind::Move,
            path: "Pictures/Library".into(),
            bytes: 10,
            dest: "vault-a".into(),
            reason: "below floor".into(),
            class: Class::PersonalUnique,
            label: "Photos library".into(),
            icloud: false,
        }],
        budgets: vec![],
        violations: vec![],
    };
    let mut opts = opts_for(&tmp, &home);
    let report = apply(&policy, &seen, &plan, &opts).unwrap();
    assert!(report.moved.is_empty());
    assert!(!report.refused.is_empty());
    assert_eq!(
        fs::read(home.join("Pictures/Library/a.jpg")).unwrap(),
        b"only copy"
    );
    assert!(
        fs::read_dir(tmp.path().join("trash"))
            .map(|mut d| d.next().is_none())
            .unwrap_or(true)
    );
    let _ = &mut opts;
}

#[test]
fn apply_defers_when_the_volume_writer_lock_is_live_and_ignores_a_stale_one() {
    let tmp = Tmp::new();
    let home = tmp.path().join("hosts/primary");
    fs::create_dir_all(home.join("Games/Demo")).unwrap();
    fs::write(home.join("Games/Demo/readme.txt"), b"locked").unwrap();
    let volume = tmp.path().join("volumes/media");
    fs::create_dir_all(&volume).unwrap();
    fs::write(
        volume.join(".writer"),
        format!("other@hub otherhost 1 {NOW}\n"),
    )
    .unwrap();
    let report = run_move(&tmp, &home, false, false, None);
    assert!(report.deferred);
    assert_eq!(
        fs::read(home.join("Games/Demo/readme.txt")).unwrap(),
        b"locked"
    );

    fs::write(
        volume.join(".writer"),
        format!("other@hub otherhost 1 {}\n", NOW - 50_000),
    )
    .unwrap();
    let report = run_move(&tmp, &home, false, false, None);
    assert!(!report.deferred, "{}", report.defer_reason);
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert!(!home.join("Games/Demo").exists());
}

#[test]
fn apply_skips_icloud_paths_on_a_metered_link_and_still_moves_local_games() {
    let tmp = Tmp::new();
    let home = tmp.path().join("hosts/primary");
    fs::create_dir_all(home.join("Documents/Taxes")).unwrap();
    fs::write(home.join("Documents/Taxes/a.txt"), b"cloud").unwrap();
    fs::create_dir_all(home.join("Games/Demo")).unwrap();
    fs::write(home.join("Games/Demo/readme.txt"), b"local").unwrap();
    let policy = Policy {
        volumes: vec![media()],
        ..Policy::default()
    };
    let plan = Plan {
        actions: vec![
            Action {
                host: "primary".into(),
                kind: ActionKind::Move,
                path: "Documents/Taxes".into(),
                bytes: 5,
                dest: "media".into(),
                reason: "icloud".into(),
                class: Class::Reobtainable,
                label: "taxes".into(),
                icloud: true,
            },
            Action {
                host: "primary".into(),
                kind: ActionKind::Move,
                path: "Games/Demo".into(),
                bytes: 5,
                dest: "media".into(),
                reason: "not placed".into(),
                class: Class::Reobtainable,
                label: "games/Demo".into(),
                icloud: false,
            },
        ],
        budgets: vec![],
        violations: vec![],
    };
    let mut opts = opts_for(&tmp, &home);
    opts.metered = true;
    let report = apply(&policy, &obs(vec![], vec![], vec![], vec![]), &plan, &opts).unwrap();
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert_eq!(
        fs::read(home.join("Documents/Taxes/a.txt")).unwrap(),
        b"cloud"
    );
    assert!(!home.join("Games/Demo").exists());
    assert_eq!(
        fs::read(tmp.path().join("volumes/media/Games/Demo/readme.txt")).unwrap(),
        b"local"
    );
}

#[test]
fn apply_defers_everything_when_focus_is_active() {
    let tmp = Tmp::new();
    let home = tmp.path().join("hosts/primary");
    fs::create_dir_all(home.join("Games/Demo")).unwrap();
    fs::write(home.join("Games/Demo/readme.txt"), b"busy").unwrap();
    let mut opts = opts_for(&tmp, &home);
    opts.focus_active = true;
    let plan = Plan {
        actions: vec![Action {
            host: "primary".into(),
            kind: ActionKind::Move,
            path: "Games/Demo".into(),
            bytes: 4,
            dest: "media".into(),
            reason: "not placed".into(),
            class: Class::Reobtainable,
            label: "games/Demo".into(),
            icloud: false,
        }],
        budgets: vec![],
        violations: vec![],
    };
    let report = apply(
        &games_policy(),
        &obs(vec![], vec![], vec![], vec![]),
        &plan,
        &opts,
    )
    .unwrap();
    assert!(report.deferred);
    assert_eq!(report.defer_reason, "focus");
    assert_eq!(
        fs::read(home.join("Games/Demo/readme.txt")).unwrap(),
        b"busy"
    );
    assert!(!home.join("Games/.stignore").exists());
}

#[test]
fn verify_trees_rejects_a_different_checksum() {
    let tmp = Tmp::new();
    let a = tmp.path().join("a");
    let b = tmp.path().join("b");
    fs::create_dir_all(&a).unwrap();
    fs::create_dir_all(&b).unwrap();
    fs::write(a.join("f"), b"one").unwrap();
    fs::write(b.join("f"), b"two").unwrap();
    assert!(verify_trees(&a, &b).is_err());
    fs::write(b.join("f"), b"one").unwrap();
    assert!(verify_trees(&a, &b).is_ok());
}

#[test]
fn init_encodes_the_fleet_preset_and_drops_an_unencrypted_personal_volume() {
    let drives = r#"{"drives":[{"name":"m2-ssd","volumes":{"Vault-T":{"encrypted":false},"OTV Mirror":{"encrypted":false}}}]}"#;
    let datasets = r#"{"datasets":[
        {"name":"Photos library","paths":["Pictures/Photos Library.photoslibrary"],"class":"personal"},
        {"name":"Projects","paths":["Projects"],"class":"work"},
        {"name":"Documents","paths":["Documents"],"class":"personal"},
        {"name":"Films","paths":["Movies/films"],"class":"re-obtainable"}
    ]}"#;
    let policy = fleet_policy(drives, datasets).unwrap();
    let m5 = policy.hosts.iter().find(|h| h.id == "m5").unwrap();
    assert_eq!(m5.floor_gb, 150.0);
    assert_eq!(m5.band_low_gb, Some(200.0));
    assert_eq!(m5.band_high_gb, Some(400.0));
    assert_eq!(m5.bulk, Bulk::WorkingSet);
    let m2 = policy.hosts.iter().find(|h| h.id == "m2").unwrap();
    assert_eq!(m2.floor_gb, 40.0);
    assert_eq!(m2.bulk, Bulk::MastersOnExternal);
    let m1 = policy.hosts.iter().find(|h| h.id == "m1").unwrap();
    assert_eq!(m1.floor_gb, 30.0);
    assert_eq!(m1.bulk, Bulk::NoBulk);
    let games = policy
        .sync_folders
        .iter()
        .find(|f| f.root == "Games")
        .unwrap();
    assert_eq!(games.played_within_days, 30);
    let saves = games.parts.iter().find(|p| p.path == "Saves").unwrap();
    assert_eq!(saves.class, Class::SyncedSmall);
    assert!(saves.keep.iter().any(|k| k == "*"));
    let roms = games.parts.iter().find(|p| p.path == "ROMs").unwrap();
    assert!(roms.archive);
    assert_eq!(roms.master_volume, "OTV Mirror");
    let star = games.parts.iter().find(|p| p.path == "*").unwrap();
    assert_eq!(star.keep, vec!["m5".to_string()]);
    let photos = policy
        .datasets
        .iter()
        .find(|d| d.name == "Photos library")
        .unwrap();
    assert_eq!(photos.class, Class::PersonalUnique);
    assert!(
        photos
            .replica_volumes
            .iter()
            .any(|v| v == "Archive_Encrypted2")
    );
    assert!(!photos.replica_volumes.iter().any(|v| v == "Vault-T"));
    let projects = policy
        .datasets
        .iter()
        .find(|d| d.name == "Projects")
        .unwrap();
    assert_eq!(projects.class, Class::PersonalUnique);
    let documents = policy
        .datasets
        .iter()
        .find(|d| d.name == "Documents")
        .unwrap();
    assert!(documents.icloud);
    let films = policy.datasets.iter().find(|d| d.name == "Films").unwrap();
    assert_eq!(films.class, Class::Reobtainable);
    assert_eq!(films.master_volume, "OTV Mirror");
    assert!(
        policy
            .volumes
            .iter()
            .any(|v| v.name == "Stash" && v.subpath == "Media")
    );
    let text = render_policy(&policy).unwrap();
    let again = load_policy(&text).unwrap();
    assert_eq!(again.hosts[0].floor_gb, 150.0);
}

#[test]
fn example_config_parses_and_uses_role_ids() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../docs/examples/placement.toml");
    let text = fs::read_to_string(&path).unwrap();
    let policy = load_policy(&text).unwrap();
    assert!(
        policy
            .hosts
            .iter()
            .any(|h| h.role == Role::TravellingPrimary && h.floor_gb == 150.0)
    );
    assert!(policy.hosts.iter().any(|h| h.role == Role::HomeHub));
    assert!(
        policy
            .hosts
            .iter()
            .any(|h| h.role == Role::SharedLaptop && h.bulk == Bulk::NoBulk)
    );
    assert!(
        !policy
            .hosts
            .iter()
            .any(|h| h.id == "m5" || h.id == "m1" || h.id == "m2")
    );
    assert!(
        policy
            .sync_folders
            .iter()
            .any(|f| f.parts.iter().any(|p| p.path == "Saves"))
    );
}

#[test]
fn diskmap_documents_supply_free_space_and_access_without_a_walk() {
    let tmp = Tmp::new();
    fs::write(
        tmp.path().join("summary.json"),
        r#"{"generated":1,"locations":[{"kind":"volume","host":"primary","name":"internal","physical_drive":"internal","free_bytes":120000000000}]}"#,
    )
    .unwrap();
    fs::write(
        tmp.path().join("where.json"),
        r#"{"matches":[{"host":"primary","location":"internal","home_path":"Games/Demo","bytes":4000000000}]}"#,
    )
    .unwrap();
    fs::write(
        tmp.path().join("access.json"),
        &format!(
            r#"{{"rows":[{{"host":"primary","path":"Games/Demo","last_access":{}}}]}}"#,
            NOW - DAY
        ),
    )
    .unwrap();
    let seen = load_observation(tmp.path(), None, NOW);
    assert!(seen.diskmap_present);
    assert_eq!(seen.frees[0].free_bytes, 120_000_000_000);
    assert_eq!(seen.present[0].path, "Games/Demo");
    assert_eq!(seen.access[0].last_access, NOW - DAY);
}

#[test]
fn diskmap_host_map_resolves_a_local_name() {
    let text = r#"{"hosts":{"desk":{"id":"primary","floor_gb":150}}}"#;
    assert_eq!(
        host_id_from_diskmap(text, "desk").as_deref(),
        Some("primary")
    );
    assert_eq!(host_id_from_diskmap(text, "other"), None);
}

#[test]
fn placement_does_not_take_a_heavy_slot() {
    assert!(!TAKES_HEAVY_SLOT);
}

#[test]
fn utc_dates_match_the_unix_epoch() {
    assert_eq!(ymd(0), (1970, 1, 1));
    assert_eq!(ymd(86_400), (1970, 1, 2));
}

#[test]
fn plan_and_status_commands_stay_inside_the_given_directories() {
    let tmp = Tmp::new();
    let config = tmp.path().join("placement.toml");
    fs::write(
        &config,
        r#"
[[host]]
id = "primary"
role = "travelling-primary"
floor_gb = 150
band_low_gb = 200
band_high_gb = 400
bulk = "working-set"
"#,
    )
    .unwrap();
    let diskmap = tmp.path().join("diskmap");
    fs::create_dir_all(&diskmap).unwrap();
    let replicas = tmp.path().join("replicas.json");
    fs::write(&replicas, "{\"copies\":[]}\n").unwrap();
    let plan = placement::run(vec![
        "placement".into(),
        "plan".into(),
        "--config".into(),
        config.display().to_string(),
        "--diskmap".into(),
        diskmap.display().to_string(),
        "--replicas".into(),
        replicas.display().to_string(),
    ]);
    assert_eq!(plan, 0);
    let out = tmp.path().join("status.json");
    let status = placement::run(vec![
        "placement".into(),
        "status".into(),
        "--config".into(),
        config.display().to_string(),
        "--diskmap".into(),
        diskmap.display().to_string(),
        "--replicas".into(),
        replicas.display().to_string(),
        "--out".into(),
        out.display().to_string(),
        "--json".into(),
    ]);
    assert_eq!(status, 0);
    let body: serde_json::Value = serde_json::from_str(&fs::read_to_string(&out).unwrap()).unwrap();
    assert_eq!(body["hosts"][0]["state"], "unknown");
}

fn run_move(
    tmp: &Tmp,
    home: &Path,
    force_copy: bool,
    fail_verify: bool,
    _lock: Option<()>,
) -> ApplyReport {
    let policy = Policy {
        volumes: vec![media()],
        sync_folders: vec![],
        ..Policy::default()
    };
    let plan = Plan {
        actions: vec![Action {
            host: "primary".into(),
            kind: ActionKind::Move,
            path: "Games/Demo".into(),
            bytes: 15,
            dest: "media".into(),
            reason: "not placed".into(),
            class: Class::Reobtainable,
            label: "games/Demo".into(),
            icloud: false,
        }],
        budgets: vec![],
        violations: vec![],
    };
    let mut opts = opts_for(tmp, home);
    opts.force_trash_copy = force_copy;
    opts.fail_verify = fail_verify;
    opts.now = NOW;
    apply(&policy, &obs(vec![], vec![], vec![], vec![]), &plan, &opts).unwrap()
}

fn opts_for(tmp: &Tmp, home: &Path) -> ApplyOpts {
    let mut opts = ApplyOpts::new(home.to_path_buf(), "primary");
    opts.trash = tmp.path().join("trash");
    opts.ledger = tmp.path().join("OFFLOADS.md");
    opts.volume_root = tmp.path().join("volumes");
    opts.now = NOW;
    opts.hostname = "primary".into();
    opts.replica_log = Some(tmp.path().join("replicas.json"));
    opts
}

fn only_child(dir: &Path) -> PathBuf {
    let mut kids: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .contains("Demo")
        })
        .collect();
    assert_eq!(kids.len(), 1, "{kids:?}");
    kids.pop().unwrap()
}
