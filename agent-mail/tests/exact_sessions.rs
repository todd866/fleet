use serde_json::{Value, json};
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

static SEQ: AtomicU64 = AtomicU64::new(0);
struct Fixture {
    root: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "agent-mail-session-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        for dir in ["state", "config", "home", "project"] {
            fs::create_dir_all(root.join(dir)).unwrap();
        }
        fs::write(root.join("config/name"), "m5\n").unwrap();
        Self { root }
    }
    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_agent-mail"));
        cmd.current_dir(self.root.join("project"))
            .env("HOME", self.root.join("home"))
            .env("AGENT_MAIL_DIR", self.root.join("state"))
            .env("AGENT_MAIL_CONFIG", self.root.join("config"))
            // No real SSH, hooks, or shared state in these tests.
            .env("AGENT_MAIL_SSH", "/usr/bin/false")
            .env_remove("AGENT_MAIL_SESSION")
            .env_remove("AGENT_MAIL_ROLE")
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        cmd
    }
    fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().unwrap()
    }
    fn write(&self, name: &str, text: &str) {
        fs::write(self.root.join("state").join(name), text).unwrap();
    }
    fn read(&self, name: &str) -> String {
        fs::read_to_string(self.root.join("state").join(name)).unwrap_or_default()
    }
    fn send(&self, host: &str, session: &str) -> String {
        let out = self.run(&[
            "send",
            host,
            "one session",
            "--to-provider",
            "cursor",
            "--to-session",
            session,
        ]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().into()
    }
    fn unread(&self, provider: &str, session: Option<&str>, mark: bool) -> Value {
        let mut args = vec!["unread", "--provider", provider, "--json"];
        if let Some(s) = session {
            args.extend(["--session", s]);
        }
        if mark {
            args.push("--mark");
        }
        let out = self.run(&args);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}
fn receipt(id: &str, reader: &str) -> String {
    format!("{}\n", json!({"id":id,"reader":reader,"project":"project"}))
}
fn targeted(id: &str, session: &str) -> Value {
    json!({"id":id,"project":"project","text":"exact", "to_provider":"cursor", "to_session":session, "role":format!("__agent_mail_session_v1__:cursor:{session}")})
}

#[test]
fn two_cursor_sessions_same_project_are_isolated() {
    let f = Fixture::new();
    let a = f.send("m5:project", "session-a");
    let b = f.send("local:project", "session-b");
    let a_mail = f.unread("cursor", Some("session-a"), true);
    assert_eq!(a_mail["unread"].as_array().unwrap().len(), 1);
    assert_eq!(a_mail["unread"][0]["id"], a);
    let b_mail = f.unread("cursor", Some("session-b"), false);
    assert_eq!(b_mail["unread"].as_array().unwrap().len(), 1);
    assert_eq!(b_mail["unread"][0]["id"], b);
    assert_eq!(
        f.unread("cursor", Some("session-a"), false)["unread"],
        json!([])
    );
    assert!(!f.read("read.txt").contains(&a));
}

#[test]
fn wrong_provider_session_and_missing_session_cannot_receive_or_mark() {
    let f = Fixture::new();
    f.send("local:project", "session-a");
    for (p, s) in [
        ("codex", Some("session-a")),
        ("cursor", Some("session-b")),
        ("cursor", None),
    ] {
        assert_eq!(f.unread(p, s, true)["unread"], json!([]));
    }
    assert!(f.read("receipts.jsonl").is_empty());
    assert_eq!(
        f.unread("cursor", Some("session-a"), false)["unread"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn targeted_mail_survives_provider_and_legacy_receipts_but_not_exact_receipt() {
    let f = Fixture::new();
    let id = f.send("local:project", "session-a");
    f.write("read.txt", &format!("{id}\n"));
    f.write(
        "receipts.jsonl",
        &(receipt(&id, "cursor:session-b@m5")
            + &receipt(&id, "codex:session-a@m5")
            + &receipt(&id, "cursor:session-a@other")),
    );
    assert_eq!(
        f.unread("cursor", Some("session-a"), false)["unread"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        stdout(&f.run(&["status", "m5", &id])),
        format!("{id} unread\n")
    );
    f.unread("cursor", Some("session-a"), true);
    assert_eq!(
        stdout(&f.run(&["status", "local", &id])),
        format!("{id} read cursor:session-a@m5\n")
    );
    assert_eq!(
        f.unread("cursor", Some("session-a"), false)["unread"],
        json!([])
    );
}

#[test]
fn shared_mail_keeps_provider_wide_receipts_and_project_filter() {
    let f = Fixture::new();
    f.write(
        "inbox.jsonl",
        &format!(
            "{}\n{}\n",
            json!({"id":"shared","project":"project","text":"shared"}),
            json!({"id":"elsewhere","project":"other","text":"other"})
        ),
    );
    assert_eq!(
        f.unread("cursor", Some("session-a"), true)["unread"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        f.unread("cursor", Some("session-b"), false)["unread"],
        json!([])
    );
    assert_eq!(
        f.unread("codex", Some("session-b"), false)["unread"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn session_target_follows_reader_after_cd_but_preserves_role() {
    let f = Fixture::new();
    f.send("local:elsewhere", "session-a");
    assert_eq!(
        f.unread("cursor", Some("session-a"), false)["unread"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    f.send("local:elsewhere:reviewer", "session-a");
    assert_eq!(
        f.unread("cursor", Some("session-a"), false)["unread"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let out = f.run(&[
        "unread",
        "--provider",
        "cursor",
        "--session",
        "session-a",
        "--role",
        "reviewer",
        "--json",
    ]);
    assert_eq!(
        serde_json::from_slice::<Value>(&out.stdout).unwrap()["unread"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn explicit_hook_and_environment_session_sources_have_defined_precedence() {
    let f = Fixture::new();
    f.send("local:project", "session-a");
    let out = f
        .command(&["unread", "--provider", "cursor", "--json"])
        .env("AGENT_MAIL_SESSION", "session-a")
        .output()
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&out.stdout).unwrap()["unread"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    for key in [
        "session_id",
        "sessionId",
        "conversation_id",
        "conversationId",
    ] {
        let mut child = f
            .command(&["unread", "--provider", "cursor", "--hook", "--json"])
            .env("AGENT_MAIL_SESSION", "session-b")
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(
                json!({key:"session-a","cwd":f.root.join("project")})
                    .to_string()
                    .as_bytes(),
            )
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success());
        assert_eq!(
            serde_json::from_slice::<Value>(&out.stdout).unwrap()["unread"]
                .as_array()
                .unwrap()
                .len(),
            1,
            "hook key {key}"
        );
        f.write("receipts.jsonl", "");
    }
    let mut child = f
        .command(&[
            "unread",
            "--provider",
            "cursor",
            "--session",
            "session-a",
            "--hook",
            "--json",
        ])
        .env("AGENT_MAIL_SESSION", "session-b")
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(br#"{"conversation_id":"session-b"}"#)
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&out.stdout).unwrap()["unread"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn missing_or_malformed_hook_identity_does_not_receive_targeted_mail() {
    let f = Fixture::new();
    f.send("local:project", "session-a");
    for event in [
        "{}",
        "not JSON",
        r#"{"session_id":42}"#,
        r#"{"session_id":"../../outside"}"#,
        r#"{"session_id":""}"#,
    ] {
        let mut child = f
            .command(&["unread", "--provider", "cursor", "--hook", "--json"])
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(event.as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        if out.status.success() {
            assert_eq!(
                serde_json::from_slice::<Value>(&out.stdout).unwrap()["unread"],
                json!([])
            );
        } else {
            assert_eq!(out.status.code(), Some(2));
        }
        assert!(f.read("receipts.jsonl").is_empty());
    }
}

#[test]
fn partial_or_invalid_target_json_fails_closed() {
    let f = Fixture::new();
    let mut messages = Vec::new();
    for patch in [
        json!({"to_provider":"cursor"}),
        json!({"to_session":"session-a"}),
        json!({"to_provider":"cursor","to_session":null}),
        json!({"to_provider":"cursor","to_session":""}),
        json!({"to_provider":"bad","to_session":"session-a"}),
    ] {
        let mut m = json!({"id":"invalid","text":"must not display"});
        m.as_object_mut()
            .unwrap()
            .extend(patch.as_object().unwrap().clone());
        messages.push(m.to_string());
    }
    f.write("inbox.jsonl", &messages.join("\n"));
    assert_eq!(
        f.unread("cursor", Some("session-a"), true)["unread"],
        json!([])
    );
    assert!(f.read("receipts.jsonl").is_empty());
    assert_eq!(
        stdout(&f.run(&["status", "m5", "invalid"])),
        "invalid invalid recipient\n"
    );
}

#[test]
fn malformed_flags_never_send_or_consume_mail() {
    let f = Fixture::new();
    let invalid: &[&[&str]] = &[
        &["send", "local", "text", "--to-session", "a"],
        &["send", "local", "text", "--to-provider", "cursor"],
        &[
            "send",
            "local",
            "text",
            "--to-provider",
            "cursor",
            "--to-session",
        ],
        &[
            "send",
            "local",
            "text",
            "--to-provider",
            "cursor",
            "--to-session",
            "--mark",
        ],
        &[
            "send",
            "local",
            "text",
            "--to-provider",
            "cursor",
            "--to-session",
            "",
        ],
        &[
            "send",
            "local",
            "text",
            "--to-provider",
            "cursor",
            "--to-session",
            "a",
            "--to-session",
            "b",
        ],
        &[
            "send",
            "local",
            "text",
            "--provider",
            "cursor",
            "--session",
            "a",
        ],
        &["send", "-bad", "text"],
        &["unread", "--session"],
        &["unread", "--session", "--json"],
        &["unread", "--session", "../bad"],
        &["unread", "--session", "a", "--session", "b"],
        &["unread", "--role", "--mark"],
        &["unread", "--provider", "cursor", "--provider", "codex"],
    ];
    for args in invalid {
        let out = f.run(args);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {}", stdout(&out));
    }
    assert!(f.read("inbox.jsonl").is_empty());
    assert!(f.read("receipts.jsonl").is_empty());
}

#[test]
fn local_alias_status_and_sent_listing_do_not_use_ssh_or_claim_queue() {
    let f = Fixture::new();
    let id = f.send("m5:project", "session-a");
    for host in ["local", "m5"] {
        assert_eq!(
            stdout(&f.run(&["status", host, &id])),
            format!("{id} unread\n")
        );
        assert_eq!(stdout(&f.run(&["status", host])), format!("{id} unread\n"));
        assert_eq!(
            stdout(&f.run(&["status", host, "nonexistent"])),
            "nonexistent unknown\n"
        );
    }
}

#[test]
fn status_distinguishes_proven_queue_from_sent_and_unknown() {
    let f = Fixture::new();
    let out = f.run(&["send", "m1", "queued"]);
    let id = stdout(&out).trim().to_string();
    assert!(out.status.success());
    assert_eq!(
        stdout(&f.run(&["status", "m1", &id])),
        format!("{id} queued for the relay\n")
    );
    f.write("outbox.jsonl", "");
    assert_eq!(
        stdout(&f.run(&["status", "m1", &id])),
        format!("{id} sent (read status unknown: no route)\n")
    );
    assert_eq!(
        stdout(&f.run(&["status", "m1", "never-sent"])),
        "never-sent unknown\n"
    );
}

#[test]
fn inbox_and_outbox_write_failures_are_errors_without_sent_record() {
    for path in ["inbox.jsonl", "outbox.jsonl"] {
        let f = Fixture::new();
        fs::create_dir(f.root.join("state").join(path)).unwrap();
        let host = if path == "inbox.jsonl" { "m5" } else { "m1" };
        let out = f.run(&["send", host, "text"]);
        assert_eq!(out.status.code(), Some(1));
        assert!(out.stdout.is_empty());
        assert!(f.read("sent.tsv").is_empty());
        assert!(String::from_utf8_lossy(&out.stderr).contains("append failed"));
    }
}

#[test]
fn sent_log_failure_reports_partial_success_and_receipt_failure_is_visible() {
    let f = Fixture::new();
    fs::create_dir(f.root.join("state/sent.tsv")).unwrap();
    let out = f.run(&["send", "local", "text"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
    assert!(!f.read("inbox.jsonl").is_empty());
    assert!(String::from_utf8_lossy(&out.stderr).contains("do not resend blindly"));
    fs::create_dir(f.root.join("state/receipts.jsonl")).unwrap();
    let out = f.run(&["unread", "--provider", "cursor", "--mark"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("receipt write failed"));
}

#[test]
fn wait_ignores_wrong_session_then_returns_exact_session_even_after_cd() {
    let f = Fixture::new();
    let mut child = f
        .command(&[
            "wait",
            "--provider",
            "cursor",
            "--session",
            "session-a",
            "--timeout",
            "3",
            "--json",
        ])
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(400));
    f.write(
        "inbox.jsonl",
        &format!("{}\n", targeted("wrong", "session-b")),
    );
    std::thread::sleep(std::time::Duration::from_millis(400));
    assert!(child.try_wait().unwrap().is_none());
    let mut correct = targeted("correct", "session-a");
    correct["project"] = json!("old-project");
    f.write(
        "inbox.jsonl",
        &format!("{}\n{}\n", targeted("wrong", "session-b"), correct),
    );
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["unread"].as_array().unwrap().len(), 1);
    assert_eq!(v["unread"][0]["id"], "correct");
    assert!(f.read("receipts.jsonl").is_empty());
}

#[test]
fn targeted_claude_mail_ignores_legacy_marks_and_writes_only_exact_receipts() {
    let f = Fixture::new();
    let mut m = targeted("claude-exact", "session-a");
    m["to_provider"] = json!("claude");
    m["role"] = json!("__agent_mail_session_v1__:claude:session-a");
    f.write("inbox.jsonl", &format!("{m}\n"));
    f.write("read.txt", "claude-exact\n");
    f.write("read-session-a.txt", "claude-exact\n");
    f.write(
        "receipts.jsonl",
        &receipt("claude-exact", "claude:session-b@m5"),
    );
    let out = f.run(&["unread", "--session", "session-a", "--mark", "--json"]);
    assert!(out.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&out.stdout).unwrap()["unread"][0]["id"],
        "claude-exact"
    );
    assert_eq!(f.read("read.txt"), "claude-exact\n");
    assert_eq!(f.read("read-session-a.txt"), "claude-exact\n");
    assert!(f.read("receipts.jsonl").contains("claude:session-a@m5"));
    let out = f.run(&["unread", "--session", "session-a", "--json"]);
    assert_eq!(
        serde_json::from_slice::<Value>(&out.stdout).unwrap()["unread"],
        json!([])
    );
}

#[test]
fn archived_exact_mail_retains_target_for_status_and_io_errors_are_not_unread() {
    let f = Fixture::new();
    let id = f.send("local:project", "session-a");
    f.write("inbox.jsonl", "");
    f.write("read.txt", &format!("{id}\n"));
    f.write("receipts.jsonl", &receipt(&id, "cursor:session-b@m5"));
    assert_eq!(
        stdout(&f.run(&["status", "m5", &id])),
        format!("{id} sent (read status unknown)\n")
    );
    f.write("receipts.jsonl", &receipt(&id, "cursor:session-a@m5"));
    assert_eq!(
        stdout(&f.run(&["status", "m5", &id])),
        format!("{id} read cursor:session-a@m5\n")
    );
    fs::remove_file(f.root.join("state/inbox.jsonl")).unwrap();
    fs::create_dir(f.root.join("state/inbox.jsonl")).unwrap();
    let out = f.run(&["status", "m5", &id]);
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
    assert!(String::from_utf8_lossy(&out.stderr).contains("status unavailable"));
}

// Frozen from afcb9bc1edab1557f15451c1adf69993fa276f04, before session fields.
// This predicate controls both old unread output and the IDs it marks.
fn old_message_for(
    m: &Value,
    project: &str,
    role: &str,
    seen: &std::collections::HashSet<String>,
) -> bool {
    let id = m.get("id").and_then(|x| x.as_str()).unwrap_or("");
    if id.is_empty() || seen.contains(id) {
        return false;
    }
    let want = m.get("project").and_then(|x| x.as_str()).unwrap_or("");
    if !want.is_empty() && want != project {
        return false;
    }
    let want_role = m.get("role").and_then(|x| x.as_str()).unwrap_or("");
    want_role.is_empty() || want_role == role
}

#[test]
fn legacy_readers_cannot_show_or_mark_exact_envelopes_during_upgrade() {
    let f = Fixture::new();
    f.send("local:project", "session-a");
    f.send("local:project:reviewer", "session-b");
    let messages: Vec<Value> = f
        .read("inbox.jsonl")
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    for m in &messages {
        for role in ["", "reviewer", "session-a", "session-b"] {
            assert!(
                !old_message_for(m, "project", role, &std::collections::HashSet::new()),
                "legacy reader matched {m}"
            );
        }
    }
    assert!(
        messages[0]["role"]
            .as_str()
            .unwrap()
            .starts_with("__agent_mail_session_v1__:")
    );
    assert_eq!(messages[1]["to_role"], "reviewer");
    assert_eq!(
        f.unread("cursor", Some("session-a"), false)["unread"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let out = f.run(&[
        "unread",
        "--provider",
        "cursor",
        "--session",
        "session-b",
        "--role",
        "reviewer",
        "--mark",
        "--json",
    ]);
    assert_eq!(
        serde_json::from_slice::<Value>(&out.stdout).unwrap()["unread"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn orphan_target_fields_never_become_shared_mail_or_read_status() {
    for patch in [
        json!({"to_role": "reviewer"}),
        json!({"to_role": ""}),
        json!({"to_role": null}),
        json!({"role": "__agent_mail_session_v1__:cursor:session-a"}),
    ] {
        let f = Fixture::new();
        let mut m = json!({"id":"orphan","project":"project","text":"must not broaden"});
        m.as_object_mut()
            .unwrap()
            .extend(patch.as_object().unwrap().clone());
        f.write("inbox.jsonl", &format!("{m}\n"));
        for mark in [false, true] {
            assert_eq!(
                f.unread("codex", Some("wrong"), mark)["unread"],
                json!([]),
                "{m}"
            );
        }
        assert!(f.read("receipts.jsonl").is_empty(), "{m}");
        assert!(f.read("read.txt").is_empty(), "{m}");
        assert_eq!(
            stdout(&f.run(&["status", "m5", "orphan"])),
            "orphan invalid recipient\n"
        );
        // Existing unrelated receipts cannot turn an invalid envelope into read.
        f.write("read.txt", "orphan\n");
        f.write("receipts.jsonl", &receipt("orphan", "codex:wrong@m5"));
        assert_eq!(
            stdout(&f.run(&["status", "local", "orphan"])),
            "orphan invalid recipient\n"
        );
    }
}

#[cfg(unix)]
#[test]
fn partial_append_errors_report_id_and_uncertainty_for_inbox_and_outbox() {
    use std::os::unix::process::CommandExt;
    for (host, filename) in [("m5", "inbox.jsonl"), ("m1", "outbox.jsonl")] {
        let f = Fixture::new();
        let mut cmd = f.command(&[
            "send",
            host,
            "force a partial append with a child-only file size limit",
        ]);
        unsafe {
            cmd.pre_exec(|| {
                libc::signal(libc::SIGXFSZ, libc::SIG_IGN);
                let limit = libc::rlimit {
                    rlim_cur: 16,
                    rlim_max: 16,
                };
                if libc::setrlimit(libc::RLIMIT_FSIZE, &limit) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let out = cmd.output().unwrap();
        assert_eq!(out.status.code(), Some(1));
        assert!(out.stdout.is_empty());
        assert_eq!(
            f.read(filename).len(),
            16,
            "fixture must leave partial bytes"
        );
        assert!(f.read("sent.tsv").is_empty());
        let err = String::from_utf8_lossy(&out.stderr);
        let id = err.split_whitespace().nth(1).unwrap();
        assert!(id.contains("-m5-"), "generated message ID missing: {err}");
        assert!(err.contains("outcome uncertain"), "{err}");
        assert!(err.contains("do not resend blindly"), "{err}");
        assert!(!err.contains("not queued"), "{err}");
    }
}
