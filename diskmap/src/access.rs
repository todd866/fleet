//! Full Disk Access: whether diskmap holds it, and the privacy gate that keeps
//! the walker away from consent-gated folders while it does not.
//!
//! Without Full Disk Access, opening Desktop, Documents, Downloads, a Photos or
//! media library, another app's data or a disk under /Volumes makes macOS ask
//! the owner (or refuse, or block a launchd process in open(2) for good, W28).
//! The owner was asked again and again (18 Nov 2026: "can we fix diskmap so I'm
//! not constantly approving stuff?"). So the tick checks for the grant with a
//! read that never prompts, and until it is there the walker never opens a
//! gated folder; their rows keep their last sizes and the summary counts them.

use crate::darwin;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Readable only with Full Disk Access; a refusal is a policy denial that
/// never shows a dialog (W29: measured under launchd).
const PROBE_FILE: &str = "/Library/Application Support/com.apple.TCC/TCC.db";
/// Each probe is one logged (silent) access request, so it is cached.
pub const PROBE_EVERY_SECS: i64 = 600;
/// A cached answer older than this is not trusted: the gate goes on.
const STALE_SECS: i64 = 3 * PROBE_EVERY_SECS;
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AccessState {
    pub full_disk: bool,
    pub checked_at: i64,
    /// Set when Full Disk Access arrives; cleared once the gated trees are
    /// queued for a fresh walk.
    #[serde(default)]
    pub requeue: bool,
}

fn state_path(state: &Path) -> PathBuf {
    state.join("access.json")
}

/// Folders refused at run time without Full Disk Access.
pub fn learned_path(state: &Path) -> PathBuf {
    state.join("gated.tsv")
}

pub fn read_state(state: &Path) -> Option<AccessState> {
    let text = fs::read_to_string(state_path(state)).ok()?;
    serde_json::from_str(&text).ok()
}

pub fn write_state(state: &Path, access: &AccessState) -> Result<(), String> {
    fs::create_dir_all(state).map_err(|e| e.to_string())?;
    let path = state_path(state);
    let tmp = path.with_extension("json.tmp");
    let text = serde_json::to_string(access).map_err(|e| e.to_string())? + "\n";
    fs::write(&tmp, text).map_err(|e| e.to_string())?;
    fs::rename(&tmp, &path).map_err(|e| e.to_string())
}

/// Open the probe file once. A hang (never seen, but W28 saw opens that did)
/// counts as no access.
pub fn probe() -> bool {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let ok = fs::File::open(PROBE_FILE).is_ok();
        let _ = tx.send(ok);
    });
    rx.recv_timeout(PROBE_TIMEOUT).unwrap_or(false)
}

/// True for the launchd tick (its parent is launchd). Only that process may
/// probe and record the answer: a run from a shell is charged to the terminal
/// app, which may hold Full Disk Access when diskmap does not.
pub fn may_probe() -> bool {
    if let Ok(v) = std::env::var("DISKMAP_ACCESS_PROBE") {
        return v == "1";
    }
    unsafe { libc::getppid() == 1 }
}

/// The tick's view: probe when the cached answer is due and this is the
/// launchd tick, record it, and flag the off-to-on change.
pub fn refresh(state: &Path, now: i64) -> AccessState {
    let cached = read_state(state);
    let due = cached.as_ref().is_none_or(|c| now - c.checked_at >= PROBE_EVERY_SECS);
    if !due || !may_probe() {
        return effective(cached, now);
    }
    let full_disk = probe();
    let was = cached.as_ref().map(|c| c.full_disk).unwrap_or(false);
    let requeue = full_disk && (cached.as_ref().is_some_and(|c| c.requeue) || !was);
    let next = AccessState { full_disk, checked_at: now, requeue };
    if let Err(err) = write_state(state, &next) {
        eprintln!("diskmap: access state: {err}");
    }
    next
}

/// The cached answer, or "no access" when it is missing or stale.
pub fn current(state: &Path, now: i64) -> AccessState {
    effective(read_state(state), now)
}

fn effective(cached: Option<AccessState>, now: i64) -> AccessState {
    match cached {
        Some(c) if now - c.checked_at < STALE_SECS => c,
        Some(c) => AccessState { full_disk: false, ..c },
        None => AccessState::default(),
    }
}

/// Folders refused before (unix time, tab, absolute path).
pub fn load_learned(state: &Path) -> Vec<PathBuf> {
    let Ok(text) = fs::read_to_string(learned_path(state)) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| line.split_once('\t'))
        .map(|(_, p)| PathBuf::from(p))
        .collect()
}

/// Turn this process's gate on or off to match `access`.
pub fn apply(state: &Path, access: &AccessState) {
    if access.full_disk {
        darwin::set_gate(None, None);
    } else {
        darwin::set_gate(Some(load_learned(state)), Some(learned_path(state)));
    }
}
