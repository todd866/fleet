//! Copy, verify, then move the source into Trash. Never unlink a source that is not
//! already in Trash. I/O is not a heavy-slot job: this module does not call heavy-slot.

use crate::model::{
    Action, ActionKind, Class, GB, Observation, Plan, Policy, Volume, icloud_path, path_covers,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

pub const TAKES_HEAVY_SLOT: bool = false;
const WRITER_TTL_SECS: i64 = 12 * 60 * 60;

#[derive(Clone)]
pub struct ApplyOpts {
    pub home: PathBuf,
    pub host_id: String,
    pub trash: PathBuf,
    pub ledger: PathBuf,
    pub volume_root: PathBuf,
    pub metered: bool,
    pub focus_active: bool,
    pub now: i64,
    pub hostname: String,
    pub pid: u32,
    /// Test seam: skip `rename` and copy into Trash, verify, then remove the source.
    pub force_trash_copy: bool,
    /// Test seam: the copy is declared mismatched after it is written.
    pub fail_verify: bool,
    pub replica_log: Option<PathBuf>,
}

impl ApplyOpts {
    pub fn new(home: PathBuf, host_id: impl Into<String>) -> Self {
        Self {
            home,
            host_id: host_id.into(),
            trash: PathBuf::from(".Trash"),
            ledger: PathBuf::from("OFFLOADS.md"),
            volume_root: PathBuf::from("/Volumes"),
            metered: false,
            focus_active: false,
            now: 0,
            hostname: String::new(),
            pid: std::process::id(),
            force_trash_copy: false,
            fail_verify: false,
            replica_log: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyReport {
    pub deferred: bool,
    pub defer_reason: String,
    pub moved: Vec<String>,
    pub refused: Vec<String>,
    pub skipped: Vec<String>,
    pub errors: Vec<String>,
    pub stignore: Vec<String>,
}

pub fn apply(
    policy: &Policy,
    obs: &Observation,
    plan: &Plan,
    opts: &ApplyOpts,
) -> Result<ApplyReport, String> {
    let mut report = ApplyReport {
        deferred: false,
        defer_reason: String::new(),
        moved: Vec::new(),
        refused: Vec::new(),
        skipped: Vec::new(),
        errors: Vec::new(),
        stignore: Vec::new(),
    };
    if opts.focus_active {
        report.deferred = true;
        report.defer_reason = "focus".into();
        return Ok(report);
    }
    let mine: Vec<&Action> = plan
        .actions
        .iter()
        .filter(|a| a.host == opts.host_id)
        .filter(|a| matches!(a.kind, ActionKind::Move | ActionKind::Clear))
        .collect();
    for action in &mine {
        if action.kind == ActionKind::Move && writer_blocks(opts, &action.dest) {
            report.deferred = true;
            report.defer_reason = format!("writer {}", action.dest);
            return Ok(report);
        }
    }

    let mut claimed = Vec::new();
    for action in &mine {
        if action.kind != ActionKind::Move {
            continue;
        }
        if claimed.iter().any(|v| v == &action.dest) {
            continue;
        }
        claim_writer(opts, &action.dest)?;
        claimed.push(action.dest.clone());
    }
    let release = || {
        for volume in &claimed {
            release_writer(opts, volume);
        }
    };

    for action in plan.actions.iter().filter(|a| a.host == opts.host_id) {
        if opts.metered && (action.icloud || icloud_path(&action.path)) && is_file_op(action.kind) {
            report.skipped.push(format!("{} metered", action.path));
            continue;
        }
        match action.kind {
            ActionKind::Keep | ActionKind::Sync | ActionKind::Ignore => {}
            ActionKind::Refuse => report.refused.push(action.path.clone()),
            ActionKind::Clear => {
                if action.class == Class::PersonalUnique {
                    report
                        .refused
                        .push(format!("{} personal-unique", action.path));
                    continue;
                }
                match clear_local(opts, action) {
                    Ok(()) => report.moved.push(action.path.clone()),
                    Err(err) => report.errors.push(format!("{}: {err}", action.path)),
                }
            }
            ActionKind::Move => {
                if action.class == Class::PersonalUnique && copies_for(obs, &action.path) < 2 {
                    report.refused.push(format!(
                        "{} {} encrypted copy",
                        action.path,
                        copies_for(obs, &action.path)
                    ));
                    continue;
                }
                match move_local(policy, opts, action) {
                    Ok(()) => report.moved.push(action.path.clone()),
                    Err(err) => report.errors.push(format!("{}: {err}", action.path)),
                }
            }
        }
    }

    if report.errors.is_empty() {
        report.stignore = write_stignores(policy, obs, opts)?;
    }
    release();
    Ok(report)
}

fn is_file_op(kind: ActionKind) -> bool {
    matches!(kind, ActionKind::Move | ActionKind::Clear)
}

fn copies_for(obs: &Observation, path: &str) -> u32 {
    obs.replicas
        .iter()
        .filter(|r| {
            r.verified
                && r.encrypted
                && !r.offsite
                && r.bytes > 0
                && (path_covers(path, &r.path) || path_covers(&r.path, path))
        })
        .count() as u32
}

fn clear_local(opts: &ApplyOpts, action: &Action) -> Result<(), String> {
    let src = opts.home.join(&action.path);
    if !src.exists() {
        return Ok(());
    }
    let bytes = tree_bytes(&src)?;
    relocate_to_trash(&src, opts)?;
    write_stub(opts, action, None, bytes)?;
    append_ledger(opts, action, Some(bytes))?;
    Ok(())
}

fn move_local(policy: &Policy, opts: &ApplyOpts, action: &Action) -> Result<(), String> {
    let src = opts.home.join(&action.path);
    if !src.exists() {
        return Err("source missing".into());
    }
    let volume = policy
        .volumes
        .iter()
        .find(|v| v.name == action.dest)
        .ok_or_else(|| format!("no volume {}", action.dest))?;
    let dest = volume_dest(opts, volume, &action.path);
    if dest.exists() && !opts.fail_verify && verify_trees(&src, &dest).is_ok() {
        let bytes = tree_bytes(&src)?;
        relocate_to_trash(&src, opts)?;
        write_stub(opts, action, Some(&dest), bytes)?;
        append_ledger(opts, action, Some(bytes))?;
        record_replica(opts, action, volume, bytes)?;
        return Ok(());
    }
    if dest.exists() {
        return Err("dest differs".into());
    }
    let staging = staging_path(&dest);
    if staging.exists() {
        let _ = relocate_aside(&staging, &opts.trash, opts.now, "staging");
    }
    copy_tree(&src, &staging)?;
    let verified = !opts.fail_verify && verify_trees(&src, &staging).is_ok();
    if !verified {
        let _ = relocate_aside(&staging, &opts.trash, opts.now, "incomplete");
        return Err("verify failed".into());
    }
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    fs::rename(&staging, &dest).map_err(|e| e.to_string())?;
    let bytes = tree_bytes(&src)?;
    relocate_to_trash(&src, opts)?;
    write_stub(opts, action, Some(&dest), bytes)?;
    append_ledger(opts, action, Some(bytes))?;
    record_replica(opts, action, volume, bytes)?;
    Ok(())
}

fn volume_dest(opts: &ApplyOpts, volume: &Volume, path: &str) -> PathBuf {
    let mut dest = opts.volume_root.join(&volume.name);
    if !volume.subpath.is_empty() {
        dest.push(&volume.subpath);
    }
    dest.push(path);
    dest
}

fn staging_path(dest: &Path) -> PathBuf {
    let name = dest.file_name().and_then(|n| n.to_str()).unwrap_or("item");
    dest.with_file_name(format!("{name}.placement-new"))
}

/// Move `src` into Trash. On another filesystem, copy and verify, and only then remove `src`.
fn relocate_to_trash(src: &Path, opts: &ApplyOpts) -> Result<PathBuf, String> {
    fs::create_dir_all(&opts.trash).map_err(|e| e.to_string())?;
    let dest = trash_dest(&opts.trash, src, opts.now);
    if !opts.force_trash_copy {
        if fs::rename(src, &dest).is_ok() {
            return Ok(dest);
        }
    }
    copy_tree(src, &dest)?;
    verify_trees(src, &dest)?;
    remove_verified_source(src)?;
    Ok(dest)
}

fn relocate_aside(src: &Path, trash: &Path, now: i64, tag: &str) -> Result<(), String> {
    fs::create_dir_all(trash).map_err(|e| e.to_string())?;
    let name = src.file_name().and_then(|n| n.to_str()).unwrap_or("item");
    let dest = trash.join(format!("{name}.{tag}.{}", stamp(now)));
    if fs::rename(src, &dest).is_ok() {
        return Ok(());
    }
    copy_tree(src, &dest)?;
    verify_trees(src, &dest)?;
    remove_verified_source(src)?;
    Ok(())
}

fn trash_dest(trash: &Path, src: &Path, now: i64) -> PathBuf {
    let name = src.file_name().and_then(|n| n.to_str()).unwrap_or("item");
    let mut dest = trash.join(format!("{name}.{}", stamp(now)));
    if dest.exists() {
        dest = trash.join(format!("{name}.{}.{}", stamp(now), std::process::id()));
    }
    dest
}

fn remove_verified_source(src: &Path) -> Result<(), String> {
    if src.is_dir() {
        fs::remove_dir_all(src).map_err(|e| e.to_string())
    } else {
        fs::remove_file(src).map_err(|e| e.to_string())
    }
}

pub fn copy_tree(src: &Path, dest: &Path) -> Result<(), String> {
    let meta = fs::symlink_metadata(src).map_err(|e| e.to_string())?;
    if meta.file_type().is_symlink() {
        return Ok(());
    }
    if meta.is_file() {
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        fs::copy(src, dest).map_err(|e| e.to_string())?;
        return Ok(());
    }
    fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    let entries = fs::read_dir(src).map_err(|e| e.to_string())?;
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        if skip_name(&entry.file_name().to_string_lossy()) {
            continue;
        }
        let ty = entry.file_type().map_err(|e| e.to_string())?;
        if ty.is_symlink() {
            continue;
        }
        copy_tree(&entry.path(), &dest.join(entry.file_name()))?;
    }
    Ok(())
}

pub fn verify_trees(src: &Path, dest: &Path) -> Result<(), String> {
    let want = fingerprints(src)?;
    for (rel, size, hash) in want {
        let got = dest.join(&rel);
        if !got.is_file() {
            return Err(format!("missing {rel}"));
        }
        let meta = fs::metadata(&got).map_err(|e| e.to_string())?;
        if meta.len() != size {
            return Err(format!("size {rel}"));
        }
        if sha256_file(&got)? != hash {
            return Err(format!("checksum {rel}"));
        }
    }
    Ok(())
}

fn fingerprints(root: &Path) -> Result<Vec<(String, u64, String)>, String> {
    let mut out = Vec::new();
    walk_files(root, root, &mut out)?;
    out.sort();
    Ok(out)
}

fn walk_files(root: &Path, dir: &Path, out: &mut Vec<(String, u64, String)>) -> Result<(), String> {
    if dir.is_file() {
        let hash = sha256_file(dir)?;
        let len = fs::metadata(dir).map_err(|e| e.to_string())?.len();
        out.push((String::new(), len, hash));
        return Ok(());
    }
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(err) => return Err(err.to_string()),
    };
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name();
        if skip_name(&name.to_string_lossy()) {
            continue;
        }
        let path = entry.path();
        let ty = entry.file_type().map_err(|e| e.to_string())?;
        if ty.is_symlink() {
            continue;
        }
        if path.is_dir() {
            walk_files(root, &path, out)?;
        } else if path.is_file() {
            let rel = path
                .strip_prefix(root)
                .map_err(|e| e.to_string())?
                .to_string_lossy()
                .to_string();
            let len = fs::metadata(&path).map_err(|e| e.to_string())?.len();
            out.push((rel, len, sha256_file(&path)?));
        }
    }
    Ok(())
}

fn tree_bytes(root: &Path) -> Result<u64, String> {
    Ok(fingerprints(root)?.iter().map(|(_, n, _)| *n).sum())
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file = File::open(path).map_err(|e| e.to_string())?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = file.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn skip_name(name: &str) -> bool {
    name == ".DS_Store" || name.starts_with("._") || name == ".writer" || name == ".stignore"
}

fn write_stub(
    opts: &ApplyOpts,
    action: &Action,
    dest: Option<&Path>,
    bytes: u64,
) -> Result<(), String> {
    let src = opts.home.join(&action.path);
    let stub = stub_path(&src);
    if let Some(parent) = stub.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let where_now = dest
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "Trash".into());
    let size = fmt_size(bytes);
    let (y, m, d) = ymd(opts.now);
    let tod = opts.now.rem_euclid(86_400);
    let text = format!(
        "Moved to the external drive on {y:04}-{m:02}-{d:02} {:02}:{:02}.\nNow at: {where_now} ({size})\nWhy that is acceptable: {}\nLedger: {}\n",
        tod / 3600,
        (tod % 3600) / 60,
        action.reason,
        opts.ledger.display()
    );
    fs::write(&stub, text).map_err(|e| e.to_string())
}

fn stub_path(src: &Path) -> PathBuf {
    let name = src.file_name().and_then(|n| n.to_str()).unwrap_or("item");
    let parent = src.parent().unwrap_or_else(|| Path::new("."));
    parent.join(format!("{name}.OFFLOADED.txt"))
}

fn append_ledger(opts: &ApplyOpts, action: &Action, bytes: Option<u64>) -> Result<(), String> {
    if let Some(parent) = opts.ledger.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
    }
    let (y, m, d) = ymd(opts.now);
    let size = fmt_size(bytes.unwrap_or(action.bytes));
    let dest = if action.dest.is_empty() {
        "Trash".to_string()
    } else {
        format!("{}/{}", action.dest, action.path)
    };
    let row = format!(
        "| {y:04}-{m:02}-{d:02} | trash | `{}` | `{dest}` | {size} | {} |\n",
        action.path, action.reason
    );
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&opts.ledger)
        .map_err(|e| e.to_string())?;
    file.write_all(row.as_bytes()).map_err(|e| e.to_string())
}

