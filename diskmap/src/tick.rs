//! One pass for launchd: keep the initial-walk worker alive, fold in FSEvents
//! once that walk has finished, then summary, alerts, archive.

use crate::archive;
use crate::cloud::{self, CloudFolder};
use crate::darwin::{self, Mounted};
use crate::engine;
use crate::logic::{self, Alert, DiskConfig, DriveDef, ResolvedHost, VolumeObs};
use crate::store::{self, DirRow, HistRow};
use crate::summary::{self, Grower, Location, Summary, WhereFile, WhereHit};
use std::cell::RefCell;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub struct Roots {
    pub home: PathBuf,
    pub state: PathBuf,
    pub config: PathBuf,
    pub analytics: PathBuf,
}

impl Roots {
    pub fn from_env() -> Self {
        let home = std::env::var("DISKMAP_HOME")
            .ok()
            .map(PathBuf::from)
            .or_else(|| std::env::var("HOME").ok().map(PathBuf::from))
            .unwrap_or_else(|| PathBuf::from("."));
        let state = std::env::var("DISKMAP_STATE")
            .map(PathBuf::from)
            .unwrap_or_else(|_| home.join(".local/state/diskmap"));
        let config = std::env::var("DISKMAP_CONFIG")
            .map(PathBuf::from)
            .unwrap_or_else(|_| home.join(".config/diskmap/config.json"));
        let analytics = std::env::var("DISKMAP_ANALYTICS")
            .map(PathBuf::from)
            .unwrap_or_else(|_| home.join(".local/state/machine-analytics"));
        Roots {
            home,
            state,
            config,
            analytics,
        }
    }
}

pub fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn load(roots: &Roots) -> (DiskConfig, ResolvedHost) {
    let text = fs::read_to_string(&roots.config).ok();
    let cfg = logic::load_config(text.as_deref()).unwrap_or_else(|_| logic::default_config());
    let host = logic::resolve_host(&cfg, &darwin::local_host_name());
    (cfg, host)
}

fn drives_path(roots: &Roots, cfg: &DiskConfig) -> Option<PathBuf> {
    if let Ok(p) = std::env::var("DISKMAP_DRIVES") {
        return Some(PathBuf::from(p));
    }
    if let Some(p) = &cfg.drives_json {
        return Some(PathBuf::from(p));
    }
    let conventional = roots.home.join(".config/fleet/drives.json");
    if conventional.is_file() {
        Some(conventional)
    } else {
        None
    }
}

fn load_drives(roots: &Roots, cfg: &DiskConfig) -> Vec<DriveDef> {
    let Some(path) = drives_path(roots, cfg) else {
        return Vec::new();
    };
    fs::read_to_string(path)
        .ok()
        .and_then(|text| logic::parse_drives(&text).ok())
        .unwrap_or_default()
}

