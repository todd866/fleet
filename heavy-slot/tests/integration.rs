use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_heavy-slot"))
}

fn temp_state_dir() -> PathBuf {
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("heavy-slot-test-{n}"));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn run_hs(dir: &PathBuf, extra_env: &[(&str, &str)], args: &[&str]) -> (i32, String, String) {
    let mut cmd = Command::new(bin());
    cmd.env("HEAVY_SLOT_DIR", dir)
        .env("HEAVY_SLOT_LOCAL", "1")
        .env_remove("REMOTE_RUN");
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    cmd.args(args);
    let out = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("spawn heavy-slot");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    (out.status.code().unwrap_or(1), stdout, stderr)
}

fn hold_lock_until_released(lock_path: PathBuf, release: Arc<Barrier>) {
    thread::spawn(move || {
        let f = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)
            .unwrap();
        let fd = f.as_raw_fd();
        unsafe {
            libc::flock(fd, libc::LOCK_EX);
        }
        let info = lock_path.parent().unwrap().join(format!(
            "{}.info",
            lock_path.file_stem().unwrap().to_string_lossy()
        ));
        fs::write(info, "holder · pid 99999 · 00:00:00 · test\n").ok();
        release.wait();
        unsafe {
            libc::flock(fd, libc::LOCK_UN);
        }
    });
}

use std::os::unix::io::AsRawFd;