fn record_replica(
    opts: &ApplyOpts,
    action: &Action,
    volume: &Volume,
    bytes: u64,
) -> Result<(), String> {
    let Some(path) = &opts.replica_log else {
        return Ok(());
    };
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
    }
    let mut copies = read_copies(path);
    copies.retain(|c| !(c.path == action.path && c.volume == volume.name));
    copies.push(LoggedCopy {
        path: action.path.clone(),
        volume: volume.name.clone(),
        encrypted: volume.encrypted,
        verified: true,
        offsite: false,
        bytes,
    });
    let body = serde_json::to_string_pretty(&ReplicaFile { copies }).map_err(|e| e.to_string())?;
    fs::write(path, body + "\n").map_err(|e| e.to_string())
}

#[derive(Debug, Serialize, Deserialize)]
struct ReplicaFile {
    copies: Vec<LoggedCopy>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LoggedCopy {
    path: String,
    volume: String,
    encrypted: bool,
    verified: bool,
    offsite: bool,
    bytes: u64,
}

fn read_copies(path: &Path) -> Vec<LoggedCopy> {
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };
    serde_json::from_str::<ReplicaFile>(&text)
        .map(|f| f.copies)
        .unwrap_or_default()
}

fn write_stignores(
    policy: &Policy,
    obs: &Observation,
    opts: &ApplyOpts,
) -> Result<Vec<String>, String> {
    let mut written = Vec::new();
    for folder in &policy.sync_folders {
        let dir = opts.home.join(&folder.root);
        if !dir.is_dir() {
            continue;
        }
        let text = crate::stignore::stignore(policy, obs, &opts.host_id, &folder.name);
        let path = dir.join(".stignore");
        fs::write(&path, text).map_err(|e| e.to_string())?;
        written.push(path.display().to_string());
    }
    Ok(written)
}

