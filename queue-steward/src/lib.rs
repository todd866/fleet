mod focus;
mod jobs;
mod overrides;
mod pick;
mod registry;
mod score;
mod waiter;

pub use focus::{active_project, focus_guard_active, projects_root};
pub use jobs::median_expected_s;
pub use overrides::{OverrideEntry, append_override, parse_for_duration, read_overrides};
pub use pick::{ordered_waiters, pick_reason, pick_winner, tie_break};
pub use registry::{load_waiters, pid_alive, remove_waiter, write_waiter};
pub use score::{
    ScoreBreakdown, age_points, format_score, learned_points, origin_points, override_points,
    reason_for, score_all, score_waiter, short_points, waiting_minutes,
};
pub use waiter::Waiter;

use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

pub fn state_dir() -> PathBuf {
    if let Ok(d) = env::var("HEAVY_SLOT_DIR") {
        return PathBuf::from(d);
    }
    env::var("HOME")
        .map(|h| PathBuf::from(h).join(".local/state/heavy-slot"))
        .unwrap_or_else(|_| PathBuf::from(".local/state/heavy-slot"))
}

pub fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

pub fn detect_origin() -> String {
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

pub fn project_from_cwd() -> String {
    let Ok(cwd) = env::current_dir() else {
        return ".".to_string();
    };
    git_toplevel(&cwd)
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| {
            cwd.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| ".".to_string())
        })
}

fn git_toplevel(start: &Path) -> Option<PathBuf> {
    let mut cur = start.to_path_buf();
    loop {
        if cur.join(".git").exists() {
            return Some(cur);
        }
        if !cur.pop() {
            return None;
        }
    }
}

pub fn hostname() -> String {
    env::var("HOSTNAME")
        .or_else(|_| {
            Command::new("/bin/hostname")
                .output()
                .ok()
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .map(|s| s.trim().to_string())
                .ok_or(())
        })
        .unwrap_or_else(|_| "localhost".to_string())
}

pub fn cmd_pick(dir: &Path, now: i64) -> i32 {
    let Some((w, bd)) = pick_winner(dir, now) else {
        return 1;
    };
    let reason = pick_reason(&w, &bd, now);
    let mut stderr = io::stderr();
    let _ = writeln!(stderr, "steward: {} first — {reason}", w.label);
    let mut stdout = io::stdout();
    let _ = writeln!(stdout, "{}", w.pid);
    0
}

pub fn cmd_status(dir: &Path, now: i64) -> i32 {
    let scored = ordered_waiters(dir, now);
    if scored.is_empty() {
        return 1;
    }
    let active = active_project(now);
    for (rank, (w, bd)) in scored.iter().enumerate() {
        let mins = waiting_minutes(w.since, now);
        let reason = reason_for(bd, w, active.as_deref());
        println!(
            "  {}. {} · {} · {} · {} · waiting {mins}m · score {} — {reason}",
            rank + 1,
            w.label,
            w.project,
            w.origin,
            w.state,
            format_score(bd.total()),
        );
    }
    0
}

/// `processing N · transferring M · waiting K` for the live entries; exit 1 and no output when none.
pub fn counts_line(dir: &Path) -> Option<String> {
    let waiters = load_waiters(dir);
    if waiters.is_empty() {
        return None;
    }
    let n = |st: &str| waiters.iter().filter(|w| w.state == st).count();
    Some(format!(
        "processing {} · transferring {} · waiting {}",
        n("processing"),
        n("transferring"),
        n("waiting")
    ))
}

pub fn cmd_counts(dir: &Path) -> i32 {
    match counts_line(dir) {
        Some(l) => {
            println!("{l}");
            0
        }
        None => 1,
    }
}

pub fn cmd_explain(dir: &Path, now: i64) -> i32 {
    let waiters = load_waiters(dir);
    if waiters.is_empty() {
        return 1;
    }
    let active = active_project(now);
    let at_keyboard = focus_guard_active();
    let overrides = read_overrides(dir);
    let n = waiters.len();
    for w in &waiters {
        let bd = score_waiter(w, now, dir, n, active.as_deref(), at_keyboard, &overrides);
        println!(
            "{} pid {} · origin {} · focus {} · keyboard {} · short {} · age {} · override {} · learned {} · total {}",
            w.label,
            w.pid,
            format_score(bd.origin),
            format_score(bd.focus_project),
            format_score(bd.focus_keyboard),
            format_score(bd.short),
            format_score(bd.age),
            format_score(bd.override_bonus),
            format_score(bd.learned),
            format_score(bd.total()),
        );
    }
    0
}

