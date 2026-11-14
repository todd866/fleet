use libc::{
    LOCK_EX, LOCK_NB, LOCK_UN, WEXITSTATUS, WIFEXITED, WIFSIGNALED, WTERMSIG, c_int, flock, wait4,
};
use std::env;
use std::ffi::OsStr;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static CHILD_PID: AtomicI32 = AtomicI32::new(0);
static QUEUE_REGISTERED: AtomicI32 = AtomicI32::new(0);
/// The held slot's lock fd while a job runs; whoever swaps it to -1 owns the unlock.
static LOCK_FD: AtomicI32 = AtomicI32::new(-1);
/// Set by SIGUSR1 (`heavy-slot release PID`): give the slot back, keep the job running.
static RELEASE_REQUESTED: AtomicI32 = AtomicI32::new(0);
static JOB_DONE: AtomicI32 = AtomicI32::new(0);
/// Registry entry path as a C string, prepared before any signal can arrive: a signal handler may
/// only call async-signal-safe functions (unlink, kill, _exit), never allocate or read the env.
static QUEUE_ENTRY_C: std::sync::OnceLock<std::ffi::CString> = std::sync::OnceLock::new();

fn queue_steward_bin() -> PathBuf {
    if let Ok(p) = env::var("QUEUE_STEWARD") {
        return PathBuf::from(p);
    }
    env::var("HOME")
        .map(|h| PathBuf::from(h).join(".local/bin/queue-steward"))
        .unwrap_or_else(|_| PathBuf::from(".local/bin/queue-steward"))
}

fn steward_pick_pid(dir: &Path) -> Option<i32> {
    let bin = queue_steward_bin();
    if !bin.is_file() {
        return None;
    }
    let out = Command::new(&bin)
        .env("HEAVY_SLOT_DIR", dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .arg("pick")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .and_then(|l| l.trim().parse().ok())
}

fn steward_status_lines(dir: &Path) -> Option<String> {
    let bin = queue_steward_bin();
    if !bin.is_file() {
        return None;
    }
    let out = Command::new(&bin)
        .env("HEAVY_SLOT_DIR", dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .arg("status")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).into_owned();
    if s.trim().is_empty() { None } else { Some(s) }
}

fn detect_wait_origin() -> String {
    if env::var("CLAUDE_SESSION_INTERACTIVE")
        .ok()
        .is_some_and(|v| v == "1")
    {
        return "interactive".to_string();
    }
    let stdin_tty = unsafe { libc::isatty(libc::STDIN_FILENO) == 1 };
    let stderr_tty = unsafe { libc::isatty(libc::STDERR_FILENO) == 1 };
    if stdin_tty || stderr_tty {
        return "interactive".to_string();
    }
    if env::var("NIGHT_RUN").ok().is_some_and(|v| v == "1") {
        return "scheduled".to_string();
    }
    if let Ok(xpc) = env::var("XPC_SERVICE_NAME")
        && !xpc.is_empty()
        && xpc != "0"
        && !xpc.starts_with("application.")
    {
        return "scheduled".to_string();
    }
    "agent".to_string()
}

fn git_project_name() -> String {
    let Ok(cwd) = env::current_dir() else {
        return ".".to_string();
    };
    let mut cur = cwd.clone();
    loop {
        if cur.join(".git").exists() {
            return cur
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| ".".to_string());
        }
        if !cur.pop() {
            break;
        }
    }
    cwd.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| ".".to_string())
}

fn median_label_duration(dir: &Path, label: &str, now: i64) -> Option<u32> {
    let Ok(content) = fs::read_to_string(dir.join("jobs.jsonl")) else {
        return None;
    };
    let cutoff = now - 30 * 24 * 3600;
    let mut durations: Vec<u32> = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let label_ok = line.contains(&format!("\"label\":\"{label}\""));
        if !label_ok {
            continue;
        }
        let start = line
            .split("\"start\":")
            .nth(1)
            .and_then(|r| r.split(',').next())
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or(0);
        if start < cutoff {
            continue;
        }
        let end = line
            .split("\"end\":")
            .nth(1)
            .and_then(|r| r.split(',').next())
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or(start);
        durations.push((end - start).max(0) as u32);
    }
    if durations.is_empty() {
        return None;
    }
    durations.sort_unstable();
    let mid = durations.len() / 2;
    if durations.len() % 2 == 1 {
        Some(durations[mid])
    } else {
        Some((durations[mid - 1] + durations[mid]) / 2)
    }
}

