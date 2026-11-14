use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

static TEST_LOCK: Mutex<()> = Mutex::new(());
static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_agent-mail"))
}

fn temp_dirs() -> (PathBuf, PathBuf) {
    let n = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let state = std::env::temp_dir().join(format!("agent-mail-state-{}-{}", std::process::id(), n));
    let config =
        std::env::temp_dir().join(format!("agent-mail-config-{}-{}", std::process::id(), n));
    fs::create_dir_all(&state).unwrap();
    fs::create_dir_all(&config).unwrap();
    fs::create_dir_all(state.join("home")).unwrap();
    fs::write(config.join("name"), "testme\n").unwrap();
    (state, config)
}

fn write_fake_ssh(dir: &Path, exit_code: i32) -> PathBuf {
    let script = dir.join("fake-ssh.sh");
    let log = dir.join("ssh-log.txt");
    let body = format!(
        r#"#!/bin/sh
LOG={log}
echo "ARGV:$*" >> "$LOG"
cat >> "$LOG"
exit {exit_code}
"#,
        log = log.display(),
        exit_code = exit_code
    );
    fs::write(&script, body).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    }
    script
}

fn run_mail(
    state: &Path,
    config: &Path,
    ssh: Option<&Path>,
    args: &[&str],
    stdin: Option<&str>,
) -> (i32, String, String) {
    let _guard = TEST_LOCK.lock().unwrap();
    let mut cmd = Command::new(bin());
    cmd.env("HOME", state.join("home"))
        .env("AGENT_MAIL_SSH", "/usr/bin/false")
        .env_remove("AGENT_MAIL_SESSION")
        .env_remove("AGENT_MAIL_ROLE")
        .env("AGENT_MAIL_DIR", state)
        .env("AGENT_MAIL_CONFIG", config);
    if let Some(s) = ssh {
        cmd.env("AGENT_MAIL_SSH", s);
    }
    cmd.args(args).stdout(Stdio::piped()).stderr(Stdio::piped());
    if stdin.is_some() {
        cmd.stdin(Stdio::piped());
    }
    let mut child = cmd.spawn().expect("spawn agent-mail");
    if let Some(data) = stdin
        && let Some(mut sin) = child.stdin.take()
    {
        let _ = sin.write_all(data.as_bytes());
    }
    let out = child.wait_with_output().expect("wait");
    (
        out.status.code().unwrap_or(1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn send_local_appends_inbox() {
    let (state, config) = temp_dirs();
    let (code, stdout, _) = run_mail(&state, &config, None, &["send", "local", "hello"], None);
    assert_eq!(code, 0);
    let id = stdout.trim();
    assert!(!id.is_empty());
    let inbox = fs::read_to_string(state.join("inbox.jsonl")).unwrap();
    assert!(inbox.contains("hello"));
    assert!(inbox.contains(id));
    let _ = fs::remove_dir_all(&state);
    let _ = fs::remove_dir_all(&config);
}

#[test]
fn send_remote_success_uses_ssh() {
    let (state, config) = temp_dirs();
    fs::write(config.join("hosts"), "worker1\n").unwrap();
    let fake = write_fake_ssh(&state, 0);
    let (code, _, _) = run_mail(
        &state,
        &config,
        Some(&fake),
        &["send", "worker1", "remote hi"],
        None,
    );
    assert_eq!(code, 0);
    let log = fs::read_to_string(state.join("ssh-log.txt")).unwrap();
    assert!(log.contains("--"));
    assert!(log.contains("worker1"));
    assert!(log.contains("remote hi"));
    let _ = fs::remove_dir_all(&state);
    let _ = fs::remove_dir_all(&config);
}

#[test]
fn send_remote_failure_queues_outbox() {
    let (state, config) = temp_dirs();
    let fake = write_fake_ssh(&state, 1);
    let (code, _, stderr) = run_mail(
        &state,
        &config,
        Some(&fake),
        &["send", "worker1", "queued"],
        None,
    );
    assert_eq!(code, 0);
    assert!(stderr.contains("queued for the relay"));
    let outbox = fs::read_to_string(state.join("outbox.jsonl")).unwrap();
    assert!(outbox.contains("\"to\":\"worker1\""));
    let _ = fs::remove_dir_all(&state);
    let _ = fs::remove_dir_all(&config);
}

#[test]
fn unread_hook_marks_and_dedups_read_txt() {
    let (state, config) = temp_dirs();
    let old = r#"{"id":"msg1","at":"2026-01-01 00:00:00","from":"other","project":"","text":"hi"}"#;
    let new = r#"{"id":"msg2","at":"2026-01-02 00:00:00","from":"other","project":"","text":"yo"}"#;
    fs::write(state.join("inbox.jsonl"), format!("{old}\n{new}\n")).unwrap();
    fs::write(state.join("read.txt"), "msg1\n").unwrap();

    // A message already read by any session is not replayed to a new one.
    let hook = r#"{"session_id":"sess-a","cwd":"/tmp"}"#;
    let (code, stdout, _) = run_mail(&state, &config, None, &["unread", "--hook"], Some(hook));
    assert_eq!(code, 0);
    assert!(!stdout.contains("(msg1)"), "read mail replayed: {stdout}");
    assert!(stdout.contains("(msg2)"));
    assert!(stdout.contains("1 unread"));
    let read = fs::read_to_string(state.join("read.txt")).unwrap();
    assert_eq!(read.matches("msg1\n").count(), 1);
    assert_eq!(read.matches("msg2\n").count(), 1);
    assert!(fs::metadata(state.join("read-sess-a.txt")).is_ok());

    // Once delivered, a second session sees nothing.
    let hook_b = r#"{"session_id":"sess-b","cwd":"/tmp"}"#;
    let (code, stdout, _) = run_mail(&state, &config, None, &["unread", "--hook"], Some(hook_b));
    assert_eq!(code, 0);
    assert!(stdout.is_empty(), "second session got mail: {stdout}");
    let _ = fs::remove_dir_all(&state);
    let _ = fs::remove_dir_all(&config);
}

#[test]
fn relay_delivers_valid_to_and_secures_bad() {
    let (state, config) = temp_dirs();
    fs::write(config.join("hosts"), "worker1 lab\n").unwrap();

    let fake = state.join("relay-ssh.sh");
    let log = state.join("relay-ssh-log.txt");
    let script = format!(
        r#"#!/bin/sh
LOG={log}
echo "ARGV:$*" >> "$LOG"
if echo "$*" | grep -q 'outbox.jsonl'; then
  echo '{{"id":"x1","at":"t","from":"forged","project":"","text":"m","to":"testme"}}'
  echo '{{"id":"x2","at":"t","from":"a","project":"","text":"m","to":"-oProxyCommand=evil"}}'
  echo '{{"id":"x3","at":"t","from":"a","project":"","text":"m","to":"m9"}}'
fi
cat >> "$LOG"
exit 0
"#,
        log = log.display()
    );
    fs::write(&fake, script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
    }

    let (code, _, _) = run_mail(&state, &config, Some(&fake), &["relay"], None);
    assert_eq!(code, 0);

    let inbox = fs::read_to_string(state.join("inbox.jsonl")).unwrap();
    assert!(inbox.contains("\"from\":\"worker1\""));
    assert!(inbox.contains("claimed_from"));
    assert!(inbox.contains("forged"));

    let undel = fs::read_to_string(state.join("undeliverable.jsonl")).unwrap();
    assert!(undel.contains("-oProxyCommand=evil"));
    assert!(undel.contains("m9"));

    let _ = fs::remove_dir_all(&state);
    let _ = fs::remove_dir_all(&config);
}

#[test]
fn usage_exit_2() {
    let (state, config) = temp_dirs();
    let (code, stdout, _) = run_mail(&state, &config, None, &["nope"], None);
    assert_eq!(code, 2);
    assert!(stdout.contains("#   agent-mail unread"));
    assert!(stdout.contains("{"));
    let _ = fs::remove_dir_all(&state);
    let _ = fs::remove_dir_all(&config);
}

fn inbox_line(id: &str, project: &str, text: &str) -> String {
    format!(
        r#"{{"id":"{id}","at":"2026-01-01 00:00:00","from":"other","project":"{project}","text":"{text}"}}"#
    )
}

#[test]
fn provider_receipts_do_not_clobber() {
    let (state, config) = temp_dirs();
    fs::write(
        state.join("inbox.jsonl"),
        format!("{}\n", inbox_line("msgp", "", "shared")),
    )
    .unwrap();

    let (code, stdout, _) = run_mail(
        &state,
        &config,
        None,
        &["unread", "--provider", "claude", "--mark"],
        None,
    );
    assert_eq!(code, 0);
    assert!(
        stdout.contains("(msgp)"),
        "claude did not see the message: {stdout}"
    );

    let (code, stdout, _) = run_mail(
        &state,
        &config,
        None,
        &["unread", "--provider", "codex"],
        None,
    );
    assert_eq!(code, 0);
    assert!(
        stdout.contains("(msgp)"),
        "claude receipt hid the message from codex: {stdout}"
    );

    let (code, stdout, _) = run_mail(
        &state,
        &config,
        None,
        &["unread", "--provider", "codex", "--mark"],
        None,
    );
    assert_eq!(code, 0);
    assert!(
        stdout.contains("(msgp)"),
        "codex mark did not list the message: {stdout}"
    );

    let receipts = fs::read_to_string(state.join("receipts.jsonl")).unwrap_or_default();
    assert!(
        receipts.contains("\"reader\":\"claude:") && receipts.contains("@testme\""),
        "claude reader identity missing: {receipts}"
    );
    assert!(
        receipts.contains("\"reader\":\"codex:") && receipts.matches("@testme\"").count() >= 2,
        "codex receipt clobbered or missing: {receipts}"
    );
    assert!(
        !fs::read_to_string(state.join("read.txt"))
            .unwrap_or_default()
            .contains("msgp"),
        "provider marks wrote the legacy read file and would hide mail across providers"
    );

    let (code, stdout, _) = run_mail(&state, &config, None, &["status", "local", "msgp"], None);
    assert_eq!(code, 0);
    assert!(
        stdout.contains("msgp read"),
        "status did not count a project read: {stdout}"
    );
    assert!(
        stdout.contains("claude:"),
        "status omitted claude: {stdout}"
    );
    assert!(stdout.contains("codex:"), "status omitted codex: {stdout}");

    let (code, stdout, _) = run_mail(
        &state,
        &config,
        None,
        &["unread", "--provider", "codex"],
        None,
    );
    assert_eq!(code, 0);
    assert!(stdout.is_empty(), "codex saw the message again: {stdout}");

    let _ = fs::remove_dir_all(&state);
    let _ = fs::remove_dir_all(&config);
}

#[test]
fn legacy_read_txt_still_honoured() {
    let (state, config) = temp_dirs();
    fs::write(
        state.join("inbox.jsonl"),
        format!(
            "{}\n{}\n",
            inbox_line("msg1", "", "old"),
            inbox_line("msg2", "", "new")
        ),
    )
    .unwrap();
    fs::write(state.join("read.txt"), "msg1\n").unwrap();

    let (code, stdout, _) = run_mail(&state, &config, None, &["unread"], None);
    assert_eq!(code, 0);
    assert!(
        !stdout.contains("(msg1)"),
        "legacy read.txt was ignored: {stdout}"
    );
    assert!(
        stdout.contains("(msg2)"),
        "unmarked message was hidden: {stdout}"
    );

    let (code, stdout, _) = run_mail(&state, &config, None, &["status", "local", "msg1"], None);
    assert_eq!(code, 0);
    assert!(
        stdout.contains("msg1 read"),
        "legacy receipt not read: {stdout}"
    );
    assert!(
        stdout.contains("legacy@testme"),
        "status did not name the legacy reader: {stdout}"
    );

    let _ = fs::remove_dir_all(&state);
    let _ = fs::remove_dir_all(&config);
}

#[test]
fn snippet_includes_provider_instruction() {
    let (state, config) = temp_dirs();
    let (code, stdout, _) = run_mail(&state, &config, None, &["snippet"], None);
    assert_eq!(code, 0, "snippet failed");
    assert!(
        stdout.contains(
            "at session start and before finishing, run `agent-mail unread --provider codex`"
        ),
        "instruction block missing: {stdout}"
    );
    assert!(
        stdout.contains("--provider cursor"),
        "cursor check missing: {stdout}"
    );
    assert!(
        stdout.contains("--provider other --json"),
        "generic check missing: {stdout}"
    );
    assert!(stdout.contains("agent-mail wait"), "wait missing: {stdout}");
    let _ = fs::remove_dir_all(&state);
    let _ = fs::remove_dir_all(&config);
}

#[test]
fn unread_json_for_other_provider() {
    let (state, config) = temp_dirs();
    fs::write(
        state.join("inbox.jsonl"),
        format!("{}\n", inbox_line("msgj", "", "yo")),
    )
    .unwrap();
    let (code, stdout, _) = run_mail(
        &state,
        &config,
        None,
        &["unread", "--provider", "other", "--json"],
        None,
    );
    assert_eq!(code, 0);
    assert!(
        !stdout.contains("📬"),
        "json mode printed the banner: {stdout}"
    );
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect(&stdout);
    assert_eq!(v["unread"][0]["id"], "msgj");
    assert_eq!(v["unread"][0]["text"], "yo");
    let reader = v["reader"].as_str().unwrap_or("");
    assert!(reader.starts_with("other:"), "reader {reader}");
    assert!(reader.ends_with("@testme"), "reader {reader}");
    assert!(
        !state.join("receipts.jsonl").exists(),
        "json listing marked the message read"
    );
    let _ = fs::remove_dir_all(&state);
    let _ = fs::remove_dir_all(&config);
}

#[test]
fn role_is_part_of_the_address_not_the_provider() {
    let (state, config) = temp_dirs();
    let (code, _, _) = run_mail(
        &state,
        &config,
        None,
        &["send", "local::reviewer", "for reviewers"],
        None,
    );
    assert_eq!(code, 0);
    let inbox = fs::read_to_string(state.join("inbox.jsonl")).unwrap();
    assert!(
        inbox.contains("\"project\":\"\""),
        "project was rewritten: {inbox}"
    );
    assert!(
        inbox.contains("\"role\":\"reviewer\""),
        "role missing: {inbox}"
    );
    assert!(
        !inbox.contains("codex") && !inbox.contains("claude"),
        "provider leaked into the message: {inbox}"
    );

    let (code, stdout, _) = run_mail(&state, &config, None, &["unread"], None);
    assert_eq!(code, 0);
    assert!(
        !stdout.contains("for reviewers"),
        "role-addressed mail shown to everyone: {stdout}"
    );

    let (code, stdout, _) = run_mail(
        &state,
        &config,
        None,
        &["unread", "--role", "reviewer"],
        None,
    );
    assert_eq!(code, 0);
    assert!(
        stdout.contains("for reviewers"),
        "role holder did not see the message: {stdout}"
    );
    let _ = fs::remove_dir_all(&state);
    let _ = fs::remove_dir_all(&config);
}

#[test]
fn wait_returns_on_new_inbox_message() {
    let (state, config) = temp_dirs();
    let mut cmd = Command::new(bin());
    cmd.env("HOME", state.join("home"))
        .env("AGENT_MAIL_SSH", "/usr/bin/false")
        .env_remove("AGENT_MAIL_SESSION")
        .env_remove("AGENT_MAIL_ROLE")
        .env("AGENT_MAIL_DIR", &state)
        .env("AGENT_MAIL_CONFIG", &config)
        .args(["wait", "--timeout", "5"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("spawn wait");
    let started = std::time::Instant::now();
    std::thread::sleep(std::time::Duration::from_millis(400));
    fs::write(
        state.join("inbox.jsonl"),
        format!("{}\n", inbox_line("w1", "", "arrived")),
    )
    .unwrap();
    let out = loop {
        if let Some(status) = child.try_wait().unwrap() {
            let mut stdout = Vec::new();
            if let Some(mut pipe) = child.stdout.take() {
                use std::io::Read;
                let _ = pipe.read_to_end(&mut stdout);
            }
            break (
                status.code().unwrap_or(1),
                String::from_utf8_lossy(&stdout).into_owned(),
            );
        }
        if started.elapsed() > std::time::Duration::from_secs(8) {
            let _ = child.kill();
            let _ = child.wait();
            panic!("wait did not return after a new inbox line");
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    assert_eq!(out.0, 0, "wait exit {}", out.0);
    assert!(out.1.contains("arrived"), "wait output: {}", out.1);
    assert!(out.1.contains("(w1)"), "wait output: {}", out.1);
    let _ = fs::remove_dir_all(&state);
    let _ = fs::remove_dir_all(&config);
}

#[test]
fn install_is_idempotent_and_uninstall_reverses_it() {
    let (state, config) = temp_dirs();
    let settings = state.join("settings.json");
    let cursor = state.join("cursor-hooks.json");
    fs::write(&settings, "{}\n").unwrap();
    fs::write(config.join("name"), "testme\n").unwrap();

    let run = |args: &[&str]| {
        let _guard = TEST_LOCK.lock().unwrap();
        let mut cmd = Command::new(bin());
        cmd.env("HOME", state.join("home"))
            .env("AGENT_MAIL_SSH", "/usr/bin/false")
            .env_remove("AGENT_MAIL_SESSION")
            .env_remove("AGENT_MAIL_ROLE")
            .env("AGENT_MAIL_DIR", &state)
            .env("AGENT_MAIL_CONFIG", &config)
            .env("AGENT_MAIL_SETTINGS", &settings)
            .env("AGENT_MAIL_CURSOR_HOOKS", &cursor)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let out = cmd.output().expect("spawn");
        (
            out.status.code().unwrap_or(1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    };

    let (code, _, _) = run(&["install", "desk"]);
    assert_eq!(code, 0);
    assert_eq!(fs::read_to_string(config.join("name")).unwrap(), "desk\n");
    let body = fs::read_to_string(&settings).unwrap();
    assert_eq!(body.matches("unread --hook").count(), 2);

    let (code, _, _) = run(&["install", "--provider", "claude"]);
    assert_eq!(code, 0);
    assert_eq!(fs::read_to_string(config.join("name")).unwrap(), "desk\n");
    let body = fs::read_to_string(&settings).unwrap();
    assert_eq!(body.matches("unread --hook").count(), 2);

    let codex_hooks = state.join("home/.codex/hooks.json");
    let codex_before = fs::metadata(&codex_hooks)
        .ok()
        .and_then(|m| m.modified().ok());
    let (code, stdout, _) = run(&["install", "--provider", "codex"]);
    assert_eq!(code, 0, "codex install failed");
    assert!(
        stdout.contains("SessionStart"),
        "codex hook snippet missing: {stdout}"
    );
    assert!(
        stdout.contains("\"Stop\""),
        "codex Stop hook missing: {stdout}"
    );
    assert!(
        stdout.contains("unread --hook --provider codex"),
        "codex command missing: {stdout}"
    );
    assert!(
        stdout.contains("/hooks"),
        "trust review note missing: {stdout}"
    );
    assert!(
        stdout.contains(
            "at session start and before finishing, run `agent-mail unread --provider codex`"
        ),
        "fallback instruction missing: {stdout}"
    );
    assert!(
        config.join("snippets/codex-hooks.json").is_file(),
        "snippet was not emitted under the config dir"
    );
    let codex_after = fs::metadata(&codex_hooks)
        .ok()
        .and_then(|m| m.modified().ok());
    assert_eq!(
        codex_before, codex_after,
        "install edited ~/.codex/hooks.json"
    );

    let (code, _, _) = run(&["install", "--provider", "cursor"]);
    assert_eq!(code, 0);
    let (code, _, _) = run(&["install", "--provider", "cursor"]);
    assert_eq!(code, 0);
    let cursor_body = fs::read_to_string(&cursor).unwrap();
    assert_eq!(
        cursor_body
            .matches("unread --hook --provider cursor")
            .count(),
        3
    );
    assert!(cursor_body.contains("sessionStart"));
    assert!(cursor_body.contains("\"stop\""));
    assert!(cursor_body.contains("\"version\": 1") || cursor_body.contains("\"version\":1"));

    let (code, _, _) = run(&["uninstall", "--provider", "cursor"]);
    assert_eq!(code, 0);
    let cursor_body = fs::read_to_string(&cursor).unwrap_or_default();
    assert!(
        !cursor_body.contains("agent-mail"),
        "cursor hook remained: {cursor_body}"
    );

    let (code, _, _) = run(&["uninstall", "--provider", "codex"]);
    assert_eq!(code, 0);
    assert!(!config.join("snippets/codex-hooks.json").exists());

    let (code, _, _) = run(&["uninstall"]);
    assert_eq!(code, 0);
    let body = fs::read_to_string(&settings).unwrap();
    assert!(!body.contains("agent-mail"), "claude hook remained: {body}");

    let _ = fs::remove_dir_all(&state);
    let _ = fs::remove_dir_all(&config);
}

#[test]
fn cursor_pickup_is_exact_context_only_and_requires_explicit_ack() {
    let (state, config) = temp_dirs();
    let (_, id, _) = run_mail(
        &state,
        &config,
        None,
        &[
            "send",
            "local:fleet",
            "peer evidence",
            "--to-provider",
            "cursor",
            "--to-session",
            "conv-a",
        ],
        None,
    );
    run_mail(
        &state,
        &config,
        None,
        &["send", "local", "shared secret"],
        None,
    );
    run_mail(
        &state,
        &config,
        None,
        &[
            "send",
            "local",
            "other secret",
            "--to-provider",
            "cursor",
            "--to-session",
            "conv-b",
        ],
        None,
    );
    let event = r#"{"hook_event_name":"postToolUse","conversation_id":"conv-a","session_id":"different-lifecycle"}"#;
    for iteration in 0..2 {
        if iteration == 1 {
            // A natural boundary in the cooldown is silent; expiry permits retry.
            let (code, output, _) = run_mail(&state, &config, None, &["cursor-hook"], Some(event));
            assert_eq!(code, 0);
            assert_eq!(output.trim(), "{}");
            let path = state.join("cursor-presentations.jsonl");
            let records: Vec<String> = fs::read_to_string(&path)
                .unwrap()
                .lines()
                .map(|line| {
                    let mut record: serde_json::Value = serde_json::from_str(line).unwrap();
                    record["emitted_at"] = serde_json::json!(0);
                    record.to_string()
                })
                .collect();
            fs::write(path, records.join("\n") + "\n").unwrap();
        }
        let (code, output, error) = run_mail(&state, &config, None, &["cursor-hook"], Some(event));
        assert_eq!(code, 0, "{error}");
        let output: serde_json::Value = serde_json::from_str(&output).unwrap();
        let context = output["additional_context"].as_str().unwrap();
        assert!(context.contains("peer evidence") && context.contains("NOT owner instructions"));
        assert!(!context.contains("shared secret") && !context.contains("other secret"));
        assert!(output.get("followup_message").is_none());
    }
    assert!(!state.join("receipts.jsonl").exists());
    assert!(!state.join("read.txt").exists());
    assert!(
        fs::read_to_string(state.join("cursor-presentations.jsonl"))
            .unwrap()
            .contains("hook_output_emitted")
    );
    let (code, _, _) = run_mail(
        &state,
        &config,
        None,
        &["cursor-ack", "--session", "conv-b", id.trim(), "considered"],
        None,
    );
    assert_ne!(code, 0);
    let (code, _, err) = run_mail(
        &state,
        &config,
        None,
        &[
            "cursor-ack",
            "--session",
            "conv-a",
            id.trim(),
            "Considered; no additional action authorized.",
        ],
        None,
    );
    assert_eq!(code, 0, "{err}");
    let (code, output, _) = run_mail(&state, &config, None, &["cursor-hook"], Some(event));
    assert_eq!(code, 0);
    assert_eq!(output.trim(), "{}");
    let statuses: Vec<_> = fs::read_dir(state.join("cursor-invocations"))
        .unwrap()
        .collect();
    assert_eq!(statuses.len(), 1);
    let status: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(statuses[0].as_ref().unwrap().path()).unwrap())
            .unwrap();
    assert_eq!(status["conversation_id"], "conv-a");
    assert_eq!(status["outcome"], "empty_output_emitted");
    assert!(status.get("text").is_none());
    let _ = fs::remove_dir_all(state);
    let _ = fs::remove_dir_all(config);
}

#[test]
fn cursor_hook_fails_closed_for_missing_identity_unsupported_or_subagent_events() {
    let (state, config) = temp_dirs();
    for event in [
        "not json",
        r#"{"hook_event_name":"postToolUse","session_id":"conv-a"}"#,
        r#"{"hook_event_name":"stop","conversation_id":"conv-a"}"#,
        r#"{"hook_event_name":"postToolUse","conversation_id":"../bad"}"#,
        r#"{"hook_event_name":"postToolUse","conversation_id":"conv-a","parent_tool_call_id":"tool"}"#,
    ] {
        let (code, output, _) = run_mail(&state, &config, None, &["cursor-hook"], Some(event));
        assert_ne!(code, 0);
        assert!(output.is_empty());
    }
    assert!(!state.join("cursor-presentations.jsonl").exists());
    assert!(!state.join("receipts.jsonl").exists());
    let _ = fs::remove_dir_all(state);
    let _ = fs::remove_dir_all(config);
}

#[test]
fn cursor_hook_keeps_oversized_mail_at_source_without_ack() {
    let (state, config) = temp_dirs();
    let text = "a".repeat(9500);
    run_mail(
        &state,
        &config,
        None,
        &[
            "send",
            "local",
            &text,
            "--to-provider",
            "cursor",
            "--to-session",
            "conv-a",
        ],
        None,
    );
    let event = r#"{"hook_event_name":"sessionStart","conversation_id":"conv-a"}"#;
    let (code, output, _) = run_mail(&state, &config, None, &["cursor-hook"], Some(event));
    assert_eq!(code, 0);
    assert_eq!(output.trim(), "{}");
    assert!(!state.join("receipts.jsonl").exists());
    assert!(
        fs::read_to_string(state.join("inbox.jsonl"))
            .unwrap()
            .contains(&text)
    );
    let _ = fs::remove_dir_all(state);
    let _ = fs::remove_dir_all(config);
}
