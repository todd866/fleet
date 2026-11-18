//! One pass for launchd: keep the initial-walk worker alive, fold in FSEvents
//! once that walk has finished, then summary, alerts, archive.

use crate::access;
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

/// How long the tick waits for the worker to finish its current slice (a
/// slice is at most `SLICE_WALL`, so this is ample).
const TICK_LOCK_WAIT: Duration = Duration::from_secs(5);

/// What the tick is doing, for its deadline message.
static STAGE: std::sync::Mutex<&'static str> = std::sync::Mutex::new("start");

fn stage(name: &'static str) {
    if let Ok(mut s) = STAGE.lock() {
        *s = name;
    }
}

/// A tick that has not finished after this long is stuck (18 Nov 2026: one sat
/// 24 minutes in open(2) on the archive drive, holding `walk.lock`, and
/// launchd never starts a second tick while one runs). The summary is written
/// before the archive ship, so ending the tick loses nothing but that ship.
const TICK_DEADLINE: Duration = Duration::from_secs(150);

pub fn start_tick_deadline() {
    std::thread::spawn(|| {
        std::thread::sleep(TICK_DEADLINE);
        let at = STAGE.lock().map(|s| *s).unwrap_or("?");
        eprintln!("diskmap: tick still running after {}s (stuck in {at}); exiting", TICK_DEADLINE.as_secs());
        std::process::exit(3);
    });
}