fn queue_register(dir: &Path, pid: i32, label: &str, args: &[String], state: &str) {
    let queue_dir = dir.join("queue");
    fs::create_dir_all(&queue_dir).ok();
    let now = unix_now();
    let project = git_project_name();
    let origin = detect_wait_origin();
    let expected = median_label_duration(dir, label, now);
    let expected_json = match expected {
        Some(s) => s.to_string(),
        None => "null".to_string(),
    };
    let host = {
        let mut buf = [0u8; 256];
        let rc = unsafe { libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, buf.len()) };
        if rc == 0 {
            let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
            String::from_utf8_lossy(&buf[..end])
                .split('.')
                .next()
                .unwrap_or("localhost")
                .to_string()
        } else {
            "localhost".to_string()
        }
    };
    let cmd = format_cmd_for_log(args);
    let line = format!(
        "{{\"pid\":{pid},\"label\":\"{}\",\"project\":\"{}\",\"origin\":\"{}\",\"cmd\":\"{}\",\"expected_s\":{expected_json},\"since\":{now},\"host\":\"{}\",\"state\":\"{}\"}}\n",
        json_escape(label),
        json_escape(&project),
        json_escape(&origin),
        json_escape(&cmd),
        json_escape(&host),
        json_escape(state),
    );
    let entry = queue_dir.join(format!("{pid}.json"));
    fs::write(&entry, line).ok();
    if let Ok(c) = std::ffi::CString::new(entry.as_os_str().as_encoded_bytes()) {
        let _ = QUEUE_ENTRY_C.set(c);
    }
    QUEUE_REGISTERED.store(pid, Ordering::Relaxed);
}

fn queue_deregister(dir: &Path) {
    let pid = QUEUE_REGISTERED.swap(0, Ordering::Relaxed);
    if pid > 0 {
        fs::remove_file(dir.join("queue").join(format!("{pid}.json"))).ok();
    }
}

pub fn state_dir() -> PathBuf {
    if let Ok(d) = env::var("HEAVY_SLOT_DIR") {
        return PathBuf::from(d);
    }
    env::var("HOME")
        .map(|h| PathBuf::from(h).join(".local/state/heavy-slot"))
        .unwrap_or_else(|_| PathBuf::from(".local/state/heavy-slot"))
}

pub fn heavy_slots() -> u32 {
    env::var("HEAVY_SLOTS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3)
}

pub fn cool_max_secs() -> u32 {
    env::var("HEAVY_SLOT_COOL_MAX")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1200)
}

pub fn parse_limit(raw: Option<&str>, default_slots: u32) -> u32 {
    match raw {
        Some(s) if !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()) => {
            s.parse().unwrap_or(default_slots)
        }
        _ => default_slots,
    }
}

pub fn read_limit_file(dir: &Path, default_slots: u32) -> u32 {
    let p = dir.join("limit");
    let raw = fs::read_to_string(&p).ok();
    parse_limit(raw.as_deref().map(str::trim), default_slots)
}

pub fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\t' | '\n' | '\r' => out.push(' '),
            _ => out.push(ch),
        }
    }
    out
}

pub fn truncate_utf8(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

pub fn format_info_line(label: &str, pid: i32, time_hms: &str, args: &[String]) -> String {
    let args_joined = args.join(" ");
    let line = format!("{label} · pid {pid} · {time_hms} · {args_joined}");
    truncate_utf8(&line, 200).to_string()
}

pub fn format_cmd_for_log(args: &[String]) -> String {
    let mut s = args.join(" ");
    s.push(' ');
    truncate_utf8(&s, 300).to_string()
}

#[derive(Debug, Default)]
pub struct GlanceSample {
    pub chip_c: Option<u32>,
    pub thermal: Option<String>,
}

pub fn parse_glance_sample(s: &str) -> GlanceSample {
    let mut g = GlanceSample::default();
    if let Some(idx) = s.find("\"chipC\":") {
        let rest = &s[idx + 8..];
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if !digits.is_empty() {
            g.chip_c = digits.parse().ok();
        }
    }
    if let Some(idx) = s.find("\"thermal\":\"") {
        let rest = &s[idx + 11..];
        let word: String = rest
            .chars()
            .take_while(|c| c.is_ascii_lowercase())
            .collect();
        if !word.is_empty() {
            g.thermal = Some(word);
        }
    }
    g
}

pub fn read_cool_target(dir: &Path) -> i32 {
    let p = dir.join("cool");
    let Ok(content) = fs::read_to_string(&p) else {
        return 0;
    };
    content
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().next())
        .and_then(|w| w.parse::<f64>().ok())
        .map(|f| f as i32)
        .unwrap_or(0)
}

