use fleet_run::{Context, Entry, Refusal, authorize, launchctl_pid, parse, sha256_file};
use std::path::{Path, PathBuf};
use std::process::Command;

const H: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn entries() -> Vec<Entry> {
    parse(
        &format!("# comment\ncom.example.job\t~/jobs/run.py\t{H}\t/opt/homebrew/bin/python3\t~/jobs/run.py\t--quiet\n"),
        Path::new("/home/someone"),
    )
}

fn argv(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

fn good_argv() -> Vec<String> {
    argv(&["/opt/homebrew/bin/python3", "/home/someone/jobs/run.py", "--quiet"])
}

fn check(argv: &[String], ppid: i32, label: Option<&str>, is_job: bool, hash: &str) -> Result<Entry, Refusal> {
    let es = entries();
    let h = hash.to_string();
    let job = move |_: &str| is_job;
    let hf = move |_: &Path| Ok(h.clone());
    let cx = Context { argv, ppid, label, is_job_process: &job, hash: &hf };
    authorize(&es, &cx).cloned()
}

#[test]
fn the_exact_allowlisted_job_is_allowed() {
    let e = check(&good_argv(), 1, Some("com.example.job"), true, H).unwrap();
    assert_eq!(e.hashed, PathBuf::from("/home/someone/jobs/run.py"));
}

#[test]
fn an_empty_command_is_refused() {
    assert_eq!(check(&[], 1, Some("com.example.job"), true, H), Err(Refusal::NoCommand));
}

#[test]
fn a_path_resolved_command_is_refused() {
    let a = argv(&["python3", "/home/someone/jobs/run.py", "--quiet"]);
    assert!(matches!(check(&a, 1, Some("com.example.job"), true, H), Err(Refusal::NotAbsolute(_))));
}

#[test]
fn a_parent_other_than_launchd_is_refused() {
    assert_eq!(check(&good_argv(), 4242, Some("com.example.job"), true, H), Err(Refusal::NotUnderLaunchd(4242)));
}

#[test]
fn a_missing_label_is_refused() {
    assert_eq!(check(&good_argv(), 1, None, true, H), Err(Refusal::NoLabel));
    assert_eq!(check(&good_argv(), 1, Some(""), true, H), Err(Refusal::NoLabel));
}

#[test]
fn a_label_not_in_the_list_is_refused() {
    assert!(matches!(check(&good_argv(), 1, Some("com.example.other"), true, H), Err(Refusal::LabelNotAllowed(_))));
}

#[test]
fn extra_or_different_arguments_are_refused() {
    let mut a = good_argv();
    a.push("--evil".into());
    assert!(matches!(check(&a, 1, Some("com.example.job"), true, H), Err(Refusal::ArgvMismatch(_))));
    let b = argv(&["/opt/homebrew/bin/python3", "-c", "print(1)"]);
    assert!(matches!(check(&b, 1, Some("com.example.job"), true, H), Err(Refusal::ArgvMismatch(_))));
}

#[test]
fn a_process_launchd_does_not_list_as_the_job_is_refused() {
    assert!(matches!(check(&good_argv(), 1, Some("com.example.job"), false, H), Err(Refusal::NotTheJobProcess(_))));
}

#[test]
fn a_changed_script_is_refused() {
    let other = "f".repeat(64);
    assert!(matches!(check(&good_argv(), 1, Some("com.example.job"), true, &other), Err(Refusal::HashMismatch { .. })));
}

#[test]
fn malformed_and_relative_lines_grant_nothing() {
    let text = format!(
        "a\tjobs/x.py\t{H}\t/usr/bin/python3\tjobs/x.py\n\
         b\t/abs/x.py\tnothex\t/usr/bin/python3\t/abs/x.py\n\
         c\t/abs/x.py\t{H}\tpython3\t/abs/x.py\n\
         d\t/abs/x.py\n"
    );
    assert!(parse(&text, Path::new("/h")).is_empty());
}

#[test]
fn sha256_of_a_file() {
    let d = std::env::temp_dir().join(format!("fleet-run-test-{}", std::process::id()));
    std::fs::write(&d, b"abc").unwrap();
    assert_eq!(sha256_file(&d).unwrap(), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    let _ = std::fs::remove_file(&d);
    assert!(matches!(sha256_file(Path::new("/nonexistent/x")), Err(Refusal::Unreadable(_))));
}

#[test]
fn launchctl_print_pid_is_parsed() {
    assert_eq!(launchctl_pid("gui/501/x = {\n\tactive count = 1\n\tpid = 1234\n\tstate = running\n"), Some(1234));
    assert_eq!(launchctl_pid("state = not running\n"), None);
}

#[test]
fn the_binary_refuses_a_command_started_outside_launchd() {
    // The test runner is not launchd, so even /usr/bin/true is refused with 77 and never runs.
    let st = Command::new(env!("CARGO_BIN_EXE_fleet-run")).arg("/usr/bin/true").env("XPC_SERVICE_NAME", "x").status().unwrap();
    assert_eq!(st.code(), Some(77));
}

#[test]
fn no_command_is_a_usage_error() {
    assert_eq!(Command::new(env!("CARGO_BIN_EXE_fleet-run")).status().unwrap().code(), Some(64));
}
