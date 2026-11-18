use std::process::Command;
use std::time::{Duration, Instant};

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_fleet-run"))
}

#[test]
fn passes_the_childs_exit_code_through() {
    let st = bin().args(["/bin/sh", "-c", "exit 7"]).status().unwrap();
    assert_eq!(st.code(), Some(7));
}

#[test]
fn passes_arguments_and_output_through_unchanged() {
    let out = bin().args(["/bin/echo", "a b", "c"]).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "a b c\n");
}

#[test]
fn a_missing_command_is_127() {
    let st = bin().arg("/nonexistent/fleet-run-test").status().unwrap();
    assert_eq!(st.code(), Some(127));
}

#[test]
fn no_command_is_a_usage_error() {
    assert_eq!(bin().status().unwrap().code(), Some(64));
}

#[test]
fn sigterm_reaches_the_child_and_its_status_comes_back() {
    // The child traps TERM and exits 42, so 42 proves the signal was forwarded rather than
    // fleet-run dying on its own.
    let mut c = bin()
        .args(["/bin/sh", "-c", "trap 'exit 42' TERM; while :; do sleep 0.05; done"])
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(300));
    unsafe { libc::kill(c.id() as i32, libc::SIGTERM) };
    let t = Instant::now();
    let st = c.wait().unwrap();
    assert_eq!(st.code(), Some(42));
    assert!(t.elapsed() < Duration::from_secs(5));
}

#[test]
fn a_child_killed_by_a_signal_is_128_plus_n() {
    let st = bin().args(["/bin/sh", "-c", "kill -KILL $$"]).status().unwrap();
    assert_eq!(st.code(), Some(128 + 9));
}

#[test]
fn it_waits_for_the_child_instead_of_exec_ing_it() {
    // After an exec the pid would be the child's; here the child prints its parent's pid,
    // which must be fleet-run's own pid.
    let c = bin().args(["/bin/sh", "-c", "echo $PPID"]).stdout(std::process::Stdio::piped()).spawn().unwrap();
    let pid = c.id();
    let out = c.wait_with_output().unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), pid.to_string());
}
