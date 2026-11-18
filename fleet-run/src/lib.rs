//! fleet-run's policy: which launchd job may run what, under this binary's privacy grants.
//!
//! A Full Disk Access grant on a launcher that runs anything would give FDA to anything that can
//! start a process. So fleet-run runs only an exact command line that is compiled into it, for
//! the launchd job named next to it, when the pinned script still has the pinned SHA-256.
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub label: String,
    pub hashed: PathBuf,
    pub sha256: String,
    pub argv: Vec<String>,
}

#[derive(Debug, PartialEq)]
pub enum Refusal {
    NoCommand,
    NotAbsolute(String),
    NotUnderLaunchd(i32),
    NoLabel,
    LabelNotAllowed(String),
    ArgvMismatch(String),
    NotTheJobProcess(String),
    HashMismatch { file: String, want: String, got: String },
    Unreadable(String),
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Refusal::NoCommand => write!(f, "no command"),
            Refusal::NotAbsolute(c) => write!(f, "command {c:?} is not an absolute path"),
            Refusal::NotUnderLaunchd(pp) => write!(f, "parent is pid {pp}, not launchd"),
            Refusal::NoLabel => write!(f, "no launchd job label (XPC_SERVICE_NAME)"),
            Refusal::LabelNotAllowed(l) => write!(f, "job {l} is not in the compiled allowlist"),
            Refusal::ArgvMismatch(l) => write!(f, "command line differs from the allowlist entry for {l}"),
            Refusal::NotTheJobProcess(l) => write!(f, "launchd does not list this process as job {l}"),
            Refusal::HashMismatch { file, want, got } => write!(f, "{file}: sha256 {got}, allowlist says {want}"),
            Refusal::Unreadable(p) => write!(f, "cannot read {p}"),
        }
    }
}

fn expand(p: &str, home: &Path) -> PathBuf {
    match p.strip_prefix("~/") {
        Some(rest) => home.join(rest),
        None => PathBuf::from(p),
    }
}

/// Parse the allowlist text. Lines that are malformed or carry relative paths are dropped, so
/// a bad line can only remove permission, never add it.
pub fn parse(text: &str, home: &Path) -> Vec<Entry> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim_end();
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() < 4 || f[2].len() != 64 || !f[2].chars().all(|c| c.is_ascii_hexdigit()) {
            continue;
        }
        let argv: Vec<String> = f[3..].iter().map(|a| {
            if a.starts_with("~/") { expand(a, home).to_string_lossy().into_owned() } else { a.to_string() }
        }).collect();
        let hashed = expand(f[1], home);
        if !hashed.is_absolute() || !Path::new(&argv[0]).is_absolute() {
            continue;
        }
        out.push(Entry { label: f[0].to_string(), hashed, sha256: f[2].to_ascii_lowercase(), argv });
    }
    out
}

pub fn sha256_file(p: &Path) -> Result<String, Refusal> {
    let bytes = std::fs::read(p).map_err(|_| Refusal::Unreadable(p.display().to_string()))?;
    Ok(Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect())
}

/// What the caller observed about this process; injected so each refusal can be tested.
pub struct Context<'a> {
    pub argv: &'a [String],
    pub ppid: i32,
    pub label: Option<&'a str>,
    /// Does launchd list this process as the running instance of `label`?
    pub is_job_process: &'a dyn Fn(&str) -> bool,
    pub hash: &'a dyn Fn(&Path) -> Result<String, Refusal>,
}

pub fn authorize<'e>(entries: &'e [Entry], cx: &Context) -> Result<&'e Entry, Refusal> {
    let cmd = cx.argv.first().ok_or(Refusal::NoCommand)?;
    if !Path::new(cmd).is_absolute() {
        return Err(Refusal::NotAbsolute(cmd.clone()));
    }
    if cx.ppid != 1 {
        return Err(Refusal::NotUnderLaunchd(cx.ppid));
    }
    let label = cx.label.filter(|l| !l.is_empty()).ok_or(Refusal::NoLabel)?;
    let candidates: Vec<&Entry> = entries.iter().filter(|e| e.label == label).collect();
    if candidates.is_empty() {
        return Err(Refusal::LabelNotAllowed(label.to_string()));
    }
    let entry = candidates
        .into_iter()
        .find(|e| e.argv.as_slice() == cx.argv)
        .ok_or_else(|| Refusal::ArgvMismatch(label.to_string()))?;
    if !(cx.is_job_process)(label) {
        return Err(Refusal::NotTheJobProcess(label.to_string()));
    }
    let got = (cx.hash)(&entry.hashed)?;
    if got != entry.sha256 {
        return Err(Refusal::HashMismatch {
            file: entry.hashed.display().to_string(),
            want: entry.sha256.clone(),
            got,
        });
    }
    Ok(entry)
}

/// `launchctl print gui/UID/LABEL` names the job's pid as `pid = N`.
pub fn launchctl_pid(print_output: &str) -> Option<i32> {
    print_output.lines().find_map(|l| l.trim().strip_prefix("pid = ").and_then(|v| v.trim().parse().ok()))
}