fn find_waiter_by_label_or_pid(dir: &Path, target: &str) -> Option<Waiter> {
    if let Ok(pid) = target.parse::<i32>() {
        return load_waiters(dir).into_iter().find(|w| w.pid == pid);
    }
    load_waiters(dir).into_iter().find(|w| w.label == target)
}

pub fn cmd_bump(dir: &Path, target: &str, for_raw: Option<&str>, now: i64) -> i32 {
    let Some(waiter) = find_waiter_by_label_or_pid(dir, target) else {
        eprintln!("queue-steward: no waiter matching {target}");
        return 1;
    };
    let duration = for_raw.and_then(parse_for_duration).unwrap_or(2 * 3600);
    let (hour, weekday) = overrides::local_hour_weekday(now);
    let entry = OverrideEntry {
        at: now,
        label: waiter.label.clone(),
        project: waiter.project.clone(),
        until: now + duration as i64,
        hour,
        weekday,
    };
    if append_override(dir, &entry).is_err() {
        eprintln!("queue-steward: could not write override");
        return 1;
    }
    0
}

pub fn run(argv: Vec<String>) -> i32 {
    let mut args = argv;
    if !args.is_empty() {
        args.remove(0);
    }
    let dir = state_dir();
    fs::create_dir_all(&dir).ok();
    let now = unix_now();

    match args.first().map(String::as_str) {
        None => {
            eprintln!(
                "usage: queue-steward pick | status | counts | explain | bump <label|pid> [--for DURATION]"
            );
            2
        }
        Some("pick") => cmd_pick(&dir, now),
        Some("status") => cmd_status(&dir, now),
        Some("counts") => cmd_counts(&dir),
        Some("explain") => cmd_explain(&dir, now),
        Some("bump") => {
            let target = match args.get(1) {
                Some(t) => t.as_str(),
                None => {
                    eprintln!("usage: queue-steward bump <label|pid> [--for 2h]");
                    return 2;
                }
            };
            let mut for_dur = None;
            let mut i = 2;
            while i < args.len() {
                if args[i] == "--for" {
                    for_dur = args.get(i + 1).map(String::as_str);
                    break;
                }
                i += 1;
            }
            cmd_bump(&dir, target, for_dur, now)
        }
        Some(cmd) => {
            eprintln!("queue-steward: unknown command {cmd}");
            2
        }
    }
}

/// Resolve the queue-steward binary for heavy-slot and other callers.
pub fn steward_bin() -> PathBuf {
    if let Ok(p) = env::var("QUEUE_STEWARD") {
        return PathBuf::from(p);
    }
    env::var("HOME")
        .map(|h| PathBuf::from(h).join(".local/bin/queue-steward"))
        .unwrap_or_else(|_| PathBuf::from(".local/bin/queue-steward"))
}

/// Run `pick`; returns None if the steward is unavailable or the queue is empty.
pub fn pick_pid_external(dir: &Path) -> Option<i32> {
    let bin = steward_bin();
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
    let line = String::from_utf8_lossy(&out.stdout);
    line.lines()
        .next()
        .and_then(|l| l.trim().parse::<i32>().ok())
}

