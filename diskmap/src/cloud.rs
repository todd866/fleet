//! iCloud and IDrive as locations. Reads local state only: no login, no API.

use crate::logic::{self, PhotoKind};
use crate::store::DirRow;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CloudFolder {
    pub area: String,
    pub name: String,
    pub local_bytes: u64,
    pub dataless_bytes: u64,
}

#[derive(Clone, Debug)]
pub struct IdriveView {
    pub installed: bool,
    pub last_success: Option<i64>,
    /// Latest summary's failed-file count. Surfaced only when a reader asks; the fleet row uses `last_success`.
    #[allow(dead_code)]
    pub failed: Option<u64>,
    pub paths: Vec<(String, i64)>,
}

pub fn icloud_folders(rows: &[DirRow], home_prefix: &str) -> Vec<CloudFolder> {
    let prefix = home_prefix.trim_matches('/');
    let mut out = Vec::new();
    let cloud = format!("{prefix}/Library/Mobile Documents/com~apple~CloudDocs");
    for row in rows {
        if row.parent == cloud {
            let name = row.path.rsplit('/').next().unwrap_or(&row.path);
            if name == "Desktop" || name == "Documents" {
                continue;
            }
            out.push(CloudFolder {
                area: "drive".into(),
                name: name.to_string(),
                local_bytes: row.total_bytes.max(0) as u64,
                dataless_bytes: row.dataless_bytes.max(0) as u64,
            });
        }
    }
    for (area, name) in [("desktop", "Desktop"), ("documents", "Documents")] {
        let path = format!("{prefix}/{name}");
        if let Some(row) = rows.iter().find(|r| r.path == path) {
            out.push(CloudFolder {
                area: area.into(),
                name: name.into(),
                local_bytes: row.total_bytes.max(0) as u64,
                dataless_bytes: row.dataless_bytes.max(0) as u64,
            });
        }
    }
    let library = format!("{prefix}/Pictures/Photos Library.photoslibrary");
    let mut originals_l = 0u64;
    let mut originals_d = 0u64;
    let mut deriv_l = 0u64;
    let mut deriv_d = 0u64;
    for row in rows {
        if row.path != library && !row.path.starts_with(&(library.clone() + "/")) {
            continue;
        }
        match logic::photos_kind(&row.path) {
            PhotoKind::Originals if row.path.ends_with("/originals") || row.path.ends_with("/Masters") || row.path.ends_with("/masters") => {
                originals_l = originals_l.max(row.total_bytes.max(0) as u64);
                originals_d = originals_d.max(row.dataless_bytes.max(0) as u64);
            }
            PhotoKind::Derivatives
                if row.path.ends_with("/derivatives")
                    || row.path.ends_with("/renders")
                    || row.path.ends_with("/caches") =>
            {
                deriv_l = deriv_l.max(row.total_bytes.max(0) as u64);
                deriv_d = deriv_d.max(row.dataless_bytes.max(0) as u64);
            }
            _ => {}
        }
    }
    if originals_l + originals_d > 0 {
        out.push(CloudFolder {
            area: "photos".into(),
            name: "originals".into(),
            local_bytes: originals_l,
            dataless_bytes: originals_d,
        });
    }
    if deriv_l + deriv_d > 0 {
        out.push(CloudFolder {
            area: "photos".into(),
            name: "derivatives".into(),
            local_bytes: deriv_l,
            dataless_bytes: deriv_d,
        });
    }
    out.sort_by(|a, b| a.area.cmp(&b.area).then(a.name.cmp(&b.name)));
    out
}

pub fn read_idrive(home: &Path, size_of: impl Fn(&str) -> i64) -> IdriveView {
    let app = PathBuf::from("/Applications/IDrive.app");
    let base = home.join("Library/Application Support/IDriveforMac");
    if !base.is_dir() && !app.exists() {
        return IdriveView {
            installed: false,
            last_success: None,
            failed: None,
            paths: Vec::new(),
        };
    }
    let account = find_account(&base);
    let mut paths = Vec::new();
    if let Some(dir) = &account {
        if let Some(json) = plist_json(&dir.join("backupReferenceFile.plist")) {
            for raw in logic::plist_path_strings(&json) {
                let rel = home_rel(home, &raw);
                let bytes = rel.as_deref().map(&size_of).unwrap_or(0);
                let shown = rel.unwrap_or(raw);
                if !shown.is_empty() && !paths.iter().any(|(p, _)| p == &shown) {
                    paths.push((shown, bytes));
                }
            }
        }
    }
    let (last_success, failed) = newest_success(account.as_deref(), &base);
    IdriveView {
        installed: true,
        last_success,
        failed,
        paths,
    }
}

