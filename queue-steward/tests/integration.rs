use queue_steward::{Waiter, load_waiters, score_waiter, write_waiter};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_state() -> PathBuf {
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let d = std::env::temp_dir().join(format!("qs-int-{n}"));
    fs::create_dir_all(&d).unwrap();
    d
}

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_queue-steward"))
}

fn run_qs(dir: &PathBuf, extra: &[(&str, &str)], args: &[&str]) -> (i32, String, String) {
    let mut cmd = Command::new(bin());
    cmd.env("HEAVY_SLOT_DIR", dir);
    for (k, v) in extra {
        cmd.env(k, v);
    }
    cmd.args(args);
    let out = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    (
        out.status.code().unwrap_or(1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn alive_waiter(_dir: &Path, label: &str, project: &str, origin: &str, since: i64) -> Waiter {
    let pid = std::process::id() as i32;
    Waiter {
        pid,
        label: label.to_string(),
        project: project.to_string(),
        origin: origin.to_string(),
        cmd: "test".to_string(),
        expected_s: None,
        since,
        host: "test".to_string(),
        state: "waiting".to_string(),
    }
}

#[test]
fn transferring_is_never_picked_and_counts_are_right() {
    let dir = temp_state();
    let now = queue_steward::unix_now();
    let pid = std::process::id() as i32;
    // An interactive transferring job would outscore the waiting agent job, but it is not asking
    // for a slot yet.
    let mut t = alive_waiter(&dir, "mover", "proj", "interactive", now - 60);
    t.state = "transferring".to_string();
    t.pid = pid; // alive
    write_waiter(&dir, &t).unwrap();
    // A second alive pid for the waiting entry: the parent of the test process.
    let ppid = unsafe { libc::getppid() };
    let mut w = alive_waiter(&dir, "worker", "proj", "agent", now - 10);
    w.pid = ppid;
    write_waiter(&dir, &w).unwrap();
    let (code, out, _) = run_qs(&dir, &[], &["pick"]);
    assert_eq!(code, 0);
    assert_eq!(
        out.trim(),
        ppid.to_string(),
        "waiting entry wins over transferring"
    );
    let (_, out, _) = run_qs(&dir, &[], &["counts"]);
    assert_eq!(out.trim(), "processing 0 · transferring 1 · waiting 1");
    let (_, out, _) = run_qs(&dir, &[], &["status"]);
    assert!(
        out.contains("· transferring ·") && out.contains("· waiting ·"),
        "{out}"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn counts_exit_1_when_empty_and_old_entries_default_to_waiting() {
    let dir = temp_state();
    let (code, out, _) = run_qs(&dir, &[], &["counts"]);
    assert_eq!((code, out.as_str()), (1, ""));
    let pid = std::process::id() as i32;
    fs::create_dir_all(dir.join("queue")).unwrap();
    fs::write(
        dir.join("queue").join(format!("{pid}.json")),
        format!("{{\"pid\":{pid},\"label\":\"old\",\"project\":\"p\",\"origin\":\"agent\",\"cmd\":\"x\",\"expected_s\":null,\"since\":1,\"host\":\"h\"}}\n"),
    )
    .unwrap();
    let (_, out, _) = run_qs(&dir, &[], &["counts"]);
    assert_eq!(out.trim(), "processing 0 · transferring 0 · waiting 1");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn pick_orders_by_origin_score() {
    let dir = temp_state();
    let now = queue_steward::unix_now();
    let pid = std::process::id() as i32;
    write_waiter(
        &dir,
        &Waiter {
            pid,
            label: "low".to_string(),
            project: "p".to_string(),
            origin: "scheduled".to_string(),
            cmd: "c".to_string(),
            expected_s: None,
            since: now,
            host: "h".to_string(),
            state: "waiting".to_string(),
        },
    )
    .unwrap();
    // Second entry must be a different live pid — use parent shell isn't available.
    // Score in-process with two waiters sharing our pid isn't possible; use explain path
    // with manual ordering via two files where both use current pid — only one file per pid.
    // Spawn a sleeping child and use its pid.
    let mut child = Command::new("/bin/sleep").arg("30").spawn().unwrap();
    let child_pid = child.id() as i32;
    write_waiter(
        &dir,
        &Waiter {
            pid: child_pid,
            label: "high".to_string(),
            project: "p".to_string(),
            origin: "interactive".to_string(),
            cmd: "c".to_string(),
            expected_s: None,
            since: now,
            host: "h".to_string(),
            state: "waiting".to_string(),
        },
    )
    .unwrap();
    let (code, stdout, stderr) = run_qs(&dir, &[], &["pick"]);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(stdout.trim(), child_pid.to_string());
    assert!(stderr.contains("high first"));
    child.kill().ok();
    child.wait().ok();
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn status_and_explain_commands() {
    let dir = temp_state();
    let now = queue_steward::unix_now();
    write_waiter(&dir, &alive_waiter(&dir, "lab", "proj", "agent", now - 120)).unwrap();
    let (code, stdout, _) = run_qs(&dir, &[], &["status"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("lab · proj · agent"));
    assert!(stdout.contains("waiting 2m"));
    let (code2, out2, _) = run_qs(&dir, &[], &["explain"]);
    assert_eq!(code2, 0);
    assert!(out2.contains("origin 2"));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn bump_override_affects_pick() {
    let dir = temp_state();
    let now = queue_steward::unix_now();
    let mut low = Command::new("/bin/sleep").arg("30").spawn().unwrap();
    let mut high = Command::new("/bin/sleep").arg("30").spawn().unwrap();
    let pid_low = low.id() as i32;
    let pid_high = high.id() as i32;
    write_waiter(
        &dir,
        &Waiter {
            pid: pid_high,
            label: "winner".to_string(),
            project: "alpha".to_string(),
            origin: "interactive".to_string(),
            cmd: "c".to_string(),
            expected_s: None,
            since: now,
            host: "h".to_string(),
            state: "waiting".to_string(),
        },
    )
    .unwrap();
    write_waiter(
        &dir,
        &Waiter {
            pid: pid_low,
            label: "loser".to_string(),
            project: "beta".to_string(),
            origin: "interactive".to_string(),
            cmd: "c".to_string(),
            expected_s: None,
            since: now - 1000,
            host: "h".to_string(),
            state: "waiting".to_string(),
        },
    )
    .unwrap();
    let (code, _, _) = run_qs(&dir, &[], &["bump", "loser", "--for", "1h"]);
    assert_eq!(code, 0);
    let (code, stdout, _) = run_qs(&dir, &[], &["pick"]);
    assert_eq!(code, 0);
    assert_eq!(stdout.trim(), pid_low.to_string());
    low.kill().ok();
    high.kill().ok();
    low.wait().ok();
    high.wait().ok();
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn active_project_via_env() {
    let projects = temp_state();
    let repo = projects.join("active-repo");
    fs::create_dir_all(repo.join(".git")).unwrap();
    fs::write(repo.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
    let dir = temp_state();
    let now = queue_steward::unix_now();
    let mut child = Command::new("/bin/sleep").arg("20").spawn().unwrap();
    let cpid = child.id() as i32;
    write_waiter(
        &dir,
        &Waiter {
            pid: cpid,
            label: "match".to_string(),
            project: "active-repo".to_string(),
            origin: "agent".to_string(),
            cmd: "c".to_string(),
            expected_s: None,
            since: now,
            host: "h".to_string(),
            state: "waiting".to_string(),
        },
    )
    .unwrap();
    let bd = score_waiter(
        &load_waiters(&dir)[0],
        now,
        &dir,
        1,
        Some("active-repo"),
        false,
        &[],
    );
    assert_eq!(bd.focus_project, 3.0);
    let _ = (
        run_qs(
            &dir,
            &[("QUEUE_STEWARD_PROJECTS", projects.to_str().unwrap())],
            &["pick"],
        ),
        child.kill(),
    );
    child.wait().ok();
    let _ = fs::remove_dir_all(&dir);
    let _ = fs::remove_dir_all(&projects);
}
