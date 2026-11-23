use agentkit::guard::Decision;
use agentkit::ledger;
use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::io::Write;

fn tmpdir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("agentkit-ledger-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn a_deny_line_carries_rule_pid_and_a_redacted_command() {
    let key: &[u8] = &[7u8; 32];
    let call = json!({
        "tool_name": "Bash", "session_id": "s-1", "cwd": "/w",
        "tool_input": {"command": concat!("curl -H 'Author", "ization: Bearer abcdefghijklmnop1234' https://x.test?token=hunter2 && rm -rf ~/x")}
    });
    let d = Decision::deny("hard-delete", "no");
    let line = ledger::build_line("claude", "all", &call, &d, Some(key), "2026-11-22T00:00:00.000Z".into(), 10, 9);
    let v: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(v["decision"], "deny");
    assert_eq!(v["rule"], "hard-delete");
    assert_eq!(v["provider"], "claude");
    assert_eq!(v["session"], "s-1");
    assert_eq!(v["ppid"], 9);
    let cmd = v["cmd"].as_str().unwrap();
    assert!(!cmd.contains("abcdefghijklmnop1234"), "{cmd}");
    assert!(!cmd.contains("hunter2"), "{cmd}");
    assert!(cmd.contains("token=<redacted:"), "{cmd}");
    assert!(cmd.contains("rm -rf ~/x"));
    assert_eq!(v["cmd_h"].as_str().unwrap().len(), 16);
    assert!(v["redacted"].as_u64().unwrap() >= 2);
}

#[test]
fn redaction_is_keyed_and_repeatable() {
    let (a, n) = ledger::redact("export X=sk-ant-api03-AAAAAAAAAAAAAAAAAAAAAAAA", Some(b"kkkkkkkkkkkkkkkk"));
    let (b, _) = ledger::redact("again sk-ant-api03-AAAAAAAAAAAAAAAAAAAAAAAA", Some(b"kkkkkkkkkkkkkkkk"));
    let (c, _) = ledger::redact("again sk-ant-api03-AAAAAAAAAAAAAAAAAAAAAAAA", Some(b"zzzzzzzzzzzzzzzz"));
    assert_eq!(n, 1);
    let tag = |s: &str| s[s.find("<redacted:").unwrap()..].to_string();
    assert_eq!(tag(&a), tag(&b));
    assert_ne!(tag(&a), tag(&c));
    // Paths, names and ordinary flags survive.
    let (p, n) = ledger::redact("cd ~/Projects/fleet/.claude/worktrees/agent-a13fd94a9ede4dd99 && mkdir -p /var/tmp/x/Projects/fleet/bin && cargo test --offline -p agentkit --test ledger && ls ./target/debug/build/agentkit-0123abcd45", None);
    assert_eq!(n, 0, "{p}");
    let (h, n) = ledger::redact("echo 3f7a0c9e1b2d4f6a8c0e2b4d6f8a0c2e4b6d8f0a2c4e6b8d0f2a4c6e8b0d2f4a", None);
    assert_eq!(n, 1, "{h}");
    let (k, _) = ledger::redact("cat <<EOF\n-----BEGIN OPENSSH PRIVATE KEY-----\nAAAA\n-----END OPENSSH PRIVATE KEY-----\nEOF", None);
    assert!(!k.contains("AAAA"), "{k}");
    let (u, _) = ledger::redact("psql postgres://user:pa55word@db.test/x", None);
    assert!(!u.contains("pa55word"), "{u}");
}

#[test]
fn iso_dates() {
    assert_eq!(ledger::iso_from(0, 0), "1970-01-01T00:00:00.000Z");
    assert_eq!(ledger::iso_from(1_795_261_397, 170), "2026-11-21T11:43:17.170Z");
    assert_eq!(ledger::iso_from(951_782_400, 0), "2000-02-29T00:00:00.000Z");
}

fn run_hook(state: &PathBuf, ledger_env: Option<&str>, payload: &str) -> (String, i32) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_agentkit"));
    cmd.args(["hook", "claude", "all"])
        .env("AGENTKIT_STATE", state)
        .env("AGENTKIT_CONFIG", state.join("no-config"))
        .env("AGENTKIT_SKIP_SSH_RESOLVE", "1")
        .env("HOME", state)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped());
    match ledger_env {
        Some(v) => { cmd.env("AGENTKIT_DECISIONS", v); }
        None => { cmd.env_remove("AGENTKIT_DECISIONS"); }
    }
    let mut child = cmd.spawn().unwrap();
    child.stdin.take().unwrap().write_all(payload.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    (String::from_utf8_lossy(&out.stdout).into_owned(), out.status.code().unwrap_or(-1))
}

