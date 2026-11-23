//! The decision ledger: one JSON line per guard decision, appended to a local file that a
//! collector ships off the machine. Without it a refusal left no trace outside the provider's
//! own transcript, which an agent can write.
//!
//! It must never slow or break a hook: every failure is swallowed, the line is built in memory
//! and written with one `write` on an `O_APPEND` descriptor (no fsync), and a file past
//! `MAX_LEDGER_BYTES` is left alone until the collector rotates it.
//!
//! Secrets: the command is redacted before it is written (tokens, keys, `password=` values,
//! long base64 or hex runs) and cut to `MAX_CMD`; each redaction carries the first 8 hex of an
//! HMAC of the secret, so a repeat can be matched without revealing it. `cmd_h` is the same
//! HMAC of the whole command. The HMAC key is a random file beside the ledger and never leaves
//! this machine. No environment variables are written.
//!
//! Path: `$AGENTKIT_DECISIONS` when set (`off` disables the ledger), else
//! `<agentkit state>/decisions.jsonl`.

use crate::guard::Decision;
use fancy_regex::{Captures, Regex};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub const MAX_LEDGER_BYTES: u64 = 256 * 1024 * 1024;
pub const MAX_CMD: usize = 2048;
const MAX_FIELD: usize = 512;

/// Where the ledger goes, or None when it is switched off.
pub fn ledger_path(state_dir: &Path) -> Option<PathBuf> {
    match std::env::var_os("AGENTKIT_DECISIONS") {
        Some(v) if v == "off" => None,
        Some(v) if !v.is_empty() => Some(PathBuf::from(v)),
        _ => Some(state_dir.join("decisions.jsonl")),
    }
}

/// Append one line for this decision. Never panics, never returns an error.
pub fn record(provider: &str, guard: &str, value: &Value, decision: &Decision, state_dir: &Path) {
    let Some(path) = ledger_path(state_dir) else { return };
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let key = hmac_key(&path);
        let line = build_line(provider, guard, value, decision, key.as_deref(), now_iso(), std::process::id(), parent_pid());
        let _ = append_line(&path, &line);
    }));
}

/// The JSON line (without its newline). Public for tests.
#[allow(clippy::too_many_arguments)]
pub fn build_line(
    provider: &str,
    guard: &str,
    value: &Value,
    decision: &Decision,
    key: Option<&[u8]>,
    at: String,
    pid: u32,
    ppid: i64,
) -> String {
    let command = command_of(value);
    let tool = value.get("tool_name").and_then(|t| t.as_str()).unwrap_or(if value.get("command").is_some() { "shell" } else { "" });
    let session = ["session_id", "conversation_id", "thread_id"]
        .iter()
        .find_map(|k| value.get(*k).and_then(|s| s.as_str()))
        .unwrap_or("");
    let cwd = value.get("cwd").and_then(|c| c.as_str()).unwrap_or("");
    let target = value
        .get("tool_input")
        .and_then(|i| i.get("file_path").or_else(|| i.get("notebook_path")).or_else(|| i.get("path")))
        .and_then(|p| p.as_str())
        .unwrap_or("");
    let mut row = json!({
        "t": at,
        "v": 1,
        "provider": provider,
        "guard": guard,
        "decision": decision.decision,
        "rule": decision.rule,
        "pid": pid,
        "ppid": ppid,
        "session": cut(&redact(session, key).0, 128),
        "tool": cut(tool, 64),
        "cwd": cut(&redact(cwd, key).0, MAX_FIELD),
    });
    let obj = row.as_object_mut().expect("object");
    if !command.is_empty() {
        let (red, n) = redact(&command, key);
        obj.insert("cmd".into(), Value::String(cut(&red, MAX_CMD)));
        obj.insert("cmd_len".into(), json!(command.len()));
        obj.insert("cmd_h".into(), Value::String(mac_hex(key, command.as_bytes(), 16)));
        if n > 0 {
            obj.insert("redacted".into(), json!(n));
        }
    }
    if !target.is_empty() {
        obj.insert("path".into(), Value::String(cut(&redact(target, key).0, MAX_FIELD)));
    }
    if !decision.is_allow() {
        obj.insert("reason".into(), Value::String(cut(&redact(&decision.reason, key).0, 300)));
    }
    row.to_string()
}