pub fn cool_reading_display(sample: &str) -> (String, String) {
    let g = parse_glance_sample(sample);
    let chip = g
        .chip_c
        .map(|c| c.to_string())
        .unwrap_or_else(|| "?".to_string());
    let th = g
        .thermal
        .map(|t| t.to_string())
        .unwrap_or_else(|| "?".to_string());
    (chip, th)
}

pub fn cool_ok(dir: &Path, sample: &str) -> bool {
    let max = read_cool_target(dir);
    if max <= 0 {
        return true;
    }
    let g = parse_glance_sample(sample);
    if let Some(ref th) = g.thermal
        && th != "nominal"
    {
        return false;
    }
    if let Some(chip) = g.chip_c
        && chip > max as u32
    {
        return false;
    }
    true
}

fn glancebar_paths() -> [PathBuf; 2] {
    let home = env::var("HOME").unwrap_or_default();
    [
        PathBuf::from("/Applications/Glancebar.app/Contents/MacOS/Glancebar"),
        PathBuf::from(home).join("Applications/Glancebar.app/Contents/MacOS/Glancebar"),
    ]
}

fn path_is_executable(p: &Path) -> bool {
    fs::metadata(p)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

pub fn run_glancebar_sample() -> Option<String> {
    for gb in glancebar_paths() {
        if path_is_executable(&gb) {
            let out = Command::new(&gb).arg("--sample").output().ok()?;
            if out.status.success() || !out.stdout.is_empty() {
                return String::from_utf8(out.stdout).ok();
            }
        }
    }
    None
}

pub fn try_lock_nonblocking(lock_path: &Path) -> Option<std::fs::File> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .read(true)
        .write(true)
        .open(lock_path)
        .ok()?;
    let fd = file.as_raw_fd();
    let rc = unsafe { flock(fd, LOCK_EX | LOCK_NB) };
    if rc == 0 { Some(file) } else { None }
}

pub fn probe_lock_free(lock_path: &Path) -> bool {
    let Some(file) = try_lock_nonblocking(lock_path) else {
        return false;
    };
    let fd = file.as_raw_fd();
    unsafe {
        flock(fd, LOCK_UN);
    }
    true
}

pub fn local_time_hms() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as libc::time_t;
    unsafe {
        let tm = libc::localtime(&now);
        if tm.is_null() {
            return "00:00:00".to_string();
        }
        let tm = *tm;
        format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec)
    }
}

pub fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

pub fn cmd_status(_args: &[String]) -> i32 {
    let dir = state_dir();
    fs::create_dir_all(&dir).ok();
    let slots = heavy_slots();

    for i in 1..=slots {
        let lock_path = dir.join(format!("slot{i}.lock"));
        let info_path = dir.join(format!("slot{i}.info"));
        let limit = read_limit_file(&dir, slots);
        if probe_lock_free(&lock_path) {
            if i > limit {
                println!("slot {i}: off (limit {limit})");
            } else {
                println!("slot {i}: free");
            }
        } else {
            let info = fs::read_to_string(&info_path)
                .ok()
                .map(|s| s.trim_end().to_string())
                .unwrap_or_else(|| "busy".to_string());
            println!("slot {i}: {info}");
        }
    }
    if let Some(lines) = steward_status_lines(&dir) {
        print!("{lines}");
        if !lines.ends_with('\n') {
            println!();
        }
    }
    if let Some(counts) = steward_counts_line(&dir) {
        println!("{counts}");
    }
    0
}

