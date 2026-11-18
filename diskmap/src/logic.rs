//! Pure diskmap rules: config, sizes, FSEvents coalescing, growth collapse,
//! free-space alerts, drive grouping, archive permission. No I/O.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap};

pub const SF_DATALESS: u32 = 0x4000_0000;
pub const EVENT_MIN_BYTES: i64 = 50 * 1024 * 1024;
pub const HISTORY_MIN_BYTES: i64 = 100 * 1024 * 1024;
pub const HOT_DAYS: i64 = 30;
pub const ALERT_GB_PER_HOUR: f64 = 15.0;
pub const ALERT_SUSTAIN_SECS: i64 = 30 * 60;
pub const ALERT_COOLDOWN_SECS: i64 = 3600;

pub const MUST_SCAN_SUBDIRS: u32 = 0x0000_0001;
pub const HISTORY_DONE: u32 = 0x0000_0010;
pub const ROOT_CHANGED: u32 = 0x0000_0020;
pub const ITEM_IS_FILE: u32 = 0x0001_0000;
pub const ITEM_IS_DIR: u32 = 0x0002_0000;

#[derive(Clone, Debug, PartialEq)]
pub struct HostProfile {
    pub id: String,
    pub floor_gb: Option<f64>,
}

#[derive(Clone, Debug)]
pub struct DiskConfig {
    pub hosts: BTreeMap<String, HostProfile>,
    pub peers: Vec<String>,
    pub archive_volume: String,
    pub archive_rel: String,
    pub event_min_bytes: i64,
    pub history_min_bytes: i64,
    pub hot_days: i64,
    pub alert_gb_per_hour: f64,
    pub alert_sustain_secs: i64,
    pub alert_cooldown_secs: i64,
    pub drives_json: Option<String>,
    /// Extra temp, cache or churn trees (volume-relative patterns, see
    /// `churn_root`), mapped as one opaque total instead of row by row.
    pub churn_trees: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedHost {
    pub local_host_name: String,
    pub id: String,
    pub floor_gb: Option<f64>,
}

pub fn default_config() -> DiskConfig {
    let mut hosts = BTreeMap::new();
    hosts.insert(
        "Coordinator-Mac".to_string(),
        HostProfile {
            id: "desk".into(),
            floor_gb: Some(150.0),
        },
    );
    hosts.insert(
        "Worker-One".to_string(),
        HostProfile {
            id: "lab".into(),
            floor_gb: Some(40.0),
        },
    );
    hosts.insert(
        "Worker-Two".to_string(),
        HostProfile {
            id: "studio".into(),
            floor_gb: Some(25.0),
        },
    );
    DiskConfig {
        hosts,
        peers: Vec::new(),
        archive_volume: "SecureArchive".into(),
        archive_rel: "diskmap".into(),
        event_min_bytes: EVENT_MIN_BYTES,
        history_min_bytes: HISTORY_MIN_BYTES,
        hot_days: HOT_DAYS,
        alert_gb_per_hour: ALERT_GB_PER_HOUR,
        alert_sustain_secs: ALERT_SUSTAIN_SECS,
        alert_cooldown_secs: ALERT_COOLDOWN_SECS,
        drives_json: None,
        churn_trees: Vec::new(),
    }
}

#[derive(Deserialize)]
struct ConfigFile {
    hosts: Option<BTreeMap<String, HostJson>>,
    peers: Option<Vec<String>>,
    archive_volume: Option<String>,
    archive_rel: Option<String>,
    event_min_bytes: Option<i64>,
    history_min_bytes: Option<i64>,
    hot_days: Option<i64>,
    alert_gb_per_hour: Option<f64>,
    alert_sustain_secs: Option<i64>,
    alert_cooldown_secs: Option<i64>,
    drives_json: Option<String>,
    churn_trees: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct HostJson {
    id: String,
    floor_gb: Option<f64>,
}

pub fn load_config(text: Option<&str>) -> Result<DiskConfig, String> {
    let mut cfg = default_config();
    let Some(text) = text.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(cfg);
    };
    let parsed: ConfigFile =
        serde_json::from_str(text).map_err(|e| format!("config: {e}"))?;
    if let Some(hosts) = parsed.hosts {
        for (name, host) in hosts {
            cfg.hosts.insert(
                name,
                HostProfile {
                    id: host.id,
                    floor_gb: host.floor_gb,
                },
            );
        }
    }
    if let Some(peers) = parsed.peers {
        cfg.peers = peers.into_iter().filter(|p| !p.is_empty()).collect();
    }
    if let Some(v) = parsed.archive_volume {
        cfg.archive_volume = v;
    }
    if let Some(v) = parsed.archive_rel {
        cfg.archive_rel = v;
    }
    if let Some(v) = parsed.event_min_bytes {
        cfg.event_min_bytes = v;
    }
    if let Some(v) = parsed.history_min_bytes {
        cfg.history_min_bytes = v;
    }
    if let Some(v) = parsed.hot_days {
        cfg.hot_days = v;
    }
    if let Some(v) = parsed.alert_gb_per_hour {
        cfg.alert_gb_per_hour = v;
    }
    if let Some(v) = parsed.alert_sustain_secs {
        cfg.alert_sustain_secs = v;
    }
    if let Some(v) = parsed.alert_cooldown_secs {
        cfg.alert_cooldown_secs = v;
    }
    if let Some(v) = parsed.drives_json.filter(|s| !s.is_empty()) {
        cfg.drives_json = Some(v);
    }
    if let Some(v) = parsed.churn_trees {
        cfg.churn_trees = v.into_iter().map(|p| norm_rel(&p)).filter(|p| !p.is_empty()).collect();
    }
    Ok(cfg)
}

pub fn resolve_host(cfg: &DiskConfig, local_host_name: &str) -> ResolvedHost {
    if let Some(host) = cfg.hosts.get(local_host_name) {
        return ResolvedHost {
            local_host_name: local_host_name.to_string(),
            id: host.id.clone(),
            floor_gb: host.floor_gb,
        };
    }
    ResolvedHost {
        local_host_name: local_host_name.to_string(),
        id: local_host_name.to_string(),
        floor_gb: None,
    }
}

pub fn parse_since(text: &str) -> Result<i64, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("empty duration".into());
    }
    let (num, unit) = text.split_at(text.len() - 1);
    let n: i64 = num
        .parse()
        .map_err(|_| format!("bad duration {text}"))?;
    let secs = match unit {
        "s" => n,
        "m" => n.saturating_mul(60),
        "h" => n.saturating_mul(3600),
        "d" => n.saturating_mul(86400),
        _ => return Err(format!("bad duration {text}")),
    };
    if secs < 0 {
        return Err(format!("bad duration {text}"));
    }
    Ok(secs)
}