thread_local! {
    static SKIP_LOGS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

fn log_walker_skip(logged: &mut bool) {
    if *logged {
        return;
    }
    *logged = true;
    let msg = "diskmap: walker holds the database; skipping writes";
    eprintln!("{msg}");
    SKIP_LOGS.with(|logs| logs.borrow_mut().push(msg.to_string()));
}

pub fn tick(roots: &Roots) -> Result<Summary, String> {
    darwin::be_gentle();
    let now = unix_now();
    let (cfg, host) = load(roots);
    let drives = load_drives(roots, &cfg);
    let mounted = darwin::list_volumes().unwrap_or_default();
    let walker = walk_lock_held(&roots.state.join("walk.lock"))?;
    let mut logged = false;
    if walker {
        log_walker_skip(&mut logged);
    } else {
        // Replaying FSEvents only queues rescans, so it runs even while the
        // owner is at the Mac; the background worker drains the queue.
        refresh_events(roots, &cfg, &mounted);
        if let Err(err) = ensure_walk_worker(roots, &mounted) {
            eprintln!("diskmap: {err}");
        }
        for vol in &mounted {
            let db_path = db_file(roots, &vol.uuid);
            if let Err(err) = engine::remember_volume(
                &db_path,
                &vol.uuid,
                &vol.name,
                &vol.mount.to_string_lossy(),
                vol.total_bytes,
                vol.free_bytes,
                now,
            ) {
                if crate::sqlite::is_locked(&err) {
                    log_walker_skip(&mut logged);
                    break;
                }
                return Err(err);
            }
        }
    }
    let summary = build_summary(roots, &cfg, &host, &drives, &mounted, now)?;
    fs::create_dir_all(&roots.state).map_err(|e| e.to_string())?;
    fs::write(roots.state.join("summary.json"), summary::to_json(&summary)).map_err(|e| e.to_string())?;
    let where_file = build_where(roots, &host, &summary, now)?;
    let where_text = serde_json::to_string_pretty(&where_file).map_err(|e| e.to_string())? + "\n";
    fs::write(roots.state.join("where.json"), where_text).map_err(|e| e.to_string())?;
    if !walker {
        if let Err(err) = ship_archive(roots, &cfg, &drives, &host, &mounted) {
            if crate::sqlite::is_locked(&err) {
                log_walker_skip(&mut logged);
            } else {
                return Err(err);
            }
        }
    }
    Ok(summary)
}

fn refresh_events(roots: &Roots, cfg: &DiskConfig, mounted: &[Mounted]) {
    let start = Instant::now();
    for vol in mounted {
        if start.elapsed() > Duration::from_millis(800) {
            break;
        }
        let db_path = db_file(roots, &vol.uuid);
        if !baseline_done(&db_path) {
            continue;
        }
        fold_events(vol, &db_path);
        if engine::queue_len(&db_path).unwrap_or(0) == 0 {
            let _ = snapshot_if_due(&db_path, cfg.history_min_bytes);
        }
    }
}

/// Replay FSEvents since the stored id into the rescan queue. Cheap: it reads
/// the event log and queues directories; scanning happens in the worker.
fn fold_events(vol: &Mounted, db_path: &Path) {
    if let Ok((events, max_id, fs_uuid)) =
        darwin::replay_events(&vol.mount, vol.dev, stored_event_id(db_path), Duration::from_millis(250))
    {
        let _ = remember_fs(db_path, max_id, &fs_uuid);
        let scans = logic::coalesce(&events);
        let _ = engine::apply_scans(db_path, &scans);
    }
}

fn baseline_done(db_path: &Path) -> bool {
    engine::meta(db_path, "baseline").ok().flatten().as_deref() == Some("1")
}

const PRUNE_MIN_BYTES: i64 = 1_000_000;

/// `diskmap prune`: run the one-off repair now on every mounted, mapped volume and say what
/// it did. Refuses while the walk worker holds the databases.
pub fn prune_now(roots: &Roots) -> Result<Vec<(String, usize)>, String> {
    let Some(_lock) = hold_walk_lock(&roots.state.join("walk.lock"))? else {
        return Err("the walk worker is running; try again in a minute".into());
    };
    let (cfg, _) = load(roots);
    let now = unix_now();
    let mut out = Vec::new();
    for vol in darwin::list_volumes().unwrap_or_default() {
        let db_path = db_file(roots, &vol.uuid);
        if !baseline_done(&db_path) {
            continue;
        }
        let removed = engine::prune_missing(&vol.mount, &db_path, now, cfg.event_min_bytes, PRUNE_MIN_BYTES)
            .map_err(|e| format!("{}: {e}", vol.name))?;
        out.push((vol.name.clone(), removed));
    }
    Ok(out)
}

pub fn ensure_walk_worker(roots: &Roots, mounted: &[Mounted]) -> Result<(), String> {
    let volumes: Vec<(bool, i64)> = mounted
        .iter()
        .map(|vol| {
            let db_path = db_file(roots, &vol.uuid);
            (baseline_done(&db_path), engine::queue_len(&db_path).unwrap_or(0))
        })
        .collect();
    if !logic::needs_worker(&volumes) {
        return Ok(());
    }
    let lock_path = roots.state.join("walk.lock");
    if walk_lock_held(&lock_path)? {
        return Ok(());
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    Command::new(exe)
        .arg("walk")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn walk_lock_held(path: &Path) -> Result<bool, String> {
    match hold_walk_lock(path)? {
        Some(file) => {
            drop(file);
            Ok(false)
        }
        None => Ok(true),
    }
}

pub fn hold_walk_lock(path: &Path) -> Result<Option<File>, String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|e| e.to_string())?;
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc != 0 {
        return Ok(None);
    }
    file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    file.set_len(0).map_err(|e| e.to_string())?;
    writeln!(file, "{}", std::process::id()).map_err(|e| e.to_string())?;
    Ok(Some(file))
}

pub fn walk_worker(roots: &Roots) -> Result<(), String> {
    let mut checked = Instant::now() - Duration::from_secs(60);
    let mut folded = Instant::now() - Duration::from_secs(60);
    let mut paused = false;
    loop {
        // Only serious heat stops the whole worker. Owner focus pauses the
        // initial walk (via `walk_paused`) and slows the rescan drain.
        if drain_pace_now(roots) == logic::DrainPace::Paused {
            std::thread::sleep(Duration::from_secs(5));
            continue;
        }
        let mounted = darwin::list_volumes().unwrap_or_default();
        let now = unix_now();
        let (cfg, _) = load(roots);
        let mut remaining = false;
        let fold_due = folded.elapsed() >= Duration::from_secs(30);
        if fold_due {
            folded = Instant::now();
        }
        for vol in &mounted {
            let db_path = db_file(roots, &vol.uuid);
            if baseline_done(&db_path) {
                // The tick cannot fold events while this worker holds the
                // walk lock, so the worker does it, then drains the queue.
                if fold_due {
                    // Once per map: drop rows for directories deleted before scans did that.
                    if let Err(err) = engine::prune_missing(&vol.mount, &db_path, now, cfg.event_min_bytes, PRUNE_MIN_BYTES) {
                        eprintln!("diskmap: prune {}: {err}", vol.name);
                    }
                    fold_events(vol, &db_path);
                }
                if engine::queue_len(&db_path).unwrap_or(0) == 0 {
                    let _ = snapshot_if_due(&db_path, cfg.history_min_bytes);
                    continue;
                }
                let pace = drain_pace_now(roots);
                if pace == logic::DrainPace::Paused {
                    remaining = true;
                    std::thread::sleep(Duration::from_secs(5));
                    continue;
                }
                remaining = true;
                let batch = if pace == logic::DrainPace::Gentle { 500 } else { engine::WALK_BATCH };
                engine::walk_one_batch(&vol.mount, &db_path, now, cfg.event_min_bytes, batch, |_| false)?;
                if pace == logic::DrainPace::Gentle {
                    std::thread::sleep(Duration::from_secs(2));
                }
                continue;
            }
            remaining = true;
            let _ = engine::remember_volume(
                &db_path,
                &vol.uuid,
                &vol.name,
                &vol.mount.to_string_lossy(),
                vol.total_bytes,
                vol.free_bytes,
                now,
            );
            let stats = engine::walk_one_batch(
                &vol.mount,
                &db_path,
                now,
                cfg.event_min_bytes,
                engine::WALK_BATCH,
                |_| {
                    if checked.elapsed() >= Duration::from_secs(5) {
                        paused = walk_paused(roots);
                        checked = Instant::now();
                    }
                    paused
                },
            )?;
            if stats.yielded {
                std::thread::sleep(Duration::from_secs(5));
                break;
            }
        }
        if !remaining {
            return Ok(());
        }
    }
}

fn drain_pace_now(roots: &Roots) -> logic::DrainPace {
    let force = std::env::var("DISKMAP_FORCE").ok().as_deref() == Some("1");
    let thermal = latest_analytics_line(roots)
        .map(|line| darwin::thermal_blocks_walk(&line, unix_now()))
        .unwrap_or(false);
    logic::drain_pace(darwin::focus_active(), thermal, force)
}

fn walk_paused(roots: &Roots) -> bool {
    let force = std::env::var("DISKMAP_FORCE").ok().as_deref() == Some("1");
    let now = unix_now();
    let thermal = latest_analytics_line(roots)
        .map(|line| darwin::thermal_blocks_walk(&line, now))
        .unwrap_or(false);
    logic::pause_walk(darwin::focus_active(), thermal, force)
}

fn stored_event_id(db_path: &Path) -> u64 {
    engine::meta(db_path, "fsevent_id")
        .ok()
        .flatten()
        .and_then(|s| s.parse().ok())
        .unwrap_or(u64::MAX)
}

fn remember_fs(db_path: &Path, id: u64, uuid: &str) -> Result<(), String> {
    let db = store::open(db_path)?;
    let prev = store::meta_get(&db, "fsevent_uuid")?;
    if let Some(prev) = prev {
        if !prev.is_empty() && !uuid.is_empty() && prev != uuid {
            store::meta_set(&db, "pending", r#"[{"path":"","deep":true}]"#)?;
        }
    }
    if !uuid.is_empty() {
        store::meta_set(&db, "fsevent_uuid", uuid)?;
    }
    if id > 0 {
        store::meta_set(&db, "fsevent_id", &id.to_string())?;
    }
    Ok(())
}

fn snapshot_if_due(db_path: &Path, min_bytes: i64) -> Result<(), String> {
    let db = store::open(db_path)?;
    let day = logic::local_day(unix_now());
    if store::meta_get(&db, "history_day")?.as_deref() == Some(day.as_str()) {
        return Ok(());
    }
    store::snapshot_history(&db, &day, min_bytes)?;
    store::meta_set(&db, "history_day", &day)?;
    Ok(())
}

fn db_file(roots: &Roots, uuid: &str) -> PathBuf {
    let safe: String = uuid
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' })
        .collect();
    roots.state.join(format!("{safe}.db"))
}

pub fn build_summary(
    roots: &Roots,
    cfg: &DiskConfig,
    host: &ResolvedHost,
    drives: &[DriveDef],
    mounted: &[Mounted],
    now: i64,
) -> Result<Summary, String> {
    let samples = read_samples(roots, 8);
    let trend = logic::trend(&samples, now);
    let trend_bytes = summary::trend_bytes(&trend);
    let mut obs: Vec<VolumeObs> = Vec::new();
    let mut grower_for: Vec<(String, Option<Grower>)> = Vec::new();
    let mut known_uuids = Vec::new();
    for vol in mounted {
        known_uuids.push(vol.uuid.to_ascii_uppercase());
        obs.push(VolumeObs {
            uuid: vol.uuid.clone(),
            name: vol.name.clone(),
            total_bytes: vol.total_bytes,
            free_bytes: vol.free_bytes,
            mounted: true,
            last_seen: now,
            encrypted: logic::drive_of(drives, &vol.uuid).map(|(_, v)| v.encrypted).unwrap_or(false),
        });
        let db_path = db_file(roots, &vol.uuid);
        grower_for.push((vol.uuid.clone(), grower_of(&db_path, now - 86400)));
    }
    if let Ok(rd) = fs::read_dir(&roots.state) {
        for ent in rd.flatten() {
            let path = ent.path();
            if path.extension().and_then(|e| e.to_str()) != Some("db") {
                continue;
            }
            let uuid = engine::meta(&path, "uuid")?.unwrap_or_default();
            if uuid.is_empty() || known_uuids.iter().any(|u| u == &uuid.to_ascii_uppercase()) {
                continue;
            }
            let name = engine::meta(&path, "name")?.unwrap_or_else(|| "volume".into());
            let total = engine::meta(&path, "total_bytes")?.and_then(|s| s.parse().ok()).unwrap_or(0);
            let free = engine::meta(&path, "free_bytes")?.and_then(|s| s.parse().ok()).unwrap_or(0);
            let seen = engine::meta(&path, "last_seen")?.and_then(|s| s.parse().ok()).unwrap_or(0);
            obs.push(VolumeObs {
                uuid: uuid.clone(),
                name,
                total_bytes: total,
                free_bytes: free,
                mounted: false,
                last_seen: seen,
                encrypted: logic::drive_of(drives, &uuid).map(|(_, v)| v.encrypted).unwrap_or(false),
            });
            known_uuids.push(uuid.to_ascii_uppercase());
        }
    }
    let rollups = logic::group_drives(drives, &obs);
    let mut locations = Vec::new();
    for drive in &rollups {
        let grower = drive.volume_uuids.iter().find_map(|uuid| {
            grower_for
                .iter()
                .find(|(id, _)| id.eq_ignore_ascii_case(uuid))
                .and_then(|(_, g)| g.clone())
        });
        let map = map_for_drive(roots, &drive.volume_uuids);
        let mut loc = summary::location_from_drive(&host.id, drive, trend_bytes.clone(), grower, map);
        if drive.name != "internal" {
            loc.trend_bytes = summary::TrendBytes { h1: None, h24: None, d7: None };
        }
        locations.push(loc);
    }
    let prefix = home_prefix(&roots.home);
    let internal_db = mounted
        .iter()
        .find(|v| v.name == "internal")
        .map(|v| db_file(roots, &v.uuid));
    let rows = if let Some(path) = &internal_db {
        if path.exists() {
            match store::open_readonly(path) {
                Ok(db) => cloud_rows(&db, &prefix)?,
                Err(err) if crate::sqlite::is_locked(&err) => Vec::new(),
                Err(err) => return Err(err),
            }
        } else {
            Vec::new()
        }
    } else {
        Vec::new()
    };
    let folders = cloud::icloud_folders(&rows, &prefix);
    locations.push(icloud_location(&host.id, &folders, now));
    let idrive_db = internal_db.clone();
    let idrive = cloud::read_idrive(&roots.home, |rel| {
        let Some(path) = &idrive_db else { return 0 };
        if !path.exists() {
            return 0;
        }
        let Ok(db) = store::open_readonly(path) else { return 0 };
        let full = format!("{prefix}/{rel}");
        store::dir(&db, &full)
            .ok()
            .flatten()
            .map(|row| row.total_bytes)
            .unwrap_or(0)
    });
    locations.push(idrive_location(&host.id, &idrive, now));
    let alerts = evaluate_alerts(roots, cfg, host, &locations, &samples, now)?;
    let summary = Summary {
        generated: now,
        host_id: host.id.clone(),
        local_host_name: host.local_host_name.clone(),
        locations,
        drives: rollups.iter().map(summary::drive_out).collect(),
        alerts,
    };
    Ok(summary)
}

fn cloud_rows(db: &crate::sqlite::Db, prefix: &str) -> Result<Vec<DirRow>, String> {
    let cloud = format!("{prefix}/Library/Mobile Documents/com~apple~CloudDocs");
    let mut rows = store::children(db, &cloud)?;
    for name in ["Desktop", "Documents"] {
        if let Some(row) = store::dir(db, &format!("{prefix}/{name}"))? {
            rows.push(row);
        }
    }
    let library = format!("{prefix}/Pictures/Photos Library.photoslibrary");
    rows.extend(store::photo_dirs(db, &library)?);
    Ok(rows)
}

fn home_prefix(home: &Path) -> String {
    let name = home.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    format!("Users/{name}")
}

fn grower_of(db_path: &Path, since: i64) -> Option<Grower> {
    let growth = engine::growth_since(db_path, since).ok()?;
    let grew = logic::collapse_grew(&growth)?;
    if grew.delta <= 0 {
        return None;
    }
    Some(Grower {
        path: if grew.path.is_empty() { "/".into() } else { grew.path },
        delta_bytes: grew.delta,
    })
}

fn icloud_location(host: &str, folders: &[CloudFolder], now: i64) -> Location {
    let local: u64 = folders.iter().map(|f| f.local_bytes).sum();
    let dataless: u64 = folders.iter().map(|f| f.dataless_bytes).sum();
    let grower = None;
    Location {
        kind: "icloud".into(),
        id: format!("{host}:icloud"),
        host: host.into(),
        name: "iCloud".into(),
        volume_uuid: None,
        physical_drive: None,
        mount: None,
        mounted: true,
        encrypted: false,
        total_bytes: local.saturating_add(dataless),
        free_bytes: 0,
        local_bytes: local,
        dataless_bytes: dataless,
        trend_bytes: summary::TrendBytes { h1: None, h24: None, d7: None },
        top_grower: grower,
        last_seen: now,
        detail: String::new(),
        folders: folders.to_vec(),
        map: None,
    }
}

fn idrive_location(host: &str, view: &cloud::IdriveView, now: i64) -> Location {
    let folders: Vec<CloudFolder> = view
        .paths
        .iter()
        .map(|(path, bytes)| CloudFolder {
            area: "selected".into(),
            name: path.clone(),
            local_bytes: (*bytes).max(0) as u64,
            dataless_bytes: 0,
        })
        .collect();
    let sum: u64 = folders.iter().map(|f| f.local_bytes).sum();
    let detail = match view.last_success {
        Some(t) => format!("ok {}", logic::fmt_age(now, t)),
        None if view.installed => "no success".into(),
        None => "not installed".into(),
    };
    let grower = folders.iter().max_by_key(|f| f.local_bytes).map(|f| Grower {
        path: f.name.clone(),
        delta_bytes: f.local_bytes as i64,
    });
    Location {
        kind: "idrive".into(),
        id: format!("{host}:idrive"),
        host: host.into(),
        name: "IDrive".into(),
        volume_uuid: None,
        physical_drive: None,
        mount: None,
        mounted: view.installed,
        encrypted: false,
        total_bytes: sum,
        free_bytes: 0,
        local_bytes: sum,
        dataless_bytes: 0,
        trend_bytes: summary::TrendBytes { h1: None, h24: None, d7: None },
        top_grower: grower,
        last_seen: view.last_success.unwrap_or(0),
        detail,
        folders,
        map: None,
    }
}

fn evaluate_alerts(
    roots: &Roots,
    cfg: &DiskConfig,
    host: &ResolvedHost,
    locations: &[Location],
    samples: &[(i64, f64)],
    now: i64,
) -> Result<Vec<Alert>, String> {
    let internal = locations.iter().find(|l| l.kind == "volume" && l.name == "internal");
    let free_gb = internal
        .map(|l| l.free_bytes as f64 / 1e9)
        .or_else(|| samples.last().map(|(_, g)| *g))
        .unwrap_or(0.0);
    let grower = internal.and_then(|l| l.top_grower.clone());
    let grower_name = grower
        .as_ref()
        .map(|g| logic::basename(&g.path).to_string())
        .unwrap_or_else(|| "disk".into());
    let mut fresh = Vec::new();
    if let Some(rate) = logic::sustained_drop(samples, now, cfg.alert_sustain_secs, cfg.alert_gb_per_hour) {
        fresh.push(Alert {
            t: now,
            cause: logic::rate_cause(&host.id, "internal", &grower_name),
            host: host.id.clone(),
            location: "internal".into(),
            free_gb,
            rate_gb_h: Some(rate),
            grower: grower_name.clone(),
            message: format!("{grower_name} +{rate:.0} GB/h, {:.0} GB free", free_gb),
        });
    }
    if logic::floor_breach(free_gb, host.floor_gb) {
        fresh.push(Alert {
            t: now,
            cause: logic::floor_cause(&host.id, "internal"),
            host: host.id.clone(),
            location: "internal".into(),
            free_gb,
            rate_gb_h: None,
            grower: grower_name.clone(),
            message: format!("{grower_name}, {free_gb:.0} GB free"),
        });
    }
    let prior = read_alerts(roots);
    let gated = logic::gate_alerts(fresh, &prior, cfg.alert_cooldown_secs);
    if !gated.is_empty() {
        append_alerts(roots, &gated)?;
        for alert in &gated {
            darwin::notify(&format!("diskmap {}", alert.host), &alert.message);
        }
    }
    let mut live = prior;
    live.extend(gated);
    let cutoff = now - cfg.alert_cooldown_secs;
    live.retain(|a| a.t >= cutoff);
    Ok(live)
}

fn alerts_path(roots: &Roots) -> PathBuf {
    roots.state.join("alerts.jsonl")
}

fn read_alerts(roots: &Roots) -> Vec<Alert> {
    let Ok(text) = fs::read_to_string(alerts_path(roots)) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

fn append_alerts(roots: &Roots, alerts: &[Alert]) -> Result<(), String> {
    fs::create_dir_all(&roots.state).map_err(|e| e.to_string())?;
    let mut body = fs::read_to_string(alerts_path(roots)).unwrap_or_default();
    for alert in alerts {
        body.push_str(&serde_json::to_string(alert).map_err(|e| e.to_string())?);
        body.push('\n');
    }
    fs::write(alerts_path(roots), body).map_err(|e| e.to_string())
}

fn build_where(roots: &Roots, host: &ResolvedHost, summary: &Summary, now: i64) -> Result<WhereFile, String> {
    let mut matches = Vec::new();
    let min = logic::HISTORY_MIN_BYTES;
    if let Ok(rd) = fs::read_dir(&roots.state) {
        for ent in rd.flatten() {
            let path = ent.path();
            if path.extension().and_then(|e| e.to_str()) != Some("db") {
                continue;
            }
            let db = match store::open_readonly(&path) {
                Ok(db) => db,
                Err(err) if crate::sqlite::is_locked(&err) => continue,
                Err(err) => return Err(err),
            };
            let name = store::meta_get(&db, "name")?.unwrap_or_else(|| "volume".into());
            let seen = store::meta_get(&db, "last_seen")?.and_then(|s| s.parse().ok()).unwrap_or(now);
            for row in store::dirs_at_least(&db, min)? {
                matches.push(WhereHit {
                    host: host.id.clone(),
                    location: name.clone(),
                    path: row.path.clone(),
                    home_path: logic::home_relative(&row.path),
                    bytes: row.total_bytes.max(0) as u64,
                    as_of: seen,
                    kind: "volume".into(),
                });
            }
        }
    }
    for loc in &summary.locations {
        if loc.kind == "icloud" || loc.kind == "idrive" {
            for folder in &loc.folders {
                let bytes = folder.local_bytes.saturating_add(folder.dataless_bytes);
                if bytes < min as u64 && loc.kind == "icloud" {
                    continue;
                }
                if loc.kind == "idrive" && bytes < min as u64 {
                    continue;
                }
                matches.push(WhereHit {
                    host: host.id.clone(),
                    location: loc.name.clone(),
                    path: folder.name.clone(),
                    home_path: Some(folder.name.clone()),
                    bytes,
                    as_of: loc.last_seen,
                    kind: loc.kind.clone(),
                });
            }
        }
    }
    Ok(WhereFile {
        generated: now,
        min_bytes: min as u64,
        matches,
    })
}

fn ship_archive(roots: &Roots, cfg: &DiskConfig, drives: &[DriveDef], host: &ResolvedHost, mounted: &[Mounted]) -> Result<(), String> {
    let dest = PathBuf::from(format!("/Volumes/{}/{}", cfg.archive_volume, cfg.archive_rel));
    let mounted_ok = mounted.iter().any(|v| {
        v.mount == PathBuf::from(format!("/Volumes/{}", cfg.archive_volume))
    });
    let allowed = logic::archive_allowed(&dest.to_string_lossy(), drives).is_ok();
    let spool = roots.state.join("spool");
    if let Ok(rd) = fs::read_dir(&roots.state) {
        for ent in rd.flatten() {
            let path = ent.path();
            if path.extension().and_then(|e| e.to_str()) != Some("db") {
                continue;
            }
            let uuid = engine::meta(&path, "uuid")?.unwrap_or_else(|| "volume".into());
            let (events, history) = pending_ship(&path)?;
            if events.is_empty() && history.is_empty() {
                continue;
            }
            if mounted_ok && allowed {
                let (spooled_e, spooled_h) = archive::spool_take(&spool, &host.id, &uuid).unwrap_or_default();
                let mut ev = spooled_e;
                ev.extend(events);
                let mut hist = spooled_h;
                hist.extend(history);
                let manifest = archive::ship(&dest, &host.id, &uuid, &ev, &hist)?;
                archive::spool_clear(&spool, &host.id, &uuid)?;
                let db = store::open(&path)?;
                store::meta_set(&db, "shipped_event_id", &manifest.events_through_id.to_string())?;
                let days = serde_json::to_string(&manifest.history_days).unwrap_or_else(|_| "[]".into());
                store::meta_set(&db, "shipped_days", &days)?;
                let cutoff = unix_now() - cfg.hot_days * 86400;
                let cutoff_day = logic::local_day(cutoff);
                store::prune_confirmed(&db, cutoff, manifest.events_through_id, &cutoff_day, &manifest.history_days)?;
            } else if !events.is_empty() || !history.is_empty() {
                archive::spool_merge(&spool, &host.id, &uuid, &events, &history)?;
            }
        }
    }
    Ok(())
}

fn pending_ship(db_path: &Path) -> Result<(Vec<crate::store::EventRow>, Vec<HistRow>), String> {
    let db = store::open(db_path)?;
    let watermark = store::meta_get(&db, "shipped_event_id")?.and_then(|s| s.parse().ok()).unwrap_or(0);
    let events = store::events_after_id(&db, watermark)?;
    let history = store::history_all(&db)?;
    Ok((events, history))
}

pub fn read_samples(roots: &Roots, days: i64) -> Vec<(i64, f64)> {
    let mut out = Vec::new();
    let now = unix_now();
    for offset in 0..days {
        let day = logic::local_day(now - offset * 86400);
        let path = roots.analytics.join(format!("{day}.jsonl"));
        let Ok(file) = fs::File::open(path) else { continue };
        for line in BufReader::new(file).lines() {
            let Ok(line) = line else { continue };
            out.extend(logic::samples_from_jsonl(&line));
        }
    }
    out.sort_by_key(|(t, _)| *t);
    out
}

fn latest_analytics_line(roots: &Roots) -> Option<String> {
    let day = logic::local_day(unix_now());
    let path = roots.analytics.join(format!("{day}.jsonl"));
    let file = fs::File::open(path).ok()?;
    let mut last = None;
    for line in BufReader::new(file).lines() {
        if let Ok(line) = line {
            if !line.trim().is_empty() {
                last = Some(line);
            }
        }
    }
    last
}

pub fn rebuild_summary(roots: &Roots) -> Result<Summary, String> {
    let now = unix_now();
    let (cfg, host) = load(roots);
    let drives = load_drives(roots, &cfg);
    let mounted = darwin::list_volumes().unwrap_or_default();
    for vol in &mounted {
        let _ = engine::remember_volume(
            &db_file(roots, &vol.uuid),
            &vol.uuid,
            &vol.name,
            &vol.mount.to_string_lossy(),
            vol.total_bytes,
            vol.free_bytes,
            now,
        );
    }
    let summary = build_summary(roots, &cfg, &host, &drives, &mounted, now)?;
    fs::create_dir_all(&roots.state).map_err(|e| e.to_string())?;
    fs::write(roots.state.join("summary.json"), summary::to_json(&summary)).map_err(|e| e.to_string())?;
    Ok(summary)
}

fn map_for_drive(roots: &Roots, uuids: &[String]) -> Option<summary::MapStatus> {
    let mut chosen: Option<summary::MapStatus> = None;
    for uuid in uuids {
        let path = db_file(roots, uuid);
        if !path.exists() {
            continue;
        }
        let Ok(db) = store::open_readonly(&path) else { continue };
        let Ok(progress) = engine::map_progress(&db) else { continue };
        let status = summary::MapStatus {
            dirs: progress.dirs,
            percent: progress.percent,
            done: progress.done,
        };
        chosen = Some(match chosen {
            Some(old) if !status.done && (old.done || status.percent < old.percent) => status,
            Some(old) => old,
            None => status,
        });
    }
    chosen
}

#[derive(Clone, Debug)]
pub struct GrewHit {
    pub volume: String,
    pub path: String,
    pub delta: i64,
    pub since_label: String,
}

#[derive(Clone, Debug)]
pub struct GrewReport {
    pub hits: Vec<GrewHit>,
    pub banner: Option<String>,
}

pub fn grew_report(roots: &Roots, since: i64, since_label: &str) -> GrewReport {
    let mut hits = Vec::new();
    let mut any_history = false;
    let mut started: Option<i64> = None;
    let mut saw_db = false;
    if let Ok(rd) = fs::read_dir(&roots.state) {
        for ent in rd.flatten() {
            let path = ent.path();
            if path.extension().and_then(|e| e.to_str()) != Some("db") {
                continue;
            }
            saw_db = true;
            let name = engine::meta(&path, "name").ok().flatten().unwrap_or_else(|| "volume".into());
            let have_history = store::open(&path).ok().and_then(|db| store::has_history(&db).ok()).unwrap_or(false);
            if have_history {
                any_history = true;
            }
            if let Some(t) = engine::meta(&path, "walk_started").ok().flatten().and_then(|s| s.parse().ok()) {
                started = Some(started.map_or(t, |prev: i64| prev.min(t)));
            }
            let rows = if have_history {
                engine::growth_since(&path, since).unwrap_or_default()
            } else {
                engine::growth_against_snapshot(&path).unwrap_or_default()
            };
            if let Some(grew) = logic::collapse_grew(&rows) {
                if grew.delta > 0 {
                    hits.push(GrewHit {
                        volume: name,
                        path: if grew.path.is_empty() { "/".into() } else { grew.path },
                        delta: grew.delta,
                        since_label: if have_history { since_label.to_string() } else { "map".into() },
                    });
                }
            }
        }
    }
    let hm = started.filter(|t| *t > 0).map(logic::fmt_hm);
    let banner = if saw_db || !hits.is_empty() || !any_history {
        logic::grew_status_line(any_history, !hits.is_empty(), hm.as_deref())
    } else {
        None
    };
    GrewReport { hits, banner }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn scratch() -> PathBuf {
        let n = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let path = std::env::temp_dir().join(format!("diskmap-tick-{n}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn tick_skips_the_walker_lock_and_still_writes_summary() {
        // The initial walk holds the database. tick must exit cleanly, log
        // once, and write summary.json from a connection that can read.
        let dir = scratch();
        let host = darwin::local_host_name();
        let roots = Roots {
            home: dir.join("home"),
            state: dir.join("state"),
            config: dir.join("config.json"),
            analytics: dir.join("analytics"),
        };
        fs::create_dir_all(&roots.home).unwrap();
        fs::write(
            &roots.config,
            serde_json::json!({
                "alert_gb_per_hour": 1.0e12,
                "alert_sustain_secs": 864000,
                "hosts": {
                    host.clone(): {"id": "test", "floor_gb": null},
                    "Coordinator-Mac": {"id": "desk", "floor_gb": null},
                    "Worker-One": {"id": "lab", "floor_gb": null},
                    "Worker-Two": {"id": "studio", "floor_gb": null}
                }
            })
            .to_string(),
        )
        .unwrap();
        let mounted = darwin::list_volumes().unwrap_or_default();
        assert!(!mounted.is_empty(), "need a mounted volume to hold");
        let vol = &mounted[0];
        let db_path = db_file(&roots, &vol.uuid);
        let db = store::open(&db_path).unwrap();
        store::meta_set(&db, "uuid", &vol.uuid).unwrap();
        store::meta_set(&db, "name", &vol.name).unwrap();
        store::meta_set(&db, "last_seen", "1").unwrap();
        store::insert_event(&db, unix_now() - 30, "Library/Grew", 80_000_000_000).unwrap();
        drop(db);
        let holder = store::open(&db_path).unwrap();
        holder.exec("BEGIN IMMEDIATE").unwrap();
        let lock = hold_walk_lock(&roots.state.join("walk.lock")).unwrap();
        assert!(lock.is_some());
        super::SKIP_LOGS.with(|logs| logs.borrow_mut().clear());
        let summary = tick(&roots).unwrap_or_else(|err| panic!("tick failed: {err}"));
        assert!(roots.state.join("summary.json").is_file());
        let text = fs::read_to_string(roots.state.join("summary.json")).unwrap();
        assert!(text.contains("Library/Grew"), "{text}");
        assert!(summary.generated > 0);
        drop(holder);
        let again = store::open(&db_path).unwrap();
        assert_eq!(store::meta_get(&again, "last_seen").unwrap().as_deref(), Some("1"));
        let logs = super::SKIP_LOGS.with(|logs| logs.borrow().clone());
        assert_eq!(logs, vec!["diskmap: walker holds the database; skipping writes".to_string()]);
        drop(lock);
        let _ = fs::remove_dir_all(&dir);
    }
}