fn command_of(value: &Value) -> String {
    if let Some(s) = value.get("command").and_then(|c| c.as_str()) {
        return s.to_string();
    }
    value
        .get("tool_input")
        .and_then(|t| t.get("command"))
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .to_string()
}

/// Cut to at most `max` bytes on a character boundary.
fn cut(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

/// Flags for every open of the ledger or its key: never block on a FIFO, never follow a
/// symlink, never leak the descriptor into a child.
fn safe_flags() -> i32 {
    libc::O_NONBLOCK | libc::O_NOFOLLOW | libc::O_CLOEXEC
}

fn euid() -> u32 {
    unsafe { libc::geteuid() }
}

/// True only for a regular file owned by this user with mode 0600 (checked on the open fd,
/// so a path swapped after the open does not matter).
fn trusted(f: &fs::File) -> bool {
    use std::os::unix::fs::MetadataExt;
    match f.metadata() {
        Ok(m) => m.file_type().is_file() && m.uid() == euid() && (m.mode() & 0o7777) == 0o600,
        Err(_) => false,
    }
}

/// The folder that holds the ledger, opened without following a symlink, and used only when it
/// is a directory owned by this user that nobody else can write. Files are then opened relative
/// to this descriptor, so a folder swapped after the check does not matter.
struct Dir(fs::File);

impl Dir {
    fn open(dir: &Path) -> Option<Dir> {
        use std::os::unix::fs::MetadataExt;
        if !dir.is_dir() {
            fs::create_dir_all(dir).ok()?;
            let _ = fs::set_permissions(dir, std::os::unix::fs::PermissionsExt::from_mode(0o700));
        }
        let f = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
            .open(dir)
            .ok()?;
        let m = f.metadata().ok()?;
        (m.is_dir() && m.uid() == euid() && (m.mode() & 0o022) == 0).then_some(Dir(f))
    }

    fn open_at(&self, name: &str, flags: i32, mode: libc::c_uint) -> Option<fs::File> {
        use std::os::unix::io::{AsRawFd, FromRawFd};
        if name.is_empty() || name.contains('/') {
            return None;
        }
        let c = std::ffi::CString::new(name).ok()?;
        let fd = unsafe { libc::openat(self.0.as_raw_fd(), c.as_ptr(), flags | safe_flags(), mode) };
        (fd >= 0).then(|| unsafe { fs::File::from_raw_fd(fd) })
    }
}

fn split(path: &Path) -> Option<(PathBuf, String)> {
    let name = path.file_name()?.to_str()?.to_string();
    let dir = path.parent().filter(|d| !d.as_os_str().is_empty()).map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
    Some((dir, name))
}

fn append_line(path: &Path, line: &str) -> std::io::Result<()> {
    let bad = || std::io::Error::new(std::io::ErrorKind::Other, "untrusted ledger");
    let (dir, name) = split(path).ok_or_else(bad)?;
    let dir = Dir::open(&dir).ok_or_else(bad)?;
    let mut f = dir.open_at(&name, libc::O_WRONLY | libc::O_APPEND | libc::O_CREAT, 0o600).ok_or_else(bad)?;
    if !trusted(&f) || f.metadata().map(|m| m.len()).unwrap_or(u64::MAX) > MAX_LEDGER_BYTES {
        return Err(bad());
    }
    let mut buf = Vec::with_capacity(line.len() + 1);
    buf.extend_from_slice(line.as_bytes());
    buf.push(b'\n');
    f.write_all(&buf)
}

const MAX_KEY_BYTES: u64 = 256;

fn read_key(dir: &Dir, name: &str) -> Option<Vec<u8>> {
    let f = dir.open_at(name, libc::O_RDONLY, 0)?;
    if !trusted(&f) {
        return None;
    }
    let mut key = Vec::new();
    f.take(MAX_KEY_BYTES).read_to_end(&mut key).ok()?;
    (key.len() >= 16).then_some(key)
}

/// The HMAC key's path for a ledger: `ledger.key` beside it.
pub fn key_path(ledger: &Path) -> PathBuf {
    ledger.with_file_name("ledger.key")
}

/// The HMAC key beside the ledger, created once (32 random bytes, 0600). None when unavailable
/// or not a plain file of ours.
fn hmac_key(ledger: &Path) -> Option<Vec<u8>> {
    let (dir, name) = split(&key_path(ledger))?;
    let dir = Dir::open(&dir)?;
    if let Some(k) = read_key(&dir, &name) {
        return Some(k);
    }
    let mut key = vec![0u8; 32];
    OpenOptions::new()
        .read(true)
        .custom_flags(safe_flags())
        .open("/dev/urandom")
        .ok()?
        .read_exact(&mut key)
        .ok()?;
    // O_EXCL: fails when anything (a FIFO, a link, a stranger's file) is already there.
    let mut f = dir.open_at(&name, libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL, 0o600);
    match f.as_mut() {
        Some(f) if trusted(f) => {
            f.write_all(&key).ok()?;
            Some(key)
        }
        Some(_) => None,
        // Another hook made it first: read theirs.
        None => read_key(&dir, &name),
    }
}

/// agentkit's state folder: `$AGENTKIT_STATE`, else `~/.local/state/agentkit`.
pub fn default_state_dir(home: &Path) -> PathBuf {
    std::env::var_os("AGENTKIT_STATE")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".local/state/agentkit"))
}