#[test]
fn the_hook_writes_exactly_one_line_per_decision() {
    let state = tmpdir("hook");
    let call = json!({"tool_name": "Bash", "session_id": "abc", "tool_input": {"command": "rm -rf ~/Documents"}}).to_string();
    let (out, rc) = run_hook(&state, None, &call);
    assert_eq!(rc, 0);
    assert!(out.contains("deny"), "{out}");
    let text = fs::read_to_string(state.join("decisions.jsonl")).unwrap();
    assert_eq!(text.lines().count(), 1, "{text}");
    let v: Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
    assert_eq!(v["decision"], "deny");
    assert_eq!(v["session"], "abc");
    let allow = json!({"tool_name": "Bash", "tool_input": {"command": "ls"}}).to_string();
    run_hook(&state, None, &allow);
    let text = fs::read_to_string(state.join("decisions.jsonl")).unwrap();
    assert_eq!(text.lines().count(), 2);
    assert!(state.join("ledger.key").is_file());
    let _ = fs::remove_dir_all(&state);
}

#[test]
fn an_unwritable_ledger_never_changes_the_answer() {
    let state = tmpdir("unwritable");
    let call = json!({"tool_name": "Bash", "tool_input": {"command": "rm -rf ~/Documents"}}).to_string();
    let (out, rc) = run_hook(&state, Some("/nonexistent-dir-agentkit/x/decisions.jsonl"), &call);
    assert_eq!(rc, 0);
    assert!(out.contains("deny"), "{out}");
    let (out, rc) = run_hook(&state, Some("off"), &call);
    assert_eq!(rc, 0);
    assert!(out.contains("deny"));
    assert!(!state.join("decisions.jsonl").exists());
    let _ = fs::remove_dir_all(&state);
}

fn gone(text: &str, secret: &str) {
    let (out, n) = ledger::redact(text, Some(&[3u8; 32]));
    assert!(n >= 1, "nothing redacted in {text:?}");
    assert!(!out.contains(secret), "{secret:?} survived in {out:?}");
}