fn steward_counts_line(dir: &Path) -> Option<String> {
    let bin = queue_steward_bin();
    if !bin.is_file() {
        return None;
    }
    let out = Command::new(&bin)
        .env("HEAVY_SLOT_DIR", dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .arg("counts")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() { None } else { Some(s) }
}

/// `heavy-slot mark <waiting|transferring|processing|done> [--pid N]`: another process (remote-run
/// around its mirror, a worker fetching inputs) records what a job is doing before it asks for a
/// slot. Default pid is the caller's parent, the job itself. `done` removes the entry.
fn cmd_mark(args: &[String]) -> i32 {
    let Some(state) = args.first() else {
        eprintln!("usage: heavy-slot mark <waiting|transferring|processing|done> [--pid N]");
        return 2;
    };
    let mut pid = unsafe { libc::getppid() };
    if let Some(i) = args.iter().position(|a| a == "--pid")
        && let Some(p) = args.get(i + 1).and_then(|p| p.parse().ok())
    {
        pid = p;
    }
    let dir = state_dir();
    let entry = dir.join("queue").join(format!("{pid}.json"));
    match state.as_str() {
        "done" => {
            fs::remove_file(entry).ok();
            0
        }
        "waiting" | "transferring" | "processing" => {
            let label = env::var("HEAVY_SLOT_LABEL").unwrap_or_else(|_| git_project_name());
            let cmd: Vec<String> = args
                .iter()
                .skip(1)
                .filter(|a| *a != "--pid")
                .cloned()
                .collect();
            queue_register(&dir, pid, &label, &cmd, state);
            0
        }
        other => {
            eprintln!("heavy-slot mark: unknown state {other}");
            2
        }
    }
}

fn should_run_offload_check() -> bool {
    if env::var("HEAVY_SLOT_LOCAL").ok().as_deref() == Some("1") {
        return false;
    }
    if env::var("REMOTE_RUN").ok().as_deref() == Some("1") {
        return false;
    }
    let xpc = env::var("XPC_SERVICE_NAME").unwrap_or_else(|_| "0".to_string());
    if xpc != "0" && !xpc.starts_with("application.") {
        return false;
    }
    true
}

fn run_offload_target() -> Result<(i32, String), ()> {
    let home = env::var("HOME").map_err(|_| ())?;
    let ot = PathBuf::from(home).join(".local/bin/offload-target");
    let out = Command::new(&ot).output().map_err(|_| ())?;
    let worker = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Ok((out.status.code().unwrap_or(1), worker))
}

fn offload_message(args: &[String], worker: &str) {
    let cmd_line = args.join(" ");
    eprintln!(
        "heavy-slot: this Mac should stay cool (in use, on battery, or hot) and worker '{worker}' can take it — run it there:"
    );
    eprintln!("    remote-run -- {cmd_line}");
    eprintln!(
        "  (remote-run mirrors the worktree without .git or ignored files; --pull PATH copies results back.)"
    );
    eprintln!(
        "  If it truly needs this Mac (.git, local hardware, the installed app), rerun with HEAVY_SLOT_LOCAL=1."
    );
}

fn read_mode_gentle(dir: &Path) -> bool {
    fs::read_to_string(dir.join("mode"))
        .map(|s| s.trim() == "gentle")
        .unwrap_or(false)
}

struct JobLogEntry<'a> {
    start: i64,
    end: i64,
    waited: u32,
    slot: u32,
    label: &'a str,
    rc: i32,
    maxrss_mb: Option<u32>,
    cmd: &'a str,
}

fn append_job_log(dir: &Path, entry: JobLogEntry<'_>) -> std::io::Result<()> {
    let mb = match entry.maxrss_mb {
        Some(m) => m.to_string(),
        None => "null".to_string(),
    };
    let line = format!(
        "{{\"start\":{start},\"end\":{end},\"waited\":{waited},\"slot\":{slot},\"label\":\"{}\",\"rc\":{rc},\"maxrss_mb\":{mb},\"cmd\":\"{}\"}}\n",
        json_escape(entry.label),
        json_escape(entry.cmd),
        start = entry.start,
        end = entry.end,
        waited = entry.waited,
        slot = entry.slot,
        rc = entry.rc,
    );
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("jobs.jsonl"))?;
    f.write_all(line.as_bytes())
}

fn child_exit_rc(status: c_int) -> i32 {
    if WIFEXITED(status) {
        WEXITSTATUS(status) as i32
    } else if WIFSIGNALED(status) {
        128 + WTERMSIG(status) as i32
    } else {
        1
    }
}

fn install_signal_forwarding() {
    unsafe {
        extern "C" fn handler(sig: libc::c_int) {
            // Async-signal-safe only: kill, unlink, _exit.
            let pid = CHILD_PID.load(Ordering::Relaxed);
            if pid > 0 {
                unsafe {
                    libc::kill(pid, sig);
                }
                return;
            }
            // No job yet (still waiting for a slot or cooling): leave the queue and stop, as the
            // bash script's sleep loop did when interrupted.
            if QUEUE_REGISTERED.swap(0, Ordering::Relaxed) > 0
                && let Some(c) = QUEUE_ENTRY_C.get()
            {
                unsafe {
                    libc::unlink(c.as_ptr());
                }
            }
            unsafe {
                libc::_exit(128 + sig);
            }
        }
        libc::signal(libc::SIGINT, handler as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, handler as *const () as libc::sighandler_t);
    }
}