fn home_rel(home: &Path, path: &str) -> Option<String> {
    let home = home.to_string_lossy();
    let path = path.trim();
    if let Some(rest) = path.strip_prefix(home.as_ref()) {
        return Some(rest.trim_start_matches('/').to_string());
    }
    if path.starts_with('/') {
        return None;
    }
    Some(path.trim_start_matches("~/").to_string())
}

fn find_account(base: &Path) -> Option<PathBuf> {
    let mut found: Vec<(i64, PathBuf)> = Vec::new();
    let mut stack = vec![base.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = fs::read_dir(&dir) else { continue };
        for ent in rd.flatten() {
            let name = ent.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || name == "SessionLogsNew" || name == "LOGXML" {
                continue;
            }
            let path = ent.path();
            if path.is_dir() {
                let plist = path.join("backupReferenceFile.plist");
                if plist.is_file() {
                    let mtime = plist.metadata().map(|m| m.modified().ok()).ok().flatten();
                    let stamp = mtime
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_secs() as i64)
                        .unwrap_or(0);
                    found.push((stamp, path.clone()));
                }
                stack.push(path);
            }
        }
    }
    found.sort_by_key(|(t, _)| *t);
    found.pop().map(|(_, p)| p)
}

fn newest_success(account: Option<&Path>, base: &Path) -> (Option<i64>, Option<u64>) {
    let mut dirs = Vec::new();
    if let Some(account) = account {
        dirs.push(account.join("SessionLogsNew/LOGXML"));
        if let Some(parent) = account.parent() {
            dirs.push(parent.join("SessionLogsNew/LOGXML"));
        }
    }
    dirs.push(base.join("SessionLogsNew/LOGXML"));
    let mut best: Option<i64> = None;
    let mut failed_at_best: Option<u64> = None;
    for dir in dirs {
        let Ok(rd) = fs::read_dir(&dir) else { continue };
        for ent in rd.flatten() {
            let path = ent.path();
            if path.extension().and_then(|e| e.to_str()) != Some("xml") {
                continue;
            }
            let Ok(text) = fs::read_to_string(&path) else { continue };
            for rec in logic::parse_log_records(&text) {
                let Some(when) = logic::parse_local_mdy(&rec.datetime) else {
                    continue;
                };
                let failed = summary_failed(&rec.logfile, account, &dir);
                if !logic::confirm_success(&rec.status, failed) {
                    continue;
                }
                if best.map(|b| when > b).unwrap_or(true) {
                    best = Some(when);
                    failed_at_best = failed;
                }
            }
        }
    }
    if best.is_none() {
        if let Some((when, failed)) = newest_clean_summary(base) {
            return (Some(when), failed);
        }
    }
    (best, failed_at_best)
}

fn summary_failed(logfile: &str, account: Option<&Path>, xml_dir: &Path) -> Option<u64> {
    if logfile.is_empty() {
        return None;
    }
    let name = Path::new(logfile)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let stem = Path::new(&name)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or(name.clone());
    let mut candidates = vec![
        xml_dir.join(format!("{name}.summary")),
        xml_dir.join(format!("{stem}.summary")),
    ];
    if let Some(account) = account {
        candidates.push(account.join("SessionLogsNew").join(format!("{stem}.summary")));
        candidates.push(account.join(format!("{stem}.summary")));
    }
    for candidate in candidates {
        if let Ok(text) = fs::read_to_string(candidate) {
            if let Some(n) = logic::parse_backup_summary(&text).failed {
                return Some(n);
            }
        }
    }
    None
}

fn newest_clean_summary(base: &Path) -> Option<(i64, Option<u64>)> {
    let dir = base.join("SessionLogsNew/Backup");
    let rd = fs::read_dir(dir).ok()?;
    let mut best: Option<(i64, u64)> = None;
    for ent in rd.flatten() {
        let path = ent.path();
        if path.extension().and_then(|s| s.to_str()) != Some("summary") {
            continue;
        }
        let Ok(text) = fs::read_to_string(&path) else { continue };
        let parsed = logic::parse_backup_summary(&text);
        if parsed.failed != Some(0) {
            continue;
        }
        let mtime = path.metadata().ok()?.modified().ok()?;
        let when = mtime.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() as i64;
        if best.as_ref().map(|(t, _)| when > *t).unwrap_or(true) {
            best = Some((when, 0));
        }
    }
    best.map(|(t, f)| (t, Some(f)))
}

fn plist_json(path: &Path) -> Option<Value> {
    if !path.is_file() {
        return None;
    }
    let out = Command::new("/usr/bin/plutil")
        .args(["-convert", "json", "-o", "-", &path.to_string_lossy()])
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    serde_json::from_slice(&out.stdout).ok()
}