#[test]
fn quoted_values_are_redacted_to_the_closing_quote() {
    gone(r#"mysql --password="two words here" db"#, "two words");
    gone(r#"mysql --password="two words here" db"#, "here");
    gone("tool --token 'abc def ghi'", "def ghi");
    gone("tool --token='abc def'", "abc def");
    gone("tool --api-key s3cr3tvalue", "s3cr3tvalue");
    gone(r#"export PASSWORD="hunter2 with space""#, "hunter2");
    gone("echo password='x y z'", "x y z");
    gone(r#"echo '{"token": "abc123 xyz", "n": 1}'"#, "abc123");
    gone(r#"echo '{"client_secret":"ZZZtopsecret"}'"#, "ZZZtopsecret");
    gone("secret = 'spaced out'", "spaced out");
}

#[test]
fn headers_and_userinfo_are_redacted() {
    gone(concat!(r#"curl -H "Author"#, r#"ization: Bearer abc.def.ghi" https://x"#), "abc.def.ghi");
    gone(concat!(r#"curl -H 'Author"#, r#"ization: Basic dXNlcjpwYXNz' https://x"#), "dXNlcjpwYXNz");
    gone(concat!(r#"curl -H "author"#, r#"ization: token ghx123" https://x"#), "ghx123");
    gone(concat!(r#"curl -H "Author"#, r#"ization: rawvalue99" https://x"#), "rawvalue99");
    gone(concat!(r#"curl -H "X-Api-"#, r#"Key: k3y-v4lue" https://x"#), "k3y-v4lue");
    gone(concat!(r#"curl -H 'x-auth-"#, r#"token: tok_live_1' https://x"#), "tok_live_1");
    gone(concat!(r#"curl -H "x-goog-api-"#, r#"key: gkey" https://x"#), "gkey");
    gone(concat!(r#"curl -H "api-"#, r#"key: azurekey1" https://x"#), "azurekey1");
    gone(r#"curl -H "Cookie: session=abcdef" https://x"#, "abcdef");
    gone(concat!("curl -u ", "alice:wonderland https://x"), "wonderland");
    gone(concat!(r#"curl -u ""#, r#"alice:won der" https://x"#), "won");
    gone(concat!("curl --user=", "bob:builder https://x"), "builder");
    gone(concat!("git clone https://bob", ":p4ss@git.example/x.git"), "p4ss");
    gone(concat!("redis-cli -u redis://default", ":r3d1s@cache.test:6379"), "r3d1s");
}

#[test]
fn shorter_random_runs_with_slashes_and_dots_are_redacted() {
    gone("echo aB3dE5gH7jK9mN1pQ2rS4tU6vW", "aB3dE5gH7jK9mN1pQ2rS4tU6vW");
    gone(concat!("echo eyJhbGciOiJIUzI1NiJ9.", "eyJzdWIiOiIxMjM0NTY3ODkwIn0.sflKxwRJSMeKKF2QT4fwpM"), "eyJzdWIiOiIxMjM0NTY3ODkwIn0");
    gone("echo ab+cd/ef12GH34ij56KL78mn90opQR=", "ef12GH34ij56KL78mn90opQR");
}

fn mkfifo(path: &std::path::Path) {
    let st = Command::new("/usr/bin/mkfifo").arg(path).status().unwrap();
    assert!(st.success());
}

/// The hook answers within a few seconds or the test fails (a FIFO used to block it forever).
fn run_hook_bounded(state: &PathBuf, payload: &str) -> String {
    let mut child = Command::new(env!("CARGO_BIN_EXE_agentkit"))
        .args(["hook", "claude", "all"])
        .env("AGENTKIT_STATE", state)
        .env("AGENTKIT_CONFIG", state.join("no-config"))
        .env("AGENTKIT_SKIP_SSH_RESOLVE", "1")
        .env("HOME", state)
        .env_remove("AGENTKIT_DECISIONS")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(payload.as_bytes()).unwrap();
    let start = std::time::Instant::now();
    loop {
        if let Some(_) = child.try_wait().unwrap() {
            break;
        }
        if start.elapsed() > std::time::Duration::from_secs(5) {
            let _ = child.kill();
            panic!("hook blocked on the ledger");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let out = child.wait_with_output().unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn a_fifo_or_symlink_at_the_ledger_or_key_never_stalls_the_hook() {
    let deny = json!({"tool_name": "Bash", "tool_input": {"command": "rm -rf ~/Documents"}}).to_string();
    let allow = json!({"tool_name": "Bash", "tool_input": {"command": "ls"}}).to_string();
    for which in ["decisions.jsonl", "ledger.key"] {
        let state = tmpdir(&format!("fifo-{which}"));
        mkfifo(&state.join(which));
        assert!(run_hook_bounded(&state, &deny).contains("deny"));
        assert_eq!(run_hook_bounded(&state, &allow), "");
        let _ = fs::remove_dir_all(&state);

        let state = tmpdir(&format!("link-{which}"));
        let fifo = state.join("elsewhere");
        mkfifo(&fifo);
        std::os::unix::fs::symlink(&fifo, state.join(which)).unwrap();
        assert!(run_hook_bounded(&state, &deny).contains("deny"));
        let _ = fs::remove_dir_all(&state);
    }
    // A ledger with a loose mode is not written to.
    let state = tmpdir("mode");
    fs::write(state.join("decisions.jsonl"), "").unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(state.join("decisions.jsonl"), fs::Permissions::from_mode(0o666)).unwrap();
    assert!(run_hook_bounded(&state, &deny).contains("deny"));
    assert_eq!(fs::read_to_string(state.join("decisions.jsonl")).unwrap(), "");
    let _ = fs::remove_dir_all(&state);
}

#[test]
fn agents_cannot_replace_the_ledger_or_its_key() {
    use agentkit::guard;
    use agentkit::site::GuardConfig;
    let mut c = GuardConfig::default();
    c.home = PathBuf::from("/var/tmp/fleet-guard-home");
    c.cwd = PathBuf::from("/var/tmp/fleet-guard-home/Projects/example");
    c.resolve_ssh = false;
    let d = |cmd: &str| guard::decide_value("guard-config", &json!({"tool_name": "Bash", "tool_input": {"command": cmd}}), &c);
    for cmd in [
        "mkfifo ~/.local/state/agentkit/ledger.key",
        "ln -sf /tmp/p ~/.local/state/agentkit/decisions.jsonl",
        "mv /tmp/x ~/.local/state/agentkit/decisions.jsonl",
        ": > ~/.local/state/agentkit/decisions.jsonl",
        "echo x >> ~/.local/state/agentkit/decisions.jsonl",
        "mv ~/.local/state/agentkit /tmp/gone",
        "chmod 666 ~/.local/state/agentkit/decisions.jsonl",
        "mv ~/.local/state /tmp/elsewhere",
        "mv ~/.local /tmp/elsewhere",
        "rm -rf ~/.local/state",
        "ln -s /tmp/x ~/.local/state/agentkit",
        "ln -sfn /tmp/x ~/.local/state/agentkit",
    ] {
        assert!(d(cmd).is_deny(), "{cmd} should be refused");
    }
    assert!(d("tail -5 ~/.local/state/agentkit/decisions.jsonl").is_allow());
    let w = json!({"tool_name": "Write", "tool_input": {"file_path": "/var/tmp/fleet-guard-home/.local/state/agentkit/ledger.key", "content": "x"}});
    assert!(guard::decide_value("guard-config", &w, &c).is_deny());
}

#[test]
fn tokens_inside_urls_and_paths_are_redacted() {
    gone("curl -X POST https://hooks.example/services/T0/B0/xAbC1dEf2GhI3jKl4MnO5pQr -d x", "xAbC1dEf2GhI3jKl4MnO5pQr");
    gone("cat /var/tmp/tokens/Zx9Yw8Vu7Ts6Rq5Po4Nm3Lk2", "Zx9Yw8Vu7Ts6Rq5Po4Nm3Lk2");
    gone("open https://x.test/cb?code=Q1w2E3r4T5y6U7i8O9p0A1s2D3f4&state=1", "Q1w2E3r4T5y6U7i8O9p0A1s2D3f4");
    let (out, _) = ledger::redact("curl https://hooks.example/services/T0/B0/xAbC1dEf2GhI3jKl4MnO5pQr", None);
    assert!(out.starts_with("curl https://hooks.example/services/T0/B0/<redacted:"), "{out}");
}

#[test]
fn a_custom_ledger_path_is_guarded_and_a_loose_folder_is_not_written() {
    let state = tmpdir("custom");
    let custom = state.join("elsewhere");
    fs::create_dir_all(&custom).unwrap();
    let ledger = custom.join("d.jsonl");
    let ledger_s = ledger.to_string_lossy().into_owned();
    for cmd in [
        format!("mkfifo {}", custom.join("ledger.key").display()),
        format!("ln -sf /tmp/p {ledger_s}"),
        format!("mv {} /tmp/gone", custom.display()),
    ] {
        let call = json!({"tool_name": "Bash", "tool_input": {"command": cmd}}).to_string();
        let (out, _) = run_hook(&state, Some(&ledger_s), &call);
        assert!(out.contains("deny"), "{cmd}: {out}");
    }
    // A folder others can write is not trusted: nothing is written there.
    use std::os::unix::fs::PermissionsExt;
    let loose = state.join("loose");
    fs::create_dir_all(&loose).unwrap();
    fs::set_permissions(&loose, fs::Permissions::from_mode(0o777)).unwrap();
    let call = json!({"tool_name": "Bash", "tool_input": {"command": "ls"}}).to_string();
    run_hook(&state, Some(&loose.join("d.jsonl").to_string_lossy()), &call);
    assert!(!loose.join("d.jsonl").exists());
    // A symlinked folder is not followed.
    let real = state.join("real");
    fs::create_dir_all(&real).unwrap();
    std::os::unix::fs::symlink(&real, state.join("linked")).unwrap();
    run_hook(&state, Some(&state.join("linked/d.jsonl").to_string_lossy()), &call);
    assert!(!real.join("d.jsonl").exists());
    let _ = fs::remove_dir_all(&state);
}

#[test]
fn short_and_padded_base64_secrets_are_redacted() {
    gone("echo dGhpcyBpcyBhIHNlY3JldA== | base64 -d", "dGhpcyBpcyBhIHNlY3JldA");
    gone("export X=Ab3dE5gH7jK9mN1pQ2rS", "Ab3dE5gH7jK9mN1pQ2rS");
    let (out, _) = ledger::redact("echo dGhpcyBpcyBhIHNlY3JldA==", None);
    assert!(!out.contains("=="), "{out}");
    let (keep, n) = ledger::redact("cargo build --release --target aarch64-apple-darwin && ls node_modules/typescript-language-server", None);
    assert_eq!(n, 0, "{keep}");
}

fn hook_with_env(state: &PathBuf, envs: &[(&str, String)], payload: &str) -> String {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_agentkit"));
    cmd.args(["hook", "claude", "all"])
        .env("AGENTKIT_CONFIG", state.join("no-config"))
        .env("AGENTKIT_SKIP_SSH_RESOLVE", "1")
        .env("HOME", state.join("home"))
        .env_remove("AGENTKIT_DECISIONS")
        .env_remove("AGENTKIT_STATE")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped());
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().unwrap();
    child.stdin.take().unwrap().write_all(payload.as_bytes()).unwrap();
    String::from_utf8_lossy(&child.wait_with_output().unwrap().stdout).into_owned()
}

#[test]
fn custom_ledger_and_state_paths_and_their_ancestors_are_guarded_anywhere() {
    let base = tmpdir("anywhere");
    fs::create_dir_all(base.join("home")).unwrap();
    let state = base.join("var/agentkit");
    let ledger = base.join("logs/custom/dec.jsonl");
    fs::create_dir_all(&state).unwrap();
    fs::create_dir_all(ledger.parent().unwrap()).unwrap();
    let envs = [("AGENTKIT_STATE", state.display().to_string()), ("AGENTKIT_DECISIONS", ledger.display().to_string())];
    let l = ledger.display();
    let cases = [
        format!("rm {l}"),
        format!("rm -f {}", ledger.with_file_name("ledger.key").display()),
        format!("truncate -s 0 {l}"),
        format!("> {l}"),
        format!("echo x > {l}"),
        format!("mv {l} /tmp/elsewhere"),
        format!("mv {} /tmp/elsewhere", base.join("logs").display()),
        format!("rm -rf {}", base.join("logs/custom").display()),
        format!("mv {} /tmp/elsewhere", base.join("var").display()),
        format!("ln -sfn /tmp/x {}", state.display()),
    ];
    for c in &cases {
        let call = json!({"tool_name": "Bash", "tool_input": {"command": c}}).to_string();
        assert!(hook_with_env(&base, &envs, &call).contains("deny"), "{c} should be refused");
    }
    // Reading is fine, and so is an unrelated write next door.
    for c in [format!("tail {l}"), format!("echo x > {}", base.join("logs/other.txt").display())] {
        let call = json!({"tool_name": "Bash", "tool_input": {"command": c}}).to_string();
        assert!(!hook_with_env(&base, &envs, &call).contains("guard-config"), "{c} should be allowed");
    }
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn lower_case_tokens_and_uuids_are_redacted_but_words_are_not() {
    gone("export T=k3j4h5g6f7d8s9a0q1w2e3r4t5y6u7i8", "k3j4h5g6f7d8s9a0q1w2e3r4t5y6u7i8");
    gone("curl -d id=4be40b0a-e71e-491a-a7b6-b867f334e854 x", "4be40b0a-e71e-491a-a7b6-b867f334e854");
    gone(
        concat!("doctl auth init -t dop_", "v1_3f7a0c9e", "1b2d4f6a8c0e", "2b4d6f8a0c2e", "4b6d8f0a2c4e", "6b8d0f2a4c6e", "8b0d2f4a"),
        concat!("3f7a0c9e", "1b2d4f6a8c0e", "2b4d6f8a0c2e"),
    );
    let (keep, n) = ledger::redact("npm i typescript-language-server && ls internationalization_configuration/supercalifragilistic", None);
    assert_eq!(n, 0, "{keep}");
}

#[test]
fn prefixed_secret_names_and_whole_runs_are_redacted() {
    gone("PGPASSWORD=hunter2 psql -c 'select 1'", "hunter2");
    gone("MYSQL_PWD=s3cret mysql", "s3cret");
    gone("AWS_SECRET_ACCESS_KEY=wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY aws s3 ls", "wJalrXUtnFEMI");
    gone("AWS_SECRET_ACCESS_KEY=wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY aws s3 ls", "bPxRfiCYEXAMPLEKEY");
    gone("echo wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY", "K7MDENG");
    let tok = format!("GITHUB_{}=ghp_{}0123456789 gh api user", "TOKEN", "abcdefghijklmnopqrstuvwxyz");
    gone(&tok, "ghp_abcdef");
    gone(&format!("export OPENAI_API_{}='sk proj value'", "KEY"), "proj value");
    gone(&format!("export SLACK_{}=plainvalue", "SECRET"), "plainvalue");
    let (keep, n) = ledger::redact("cd /var/tmp/someone/Library/Application_Support/Code/User/workspaceStorage && ls ~/Projects/fleet/tools/security-evidence/evidence_chain.py", None);
    assert_eq!(n, 0, "{keep}");
}

#[test]
fn cwd_and_session_are_redacted_too() {
    let call = json!({"tool_name": "Bash", "session_id": "tok=abc sk-ant-api03-AAAAAAAAAAAAAAAAAAAAAAAA",
                      "cwd": "/tmp/aB3dE5gH7jK9mN1pQ2rS4tU6vW", "tool_input": {"command": "ls"}});
    let line = ledger::build_line("claude", "all", &call, &Decision::allow(), Some(&[1u8; 32]), "t".into(), 1, 1);
    assert!(!line.contains("AAAAAAAAAAAA"), "{line}");
    assert!(!line.contains("aB3dE5gH7jK9mN1pQ2rS4tU6vW"), "{line}");
}

#[test]
fn links_and_copies_of_the_ledger_by_source_or_inode_are_refused() {
    use agentkit::guard;
    use agentkit::site::GuardConfig;
    let home = tmpdir("inode-home");
    let state = home.join(".local/state/agentkit");
    fs::create_dir_all(&state).unwrap();
    fs::write(state.join("decisions.jsonl"), "").unwrap();
    let other = home.join("Projects");
    fs::create_dir_all(&other).unwrap();
    fs::hard_link(state.join("decisions.jsonl"), other.join("alias.jsonl")).unwrap();
    let mut c = GuardConfig::default();
    c.home = home.clone();
    c.cwd = other.clone();
    c.resolve_ssh = false;
    let d = |cmd: &str| guard::decide_value("guard-config", &json!({"tool_name": "Bash", "tool_input": {"command": cmd}}), &c);
    for cmd in [
        "ln ~/.local/state/agentkit/decisions.jsonl ~/.local/state/agentkit/notes.jsonl".to_string(),
        "ln ~/.local/state/agentkit/decisions.jsonl /tmp/notes.jsonl".to_string(),
        "ln -s ~/.local/state/agentkit/ledger.key /tmp/k".to_string(),
        "link ~/.local/state/agentkit/decisions.jsonl /tmp/n".to_string(),
        "cp -l ~/.local/state/agentkit/decisions.jsonl /tmp/n".to_string(),
        ": > ~/.local/state/agentkit/notes.jsonl".to_string(),
        format!(": > {}", other.join("alias.jsonl").display()),
        format!("truncate -s0 {}", other.join("alias.jsonl").display()),
        "mv alias.jsonl /tmp/x".to_string(),
    ] {
        assert!(d(&cmd).is_deny(), "{cmd} should be refused");
    }
    assert!(d("ls ~/.local/state/agentkit && wc -l ~/.local/state/agentkit/decisions.jsonl").is_allow());
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn a_hard_link_made_earlier_is_refused_on_the_fast_path_and_odd_copiers_are_refused() {
    use agentkit::guard;
    use agentkit::site::GuardConfig;
    let home = tmpdir("fast-home");
    let state = home.join(".local/state/agentkit");
    fs::create_dir_all(&state).unwrap();
    fs::write(state.join("decisions.jsonl"), "").unwrap();
    fs::hard_link(state.join("decisions.jsonl"), home.join("notes.jsonl")).unwrap();
    let mut c = GuardConfig::default();
    c.home = home.clone();
    c.cwd = home.clone();
    c.resolve_ssh = false;
    let d = |cmd: &str| guard::decide_value("guard-config", &json!({"tool_name": "Bash", "tool_input": {"command": cmd}}), &c);
    for cmd in [
        ": > ~/notes.jsonl",
        "echo > notes.jsonl",
        "gln ~/.local/state/agentkit/decisions.jsonl /tmp/x",
        "glink ~/.local/state/agentkit/decisions.jsonl /tmp/x",
        "ginstall ~/.local/state/agentkit/ledger.key /tmp/x",
        "pax -rw -l ~/.local/state/agentkit /tmp/x",
        "python3 tools/x.py ~/.local/state/agentkit/decisions.jsonl",
    ] {
        assert!(d(cmd).is_deny(), "{cmd} should be refused");
    }
    for cmd in ["cat ~/notes.jsonl", "tail -3 ~/.local/state/agentkit/decisions.jsonl", ": > ~/other.txt"] {
        assert!(d(cmd).is_allow(), "{cmd} should be allowed");
    }
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn every_word_of_every_program_is_checked_against_the_ledger() {
    use agentkit::guard;
    use agentkit::site::GuardConfig;
    let home = tmpdir("words-home");
    let state = home.join(".local/state/agentkit");
    fs::create_dir_all(&state).unwrap();
    fs::write(state.join("decisions.jsonl"), "").unwrap();
    let mut c = GuardConfig::default();
    c.home = home.clone();
    c.cwd = home.clone();
    c.resolve_ssh = false;
    let d = |cmd: &str| guard::decide_value("guard-config", &json!({"tool_name": "Bash", "tool_input": {"command": cmd}}), &c);
    // (command, refused?)
    let table: &[(&str, bool)] = &[
        ("tree -o ~/.local/state/agentkit/decisions.jsonl /tmp", true),
        ("less -o ~/.local/state/agentkit/decisions.jsonl /etc/hosts", true),
        ("agentkit memory import --from /tmp/x --out=~/.local/state/agentkit/ledger.key", true),
        ("sort -o~/.local/state/agentkit/decisions.jsonl /tmp/x", true),
        ("tool --output=~/.local/state/agentkit/decisions.jsonl", true),
        ("dd if=/dev/zero of=~/.local/state/agentkit/decisions.jsonl count=0", true),
        ("cd ~/.local/state/agentkit && rm decisions.jsonl", true),
        ("cd ~/.local/state && pushd agentkit && : > ledger.key", true),
        ("cd ~/.local/state && mv agentkit /tmp/x", true),
        ("cat ~/.local/state/agentkit/decisions.jsonl", false),
        ("tail -n 5 ~/.local/state/agentkit/decisions.jsonl | jq .rule", false),
        ("grep deny ~/.local/state/agentkit/decisions.jsonl", false),
        ("cd ~/.local/state/agentkit && ls", false),
        ("sort -o /tmp/out /tmp/x", false),
    ];
    for (cmd, refused) in table {
        assert_eq!(d(cmd).is_deny(), *refused, "{cmd}");
    }
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn multibyte_option_words_never_panic_and_a_panic_is_a_deny() {
    let state = tmpdir("panic");
    for cmd in ["ls -é", "grep -ü x /tmp/y", "echo -😀 🙂🙃", "tool -é~/.local/state/agentkit/decisions.jsonl"] {
        let call = json!({"tool_name": "Bash", "tool_input": {"command": cmd}}).to_string();
        let (out, rc) = run_hook(&state, Some("off"), &call);
        assert_eq!(rc, 0, "{cmd}");
        assert!(!out.contains("guard-panic"), "{cmd}: {out}");
    }
    let call = json!({"tool_name": "Bash", "tool_input": {"command": "tool -é~/.local/state/agentkit/decisions.jsonl"}}).to_string();
    assert!(run_hook(&state, Some("off"), &call).0.contains("deny"));
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_agentkit"));
    cmd.args(["hook", "claude", "all"])
        .env("AGENTKIT_STATE", &state)
        .env("AGENTKIT_CONFIG", state.join("no-config"))
        .env("AGENTKIT_DECISIONS", "off")
        .env("AGENTKIT_TEST_PANIC", "1")
        .env("HOME", &state)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped());
    let mut child = cmd.spawn().unwrap();
    child.stdin.take().unwrap().write_all(br#"{"tool_name":"Bash","tool_input":{"command":"ls"}}"#).unwrap();
    let out = child.wait_with_output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("deny") && text.contains("guard-panic"), "{text}");
    let _ = fs::remove_dir_all(&state);
}

#[test]
fn unknown_working_folders_and_glued_options_are_judged_by_name() {
    use agentkit::guard;
    use agentkit::site::GuardConfig;
    let home = tmpdir("cwd-home");
    fs::create_dir_all(home.join(".local/state/agentkit")).unwrap();
    let mut c = GuardConfig::default();
    c.home = home.clone();
    c.cwd = home.clone();
    c.resolve_ssh = false;
    let d = |cmd: &str| guard::decide_value("guard-config", &json!({"tool_name": "Bash", "tool_input": {"command": cmd}}), &c);
    let table: &[(&str, bool)] = &[
        ("cd ~/.local/state/agentkit; cd /tmp || cd -; : > decisions.jsonl", true),
        ("false || cd /tmp; rm decisions.jsonl", true),
        ("cd $X && rm ledger.key", true),
        ("pushd ~/.local/state/agentkit && pushd +1 && rm decisions.jsonl", true),
        ("sort -o~/.local/state/agentkit/decisions.jsonl /tmp/x", true),
        ("rg --pre /tmp/evil x ~/.local/state/agentkit/decisions.jsonl", true),
        ("cd /tmp && rm notes.txt", false),
        ("cd /tmp || cd -; ls", false),
    ];
    for (cmd, refused) in table {
        assert_eq!(d(cmd).is_deny(), *refused, "{cmd}");
    }
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn nested_cd_fails_closed_and_glued_values_are_resolved() {
    use agentkit::guard;
    use agentkit::site::GuardConfig;
    let home = tmpdir("nest-home");
    fs::create_dir_all(home.join(".local/state/agentkit")).unwrap();
    let mut c = GuardConfig::default();
    c.home = home.clone();
    c.cwd = home.clone();
    c.resolve_ssh = false;
    let d = |cmd: &str| guard::decide_value("guard-config", &json!({"tool_name": "Bash", "tool_input": {"command": cmd}}), &c);
    let table: &[(&str, bool)] = &[
        ("(cd ~/.local/state && : > agentkit/decisions.jsonl)", true),
        ("sh -c 'cd ~/.local/state; mv agentkit x'", true),
        ("tar -cf- -C~/.local/state agentkit", true),
        ("{ cd ~/.local/state; rm -rf agentkit; }", true),
        ("if true; then cd ~/.local; mv state /tmp/x; fi", true),
        ("cd /tmp && rm notes.txt", false),
        ("(cd /tmp && make)", false),
    ];
    for (cmd, refused) in table {
        assert_eq!(d(cmd).is_deny(), *refused, "{cmd}");
    }
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn any_cd_into_the_ledger_folder_with_a_writer_refuses_the_command() {
    use agentkit::guard;
    use agentkit::site::GuardConfig;
    let home = tmpdir("anycd-home");
    fs::create_dir_all(home.join(".local/state/agentkit")).unwrap();
    let mut c = GuardConfig::default();
    c.home = home.clone();
    c.cwd = home.clone();
    c.resolve_ssh = false;
    let d = |cmd: &str| guard::decide_value("guard-config", &json!({"tool_name": "Bash", "tool_input": {"command": cmd}}), &c);
    let table: &[(&str, bool)] = &[
        ("(cd ~/.local/state/agentkit && : > *)", true),
        ("sh -c 'cd ~/.local/state/agentkit; truncate -s0 decisions.jsonl'", true),
        ("cd ~/.local/state && python3 -c 'x'", true),
        ("cd `echo /x` && make", false),
        ("cd \"$(git rev-parse --show-toplevel)\" && make", false),
        ("cd \"$dir\" && npm test", false),
        ("cd $(mktemp -d) && tar xf ~/x.tar", false),
        ("cd \"$X\" && : > *", true),
        ("cd $(echo ~/.local/state/agentkit) && truncate -s0 decisions.jsonl", true),
        ("cd - && rm -rf .", true),
        ("cd \"$X\" && rm notes.txt", true),
        ("cd ~/.local/state/agentkit && ls && tail -2 decisions.jsonl", false),
        ("cd /tmp && make", false),
    ];
    for (cmd, refused) in table {
        assert_eq!(d(cmd).is_deny(), *refused, "{cmd}");
    }
    let _ = fs::remove_dir_all(&home);
}