/// Seconds of near-zero CPU after which a running job gives its slot back (0 disables).
pub fn idle_secs() -> u32 {
    env::var("HEAVY_SLOT_IDLE_SECS").ok().and_then(|s| s.trim().parse().ok()).unwrap_or(600)
}

/// ps `time` column: `[[dd-]hh:]mm:ss[.cc]` → seconds.
pub fn parse_cputime(s: &str) -> Option<f64> {
    let s = s.trim();
    let (days, rest) = match s.split_once('-') {
        Some((d, r)) => (d.parse::<f64>().ok()?, r),
        None => (0.0, s),
    };
    let mut total = 0.0;
    for part in rest.split(':') {
        total = total * 60.0 + part.parse::<f64>().ok()?;
    }
    Some(days * 86400.0 + total)
}

/// CPU seconds used so far by `root` and all its descendants, from `ps -axo pid=,ppid=,time=`.
pub fn tree_cpu_secs(root: i32, ps_out: &str) -> f64 {
    tree_cpu_rows(root, ps_out).iter().map(|(_, t)| t).sum()
}

/// (pid, CPU seconds) for `root` and all its living descendants.
pub fn tree_cpu_rows(root: i32, ps_out: &str) -> Vec<(i32, f64)> {
    let rows: Vec<(i32, i32, f64)> = ps_out
        .lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            Some((it.next()?.parse().ok()?, it.next()?.parse().ok()?, parse_cputime(it.next()?)?))
        })
        .collect();
    let mut tree = vec![root];
    let mut i = 0;
    while i < tree.len() {
        let parent = tree[i];
        for (pid, ppid, _) in &rows {
            if *ppid == parent && !tree.contains(pid) {
                tree.push(*pid);
            }
        }
        i += 1;
    }
    rows.iter().filter(|(pid, _, _)| tree.contains(pid)).map(|(pid, _, t)| (*pid, *t)).collect()
}

/// Running total of a job tree's CPU that keeps what exited processes used.
///
/// `tree_cpu_secs` only sums processes alive at the sample, so a build or test run made of
/// short-lived children (compilers, test binaries, node workers) showed a flat total and lost
/// its slot as "idle" while it saturated the Mac (14 Nov 2026: the M2 sat at load 2.6 per core
/// with a slot showing free, and every session's work landed there). Each sample adds the last
/// reading of any process that has since left the tree, or whose pid was reused.
#[derive(Default)]
pub struct TreeCpu {
    last: std::collections::HashMap<i32, f64>,
    gone: f64,
}

impl TreeCpu {
    /// Total CPU seconds so far for `root`'s tree, from `ps -axo pid=,ppid=,time=`. Never decreases.
    pub fn sample(&mut self, root: i32, ps_out: &str) -> f64 {
        let now: std::collections::HashMap<i32, f64> = tree_cpu_rows(root, ps_out).into_iter().collect();
        for (pid, was) in &self.last {
            match now.get(pid) {
                Some(t) if *t >= *was => {}
                _ => self.gone += *was,
            }
        }
        self.last = now;
        self.gone + self.last.values().sum::<f64>()
    }
}

/// True when the job used under `max_fraction` of one core across the trailing `window` seconds.
/// `samples` are (unix time, tree CPU seconds), oldest first.
pub fn idle_over_window(samples: &[(i64, f64)], now: i64, window: i64, max_fraction: f64) -> bool {
    let Some(&(_, cpu_now)) = samples.last() else { return false };
    let Some(&(t0, cpu0)) = samples.iter().rev().find(|(t, _)| now - t >= window) else { return false };
    (cpu_now - cpu0) < (now - t0) as f64 * max_fraction
}

fn sample_tree_cpu(root: i32, total: &mut TreeCpu) -> Option<f64> {
    let out = Command::new("/bin/ps").args(["-axo", "pid=,ppid=,time="]).stderr(Stdio::null()).output().ok()?;
    Some(total.sample(root, &String::from_utf8_lossy(&out.stdout)))
}

/// Gives the slot back without touching the job. Returns true if this call released it.
fn release_slot(dir: &Path, slot: u32, why: &str) -> bool {
    let fd = LOCK_FD.swap(-1, Ordering::SeqCst);
    if fd < 0 {
        return false;
    }
    unsafe {
        flock(fd, LOCK_UN);
    }
    queue_deregister(dir);
    eprintln!("heavy-slot: gave slot {slot} back ({why}); the job keeps running");
    true
}

