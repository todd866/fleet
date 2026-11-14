use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn projects_root() -> PathBuf {
    if let Ok(p) = env::var("QUEUE_STEWARD_PROJECTS") {
        return PathBuf::from(p);
    }
    env::var("HOME")
        .map(|h| PathBuf::from(h).join("Projects"))
        .unwrap_or_else(|_| PathBuf::from("Projects"))
}

pub fn focus_guard_bin() -> PathBuf {
    if let Ok(p) = env::var("QUEUE_STEWARD_FOCUS_GUARD") {
        return PathBuf::from(p);
    }
    env::var("HOME")
        .map(|h| PathBuf::from(h).join(".local/bin/focus-guard"))
        .unwrap_or_else(|_| PathBuf::from(".local/bin/focus-guard"))
}

pub fn focus_guard_active() -> bool {
    let bin = focus_guard_bin();
    if !bin.is_file() {
        return false;
    }
    Command::new(&bin)
        .arg("active")
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn git_activity_mtime(repo: &Path) -> Option<std::time::SystemTime> {
    let index = repo.join(".git/index");
    let head = repo.join(".git/HEAD");
    let mut best: Option<SystemTime> = None;
    for p in [index, head] {
        if let Ok(meta) = fs::metadata(&p)
            && let Ok(m) = meta.modified()
        {
            best = Some(match best {
                Some(b) if m > b => m,
                Some(b) => b,
                None => m,
            });
        }
    }
    best
}

pub fn active_project(now: i64) -> Option<String> {
    let root = projects_root();
    let Ok(entries) = fs::read_dir(&root) else {
        return None;
    };
    let cutoff = now - 3600;
    let mut best_name: Option<String> = None;
    let mut best_secs: i64 = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(mtime) = git_activity_mtime(&path) else {
            continue;
        };
        let secs = mtime
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        if secs < cutoff {
            continue;
        }
        if secs >= best_secs {
            best_secs = secs;
            best_name = path.file_name().and_then(|n| n.to_str()).map(String::from);
        }
    }
    best_name
}