/// Status lines for heavy-slot (without leading newline); empty if no waiters or steward missing.
pub fn status_lines_external(dir: &Path) -> Option<String> {
    let bin = steward_bin();
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::overrides::{OverrideEntry, append_override};
    use std::fs;

    fn temp_dir() -> PathBuf {
        let n = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let d = env::temp_dir().join(format!("qs-test-{n}"));
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn w(pid: i32, label: &str, project: &str, origin: &str, since: i64) -> Waiter {
        Waiter {
            pid,
            label: label.to_string(),
            project: project.to_string(),
            origin: origin.to_string(),
            cmd: "cmd".to_string(),
            expected_s: None,
            since,
            host: "test".to_string(),
            state: "waiting".to_string(),
        }
    }

    #[test]
    fn origin_scoring() {
        assert_eq!(origin_points("interactive"), 3.0);
        assert_eq!(origin_points("agent"), 2.0);
        assert_eq!(origin_points("scheduled"), 0.0);
    }

    #[test]
    fn short_scoring_only_with_two_waiters() {
        assert_eq!(short_points(Some(60), 1), 0.0);
        assert_eq!(short_points(Some(60), 2), 1.0);
        assert_eq!(short_points(Some(2000), 2), -1.0);
        assert_eq!(short_points(None, 2), 0.0);
    }

    #[test]
    fn age_scoring_cap() {
        assert_eq!(age_points(0, 299), 0.0);
        assert_eq!(age_points(0, 300), 0.5);
        assert_eq!(age_points(0, 3600), 3.0);
        assert_eq!(age_points(0, 7200), 3.0);
    }

    #[test]
    fn override_wins_and_expires() {
        let dir = temp_dir();
        let now = 1_000_000_i64;
        let waiter = w(100, "lab", "proj", "agent", now);
        let overrides = vec![OverrideEntry {
            at: now - 10,
            label: "lab".to_string(),
            project: "proj".to_string(),
            until: now + 100,
            hour: 12,
            weekday: 1,
        }];
        assert_eq!(override_points(&waiter, &overrides, now), 10.0);
        assert_eq!(override_points(&waiter, &overrides, now + 200), 0.0);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn learned_bucket_weekday() {
        let now = 1_700_000_000_i64;
        let (hour, wday) = overrides::local_hour_weekday(now);
        let waiter = w(1, "l", "myproj", "agent", now);
        let same_bucket = vec![OverrideEntry {
            at: now - 86400,
            label: "x".to_string(),
            project: "myproj".to_string(),
            until: now - 80000,
            hour,
            weekday: wday,
        }];
        assert_eq!(learned_points(&waiter, &same_bucket, now), 1.0);

        let other_bucket = vec![OverrideEntry {
            at: now - 86400,
            label: "x".to_string(),
            project: "myproj".to_string(),
            until: now - 80000,
            hour: (hour + 6) % 24,
            weekday: wday,
        }];
        if overrides::hour_bucket(hour) != overrides::hour_bucket((hour + 6) % 24) {
            assert_eq!(learned_points(&waiter, &other_bucket, now), 0.0);
        }

        let other_project = vec![OverrideEntry {
            at: now - 86400,
            label: "x".to_string(),
            project: "other".to_string(),
            until: now - 80000,
            hour,
            weekday: wday,
        }];
        assert_eq!(learned_points(&waiter, &other_project, now), 0.0);
        let _ = hour;
    }

    #[test]
    fn stale_pid_removed_on_load() {
        let dir = temp_dir();
        let dead = 999_999_999_i32;
        let path = Waiter::queue_path(&dir, dead);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            r#"{"pid":999999999,"label":"x","project":"p","origin":"agent","cmd":"c","expected_s":null,"since":1,"host":"h"}"#,
        )
        .unwrap();
        assert!(load_waiters(&dir).is_empty());
        assert!(!path.exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn ties_by_since_ordering() {
        let bd = ScoreBreakdown {
            origin: 2.0,
            focus_project: 0.0,
            focus_keyboard: 0.0,
            short: 0.0,
            age: 0.0,
            override_bonus: 0.0,
            learned: 0.0,
        };
        let wa = w(1, "a", "p", "agent", 100);
        let wb = w(2, "b", "p", "agent", 200);
        let mut scored = vec![(wb, bd.clone()), (wa, bd)];
        tie_break(&mut scored);
        assert_eq!(scored[0].0.since, 100);
    }

    #[test]
    fn learned_from_jsonl_integration() {
        let dir = temp_dir();
        let now = 1_700_000_000_i64;
        let (hour, wday) = overrides::local_hour_weekday(now);
        append_override(
            &dir,
            &OverrideEntry {
                at: now - 1000,
                label: "l".to_string(),
                project: "fleet".to_string(),
                until: now + 1000,
                hour,
                weekday: wday,
            },
        )
        .unwrap();
        let overrides = read_overrides(&dir);
        let waiter = w(1, "l", "fleet", "agent", now);
        assert_eq!(learned_points(&waiter, &overrides, now), 1.0);
        let other = w(2, "l", "other", "agent", now);
        assert_eq!(learned_points(&other, &overrides, now), 0.0);
        let _ = fs::remove_dir_all(&dir);
    }
}
