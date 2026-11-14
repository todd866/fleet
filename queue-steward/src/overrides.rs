use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OverrideEntry {
    pub at: i64,
    pub label: String,
    pub project: String,
    pub until: i64,
    pub hour: u32,
    pub weekday: u32,
}

pub fn overrides_path(state_dir: &Path) -> std::path::PathBuf {
    state_dir.join("queue").join("overrides.jsonl")
}

pub fn read_overrides(state_dir: &Path) -> Vec<OverrideEntry> {
    let path = overrides_path(state_dir);
    let Ok(content) = fs::read_to_string(&path) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(e) = serde_json::from_str::<OverrideEntry>(line) {
            out.push(e);
        }
    }
    out
}

pub fn append_override(state_dir: &Path, entry: &OverrideEntry) -> std::io::Result<()> {
    let queue_dir = state_dir.join("queue");
    fs::create_dir_all(&queue_dir)?;
    let line = serde_json::to_string(entry).expect("override encodes");
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(overrides_path(state_dir))?;
    f.write_all(format!("{line}\n").as_bytes())
}

pub fn parse_for_duration(raw: &str) -> Option<u64> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    if let Some(h) = raw.strip_suffix('h') {
        return h.parse::<u64>().ok().map(|n| n * 3600);
    }
    if let Some(m) = raw.strip_suffix('m') {
        return m.parse::<u64>().ok().map(|n| n * 60);
    }
    if let Some(s) = raw.strip_suffix('s') {
        return s.parse::<u64>().ok();
    }
    raw.parse::<u64>().ok()
}

pub fn weekday_class(weekday: u32) -> bool {
    // true = weekend (Sat/Sun)
    weekday == 0 || weekday == 6
}

pub fn hour_bucket(hour: u32) -> u32 {
    hour / 3
}

pub fn local_hour_weekday(now: i64) -> (u32, u32) {
    let now = now as libc::time_t;
    unsafe {
        let tm = libc::localtime(&now);
        if tm.is_null() {
            return (0, 1);
        }
        let tm = *tm;
        (tm.tm_hour as u32, tm.tm_wday as u32)
    }
}
