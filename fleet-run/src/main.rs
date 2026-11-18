//! fleet-run CMD [ARGS...]: run an allowlisted launchd job's command as a child and wait for it.
//!
//! macOS privacy controls (TCC) charge a file access to the *responsible* process; under launchd
//! that is the job's program. Measured on macOS 27.2 (18 Nov 2026, W29): an Apple program such as
//! /bin/zsh does not keep responsibility for a third-party child (Homebrew python or rsync, nvm
//! node, GNU timeout), while a Developer ID-signed launcher keeps it for the whole tree. Signed
//! with a fixed identifier (`fleet-install`), rebuilds keep the grant.
//!
//! Because a granted launcher would otherwise lend Full Disk Access to any caller, fleet-run runs
//! only what its compiled allowlist names (see build.rs and lib.rs): the exact command line, for
//! the launchd job whose label is set, when this process is that job (parent launchd, and
//! `launchctl print` lists this pid), and the pinned file's SHA-256 matches. Anything else exits
//! 77 and appends a line to ~/.local/state/fleet-run/refusals.jsonl; the job's failed exit shows on
//! the morning board.
//!
//! Never exec: after an exec the process is CMD and macOS charges CMD again. SIGTERM, SIGINT,
//! SIGHUP and SIGQUIT are passed to the child; the exit status is the child's (128+N for a signal).
use fleet_run::{Context, authorize, launchctl_pid, parse, sha256_file};
use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::{Command, exit};
use std::sync::atomic::{AtomicI32, Ordering};

mod allow {
    include!(concat!(env!("OUT_DIR"), "/allow.rs"));
}

static CHILD: AtomicI32 = AtomicI32::new(0);
static PENDING: AtomicI32 = AtomicI32::new(0);
const FORWARDED: [libc::c_int; 4] = [libc::SIGTERM, libc::SIGINT, libc::SIGHUP, libc::SIGQUIT];
const REFUSED: i32 = 77;

extern "C" fn forward(sig: libc::c_int) {
    let pid = CHILD.load(Ordering::SeqCst);
    if pid > 0 {
        unsafe { libc::kill(pid, sig) };
    } else {
        PENDING.store(sig, Ordering::SeqCst);
    }
}

/// The account's home from the password database: $HOME is the caller's to set.
fn account_home() -> PathBuf {
    unsafe {
        let pw = libc::getpwuid(libc::getuid());
        if pw.is_null() || (*pw).pw_dir.is_null() {
            return PathBuf::from("/nonexistent");
        }
        PathBuf::from(std::ffi::CStr::from_ptr((*pw).pw_dir).to_string_lossy().into_owned())
    }
}

fn is_job_process(label: &str) -> bool {
    let uid = unsafe { libc::getuid() };
    let out = Command::new("/bin/launchctl").arg("print").arg(format!("gui/{uid}/{label}")).output();
    match out {
        Ok(o) if o.status.success() => launchctl_pid(&String::from_utf8_lossy(&o.stdout)) == Some(std::process::id() as i32),
        _ => false,
    }
}

fn record_refusal(home: &PathBuf, label: &str, why: &str, argv: &[String]) {
    eprintln!("fleet-run: refused: {why}");
    let dir = home.join(".local/state/fleet-run");
    let _ = std::fs::create_dir_all(&dir);
    let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n");
    let line = format!(
        "{{\"t\":{},\"label\":\"{}\",\"why\":\"{}\",\"argv\":[{}]}}\n",
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
        esc(label),
        esc(why),
        argv.iter().map(|a| format!("\"{}\"", esc(a))).collect::<Vec<_>>().join(",")
    );
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("refusals.jsonl")) {
        let _ = f.write_all(line.as_bytes());
    }
}

fn main() {
    let argv: Vec<String> = std::env::args_os().skip(1).map(|a| a.to_string_lossy().into_owned()).collect();
    match argv.first().map(String::as_str) {
        None | Some("-h" | "--help") => {
            eprintln!("usage: fleet-run CMD [ARGS...]   (launchd jobs in the compiled allowlist only)");
            exit(64);
        }
        Some("--version") => {
            println!("fleet-run {}", env!("CARGO_PKG_VERSION"));
            return;
        }
        Some("--list") => {
            for e in parse(allow::ALLOWLIST, &account_home()) {
                println!("{}\t{}\t{}\t{}", e.label, e.hashed.display(), e.sha256, e.argv.join(" "));
            }
            return;
        }
        _ => {}
    }
    let home = account_home();
    let entries = parse(allow::ALLOWLIST, &home);
    let label = std::env::var("XPC_SERVICE_NAME").ok();
    let cx = Context {
        argv: &argv,
        ppid: unsafe { libc::getppid() },
        label: label.as_deref(),
        is_job_process: &is_job_process,
        hash: &sha256_file,
    };
    if let Err(r) = authorize(&entries, &cx) {
        record_refusal(&home, label.as_deref().unwrap_or(""), &r.to_string(), &argv);
        exit(REFUSED);
    }
    for sig in FORWARDED {
        unsafe { libc::signal(sig, forward as *const () as libc::sighandler_t) };
    }
    let mut child = match Command::new(&argv[0]).args(&argv[1..]).spawn() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("fleet-run: {}: {e}", argv[0]);
            exit(if e.kind() == std::io::ErrorKind::NotFound { 127 } else { 126 });
        }
    };
    CHILD.store(child.id() as i32, Ordering::SeqCst);
    let held = PENDING.swap(0, Ordering::SeqCst);
    if held != 0 {
        unsafe { libc::kill(child.id() as i32, held) };
    }
    let status = match child.wait() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("fleet-run: wait: {e}");
            exit(1);
        }
    };
    exit(status.code().unwrap_or_else(|| 128 + status.signal().unwrap_or(0)));
}