/// Everything the guard-config guard keeps agents from replacing, from the same functions the
/// writer uses: the state folders, the ledger's folder, the ledger and its key.
pub fn protected_paths(home: &Path) -> (Vec<PathBuf>, Vec<PathBuf>) {
    // The state folders are protected whole (any name in them, so a hard link made there is
    // covered); a custom ledger elsewhere protects its two files, and its folder as an ancestor.
    let mut dirs = vec![home.join(".local/state/agentkit"), default_state_dir(home)];
    let mut files = Vec::new();
    if let Some(ledger) = ledger_path(&default_state_dir(home)) {
        files.push(key_path(&ledger));
        files.push(ledger);
    }
    dirs.dedup();
    (dirs, files)
}

/// HMAC-SHA256, first `n` hex characters. With no key, a plain marker (no digest at all), so
/// an unkeyed hash of a short secret is never written.
pub fn mac_hex(key: Option<&[u8]>, data: &[u8], n: usize) -> String {
    let Some(key) = key else { return "nokey".into() };
    let mut k = [0u8; 64];
    if key.len() > 64 {
        k[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let ipad: Vec<u8> = k.iter().map(|b| b ^ 0x36).collect();
    let opad: Vec<u8> = k.iter().map(|b| b ^ 0x5c).collect();
    let inner = Sha256::new().chain_update(&ipad).chain_update(data).finalize();
    let outer = Sha256::new().chain_update(&opad).chain_update(inner).finalize();
    outer.iter().map(|b| format!("{b:02x}")).collect::<String>()[..n.min(64)].to_string()
}

struct Rule {
    re: Regex,
    /// Capture group holding the secret; earlier groups are kept as written.
    group: usize,
}

fn rules() -> &'static [Rule] {
    static RULES: OnceLock<Vec<Rule>> = OnceLock::new();
    RULES.get_or_init(|| {
        let r = |p: &str, group: usize| Rule { re: Regex::new(p).expect("ledger regex"), group };
        // A value: a double- or single-quoted string (to its closing quote), or a bare word.
        const VAL: &str = r#"("[^"]*"|'[^']*'|[^\s&'";|]+)"#;
        vec![
            r(r"(-----BEGIN[A-Z0-9 ]*-----[\s\S]*?(?:-----END[A-Z0-9 ]*-----|$))", 1),
            // Headers: Authorization (any scheme), x-…-key / x-…-token / x-…-secret, api-key.
            r(&format!(r#"(?i)((?:proxy-)?authorization\s*:\s*(?:(?:bearer|basic|token|digest|negotiate)\s+)?){VAL}"#), 2),
            r(&format!(r#"(?i)(\bx-[a-z0-9-]*(?:key|token|secret|auth|signature|session)[a-z0-9-]*\s*:\s*){VAL}"#), 2),
            r(&format!(r#"(?i)(\b(?:api-key|apikey|cookie|set-cookie)\s*:\s*){VAL}"#), 2),
            r(r"(?i)(bearer\s+)([A-Za-z0-9._~+/=-]{8,})", 2),
            r(r"(?i)(basic\s+)([A-Za-z0-9+/=]{8,})", 2),
            r(r"((?:sk|rk|pk)-[A-Za-z0-9_-]{16,})", 1),
            r(r"((?:ghp|gho|ghu|ghs|ghr)_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,})", 1),
            r(r"((?:AKIA|ASIA)[0-9A-Z]{16})", 1),
            r(r"(xox[abprs]-[A-Za-z0-9-]{10,})", 1),
            r(r"(AIza[0-9A-Za-z_-]{30,})", 1),
            // name=value and "name": "value", with or without quotes around the value.
            // Any name holding a secret word, with prefixes and suffixes: PGPASSWORD=, MYSQL_PWD=,
            // AWS_SECRET_ACCESS_KEY=, GITHUB_TOKEN=, OPENAI_API_KEY=, "client_secret": …
            r(&format!(r#"(?i)(\b[A-Z0-9_-]*(?:password|passwd|pass|pwd|passphrase|token|secret|key|credentials?|auth)[A-Z0-9_-]*["']?\s*[=:]\s*){VAL}"#), 2),
            // --password VALUE, --token='VALUE' …
            r(&format!(r#"(?i)(--(?:password|pass|passwd|token|secret|api-key|apikey|auth|auth-token|access-token|client-secret|key)(?:=|\s+)){VAL}"#), 2),
            // curl -u user:pass, --user user:pass (quoted or not)
            r(r#"((?:^|\s)(?:-u\s*|--user(?:=|\s+))["']?[^\s:'"]*:)([^\s'"]+)"#, 2),
            // scheme://user:pass@host
            r(r"(?i)([a-z][a-z0-9+.-]*://[^/\s:@]+:)([^@\s/]+)(@)", 2),
        ]
    })
}

fn long_run() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    // A run of base64/url-safe characters, `/ _ + =` included, so a secret such as
    // `wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY` is judged whole. `. : ? &` still end a run.
    RE.get_or_init(|| Regex::new(r"[A-Za-z0-9+/_%=-]{20,}").expect("ledger regex"))
}

/// A path segment that reads as a name (`services`, `Projects`, `node_modules`): letters, with
/// at most a capital first, and no digits.
fn wordish(seg: &str) -> bool {
    let mut chars = seg.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() => {}
        _ => return false,
    }
    seg.len() >= 3 && chars.all(|c| c.is_ascii_lowercase() || c == '_' || c == '-')
}

/// Replace a run: whole when it looks random; when it holds a word-like path segment (a URL or a
/// path), each `/` segment on its own, so the path stays and the token in it goes.
fn redact_run(run: &str, key: Option<&[u8]>, count: &mut usize) -> String {
    if run.contains("<redacted") {
        return run.to_string();
    }
    let segs: Vec<&str> = run.split('/').collect();
    if segs.len() > 1 && segs.iter().any(|s| wordish(s)) {
        return segs
            .iter()
            .map(|s| {
                if s.len() >= 20 && looks_random(s) {
                    *count += 1;
                    format!("<redacted:{}>", mac_hex(key, s.as_bytes(), 8))
                } else {
                    s.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("/");
    }
    let core = run.trim_start_matches('/');
    if looks_random(core) {
        *count += 1;
        let lead = &run[..run.len() - core.len()];
        return format!("{lead}<redacted:{}>", mac_hex(key, core.as_bytes(), 8));
    }
    run.to_string()
}

/// A long piece that looks random rather than like a name: base64-like (upper, lower and digits
/// mixed), padded base64, or long hex; and not low-entropy.
fn looks_random(piece: &str) -> bool {
    let core = piece.trim_end_matches('=');
    let core_alnum = core.bytes().filter(|b| b.is_ascii_alphanumeric()).count();
    if core_alnum < 16 {
        return false;
    }
    let upper = core.bytes().any(|b| b.is_ascii_uppercase());
    let lower = core.bytes().any(|b| b.is_ascii_lowercase());
    let digits = core.bytes().filter(|b| b.is_ascii_digit()).count();
    let letters = core.bytes().filter(|b| b.is_ascii_alphabetic()).count();
    // Any case: 24+ characters with two digits and two letters (lower-case API keys, dop_v1_…).
    let long_mixed = core.len() >= 24 && digits >= 2 && letters >= 2;
    let undashed: Vec<u8> = core.bytes().filter(|b| *b != b'-').collect();
    // Hex of 32+ digits, dashes allowed (UUIDs, hashes).
    let hex = undashed.len() >= 32 && undashed.iter().all(|b| b.is_ascii_hexdigit());
    let padded = piece.ends_with('=') && (piece.len() % 4 == 0) && (upper || lower) && core.len() >= 16;
    let mixed = upper && lower && digits >= 1;
    (long_mixed || mixed || hex || padded) && entropy(core) >= 3.0
}

fn entropy(s: &str) -> f64 {
    let mut counts = [0u32; 256];
    for b in s.bytes() {
        counts[b as usize] += 1;
    }
    let n = s.len() as f64;
    if n == 0.0 {
        return 0.0;
    }
    counts.iter().filter(|&&c| c > 0).map(|&c| {
        let p = c as f64 / n;
        -p * p.log2()
    }).sum()
}

/// Replace secrets with `<redacted:hmac8>`. Returns the text and how many were replaced.
pub fn redact(text: &str, key: Option<&[u8]>) -> (String, usize) {
    let mut out = text.to_string();
    let mut count = 0usize;
    for rule in rules() {
        let replaced = rule.re.replace_all(&out, |caps: &Captures| {
            let secret = caps.get(rule.group).map(|m| m.as_str()).unwrap_or("");
            if secret.starts_with("<redacted:") {
                return caps.get(0).map(|m| m.as_str().to_string()).unwrap_or_default();
            }
            count += 1;
            let mut s = String::new();
            for g in 1..caps.len() {
                let part = caps.get(g).map(|m| m.as_str()).unwrap_or("");
                if g == rule.group {
                    s.push_str(&format!("<redacted:{}>", mac_hex(key, part.as_bytes(), 8)));
                } else {
                    s.push_str(part);
                }
            }
            s
        });
        out = replaced.into_owned();
    }
    let replaced = long_run().replace_all(&out, |caps: &Captures| {
        let m = caps.get(0).map(|m| m.as_str()).unwrap_or("");
        redact_run(m, key, &mut count)
    });
    (replaced.into_owned(), count)
}

fn parent_pid() -> i64 {
    #[cfg(unix)]
    unsafe {
        return libc::getppid() as i64;
    }
    #[allow(unreachable_code)]
    0
}

/// UTC now as `YYYY-MM-DDTHH:MM:SS.mmmZ`.
pub fn now_iso() -> String {
    let d = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    iso_from(d.as_secs() as i64, d.subsec_millis())
}

pub fn iso_from(secs: i64, millis: u32) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{millis:03}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}