#[derive(Clone, Debug)]
pub struct FileAttr {
    pub allocated: u64,
    pub logical: u64,
    pub flags: u32,
    pub mtime: i64,
    pub is_dir: bool,
    pub mount_point: bool,
    pub symlink: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DirAccount {
    pub own_bytes: u64,
    pub own_dataless: u64,
    pub files: u64,
    pub newest: i64,
    pub subdirs: Vec<String>,
}

/// Local allocated bytes stay in `own_bytes`. Dataless (cloud-only) bytes are
/// the logical size that is not on disk. A partial download counts the
/// allocated part as local and the rest as dataless.
pub fn account_entries(entries: &[(&str, FileAttr)]) -> DirAccount {
    let mut out = DirAccount::default();
    for (name, attr) in entries {
        if *name == "." || *name == ".." || name.is_empty() {
            continue;
        }
        if attr.is_dir && !attr.symlink {
            if !attr.mount_point {
                out.subdirs.push((*name).to_string());
            }
            continue;
        }
        let (local, dataless) = split_bytes(attr.allocated, attr.logical, attr.flags);
        out.own_bytes = out.own_bytes.saturating_add(local);
        out.own_dataless = out.own_dataless.saturating_add(dataless);
        out.files += 1;
        if attr.mtime > out.newest {
            out.newest = attr.mtime;
        }
    }
    out.subdirs.sort();
    out
}

pub fn split_bytes(allocated: u64, logical: u64, flags: u32) -> (u64, u64) {
    if flags & SF_DATALESS != 0 {
        let local = allocated.min(logical);
        let cloud = logical.saturating_sub(local);
        (local, cloud)
    } else {
        (allocated, 0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PhotoKind {
    Originals,
    Derivatives,
    Other,
}

pub fn photos_kind(path: &str) -> PhotoKind {
    let p = path.replace('\\', "/");
    let lower = p.to_lowercase();
    if lower.contains("/originals/")
        || lower.ends_with("/originals")
        || lower.contains("/masters/")
        || lower.ends_with("/masters")
    {
        PhotoKind::Originals
    } else if lower.contains("/resources/derivatives")
        || lower.contains("/resources/renders")
        || lower.contains("/resources/caches")
    {
        PhotoKind::Derivatives
    } else {
        PhotoKind::Other
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawEvent {
    pub path: String,
    pub flags: u32,
    pub id: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScanReq {
    Dir(String),
    Subtree(String),
}

pub fn norm_rel(path: &str) -> String {
    let path = path.trim().trim_matches('/');
    if path == "." {
        String::new()
    } else {
        path.to_string()
    }
}

fn parent_rel(path: &str) -> String {
    match path.rfind('/') {
        Some(i) => path[..i].to_string(),
        None => String::new(),
    }
}

/// Collapse a batch of FSEvents into directory rescans.
/// `MustScanSubDirs` (and a root change) covers every descendant.
/// File events rescan the parent directory. Duplicate paths merge.
pub fn coalesce(events: &[RawEvent]) -> Vec<ScanReq> {
    let mut subtree: BTreeSet<String> = BTreeSet::new();
    let mut dirs: BTreeSet<String> = BTreeSet::new();
    for event in events {
        if event.flags & HISTORY_DONE != 0 {
            continue;
        }
        let mut path = norm_rel(&event.path);
        let file = event.flags & ITEM_IS_FILE != 0 && event.flags & ITEM_IS_DIR == 0;
        if file {
            path = parent_rel(&path);
        }
        if event.flags & (MUST_SCAN_SUBDIRS | ROOT_CHANGED) != 0 {
            subtree.insert(path);
        } else {
            dirs.insert(path);
        }
    }
    let drop_sub: Vec<String> = subtree
        .iter()
        .filter(|path| covered_by_ancestor(&subtree, path))
        .cloned()
        .collect();
    for path in drop_sub {
        subtree.remove(&path);
    }
    let drop_dirs: Vec<String> = dirs
        .iter()
        .filter(|path| subtree.iter().any(|sub| is_under(path, sub)))
        .cloned()
        .collect();
    for path in drop_dirs {
        dirs.remove(&path);
    }
    let mut out: Vec<ScanReq> = subtree.into_iter().map(ScanReq::Subtree).collect();
    out.extend(dirs.into_iter().map(ScanReq::Dir));
    out.sort_by(|a, b| scan_path(a).cmp(scan_path(b)));
    out
}

pub fn scan_path(req: &ScanReq) -> &str {
    match req {
        ScanReq::Dir(p) | ScanReq::Subtree(p) => p,
    }
}

fn covered_by_ancestor(set: &BTreeSet<String>, path: &str) -> bool {
    set.iter().any(|other| other != path && is_under(path, other))
}

fn is_under(path: &str, ancestor: &str) -> bool {
    if ancestor.is_empty() {
        return !path.is_empty();
    }
    path.starts_with(ancestor)
        && path.as_bytes().get(ancestor.len()) == Some(&b'/')
}

#[derive(Clone, Debug, PartialEq)]
pub struct Growth {
    pub path: String,
    pub delta: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Grew {
    pub path: String,
    pub delta: i64,
    pub share: f64,
}

/// Deepest directory whose contents explain at least 80% of the growth.
/// Deltas are inclusive directory totals when a parent row is present.
/// Positive growth only; deletions are ignored.
pub fn collapse_grew(rows: &[Growth]) -> Option<Grew> {
    let mut recorded: HashMap<String, i64> = HashMap::new();
    for row in rows {
        if row.delta <= 0 {
            continue;
        }
        let path = norm_rel(&row.path);
        *recorded.entry(path).or_insert(0) += row.delta;
    }
    if recorded.is_empty() {
        return None;
    }
    let mut nodes: BTreeSet<String> = BTreeSet::new();
    nodes.insert(String::new());
    for path in recorded.keys() {
        let mut cur = path.clone();
        loop {
            nodes.insert(cur.clone());
            if cur.is_empty() {
                break;
            }
            cur = parent_rel(&cur);
        }
    }
    let children = child_index(&nodes);
    let mut memo: HashMap<String, i64> = HashMap::new();
    fn inclusive(
        path: &str,
        recorded: &HashMap<String, i64>,
        children: &HashMap<String, Vec<String>>,
        memo: &mut HashMap<String, i64>,
    ) -> i64 {
        if let Some(v) = memo.get(path) {
            return *v;
        }
        let sum: i64 = children
            .get(path)
            .map(|kids| {
                kids.iter()
                    .map(|kid| inclusive(kid, recorded, children, memo))
                    .sum()
            })
            .unwrap_or(0);
        let rec = recorded.get(path).copied().unwrap_or(0).max(0);
        let v = rec.max(sum);
        memo.insert(path.to_string(), v);
        v
    }
    let root_total = inclusive("", &recorded, &children, &mut memo);
    if root_total <= 0 {
        return None;
    }
    let mut current = String::new();
    loop {
        let total = inclusive(&current, &recorded, &children, &mut memo);
        let mut best: Option<(String, i64)> = None;
        if let Some(kids) = children.get(&current) {
            for kid in kids {
                let v = inclusive(kid, &recorded, &children, &mut memo);
                if v <= 0 {
                    continue;
                }
                if best.as_ref().map(|(_, b)| v > *b).unwrap_or(true) {
                    best = Some((kid.clone(), v));
                }
            }
        }
        if let Some((kid, v)) = best {
            if total > 0 && (v as f64) >= 0.8 * (total as f64) {
                current = kid;
                continue;
            }
        }
        break;
    }
    let delta = inclusive(&current, &recorded, &children, &mut memo);
    Some(Grew {
        path: current,
        delta,
        share: delta as f64 / root_total as f64,
    })
}

fn child_index(nodes: &BTreeSet<String>) -> HashMap<String, Vec<String>> {
    let mut children: HashMap<String, Vec<String>> = HashMap::new();
    for node in nodes {
        if node.is_empty() {
            continue;
        }
        let parent = parent_rel(node);
        children.entry(parent).or_default().push(node.clone());
    }
    children
}

pub fn samples_from_jsonl(text: &str) -> Vec<(i64, f64)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(t) = value.get("t").and_then(|v| v.as_i64()) else {
            continue;
        };
        let Some(free) = value.get("diskFreeGB").and_then(|v| v.as_f64()) else {
            continue;
        };
        out.push((t, free));
    }
    out.sort_by_key(|(t, _)| *t);
    out
}

#[derive(Clone, Debug, PartialEq)]
pub struct Trend {
    pub h1: Option<f64>,
    pub h24: Option<f64>,
    pub d7: Option<f64>,
}

/// Change in free GB over each window (negative when free space fell).
pub fn trend(samples: &[(i64, f64)], now: i64) -> Trend {
    Trend {
        h1: free_delta(samples, now, 3600),
        h24: free_delta(samples, now, 86400),
        d7: free_delta(samples, now, 7 * 86400),
    }
}

fn free_delta(samples: &[(i64, f64)], now: i64, horizon: i64) -> Option<f64> {
    let newer = latest_at_or_before(samples, now)?;
    let older = latest_at_or_before(samples, now - horizon)?;
    if older.0 > now - horizon {
        return None;
    }
    if newer.0 <= older.0 {
        return None;
    }
    Some(newer.1 - older.1)
}

fn latest_at_or_before(samples: &[(i64, f64)], t: i64) -> Option<(i64, f64)> {
    samples.iter().rev().find(|(ts, _)| *ts <= t).copied()
}

/// GB/h of free space lost when the drop has held for `sustain_secs`.
pub fn sustained_drop(
    samples: &[(i64, f64)],
    now: i64,
    sustain_secs: i64,
    gb_per_hour: f64,
) -> Option<f64> {
    let newer = latest_at_or_before(samples, now)?;
    if now - newer.0 > 5 * 60 {
        return None;
    }
    let older = latest_at_or_before(samples, now - sustain_secs)?;
    if older.0 > now - sustain_secs {
        return None;
    }
    let span = newer.0 - older.0;
    if span < sustain_secs {
        return None;
    }
    let lost = older.1 - newer.1;
    let rate = lost / (span as f64 / 3600.0);
    if rate > gb_per_hour { Some(rate) } else { None }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Alert {
    pub t: i64,
    pub cause: String,
    pub host: String,
    pub location: String,
    pub free_gb: f64,
    pub rate_gb_h: Option<f64>,
    pub grower: String,
    pub message: String,
}

pub fn floor_breach(free_gb: f64, floor_gb: Option<f64>) -> bool {
    floor_gb.is_some_and(|floor| free_gb < floor)
}

pub fn gate_alerts(fresh: Vec<Alert>, prior: &[Alert], cooldown_secs: i64) -> Vec<Alert> {
    fresh
        .into_iter()
        .filter(|alert| {
            !prior.iter().any(|old| {
                old.cause == alert.cause && alert.t >= old.t && alert.t - old.t < cooldown_secs
            })
        })
        .collect()
}

pub fn rate_cause(host: &str, location: &str, grower: &str) -> String {
    format!("rate:{host}:{location}:{grower}")
}

pub fn floor_cause(host: &str, location: &str) -> String {
    format!("floor:{host}:{location}")
}

#[derive(Clone, Debug, PartialEq)]
pub struct VolumeDef {
    pub name: String,
    pub uuid: String,
    pub encrypted: bool,
    pub shared_container: bool,
    pub bundle: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DriveDef {
    pub name: String,
    pub volumes: Vec<VolumeDef>,
}

#[derive(Clone, Debug)]
pub struct VolumeObs {
    pub uuid: String,
    pub name: String,
    pub total_bytes: u64,
    pub free_bytes: u64,
    pub mounted: bool,
    pub last_seen: i64,
    pub encrypted: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DriveRollup {
    pub name: String,
    pub mounted: bool,
    pub total_bytes: u64,
    pub free_bytes: u64,
    pub last_seen: i64,
    pub volume_uuids: Vec<String>,
    pub encrypted_mounted: bool,
}

#[derive(Deserialize)]
struct DrivesFile {
    drives: Vec<DriveJson>,
}

#[derive(Deserialize)]
struct DriveJson {
    name: String,
    volumes: BTreeMap<String, VolJson>,
}

#[derive(Deserialize)]
struct VolJson {
    uuid: Option<String>,
    encrypted: Option<bool>,
    shared_container: Option<bool>,
    bundle: Option<String>,
}

pub fn parse_drives(text: &str) -> Result<Vec<DriveDef>, String> {
    let parsed: DrivesFile = serde_json::from_str(text).map_err(|e| format!("drives.json: {e}"))?;
    let mut drives = Vec::new();
    for drive in parsed.drives {
        let mut volumes = Vec::new();
        for (name, vol) in drive.volumes {
            volumes.push(VolumeDef {
                name,
                uuid: vol.uuid.unwrap_or_default(),
                encrypted: vol.encrypted.unwrap_or(false),
                shared_container: vol.shared_container.unwrap_or(false),
                bundle: vol.bundle.is_some(),
            });
        }
        drives.push(DriveDef {
            name: drive.name,
            volumes,
        });
    }
    Ok(drives)
}

pub fn drive_of<'a>(drives: &'a [DriveDef], uuid: &str) -> Option<(&'a DriveDef, &'a VolumeDef)> {
    drives.iter().find_map(|drive| {
        drive
            .volumes
            .iter()
            .find(|vol| vol.uuid.eq_ignore_ascii_case(uuid))
            .map(|vol| (drive, vol))
    })
}

/// One row per physical drive. Shared-container volumes are one capacity pool
/// (max, not sum). A sparsebundle volume sits inside its host volume, so its
/// size is not added again.
pub fn group_drives(defs: &[DriveDef], obs: &[VolumeObs]) -> Vec<DriveRollup> {
    let mut rows = Vec::new();
    let mut claimed: BTreeSet<String> = BTreeSet::new();
    for drive in defs {
        let mut matched: Vec<(&VolumeDef, Option<&VolumeObs>)> = Vec::new();
        for vol in &drive.volumes {
            let found = obs
                .iter()
                .find(|o| o.uuid.eq_ignore_ascii_case(&vol.uuid));
            if let Some(o) = found {
                claimed.insert(o.uuid.to_ascii_uppercase());
            }
            matched.push((vol, found));
        }
        let mounted = matched.iter().any(|(_, o)| o.is_some_and(|o| o.mounted));
        let last_seen = matched
            .iter()
            .filter_map(|(_, o)| o.map(|o| o.last_seen))
            .max()
            .unwrap_or(0);
        let shared = drive.volumes.iter().any(|v| v.shared_container);
        let (total, free) = if shared {
            let pool: Vec<_> = matched.iter().filter_map(|(_, o)| *o).collect();
            let total = pool.iter().map(|o| o.total_bytes).max().unwrap_or(0);
            let free = pool.iter().map(|o| o.free_bytes).max().unwrap_or(0);
            (total, free)
        } else {
            let mut total = 0u64;
            let mut free = 0u64;
            for (vol, obs) in &matched {
                if vol.bundle {
                    continue;
                }
                if let Some(o) = obs {
                    total = total.saturating_add(o.total_bytes);
                    free = free.saturating_add(o.free_bytes);
                }
            }
            (total, free)
        };
        rows.push(DriveRollup {
            name: drive.name.clone(),
            mounted,
            total_bytes: total,
            free_bytes: free,
            last_seen,
            volume_uuids: drive.volumes.iter().map(|v| v.uuid.clone()).collect(),
            encrypted_mounted: matched
                .iter()
                .any(|(v, o)| v.encrypted && o.is_some_and(|o| o.mounted)),
        });
    }
    for vol in obs {
        if claimed.contains(&vol.uuid.to_ascii_uppercase()) {
            continue;
        }
        rows.push(DriveRollup {
            name: vol.name.clone(),
            mounted: vol.mounted,
            total_bytes: vol.total_bytes,
            free_bytes: vol.free_bytes,
            last_seen: vol.last_seen,
            volume_uuids: vec![vol.uuid.clone()],
            encrypted_mounted: vol.encrypted && vol.mounted,
        });
    }
    rows
}

/// Archive writes are allowed only under `/Volumes/<name>` whose registry
/// entry is an encrypted volume. A temp directory is not.
pub fn archive_allowed(dest: &str, drives: &[DriveDef]) -> Result<String, String> {
    let parts: Vec<&str> = dest.split('/').filter(|p| !p.is_empty()).collect();
    if parts.len() < 3 || parts[0] != "Volumes" {
        return Err(format!(
            "archive {dest} is not on an encrypted volume (refusing)"
        ));
    }
    let volume = parts[1];
    let encrypted = drives.iter().any(|drive| {
        drive
            .volumes
            .iter()
            .any(|vol| vol.name == volume && vol.encrypted)
    });
    if !encrypted {
        return Err(format!(
            "archive {dest} is not on an encrypted volume (refusing)"
        ));
    }
    Ok(volume.to_string())
}

pub fn glob_match(pattern: &str, text: &str) -> bool {
    fn rec(pat: &[u8], text: &[u8]) -> bool {
        let mut i = 0;
        let mut j = 0;
        while i < pat.len() {
            if pat[i] == b'*' {
                if i + 1 == pat.len() {
                    return true;
                }
                while j <= text.len() {
                    if rec(&pat[i + 1..], &text[j..]) {
                        return true;
                    }
                    if j == text.len() {
                        return false;
                    }
                    j += 1;
                }
                return false;
            }
            if j >= text.len() {
                return false;
            }
            if pat[i] != b'?' && pat[i] != text[j] {
                return false;
            }
            i += 1;
            j += 1;
        }
        j == text.len()
    }
    let pat = pattern.to_lowercase();
    let text = text.to_lowercase();
    rec(pat.as_bytes(), text.as_bytes())
}

pub fn where_match(pattern: &str, path: &str) -> bool {
    if glob_match(pattern, path) {
        return true;
    }
    let base = path.rsplit('/').next().unwrap_or(path);
    glob_match(pattern, base)
}

pub fn home_relative(volume_path: &str) -> Option<String> {
    let path = norm_rel(volume_path);
    let mut parts = path.split('/');
    if parts.next() != Some("Users") {
        return None;
    }
    let user = parts.next().unwrap_or("");
    if user.is_empty() {
        return None;
    }
    let rest: Vec<&str> = parts.collect();
    if rest.is_empty() {
        Some(String::new())
    } else {
        Some(rest.join("/"))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct LogRecord {
    pub datetime: String,
    pub status: String,
    pub logfile: String,
}

pub fn parse_log_records(xml: &str) -> Vec<LogRecord> {
    let mut out = Vec::new();
    for chunk in xml.split("<record").skip(1) {
        let body = chunk.split("</record>").next().unwrap_or("");
        let datetime = xml_tag(body, "datetime").unwrap_or_default();
        let status = xml_tag(body, "status").unwrap_or_default();
        let logfile = xml_tag(body, "logfile").unwrap_or_default();
        if datetime.is_empty() && status.is_empty() {
            continue;
        }
        out.push(LogRecord {
            datetime,
            status,
            logfile,
        });
    }
    out
}

fn xml_tag(body: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = body.find(&open)? + open.len();
    let end = body[start..].find(&close)? + start;
    Some(body[start..end].trim().to_string())
}

pub fn confirm_success(status: &str, failed: Option<u64>) -> bool {
    status.trim().eq_ignore_ascii_case("success") && matches!(failed, None | Some(0))
}

#[derive(Clone, Debug, PartialEq)]
pub struct SummaryParse {
    pub end_time: Option<String>,
    pub failed: Option<u64>,
    pub considered: Option<u64>,
}

pub fn parse_backup_summary(text: &str) -> SummaryParse {
    SummaryParse {
        end_time: labelled(text, "End Time"),
        failed: labelled_u64(text, "Files failed to backup"),
        considered: labelled_u64(text, "Files considered for backup"),
    }
}

fn labelled(text: &str, key: &str) -> Option<String> {
    for line in text.lines() {
        let line = line.trim();
        let Some(rest) = line.split_once(':') else {
            continue;
        };
        if rest.0.trim().eq_ignore_ascii_case(key) {
            let value = rest.1.trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

fn labelled_u64(text: &str, key: &str) -> Option<u64> {
    labelled(text, key)?.parse().ok()
}

/// `MM-DD-YYYY HH:MM:SS` as a local timestamp, matching IDrive's log clock.
pub fn parse_local_mdy(text: &str) -> Option<i64> {
    let text = text.trim();
    let (date, time) = text.split_once(' ')?;
    let mut d = date.split('-');
    let month: i32 = d.next()?.parse().ok()?;
    let day: i32 = d.next()?.parse().ok()?;
    let year: i32 = d.next()?.parse().ok()?;
    let mut t = time.split(':');
    let hour: i32 = t.next()?.parse().ok()?;
    let min: i32 = t.next()?.parse().ok()?;
    let sec: i32 = t.next()?.parse().ok()?;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    tm.tm_year = year - 1900;
    tm.tm_mon = month - 1;
    tm.tm_mday = day;
    tm.tm_hour = hour;
    tm.tm_min = min;
    tm.tm_sec = sec;
    tm.tm_isdst = -1;
    let unix = unsafe { libc::mktime(&mut tm) };
    if unix < 0 { None } else { Some(unix) }
}

pub fn plist_path_strings(value: &Value) -> Vec<String> {
    fn walk(value: &Value, out: &mut Vec<String>) {
        match value {
            Value::String(s) => out.push(s.clone()),
            Value::Array(items) => {
                for item in items {
                    walk(item, out);
                }
            }
            Value::Object(map) => {
                let keyed = ["path", "Path", "backupPath", "filePath", "folderPath", "name"]
                    .iter()
                    .find_map(|key| map.get(*key).and_then(|v| v.as_str()));
                if let Some(path) = keyed {
                    out.push(path.to_string());
                    return;
                }
                for item in map.values() {
                    if item.is_array() || item.is_object() {
                        walk(item, out);
                    }
                }
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(value, &mut out);
    out
}

pub fn fmt_gb(bytes: i64) -> String {
    let v = bytes as f64 / 1e9;
    if v.abs() >= 10.0 {
        format!("{v:.0} GB")
    } else {
        format!("{v:.1} GB")
    }
}

pub fn fmt_gb_signed(bytes: i64) -> String {
    if bytes > 0 {
        format!("+{}", fmt_gb(bytes))
    } else if bytes < 0 {
        format!("-{}", fmt_gb(bytes.abs()))
    } else {
        format!("{}", fmt_gb(0))
    }
}

pub fn fmt_age(now: i64, then: i64) -> String {
    if then <= 0 {
        return "—".into();
    }
    let s = (now - then).max(0);
    if s < 90 {
        format!("{s}s")
    } else if s < 90 * 60 {
        format!("{}m", s / 60)
    } else if s < 36 * 3600 {
        format!("{}h", s / 3600)
    } else {
        format!("{}d", s / 86400)
    }
}

pub fn local_day(unix: i64) -> String {
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let t = unix as libc::time_t;
    unsafe {
        libc::localtime_r(&t, &mut tm);
    }
    format!(
        "{:04}-{:02}-{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday
    )
}

pub fn basename(path: &str) -> &str {
    path.rsplit('/').next().filter(|s| !s.is_empty()).unwrap_or(path)
}

/// Estimated walk coverage. `used` is statfs total minus free. While the walk
/// is still running the figure stays under 100 so it is not read as finished.
pub fn map_percent(mapped: u64, used: u64, done: bool) -> u8 {
    if done {
        return 100;
    }
    if used == 0 {
        return 0;
    }
    let pct = (u128::from(mapped) * 100 / u128::from(used)).min(99);
    pct as u8
}

/// Pause the long walk while the owner is at the keyboard or the machine is
/// in serious or critical thermal pressure. `force` is `DISKMAP_FORCE=1`.
pub fn pause_walk(focus_active: bool, thermal_serious: bool, force: bool) -> bool {
    if force {
        return false;
    }
    focus_active || thermal_serious
}

/// How the background worker may spend CPU.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrainPace {
    /// `DISKMAP_FORCE=1` only: no budget.
    Full,
    /// At most this many percent of one core, averaged (CPU of the worker and
    /// the checks it runs, against wall time).
    Budget(u32),
    Paused,
}

/// Rescans after the baseline: 10% of one core, always. A map that only
/// updates while nobody is looking goes stale, so owner focus does not stop
/// them; serious heat does.
pub const DRAIN_BUDGET_PCT: u32 = 10;
/// The first walk of a volume, while nobody is at the Mac (focus pauses it).
pub const BASELINE_BUDGET_PCT: u32 = 25;

pub fn drain_pace(thermal_serious: bool, force: bool) -> DrainPace {
    if force {
        DrainPace::Full
    } else if thermal_serious {
        DrainPace::Paused
    } else {
        DrainPace::Budget(DRAIN_BUDGET_PCT)
    }
}

pub fn baseline_pace(focus_active: bool, thermal_serious: bool, force: bool) -> DrainPace {
    if force {
        DrainPace::Full
    } else if pause_walk(focus_active, thermal_serious, false) {
        DrainPace::Paused
    } else {
        DrainPace::Budget(BASELINE_BUDGET_PCT)
    }
}

/// How long to rest so that `cpu` spent over `wall` averages `pct` percent of
/// one core: the work already done paid for at the budget rate, less the time
/// already passed.
pub fn budget_sleep(cpu: std::time::Duration, wall: std::time::Duration, pct: u32) -> std::time::Duration {
    if pct == 0 || pct >= 100 {
        return std::time::Duration::ZERO;
    }
    let owed = cpu.as_secs_f64() * 100.0 / f64::from(pct);
    let rest = owed - wall.as_secs_f64();
    if rest > 0.0 {
        std::time::Duration::from_secs_f64(rest)
    } else {
        std::time::Duration::ZERO
    }
}

/// Temp, cache and build-output trees that churn all day. Each is mapped as
/// one opaque total (no rows beneath it), rescanned at most every
/// `CHURN_EVERY_SECS`. Volume-relative; `*` is one path component; a leading
/// `**/` matches at any depth.
pub const CHURN_TREES: &[&str] = &[
    "private/var/folders/*/*",
    "private/tmp",
    "private/var/tmp",
    "Library/Caches",
    "Users/*/Library/Caches",
    "Users/*/.cursor/projects",
    "Users/*/.Trash",
    "Users/*/.npm",
    "**/.next",
    // Dependency trees: agent worktrees each bring tens of thousands of
    // folders (18 Nov 2026); their size matters, their layout does not.
    "**/node_modules",
    "**/_cacache",
    "**/.git/objects",
    "**/DerivedData",
];
pub const CHURN_EVERY_SECS: i64 = 3600;
/// A tree that gets at least this many rescans in one replay is bursty: its
/// rescans wait until it has been quiet for `QUIET_SECS` (or `MAX_HOLD_SECS`).
pub const BURST_MIN: usize = 256;
/// Bursty trees are at least this many components deep (never the volume root
/// or a home folder as a whole).
pub const BURST_MIN_DEPTH: usize = 3;
pub const QUIET_SECS: i64 = 600;
pub const MAX_HOLD_SECS: i64 = 7200;

/// The churn tree `path` lies in (or is), shallowest match first.
pub fn churn_root(path: &str, extra: &[String]) -> Option<String> {
    tree_root(path, CHURN_TREES.iter().copied().chain(extra.iter().map(String::as_str)))
}

/// Folders macOS guards per app (Photos): a launchd-started binary without
/// a grant blocks in open(2) on every folder inside (18 Nov 2026). The walker
/// never opens them; their rows keep their last sizes. Same pattern syntax as
/// `CHURN_TREES`.
pub const PROTECTED_TREES: &[&str] = &["**/*.photoslibrary"];

pub fn protected_root(path: &str) -> Option<String> {
    tree_root(path, PROTECTED_TREES.iter().copied())
}

/// What to record when `dir` blocked a worker: the folder itself, or its
/// parent once a sibling has blocked too (a privacy-protected area such as
/// Desktop then costs two stalls, not one per child). Never above a folder
/// three components deep, so a home folder is not skipped as a whole.
pub fn stuck_entry(dir: &std::path::Path, known: &[std::path::PathBuf]) -> std::path::PathBuf {
    let Some(parent) = dir.parent() else { return dir.to_path_buf() };
    let sibling = known.iter().any(|k| k.parent() == Some(parent) && k != dir);
    if sibling && parent.components().count() > 6 {
        parent.to_path_buf()
    } else {
        dir.to_path_buf()
    }
}

/// The shallowest prefix of `path` that a pattern matches.
fn tree_root<'a>(path: &str, patterns: impl Iterator<Item = &'a str>) -> Option<String> {
    let comps: Vec<&str> = if path.is_empty() { Vec::new() } else { path.split('/').collect() };
    let mut best: Option<usize> = None;
    for pattern in patterns {
        let (anywhere, pat) = match pattern.strip_prefix("**/") {
            Some(rest) => (true, rest),
            None => (false, pattern),
        };
        let pc: Vec<&str> = pat.split('/').filter(|c| !c.is_empty()).collect();
        if pc.is_empty() || pc.len() > comps.len() {
            continue;
        }
        let starts: Vec<usize> = if anywhere { (0..=comps.len() - pc.len()).collect() } else { vec![0] };
        for i in starts {
            if pc.iter().zip(&comps[i..]).all(|(p, c)| glob_match(p, c)) {
                let len = i + pc.len();
                if best.is_none_or(|b| len < b) {
                    best = Some(len);
                }
                break;
            }
        }
    }
    best.map(|len| comps[..len].join("/"))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HoldKind {
    /// A churn tree: one opaque rescan of the root, at most hourly.
    Churn,
    /// A bursty tree: its rescans wait until it goes quiet; later rescans
    /// beneath it join the hold.
    Burst,
    /// A whole-subtree rescan (MUST_SCAN_SUBDIRS): repeated ones coalesce and
    /// run once the tree goes quiet.
    Deep,
}

impl HoldKind {
    pub fn code(&self) -> i64 {
        match self {
            HoldKind::Churn => 1,
            HoldKind::Burst => 0,
            HoldKind::Deep => 2,
        }
    }
    pub fn from_code(code: i64) -> Self {
        match code {
            1 => HoldKind::Churn,
            2 => HoldKind::Deep,
            _ => HoldKind::Burst,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScanPlan {
    /// Queue now.
    pub now: Vec<ScanReq>,
    /// (hold root, kind, request) to park until the hold is due.
    pub held: Vec<(String, HoldKind, ScanReq)>,
}

/// Route one replay's rescans: churn trees to their root's hourly hold,
/// anything under an open burst hold into it, whole-subtree rescans into a
/// deep hold, and a new burst (`BURST_MIN` rescans under one directory at
/// least `BURST_MIN_DEPTH` deep; the deepest such) into a new burst hold.
pub fn plan_scans(reqs: &[ScanReq], burst_holds: &[String], extra_churn: &[String]) -> ScanPlan {
    let mut plan = ScanPlan::default();
    let mut rest: Vec<ScanReq> = Vec::new();
    for req in reqs {
        let path = scan_path(req);
        if let Some(root) = churn_root(path, extra_churn) {
            plan.held.push((root.clone(), HoldKind::Churn, ScanReq::Dir(root)));
        } else if let Some(hold) = burst_holds.iter().find(|h| path == h.as_str() || is_under(path, h)) {
            plan.held.push((hold.clone(), HoldKind::Burst, req.clone()));
        } else if let ScanReq::Subtree(p) = req {
            plan.held.push((p.clone(), HoldKind::Deep, req.clone()));
        } else {
            rest.push(req.clone());
        }
    }
    loop {
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for req in &rest {
            let path = scan_path(req);
            let mut end = path.len();
            loop {
                let anc = &path[..end];
                if anc.split('/').count() >= BURST_MIN_DEPTH {
                    *counts.entry(anc).or_default() += 1;
                }
                match anc.rfind('/') {
                    Some(i) => end = i,
                    None => break,
                }
            }
        }
        let hot = counts
            .into_iter()
            .filter(|(_, n)| *n >= BURST_MIN)
            .max_by(|a, b| a.0.split('/').count().cmp(&b.0.split('/').count()).then(b.0.cmp(a.0)))
            .map(|(p, _)| p.to_string());
        let Some(hot) = hot else { break };
        let (inside, outside): (Vec<ScanReq>, Vec<ScanReq>) = rest
            .into_iter()
            .partition(|r| scan_path(r) == hot || is_under(scan_path(r), &hot));
        for req in inside {
            plan.held.push((hot.clone(), HoldKind::Burst, req));
        }
        rest = outside;
    }
    plan.now = rest;
    plan.held.dedup();
    plan
}

/// Whether a hold is due for release. `root_scanned` is when a churn root was
/// last scanned (0: never).
pub fn hold_due(kind: &HoldKind, first_event: i64, last_event: i64, root_scanned: i64, now: i64) -> bool {
    match kind {
        HoldKind::Churn => root_scanned <= 0 || now - root_scanned >= CHURN_EVERY_SECS,
        HoldKind::Burst | HoldKind::Deep => {
            now - last_event >= QUIET_SECS || now - first_event >= MAX_HOLD_SECS
        }
    }
}

/// `(baseline_done, queued_rescans)` per mounted volume.
pub fn needs_worker(volumes: &[(bool, i64)]) -> bool {
    volumes.iter().any(|&(baseline, queued)| !baseline || queued > 0)
}

pub fn fmt_hm(unix: i64) -> String {
    if unix <= 0 {
        return "--:--".into();
    }
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let t = unix as libc::time_t;
    unsafe {
        libc::localtime_r(&t, &mut tm);
    }
    format!("{:02}:{:02}", tm.tm_hour, tm.tm_min)
}

/// Shown when `grew` has no daily history to compare. Absent once a history
/// row exists or a grower was printed.
pub fn grew_status_line(any_history: bool, printed_rows: bool, since_hm: Option<&str>) -> Option<String> {
    if printed_rows || any_history {
        return None;
    }
    Some(match since_hm {
        Some(hm) if !hm.is_empty() => format!("no history yet (map since {hm})"),
        _ => "no history yet".into(),
    })
}