/// Watches the running job: releases the slot on request (SIGUSR1) or once the job has sat idle
/// (waiting on I/O, a hung read) for the idle window, so compute jobs are not queued behind it.
fn spawn_idle_watch(dir: PathBuf, slot: u32) -> std::thread::JoinHandle<()> {
    let window = idle_secs() as i64;
    // Every 5 s at most, so children that live a few seconds are read before they exit.
    let every = (window / 10).clamp(1, 5) as u32;
    std::thread::spawn(move || {
        let mut samples: Vec<(i64, f64)> = Vec::new();
        let mut total = TreeCpu::default();
        let mut tick = 0u32;
        while JOB_DONE.load(Ordering::SeqCst) == 0 {
            std::thread::sleep(std::time::Duration::from_secs(1));
            if RELEASE_REQUESTED.load(Ordering::SeqCst) != 0 {
                release_slot(&dir, slot, "released on request");
                return;
            }
            tick += 1;
            if window == 0 || tick % every != 0 {
                continue;
            }
            let child = CHILD_PID.load(Ordering::Relaxed);
            if child <= 0 {
                continue;
            }
            let Some(cpu) = sample_tree_cpu(child, &mut total) else { continue };
            let now = unix_now();
            samples.push((now, cpu));
            samples.retain(|(t, _)| now - t <= window + 120);
            if idle_over_window(&samples, now, window, 0.01) {
                release_slot(&dir, slot, &format!("idle {window} s, under 1% CPU"));
                return;
            }
        }
    })
}

fn install_release_signal() {
    extern "C" fn on_usr1(_sig: libc::c_int) {
        RELEASE_REQUESTED.store(1, Ordering::SeqCst);
    }
    unsafe {
        libc::signal(libc::SIGUSR1, on_usr1 as *const () as libc::sighandler_t);
    }
}