fn writer_blocks(opts: &ApplyOpts, volume: &str) -> bool {
    if volume.is_empty() {
        return false;
    }
    lock_is_live(&opts.volume_root.join(volume).join(".writer"), opts)
}

fn claim_writer(opts: &ApplyOpts, volume: &str) -> Result<(), String> {
    if volume.is_empty() {
        return Ok(());
    }
    let dir = opts.volume_root.join(volume);
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(".writer");
    if lock_is_live(&path, opts) {
        return Err(format!("writer {volume}"));
    }
    let line = format!(
        "placement@{} {} {} {}\n",
        opts.host_id, opts.hostname, opts.pid, opts.now
    );
    fs::write(&path, line).map_err(|e| e.to_string())
}

fn release_writer(opts: &ApplyOpts, volume: &str) {
    let path = opts.volume_root.join(volume).join(".writer");
    let Ok(text) = fs::read_to_string(&path) else {
        return;
    };
    if text.starts_with(&format!("placement@{}", opts.host_id)) {
        let released = opts.volume_root.join(volume).join(".writer.released");
        let _ = fs::rename(&path, &released);
    }
}

fn lock_is_live(path: &Path, opts: &ApplyOpts) -> bool {
    let Ok(text) = fs::read_to_string(path) else {
        return false;
    };
    let mut parts = text.split_whitespace();
    let job = parts.next().unwrap_or("");
    let host = parts.next().unwrap_or("");
    let pid: u32 = parts.next().unwrap_or("0").parse().unwrap_or(0);
    let at: i64 = parts.next().unwrap_or("0").parse().unwrap_or(0);
    if job.starts_with("placement@") && pid == opts.pid {
        return false;
    }
    if opts.now.saturating_sub(at) >= WRITER_TTL_SECS {
        return false;
    }
    if host == opts.hostname {
        pid_alive(pid)
    } else {
        true
    }
}

fn pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

fn fmt_size(bytes: u64) -> String {
    if bytes >= GB {
        let v = bytes as f64 / GB as f64;
        if v >= 10.0 {
            format!("{v:.0} GB")
        } else {
            format!("{v:.1} GB")
        }
    } else if bytes >= 1_000_000 {
        format!("{} MB", ((bytes as f64) / 1_000_000.0).round() as u64)
    } else {
        format!("{bytes} B")
    }
}

pub fn stamp(now: i64) -> String {
    let (y, m, d) = ymd(now);
    let tod = now.rem_euclid(86_400);
    format!(
        "{y:04}{m:02}{d:02}-{:02}{:02}{:02}",
        tod / 3600,
        (tod % 3600) / 60,
        tod % 60
    )
}

/// Howard Hinnant's civil_from_days, UTC.
pub fn ymd(unix: i64) -> (i32, u32, u32) {
    let days = unix.div_euclid(86_400);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m as u32, d as u32)
}