pub fn tick(roots: &Roots) -> Result<Summary, String> {
    darwin::be_gentle();
    let now = unix_now();
    let (cfg, host) = load(roots);
    let drives = load_drives(roots, &cfg);
    let mounted = darwin::list_volumes().unwrap_or_default();
    // Without Full Disk Access nothing privacy-gated is opened (W28, W29).
    stage("access");
    let access = access::refresh(&roots.state, now);
    access::apply(&roots.state, &access);
    let walk_vols = walkable(&mounted, access.full_disk);
    // The worker takes the walk lock one short slice at a time, so the tick
    // waits for the current slice instead of skipping its writes.
    let lock = wait_walk_lock(&roots.state.join("walk.lock"), TICK_LOCK_WAIT)?;
    let walker = lock.is_none();
    let mut logged = false;
    if walker {
        log_walker_skip(&mut logged);
    } else {
        // Replaying FSEvents only queues rescans, so it runs even while the
        // owner is at the Mac; the background worker drains the queue.
        stage("fsevents");
        // Only volumes the worker may walk: another volume's replay cursor
        // stays put, so its changes are replayed once access is granted.
        refresh_events(roots, &cfg, &walk_vols);
        if access.full_disk && access.requeue {
            stage("requeue gated");
            match requeue_gated(roots, &mounted) {
                Ok(n) => {
                    eprintln!("diskmap: Full Disk Access granted; {n} gated trees queued for a fresh walk");
                    let done = access::AccessState { requeue: false, ..access.clone() };
                    if let Err(err) = access::write_state(&roots.state, &done) {
                        eprintln!("diskmap: access state: {err}");
                    }
                }
                Err(err) => eprintln!("diskmap: requeue gated trees: {err}"),
            }
        }
        stage("volumes");
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
    stage("summary");
    let mut summary = build_summary(roots, &cfg, &host, &drives, &mounted, now)?;
    summary.access = Some(access_summary(roots, &mounted, &access));
    fs::create_dir_all(&roots.state).map_err(|e| e.to_string())?;
    fs::write(roots.state.join("summary.json"), summary::to_json(&summary)).map_err(|e| e.to_string())?;
    let where_file = build_where(roots, &host, &summary, now)?;
    let where_text = serde_json::to_string_pretty(&where_file).map_err(|e| e.to_string())? + "\n";
    fs::write(roots.state.join("where.json"), where_text).map_err(|e| e.to_string())?;
    drop(lock);
    stage("start worker");
    // After this tick's own map writes, so the worker never races them.
    if let Err(err) = ensure_walk_worker(roots, &walk_vols) {
        eprintln!("diskmap: {err}");
    }
    // Last, and outside the walk lock: the archive drive can block open(2)
    // for minutes (the tick deadline ends that), and nothing else waits on it.
    stage("archive ship");
    // Without Full Disk Access the archive drive (a volume under /Volumes) is
    // gated too: the ship spools instead.
    if let Err(err) = ship_archive(roots, &cfg, &drives, &host, &walk_vols) {
        if crate::sqlite::is_locked(&err) {
            log_walker_skip(&mut logged);
        } else {
            return Err(err);
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
        fold_events(vol, &db_path, &cfg.churn_trees);
        if engine::queue_len(&db_path).unwrap_or(0) == 0 {
            let _ = snapshot_if_due(&db_path, cfg.history_min_bytes);
        }
    }
}

/// Replay FSEvents since the stored id into the rescan queue. Cheap: it reads
/// the event log and queues directories; scanning happens in the worker.
fn fold_events(vol: &Mounted, db_path: &Path, churn: &[String]) {
    if let Ok((events, max_id, fs_uuid)) =
        darwin::replay_events(&vol.mount, vol.dev, stored_event_id(db_path), Duration::from_millis(250))
    {
        let scans = logic::coalesce(&events);
        // Never advance the replay cursor past events we failed to queue.
        // Holds are released here too, so this runs even with no new events.
        if engine::apply_plan(db_path, &scans, unix_now(), churn).is_ok() {
            let _ = remember_fs(db_path, max_id, &fs_uuid);
        }
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
            let queued = engine::queue_len(&db_path).unwrap_or(0) + engine::opaque_len(&db_path).unwrap_or(0);
            (baseline_done(&db_path), queued)
        })
        .collect();
    if !logic::needs_worker(&volumes) {
        return Ok(());
    }
    if walk_lock_held(&roots.state.join("worker.lock"))? {
        return Ok(());
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    Command::new(exe)
        .arg("walk")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(worker_log(roots))
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// The worker's log, `walk.log` in the state folder, kept under 1 MB (one
/// previous file, `walk.log.1`).
fn worker_log(roots: &Roots) -> std::process::Stdio {
    let path = roots.state.join("walk.log");
    if fs::metadata(&path).map(|m| m.len() > 1_000_000).unwrap_or(false) {
        let _ = fs::rename(&path, roots.state.join("walk.log.1"));
    }
    match OpenOptions::new().create(true).append(true).open(&path) {
        Ok(file) => file.into(),
        Err(_) => std::process::Stdio::null(),
    }
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

/// Take the walk lock, waiting up to `wait` for its holder to let go.
pub fn wait_walk_lock(path: &Path, wait: Duration) -> Result<Option<File>, String> {
    let started = Instant::now();
    loop {
        if let Some(file) = hold_walk_lock(path)? {
            return Ok(Some(file));
        }
        if started.elapsed() >= wait {
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(50));
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

/// Work done under the walk lock in one go; then the lock is let go and the
/// worker rests to keep its CPU budget.
const SLICE_DIRS: usize = 400;
const SLICE_WALL: Duration = Duration::from_millis(400);
/// The worker exits after this long and the tick starts a fresh one (so a new
/// binary takes over within the hour).
const WORKER_LIFETIME: Duration = Duration::from_secs(30 * 60);
/// How often the heat and focus checks run (focus spawns focus-guard).
const PACE_CHECK_EVERY: Duration = Duration::from_secs(20);
/// Accounting window for the CPU budget.
const BUDGET_WINDOW: Duration = Duration::from_secs(60);
/// Longest single rest, so the worker still notices heat and exits on time.
const MAX_REST: Duration = Duration::from_secs(30);
const LOCK_WAIT: Duration = Duration::from_secs(30);
const FOLD_EVERY: Duration = Duration::from_secs(30);

/// Keeps the worker within a CPU budget: after each slice it rests long
/// enough that CPU (its own and its children's) over wall time stays at the
/// budget, averaged over a window.
struct Pacer {
    cpu0: Duration,
    wall0: Instant,
    checked: Option<Instant>,
    thermal: bool,
    force: bool,
    focus: Option<bool>,
}

impl Pacer {
    fn new() -> Self {
        Pacer {
            cpu0: darwin::cpu_time(),
            wall0: Instant::now(),
            checked: None,
            thermal: false,
            force: false,
            focus: None,
        }
    }

    fn refresh(&mut self, roots: &Roots) {
        if self.checked.is_some_and(|t| t.elapsed() < PACE_CHECK_EVERY) {
            return;
        }
        self.force = std::env::var("DISKMAP_FORCE").ok().as_deref() == Some("1");
        self.thermal = latest_analytics_line(roots)
            .map(|line| darwin::thermal_blocks_walk(&line, unix_now()))
            .unwrap_or(false);
        self.focus = None;
        self.checked = Some(Instant::now());
    }

    fn drain(&mut self, roots: &Roots) -> logic::DrainPace {
        self.refresh(roots);
        logic::drain_pace(self.thermal, self.force)
    }

    /// Focus is only asked for when a first walk is what is left to do.
    fn baseline(&mut self, roots: &Roots) -> logic::DrainPace {
        self.refresh(roots);
        let focus = *self.focus.get_or_insert_with(darwin::focus_active);
        logic::baseline_pace(focus, self.thermal, self.force)
    }

    /// Rest so the window's CPU over wall time stays within `pace`.
    fn rest(&mut self, pace: logic::DrainPace) {
        let pct = match pace {
            logic::DrainPace::Full => return,
            logic::DrainPace::Paused => {
                std::thread::sleep(Duration::from_secs(5));
                return;
            }
            logic::DrainPace::Budget(pct) => pct,
        };
        let cpu = darwin::cpu_time().saturating_sub(self.cpu0);
        let wall = self.wall0.elapsed();
        let rest = logic::budget_sleep(cpu, wall, pct).min(MAX_REST);
        if !rest.is_zero() {
            std::thread::sleep(rest);
        }
        // Start a new window once the old one is paid for and long enough.
        if self.wall0.elapsed() >= BUDGET_WINDOW
            && logic::budget_sleep(darwin::cpu_time().saturating_sub(self.cpu0), self.wall0.elapsed(), pct).is_zero()
        {
            self.cpu0 = darwin::cpu_time();
            self.wall0 = Instant::now();
        }
    }
}

/// `diskmap walk`: the background worker. One per Mac (`worker.lock`, held by
/// the caller). It takes the walk lock one short slice at a time, so the tick
/// can always write, and keeps to its CPU budget between slices.
pub fn walk_worker(roots: &Roots) -> Result<(), String> {
    let born = Instant::now();
    let mut pacer = Pacer::new();
    darwin::set_skip(load_stuck(roots, unix_now()));
    // The tick probes for Full Disk Access; the worker follows its record.
    let mut full_disk = access::current(&roots.state, unix_now()).full_disk;
    access::apply(&roots.state, &access::current(&roots.state, unix_now()));
    start_watchdog(roots);
    let mut opaque: std::collections::HashMap<String, engine::OpaqueWalk> = std::collections::HashMap::new();
    {
        // Collapse any churn backlog first, before the first slice or measure.
        let (cfg, _) = load(roots);
        logic::set_protected(cfg.protected_trees.clone());
        let Some(_lock) = wait_walk_lock(&roots.state.join("walk.lock"), LOCK_WAIT)? else {
            return Ok(());
        };
        for vol in walkable(&darwin::list_volumes().unwrap_or_default(), full_disk) {
            let db_path = db_file(roots, &vol.uuid);
            if baseline_done(&db_path) {
                engine::migrate_churn(&db_path, unix_now(), &cfg.churn_trees)?;
            }
        }
    }
    let mut folded: Option<Instant> = None;
    let lock_path = roots.state.join("walk.lock");
    let log_every = Duration::from_secs(60);
    let mut logged = Instant::now();
    while born.elapsed() < WORKER_LIFETIME {
        let access_now = access::current(&roots.state, unix_now());
        if access_now.full_disk != full_disk {
            full_disk = access_now.full_disk;
            access::apply(&roots.state, &access_now);
            // Stuck folders recorded without access are tried again with it.
            darwin::set_skip(load_stuck(roots, unix_now()));
            opaque.clear();
        }
        let mounted = walkable(&darwin::list_volumes().unwrap_or_default(), full_disk);
        let (cfg, _) = load(roots);
        let fold_due = folded.is_none_or(|t| t.elapsed() >= FOLD_EVERY);
        if fold_due {
            folded = Some(Instant::now());
        }
        let mut remaining = false;
        for vol in &mounted {
            let db_path = db_file(roots, &vol.uuid);
            if baseline_done(&db_path) {
                let pace = pacer.drain(roots);
                if pace == logic::DrainPace::Paused {
                    remaining = true;
                    pacer.rest(pace);
                    continue;
                }
                if fold_due {
                    let Some(_lock) = wait_walk_lock(&lock_path, LOCK_WAIT)? else {
                        return Ok(());
                    };
                    let now = unix_now();
                    // Once per map: drop rows for directories deleted before scans did that.
                    if let Err(err) = engine::prune_missing(&vol.mount, &db_path, now, cfg.event_min_bytes, PRUNE_MIN_BYTES) {
                        eprintln!("diskmap: prune {}: {err}", vol.name);
                    }
                    fold_events(vol, &db_path, &cfg.churn_trees);
                }
                // A churn tree is measured one slice at a time with no lock
                // held, alternating with slices of the queue, so one big tree
                // never holds the queue up.
                if !opaque.contains_key(&vol.uuid)
                    && let Some(tree) = engine::opaque_next(&db_path)?
                {
                    opaque.insert(vol.uuid.clone(), engine::OpaqueWalk::new(&vol.mount, &tree));
                }
                if let Some(walk) = opaque.get_mut(&vol.uuid) {
                    remaining = true;
                    if walk.step(SLICE_WALL) {
                        let walk = opaque.remove(&vol.uuid).expect("present");
                        let Some(_lock) = wait_walk_lock(&lock_path, LOCK_WAIT)? else {
                            return Ok(());
                        };
                        engine::apply_opaque(&db_path, &walk.rel, &walk.total, unix_now(), cfg.event_min_bytes)?;
                    }
                    pacer.rest(pace);
                }
                let queued = engine::queue_len(&db_path).unwrap_or(0);
                if logged.elapsed() >= log_every && queued > 0 {
                    eprintln!("diskmap: worker {}: {queued} queued", vol.name);
                    logged = Instant::now();
                }
                if queued == 0 {
                    // Held rescans are released by the tick's fold, which
                    // starts a worker again when they reach the queue.
                    if let Some(_lock) = wait_walk_lock(&lock_path, LOCK_WAIT)? {
                        let _ = snapshot_if_due(&db_path, cfg.history_min_bytes);
                    }
                    continue;
                }
                remaining = true;
                {
                    let Some(_lock) = wait_walk_lock(&lock_path, LOCK_WAIT)? else {
                        return Ok(());
                    };
                    engine::walk_slice(
                        &vol.mount,
                        &db_path,
                        unix_now(),
                        cfg.event_min_bytes,
                        SLICE_DIRS,
                        SLICE_WALL,
                        &cfg.churn_trees,
                        |_| false,
                    )?;
                }
                pacer.rest(pace);
                continue;
            }
            remaining = true;
            let pace = pacer.baseline(roots);
            if pace == logic::DrainPace::Paused {
                pacer.rest(pace);
                break;
            }
            let Some(_lock) = wait_walk_lock(&lock_path, LOCK_WAIT)? else {
                // Another holder (an older worker that keeps the lock for its
                // whole life): leave; the tick tries again in a minute.
                return Ok(());
            };
            let now = unix_now();
            let _ = engine::remember_volume(
                &db_path,
                &vol.uuid,
                &vol.name,
                &vol.mount.to_string_lossy(),
                vol.total_bytes,
                vol.free_bytes,
                now,
            );
            engine::walk_slice(
                &vol.mount,
                &db_path,
                now,
                cfg.event_min_bytes,
                SLICE_DIRS,
                SLICE_WALL,
                &cfg.churn_trees,
                |_| false,
            )?;
            drop(_lock);
            pacer.rest(pace);
        }
        if !remaining {
            return Ok(());
        }
    }
    Ok(())
}

/// A read that has not returned after this long is stuck for good (a
/// directory read takes milliseconds; a launchd process opening a privacy-
/// protected folder its binary has no grant for waits indefinitely).
const STUCK_AFTER: Duration = Duration::from_secs(30);
/// A stuck directory is skipped for this long, then tried again.
const STUCK_SKIP_SECS: i64 = 7 * 86400;

fn stuck_path(roots: &Roots) -> PathBuf {
    roots.state.join("stuck.tsv")
}

fn is_boot(vol: &Mounted) -> bool {
    vol.mount == Path::new("/System/Volumes/Data") || vol.mount == Path::new("/")
}

/// The volumes the walker may open: all of them with Full Disk Access, else
/// only the boot volume (macOS asks before a process reads any other disk,
/// USB and Thunderbolt disks included, W29). Their free and total space come
/// from getfsstat, which needs no grant.
pub fn walkable(mounted: &[Mounted], full_disk: bool) -> Vec<Mounted> {
    mounted.iter().filter(|v| full_disk || is_boot(v)).cloned().collect()
}

/// Mapped rows of the boot volume that lie at a consent-gated root, with
/// their last sizes. Database only: nothing on disk is touched.
pub fn consent_rows(db: &crate::sqlite::Db) -> Result<Vec<(String, i64)>, String> {
    let mut out: Vec<(String, i64)> = Vec::new();
    for pattern in logic::CONSENT_TREES {
        if pattern.starts_with("**/") {
            continue;
        }
        let mut level: Vec<String> = vec![String::new()];
        for comp in pattern.split('/') {
            let mut next = Vec::new();
            for parent in &level {
                if comp.contains('*') {
                    for child in store::children(db, parent)? {
                        let name = child.path.rsplit('/').next().unwrap_or("").to_string();
                        if logic::glob_match(comp, &name) {
                            next.push(child.path);
                        }
                    }
                } else {
                    let path = engine::join_rel(parent, comp);
                    if store::dir(db, &path)?.is_some() {
                        next.push(path);
                    }
                }
            }
            level = next;
        }
        for path in level {
            if let Some(row) = store::dir(db, &path)? {
                out.push((path, row.total_bytes));
            }
        }
    }
    // Patterns that match anywhere (media libraries) count only outside the
    // roots above.
    out.sort();
    out.dedup();
    Ok(out)
}

/// Full Disk Access just arrived: queue every gated tree on the boot volume
/// for a deep rescan, and retire the lists of folders skipped without it.
fn requeue_gated(roots: &Roots, mounted: &[Mounted]) -> Result<usize, String> {
    let mut n = 0;
    let learned: Vec<PathBuf> = access::load_learned(&roots.state)
        .into_iter()
        .chain(load_stuck(roots, unix_now()))
        .collect();
    for vol in mounted.iter().filter(|v| is_boot(v)) {
        let db_path = db_file(roots, &vol.uuid);
        if !baseline_done(&db_path) {
            continue;
        }
        let db = store::open(&db_path)?;
        let mut paths: Vec<String> = consent_rows(&db)?.into_iter().map(|(p, _)| p).collect();
        for abs in &learned {
            if let Some(rel) = darwin::boot_rel(abs) {
                if store::dir(&db, &rel)?.is_some() {
                    paths.push(rel);
                }
            }
        }
        paths.sort();
        paths.dedup();
        for path in &paths {
            store::pending_push(&db, path, true)?;
            n += 1;
        }
    }
    for name in ["stuck.tsv", "gated.tsv"] {
        let from = roots.state.join(name);
        if from.exists() {
            let _ = fs::rename(&from, roots.state.join(format!("{name}.before-fda")));
        }
    }
    darwin::set_skip(Vec::new());
    Ok(n)
}

/// The summary's access block: without Full Disk Access, each gated tree
/// that the map holds (with its last size), each folder refused at run time,
/// and each other mounted volume.
fn access_summary(roots: &Roots, mounted: &[Mounted], access: &access::AccessState) -> summary::AccessSummary {
    let mut unmeasured = Vec::new();
    if !access.full_disk {
        let learned = access::load_learned(&roots.state);
        for vol in mounted {
            if vol.name == "Recovery" {
                // The boot disk's recovery system: nothing of the owner's.
                continue;
            }
            if !is_boot(vol) {
                unmeasured.push(summary::Unmeasured {
                    path: vol.mount.to_string_lossy().into_owned(),
                    bytes: vol.total_bytes.saturating_sub(vol.free_bytes),
                });
                continue;
            }
            let db_path = db_file(roots, &vol.uuid);
            let Ok(db) = store::open_readonly(&db_path) else { continue };
            let mut rows = consent_rows(&db).unwrap_or_default();
            for abs in &learned {
                if let Some(rel) = darwin::boot_rel(abs) {
                    if logic::consent_root(&rel).is_some() || rows.iter().any(|(p, _)| rel.starts_with(&format!("{p}/"))) {
                        continue;
                    }
                    // Only folders the map holds a size for: refusals inside a
                    // churn tree (system temp folders) are part of its total.
                    let Some(row) = store::dir(&db, &rel).ok().flatten() else { continue };
                    rows.push((rel, row.total_bytes));
                }
            }
            rows.sort();
            rows.dedup_by(|a, b| a.0 == b.0);
            for (path, bytes) in rows {
                unmeasured.push(summary::Unmeasured {
                    path: format!("/{path}"),
                    bytes: bytes.max(0) as u64,
                });
            }
        }
    }
    summary::AccessSummary {
        full_disk: access.full_disk,
        checked_at: access.checked_at,
        unmeasured,
    }
}

/// Directories that blocked a worker within `STUCK_SKIP_SECS`
/// (`stuck.tsv`: unix time, tab, absolute path).
fn load_stuck(roots: &Roots, now: i64) -> Vec<PathBuf> {
    let Ok(text) = fs::read_to_string(stuck_path(roots)) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| line.split_once('\t'))
        .filter(|(t, _)| t.parse::<i64>().is_ok_and(|t| now - t < STUCK_SKIP_SECS))
        .map(|(_, p)| PathBuf::from(p))
        .collect()
}

/// A blocked open(2) cannot be interrupted, and a worker blocked in one holds
/// `worker.lock` so no other starts. The watchdog records the directory in
/// `stuck.tsv`, says so in `walk.log`, and ends the process; the next tick
/// starts a worker that skips it.
fn start_watchdog(roots: &Roots) {
    let path = stuck_path(roots);
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(5));
        let Some((held, dir)) = darwin::reading_now() else { continue };
        if held < STUCK_AFTER {
            continue;
        }
        let known: Vec<PathBuf> = fs::read_to_string(&path)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.split_once('\t').map(|(_, p)| PathBuf::from(p)))
            .collect();
        let skip = logic::stuck_entry(&dir, &known);
        eprintln!(
            "diskmap: stuck {}s reading {}; skipping {} for a week",
            held.as_secs(),
            dir.display(),
            skip.display()
        );
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&path) {
            let _ = writeln!(file, "{}\t{}", unix_now(), skip.display());
        }
        std::process::exit(3);
    });
}

fn stored_event_id(db_path: &Path) -> u64 {
    engine::meta(db_path, "fsevent_id")
        .ok()
        .flatten()
        .and_then(|s| s.parse().ok())
        // A short-lived polling stream must replay history on first use.
        // "Since now" returned no cursor and skipped its run loop, so every
        // subsequent tick missed changes again indefinitely.
        .unwrap_or(0)
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
        access: None,
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
                // Shipping is idempotent (events keyed by id), so if the
                // worker holds the map now, the next tick records it.
                let Some(_lock) = wait_walk_lock(&roots.state.join("walk.lock"), TICK_LOCK_WAIT)? else {
                    continue;
                };
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
    fn missing_or_invalid_event_cursor_replays_history() {
        let dir = scratch();
        let path = dir.join("index.db");
        let db = store::open(&path).unwrap();
        assert_eq!(stored_event_id(&path), 0);
        store::meta_set(&db, "fsevent_id", "invalid").unwrap();
        assert_eq!(stored_event_id(&path), 0);
        store::meta_set(&db, "fsevent_id", "12345").unwrap();
        assert_eq!(stored_event_id(&path), 12345);
        drop(db);
        fs::remove_dir_all(dir).unwrap();
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