/// `heavy-slot release PID`: ask the holder PID to give its slot back; its job keeps running.
fn cmd_release(args: &[String]) -> i32 {
    let Some(pid) = args.first().and_then(|a| a.parse::<i32>().ok()).filter(|p| *p > 0) else {
        eprintln!("usage: heavy-slot release PID");
        return 2;
    };
    let dir = state_dir();
    let slots = heavy_slots();
    let needle = format!(" · pid {pid} · ");
    let held = (1..=slots).find(|i| {
        !probe_lock_free(&dir.join(format!("slot{i}.lock")))
            && fs::read_to_string(dir.join(format!("slot{i}.info"))).is_ok_and(|s| s.contains(&needle))
    });
    let Some(slot) = held else {
        eprintln!("heavy-slot: pid {pid} does not hold a slot");
        return 1;
    };
    // The bash version has no SIGUSR1 handler and would die from it.
    let comm = Command::new("/bin/ps").args(["-o", "comm=", "-p", &pid.to_string()]).output().ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default();
    if !comm.ends_with("heavy-slot") {
        eprintln!("heavy-slot: pid {pid} is {comm:?}, not the heavy-slot binary; not signalling it");
        return 1;
    }
    unsafe {
        libc::kill(pid, libc::SIGUSR1);
    }
    for _ in 0..50 {
        if probe_lock_free(&dir.join(format!("slot{slot}.lock"))) {
            println!("slot {slot}: released; pid {pid}'s job keeps running");
            return 0;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    eprintln!("heavy-slot: pid {pid} did not release slot {slot}");
    1
}

pub fn run_job(args: Vec<String>) -> i32 {
    install_release_signal();
    if args.is_empty() {
        eprintln!("usage: heavy-slot -- <command...> | heavy-slot status");
        return 2;
    }

    let dir = state_dir();
    fs::create_dir_all(&dir).ok();
    let slots = heavy_slots();

    let label = env::var("HEAVY_SLOT_LABEL").unwrap_or_else(|_| {
        env::current_dir()
            .ok()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_else(|| ".".to_string())
    });

    let mut gentle_local = false;
    if should_run_offload_check() {
        match run_offload_target() {
            Ok((0, worker)) => {
                offload_message(&args, &worker);
                return 75;
            }
            Ok((2, _)) => gentle_local = true,
            _ => {}
        }
    }

    let mut waited: u32 = 0;
    let mut cool_waited: u32 = 0;
    let cool_max = cool_max_secs();
    let my_pid = std::process::id() as i32;
    let mut queue_registered = false;

    loop {
        if cool_waited < cool_max && read_cool_target(&dir) > 0 {
            let sample = run_glancebar_sample().unwrap_or_default();
            if !cool_ok(&dir, &sample) {
                if !queue_registered {
                    // Visible to the steward from the first wait, cool gate included.
                    queue_register(&dir, my_pid, &label, &args, "waiting");
                    queue_registered = true;
                }
                if cool_waited == 0 {
                    let (chip, th) = cool_reading_display(&sample);
                    let cool_line = fs::read_to_string(dir.join("cool"))
                        .ok()
                        .and_then(|c| c.lines().next().map(|l| l.to_string()))
                        .unwrap_or_else(|| "?".to_string());
                    eprintln!(
                        "heavy-slot: letting this Mac cool first ({chip} °C, thermal {th}; target ≤ {cool_line} °C)"
                    );
                }
                std::thread::sleep(std::time::Duration::from_secs(15));
                cool_waited += 15;
                waited += 15;
                continue;
            }
        }

        let limit = read_limit_file(&dir, slots);
        let try_slots = limit.min(slots);

        if queue_registered
            && let Some(pick) = steward_pick_pid(&dir)
            && pick != my_pid
            && unsafe { libc::kill(pick, 0) == 0 }
        {
            std::thread::sleep(std::time::Duration::from_secs(5));
            waited += 5;
            continue;
        }

        for i in 1..=try_slots {
            let lock_path = dir.join(format!("slot{i}.lock"));
            let Some(lock_file) = try_lock_nonblocking(&lock_path) else {
                continue;
            };

            let info =
                format_info_line(&label, std::process::id() as i32, &local_time_hms(), &args);
            fs::write(dir.join(format!("slot{i}.info")), &info).ok();

            if waited > 0 {
                eprintln!("heavy-slot: got slot {i} after {waited}s");
            }
            // The slot is full only while processing: the entry stays, with that state, until the
            // job ends, so a fleet poller can count holders.
            queue_register(&dir, my_pid, &label, &args, "processing");

            let gentle = read_mode_gentle(&dir) || gentle_local;
            if gentle_local {
                eprintln!(
                    "heavy-slot: no worker free and cool; running here at background priority"
                );
            }

            let start = unix_now();
            let cmd_for_log = format_cmd_for_log(&args);

            // heavy-slot itself holds the lock (the File stays open until after the wait), as
            // `lockf -k` does in the script: the lock lives exactly as long as the holder.
            install_signal_forwarding();
            LOCK_FD.store(lock_file.as_raw_fd(), Ordering::SeqCst);
            let watch = spawn_idle_watch(dir.clone(), i);
            let (rc, maxrss_mb) = spawn_and_wait(&args, gentle);
            JOB_DONE.store(1, Ordering::SeqCst);
            let _ = watch.join();
            LOCK_FD.store(-1, Ordering::SeqCst);
            drop(lock_file);
            queue_deregister(&dir);

            let end = unix_now();
            let _ = append_job_log(
                &dir,
                JobLogEntry {
                    start,
                    end,
                    waited,
                    slot: i,
                    label: &label,
                    rc,
                    maxrss_mb,
                    cmd: &cmd_for_log,
                },
            );
            return rc;
        }

        if !queue_registered {
            eprintln!("heavy-slot: all {slots} slots busy; waiting (heavy-slot status)");
            queue_register(&dir, my_pid, &label, &args, "waiting");
            queue_registered = true;
        }
        std::thread::sleep(std::time::Duration::from_secs(5));
        waited += 5;
    }
}

/// Spawns the job (under `taskpolicy -b` when gentle), forwards SIGINT/SIGTERM to it, and
/// reaps it with wait4 so `ru_maxrss` is the job's own peak resident size, as `/usr/bin/time -l`
/// reported in the script. Returns (exit code or 128+signal, peak MB).
fn spawn_and_wait(args: &[String], gentle: bool) -> (i32, Option<u32>) {
    let mut cmd = if gentle {
        let mut c = Command::new("/usr/sbin/taskpolicy");
        c.arg("-b").args(args.iter().map(OsStr::new));
        c
    } else {
        let mut c = Command::new(OsStr::new(&args[0]));
        c.args(args[1..].iter().map(OsStr::new));
        c
    };
    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("heavy-slot: {}: {e}", args[0]);
            return (127, None);
        }
    };
    let pid = child.id() as i32;
    CHILD_PID.store(pid, Ordering::Relaxed);
    let mut status: c_int = 0;
    let mut rusage: libc::rusage = unsafe { std::mem::zeroed() };
    loop {
        let w = unsafe { wait4(pid, &mut status, 0, &mut rusage) };
        if w == pid {
            break;
        }
        if w < 0 && std::io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
            CHILD_PID.store(0, Ordering::Relaxed);
            return (1, None);
        }
    }
    CHILD_PID.store(0, Ordering::Relaxed);
    // Already reaped by wait4; dropping the handle neither waits nor kills.
    drop(child);
    let maxrss_mb = if rusage.ru_maxrss > 0 {
        Some((rusage.ru_maxrss as u64 / 1_048_576) as u32)
    } else {
        None
    };
    (child_exit_rc(status), maxrss_mb)
}

