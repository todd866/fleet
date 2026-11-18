//! fleet-run CMD [ARGS...]: run CMD as a child and wait for it.
//!
//! macOS privacy controls (TCC: Full Disk Access, Desktop, removable volumes...) charge a file
//! access to the *responsible* process. Under launchd that is the job's own program. Measured
//! on macOS 27.2 (18 Nov 2026): when the job's program is an Apple binary such as /bin/zsh, a
//! third-party child (Homebrew python or rsync, nvm node, GNU timeout) is charged to itself,
//! so every interpreter upgrade or rebuild is a new identity with no grant. A Developer
//! ID-signed launcher keeps the responsibility for its whole tree, third-party children
//! included. Grant Full Disk Access to this one binary once; sign it with a fixed identifier
//! (`fleet-install`) and rebuilds keep the grant.
//!
//! Never exec: after an exec the process is CMD and macOS charges CMD again. SIGTERM, SIGINT,
//! SIGHUP and SIGQUIT are passed to the child; the exit status is the child's (128+N when a
//! signal ended it). 127: CMD not found; 126: CMD could not be started.
use std::os::unix::process::ExitStatusExt;
use std::process::{Command, exit};
use std::sync::atomic::{AtomicI32, Ordering};

static CHILD: AtomicI32 = AtomicI32::new(0);
static PENDING: AtomicI32 = AtomicI32::new(0);
const FORWARDED: [libc::c_int; 4] = [libc::SIGTERM, libc::SIGINT, libc::SIGHUP, libc::SIGQUIT];

extern "C" fn forward(sig: libc::c_int) {
    let pid = CHILD.load(Ordering::SeqCst);
    if pid > 0 {
        unsafe { libc::kill(pid, sig) };
    } else {
        PENDING.store(sig, Ordering::SeqCst);
    }
}

fn usage() -> ! {
    eprintln!("usage: fleet-run CMD [ARGS...]   run CMD as a child, under this binary's privacy grants");
    exit(64)
}

fn main() {
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    let Some(cmd) = args.first() else { usage() };
    match cmd.to_str() {
        Some("-h" | "--help") => usage(),
        Some("--version") => {
            println!("fleet-run {}", env!("CARGO_PKG_VERSION"));
            return;
        }
        _ => {}
    }
    // Handlers go in before the spawn, so a signal in between is not lost: it is held and
    // sent once the child exists.
    for sig in FORWARDED {
        unsafe { libc::signal(sig, forward as *const () as libc::sighandler_t) };
    }
    let mut child = match Command::new(cmd).args(&args[1..]).spawn() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("fleet-run: {}: {e}", cmd.to_string_lossy());
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