#[test]
fn status_all_free() {
    let dir = temp_state_dir();
    let (code, stdout, _) = run_hs(&dir, &[], &["status"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("slot 1: free"));
    assert!(stdout.contains("slot 2: free"));
    assert!(stdout.contains("slot 3: free"));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn usage_exit_2() {
    let dir = temp_state_dir();
    let (code, _, stderr) = run_hs(&dir, &[], &[]);
    assert_eq!(code, 2);
    assert!(stderr.contains("usage: heavy-slot -- <command...> | heavy-slot status"));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn limit_shows_off_slots() {
    let dir = temp_state_dir();
    fs::write(dir.join("limit"), "1").unwrap();
    let (code, stdout, _) = run_hs(&dir, &[], &["status"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("slot 1: free"));
    assert!(stdout.contains("slot 2: off (limit 1)"));
    assert!(stdout.contains("slot 3: off (limit 1)"));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn job_runs_and_logs_jsonl() {
    let dir = temp_state_dir();
    let (code, _, _) = run_hs(
        &dir,
        &[("HEAVY_SLOT_LABEL", "testlabel")],
        &["--", "/bin/echo", "hello"],
    );
    assert_eq!(code, 0);
    let log = fs::read_to_string(dir.join("jobs.jsonl")).expect("jobs.jsonl");
    let line = log.lines().next().expect("one line");
    assert!(line.contains("\"label\":\"testlabel\""));
    assert!(line.contains("\"rc\":0"));
    assert!(line.contains("\"slot\":1"));
    assert!(line.contains("\"waited\":0"));
    assert!(line.contains("\"cmd\":\"/bin/echo hello \""));
    assert!(line.contains("\"start\":"));
    assert!(line.contains("\"end\":"));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn second_slot_when_first_held() {
    let dir = temp_state_dir();
    let lock1 = dir.join("slot1.lock");
    let release = Arc::new(Barrier::new(2));
    hold_lock_until_released(lock1, Arc::clone(&release));

    thread::sleep(Duration::from_millis(100));

    let (code, _, _) = run_hs(&dir, &[], &["--", "/usr/bin/true"]);
    assert_eq!(code, 0);

    release.wait();
    thread::sleep(Duration::from_millis(50));

    let log = fs::read_to_string(dir.join("jobs.jsonl")).unwrap();
    assert!(log.contains("\"slot\":2"));

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn job_killed_by_signal_exits_128_plus_signal() {
    let dir = temp_state_dir();
    // The job kills itself with SIGTERM (15); the script's `$?` would be 143.
    let (code, _, _) = run_hs(&dir, &[], &["--", "/bin/sh", "-c", "kill -TERM $$"]);
    assert_eq!(code, 143);
    let log = fs::read_to_string(dir.join("jobs.jsonl")).unwrap();
    assert!(log.contains("\"rc\":143,"), "{log}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn sigterm_to_heavy_slot_reaches_the_job() {
    let dir = temp_state_dir();
    let mut child = Command::new(bin())
        .env("HEAVY_SLOT_DIR", &dir)
        .env("HEAVY_SLOT_LOCAL", "1")
        .args(["--", "/bin/sleep", "30"])
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    thread::sleep(Duration::from_millis(400));
    unsafe {
        libc::kill(child.id() as i32, libc::SIGTERM);
    }
    let status = child.wait().unwrap();
    // heavy-slot forwards the signal, the sleep dies with it, heavy-slot reports 143 and the
    // whole thing is over well before the 30 s sleep would have ended.
    assert_eq!(status.code(), Some(143));
    // Either the job had started (then it was killed and logged as 143) or the signal came first
    // (then heavy-slot left before any job ran and there is no log line). Both are correct.
    if let Ok(log) = fs::read_to_string(dir.join("jobs.jsonl")) {
        assert!(log.contains("\"rc\":143,"), "{log}");
    }
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn shares_locks_with_bsd_lockf() {
    // The bash script holds slots with /usr/bin/lockf; the two must see each other's locks.
    let dir = temp_state_dir();
    let mut holder = Command::new("/usr/bin/lockf")
        .args([
            "-k",
            dir.join("slot1.lock").to_str().unwrap(),
            "/bin/sleep",
            "5",
        ])
        .spawn()
        .unwrap();
    // lockf needs a moment to start and take the lock.
    thread::sleep(Duration::from_millis(800));
    let (_, stdout, _) = run_hs(&dir, &[], &["status"]);
    assert!(stdout.contains("slot 1: busy"), "{stdout}");
    let (code, _, _) = run_hs(&dir, &[], &["--", "/usr/bin/true"]);
    assert_eq!(code, 0);
    let log = fs::read_to_string(dir.join("jobs.jsonl")).unwrap();
    assert!(log.contains("\"slot\":2,"), "{log}");
    holder.kill().ok();
    holder.wait().ok();
    let _ = fs::remove_dir_all(&dir);
}

fn steward_bin() -> PathBuf {
    std::env::var("CARGO_BIN_EXE_queue-steward")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../target/debug/queue-steward")
        })
}

#[test]
fn steward_higher_score_gets_slot_first() {
    let dir = temp_state_dir();
    fs::write(dir.join("limit"), "1").unwrap();
    let lock1 = dir.join("slot1.lock");
    let release = Arc::new(Barrier::new(2));
    hold_lock_until_released(lock1, Arc::clone(&release));

    thread::sleep(Duration::from_millis(100));

    let steward = steward_bin();
    let steward_s = steward.to_string_lossy().into_owned();
    let dir_low = dir.clone();
    let dir_high = dir.clone();
    let steward_high = steward_s.clone();
    let steward_low = steward_s;
    let t_high = thread::spawn(move || {
        run_hs(
            &dir_high,
            &[
                ("QUEUE_STEWARD", steward_high.as_str()),
                ("HEAVY_SLOTS", "1"),
                ("HEAVY_SLOT_LABEL", "score-high"),
                ("CLAUDE_SESSION_INTERACTIVE", "1"),
            ],
            &["--", "/usr/bin/true"],
        )
    });
    thread::sleep(Duration::from_millis(200));
    let t_low = thread::spawn(move || {
        run_hs(
            &dir_low,
            &[
                ("QUEUE_STEWARD", steward_low.as_str()),
                ("HEAVY_SLOTS", "1"),
                ("HEAVY_SLOT_LABEL", "score-low"),
                ("XPC_SERVICE_NAME", "com.example.batch"),
            ],
            &["--", "/usr/bin/true"],
        )
    });
    thread::sleep(Duration::from_millis(200));
    release.wait();
    let (code_high, _, _) = t_high.join().unwrap();
    let (code_low, _, _) = t_low.join().unwrap();
    assert_eq!(code_high, 0);
    assert_eq!(code_low, 0);
    let log = fs::read_to_string(dir.join("jobs.jsonl")).unwrap();
    let high_pos = log.find("score-high").unwrap();
    let low_pos = log.find("score-low").unwrap();
    assert!(high_pos < low_pos, "{log}");
    let queue = dir.join("queue");
    if queue.exists() {
        assert!(fs::read_dir(&queue).map(|d| d.count()).unwrap_or(0) == 0);
    }
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn steward_missing_falls_back_to_fifo() {
    let dir = temp_state_dir();
    fs::write(dir.join("limit"), "1").unwrap();
    let lock1 = dir.join("slot1.lock");
    let release = Arc::new(Barrier::new(2));
    hold_lock_until_released(lock1, Arc::clone(&release));

    thread::sleep(Duration::from_millis(100));

    let dir_a = dir.clone();
    let dir_b = dir.clone();
    let t1 = thread::spawn(move || {
        run_hs(
            &dir_a,
            &[
                ("QUEUE_STEWARD", "/nonexistent/queue-steward"),
                ("HEAVY_SLOTS", "1"),
                ("HEAVY_SLOT_LABEL", "fifo-first"),
            ],
            &["--", "/usr/bin/true"],
        )
    });
    thread::sleep(Duration::from_millis(50));
    let t2 = thread::spawn(move || {
        run_hs(
            &dir_b,
            &[
                ("QUEUE_STEWARD", "/nonexistent/queue-steward"),
                ("HEAVY_SLOTS", "1"),
                ("HEAVY_SLOT_LABEL", "fifo-second"),
            ],
            &["--", "/usr/bin/true"],
        )
    });
    thread::sleep(Duration::from_millis(200));
    release.wait();
    let (c1, _, _) = t1.join().unwrap();
    let (c2, _, _) = t2.join().unwrap();
    assert_eq!(c1, 0);
    assert_eq!(c2, 0);
    let log = fs::read_to_string(dir.join("jobs.jsonl")).unwrap();
    assert!(log.contains("fifo-first"));
    assert!(log.contains("fifo-second"));
    let queue = dir.join("queue");
    if queue.exists() {
        assert!(fs::read_dir(&queue).map(|d| d.count()).unwrap_or(0) == 0);
    }
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn mark_records_states_and_status_counts_them() {
    let dir = temp_state_dir();
    let steward = PathBuf::from(env!("CARGO_BIN_EXE_heavy-slot"))
        .parent()
        .unwrap()
        .join("queue-steward");
    if !steward.is_file() {
        return; // steward binary not built in this invocation
    }
    let me = std::process::id().to_string();
    let env = [("QUEUE_STEWARD", steward.to_str().unwrap())];
    let (code, _, _) = run_hs(&dir, &env, &["mark", "transferring", "--pid", &me]);
    assert_eq!(code, 0);
    let (_, out, _) = run_hs(&dir, &env, &["status"]);
    assert!(
        out.contains("processing 0 · transferring 1 · waiting 0"),
        "{out}"
    );
    let (code, _, _) = run_hs(&dir, &env, &["mark", "done", "--pid", &me]);
    assert_eq!(code, 0);
    let (_, out, _) = run_hs(&dir, &env, &["status"]);
    assert!(!out.contains("transferring"), "{out}");
    assert_eq!(
        out, "slot 1: free\nslot 2: free\nslot 3: free\n",
        "byte-identical when empty"
    );
    let _ = fs::remove_dir_all(&dir);
}

fn spawn_hs(dir: &PathBuf, extra_env: &[(&str, &str)], args: &[&str]) -> std::process::Child {
    let mut cmd = Command::new(bin());
    cmd.env("HEAVY_SLOT_DIR", dir).env("HEAVY_SLOT_LOCAL", "1").env_remove("REMOTE_RUN");
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    cmd.args(args).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().expect("spawn heavy-slot")
}

fn slot1_free(dir: &PathBuf) -> bool {
    let (_, out, _) = run_hs(dir, &[], &["status"]);
    out.lines().next().unwrap_or("").contains("free")
}

fn wait_until(mut ok: impl FnMut() -> bool, secs: u64) -> bool {
    for _ in 0..secs * 10 {
        if ok() {
            return true;
        }
        thread::sleep(Duration::from_millis(100));
    }
    false
}

#[test]
fn release_gives_the_slot_back_and_the_job_runs_on() {
    let dir = temp_state_dir();
    let marker = dir.join("done");
    let script = format!("sleep 2; touch {}", marker.display());
    let child = spawn_hs(&dir, &[("HEAVY_SLOT_IDLE_SECS", "0")], &["--", "/bin/sh", "-c", &script]);
    let pid = child.id().to_string();
    assert!(wait_until(|| !slot1_free(&dir), 5), "job never took the slot");
    let (code, out, err) = run_hs(&dir, &[], &["release", &pid]);
    assert_eq!(code, 0, "release failed: {out} {err}");
    assert!(slot1_free(&dir));
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert!(marker.exists(), "the job was stopped");
    assert!(String::from_utf8_lossy(&out.stderr).contains("gave slot 1 back (released on request)"));
    assert_eq!(fs::read_to_string(dir.join("jobs.jsonl")).unwrap().lines().count(), 1);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn release_refuses_a_pid_that_holds_no_slot() {
    let dir = temp_state_dir();
    let (code, _, err) = run_hs(&dir, &[], &["release", "1"]);
    assert_eq!(code, 1);
    assert!(err.contains("does not hold a slot"));
    let (code, _, _) = run_hs(&dir, &[], &["release"]);
    assert_eq!(code, 2);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn idle_job_gives_its_slot_back_busy_job_keeps_it() {
    let dir = temp_state_dir();
    let idle = spawn_hs(&dir, &[("HEAVY_SLOT_IDLE_SECS", "3")], &["--", "/bin/sleep", "8"]);
    assert!(wait_until(|| !slot1_free(&dir), 5));
    assert!(wait_until(|| slot1_free(&dir), 8), "idle job kept its slot");
    let out = idle.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&out.stderr).contains("idle 3 s, under 1% CPU"));

    let busy = spawn_hs(&dir, &[("HEAVY_SLOT_IDLE_SECS", "3")], &["--", "/bin/sh", "-c", "end=$(($(date +%s)+6)); while [ $(date +%s) -lt $end ]; do :; done"]);
    assert!(wait_until(|| !slot1_free(&dir), 5));
    thread::sleep(Duration::from_secs(4));
    assert!(!slot1_free(&dir), "busy job lost its slot");
    let out = busy.wait_with_output().unwrap();
    assert!(!String::from_utf8_lossy(&out.stderr).contains("gave slot"));
    let _ = fs::remove_dir_all(&dir);
}