pub fn run(argv: Vec<String>) -> i32 {
    let mut args = argv;
    if !args.is_empty() {
        args.remove(0);
    }
    if args.first().map(String::as_str) == Some("status") {
        return cmd_status(&[]);
    }
    if args.first().map(String::as_str) == Some("release") {
        return cmd_release(&args[1..]);
    }
    if args.first().map(String::as_str) == Some("mark") {
        return cmd_mark(&args[1..]);
    }
    if args.first().map(String::as_str) == Some("--") {
        args.remove(0);
    }
    run_job(args)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cputime_formats() {
        assert_eq!(parse_cputime("0:01.50"), Some(1.5));
        assert_eq!(parse_cputime("1:02:03.00"), Some(3723.0));
        assert_eq!(parse_cputime("2-00:00:01"), Some(172801.0));
        assert_eq!(parse_cputime("x"), None);
    }

    #[test]
    fn tree_cpu_counts_descendants_only() {
        let ps = "  1     0 9:00.00\n 10     1 0:01.00\n 11    10 0:02.00\n 12    11 0:03.00\n 20     1 5:00.00\n";
        assert_eq!(tree_cpu_secs(10, ps), 6.0);
    }

    #[test]
    fn tree_cpu_total_keeps_what_exited_children_used() {
        let mut total = TreeCpu::default();
        // A build: the shell is idle while short-lived compilers come and go.
        assert!((total.sample(10, " 10 1 0:00.10\n 11 10 0:04.00\n") - 4.1).abs() < 1e-9);
        assert!((total.sample(10, " 10 1 0:00.10\n 12 10 0:03.00\n") - 7.1).abs() < 1e-9);
        // A reused pid starts again from a lower reading: the old one still counts.
        assert!((total.sample(10, " 10 1 0:00.10\n 12 10 0:01.00\n") - 8.1).abs() < 1e-9);
        assert!((total.sample(10, " 10 1 0:00.10\n") - 8.1).abs() < 1e-9);
        // Flat when nothing runs, so a hung job still goes idle.
        assert!((total.sample(10, " 10 1 0:00.10\n") - 8.1).abs() < 1e-9);
    }

    #[test]
    fn idle_needs_a_full_window_of_low_cpu() {
        let w = 600;
        assert!(!idle_over_window(&[(0, 0.0), (300, 0.1)], 300, w, 0.01));
        assert!(idle_over_window(&[(0, 5.0), (300, 5.1), (600, 5.2)], 600, w, 0.01));
        assert!(!idle_over_window(&[(0, 5.0), (600, 12.0)], 600, w, 0.01));
    }

    #[test]
    fn json_escape_basic() {
        assert_eq!(json_escape(r#"a"b\c"#), r#"a\"b\\c"#);
        assert_eq!(json_escape("a\tb\nc\rd"), "a b c d");
    }

    #[test]
    fn truncate_info_line() {
        let long = "x".repeat(250);
        assert_eq!(
            format_info_line("lab", 1, "12:00:00", std::slice::from_ref(&long)).len(),
            200
        );
    }

    #[test]
    fn parse_sample() {
        let s = r#"{"chipC":55,"thermal":"nominal"}"#;
        let g = parse_glance_sample(s);
        assert_eq!(g.chip_c, Some(55));
        assert_eq!(g.thermal.as_deref(), Some("nominal"));

        let g2 = parse_glance_sample("{}");
        assert!(g2.chip_c.is_none());
        assert!(g2.thermal.is_none());

        let g3 = parse_glance_sample("garbage chipC:abc thermal:BOOM");
        assert!(g3.chip_c.is_none());
    }

    #[test]
    fn parse_limit_digits() {
        assert_eq!(parse_limit(Some("2"), 3), 2);
        assert_eq!(parse_limit(Some("x"), 3), 3);
        assert_eq!(parse_limit(Some(""), 3), 3);
    }

    #[test]
    fn cmd_for_log_trailing_space() {
        assert_eq!(format_cmd_for_log(&["a".into(), "b".into()]), "a b ");
    }
}
