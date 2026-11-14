use agentkit::guard::{self, Decision};
use agentkit::site::GuardConfig;
use serde_json::{json, Value};
use std::path::PathBuf;

fn cfg() -> GuardConfig {
    let mut c = GuardConfig::default();
    c.home = PathBuf::from("/var/tmp/fleet-guard-home");
    c.cwd = PathBuf::from("/var/tmp/fleet-guard-home/Projects/example");
    c.allowed_cd = agentkit::site::default_cd_roots(&c.home);
    c.air_aliases = vec!["worker".into()];
    c.air_hosts = vec!["worker.example".into()];
    c.resolve_ssh = false;
    c.offload_worker = Some(String::new());
    c.push_markers = vec![
        "AGENTKIT_PUSH_APPROVED=1".into(),
        "EXAMPLE_PUSH_APPROVED=1".into(),
    ];
    c
}

fn bash(cmd: &str) -> Value {
    json!({"tool_name": "Bash", "tool_input": {"command": cmd}})
}

fn decide(guard_name: &str, cmd: &str, cfg: &GuardConfig) -> Decision {
    guard::decide_value(guard_name, &bash(cmd), cfg)
}

#[test]
fn hard_delete_uses_the_tool_call_directory_not_the_guard_process() {
    let mut c = cfg();
    // A temp process directory must not turn a project-relative delete into an allow.
    c.cwd = PathBuf::from("/tmp");
    let project = json!({
        "tool_name": "Bash",
        "cwd": "/var/tmp/fleet-guard-home/Projects/example",
        "tool_input": {"command": "rm -rf file.txt"}
    });
    assert!(guard::decide_value("hard-delete", &project, &c).is_deny());
    let temp = json!({
        "cwd": "/tmp/agentkit-job",
        "command": "rm file.txt"
    });
    assert!(guard::decide_value("hard-delete", &temp, &c).is_allow());
}

#[test]
fn hard_delete_denies_home_and_allows_tmp() {
    let c = cfg();
    assert!(decide("hard-delete", "rm -rf ~/important", &c).is_deny());
    assert!(decide("hard-delete", "rm -rf /var/tmp/fleet-guard-home/Projects/example/file", &c).is_deny());
    assert!(decide("hard-delete", "rm -rf /tmp/agentkit-ok", &c).is_allow());
    assert!(decide("hard-delete", "rm -rf /private/tmp/agentkit-ok", &c).is_allow());
    assert!(decide("hard-delete", "rm -rf /tmp/../etc/passwd", &c).is_deny());
}

#[test]
fn hard_delete_find_git_clean_and_inline() {
    let c = cfg();
    assert!(decide("hard-delete", "find ~/Projects -delete", &c).is_deny());
    assert!(decide("hard-delete", "find /tmp -name x -delete", &c).is_allow());
    assert!(decide("hard-delete", "git clean -fd", &c).is_deny());
    assert!(decide("hard-delete", "git clean -n", &c).is_allow());
    assert!(decide("hard-delete", "python3 -c 'import os; os.remove(\"/tmp/x\")'", &c).is_deny());
    assert!(decide("hard-delete", "trash ~/important", &c).is_allow());
}

#[test]
fn hard_delete_air_exemption_is_config_not_a_builtin_host() {
    let c = cfg();
    assert!(decide("hard-delete", "ssh worker 'rm -rf /var/tmp/fleet-guard-home/file'", &c).is_allow());
    assert!(decide("hard-delete", "ssh other.example 'rm -rf /var/tmp/fleet-guard-home/file'", &c).is_deny());
    assert!(decide("hard-delete", "ssh worker 'rm -rf /tmp/x' && rm -rf ~/important", &c).is_deny());
}

#[test]
fn hard_delete_ignores_rm_inside_a_file_heredoc() {
    let c = cfg();
    let writing = "cat > /tmp/script.sh <<EOF\nrm -rf ~/important\nEOF\n";
    assert!(decide("hard-delete", writing, &c).is_allow());
    let running = "bash <<EOF\nrm -rf ~/important\nEOF\n";
    assert!(decide("hard-delete", running, &c).is_deny());
}

#[test]
fn icloud_blocks_the_trash_rules_and_allows_one_file() {
    assert!(decide("icloud", "trash ~/Documents", &cfg()).is_deny());
    assert!(decide("icloud", "trash ~/Desktop/*", &cfg()).is_deny());
    assert!(decide("icloud", "mv \"$HOME/Library/Mobile Documents/com~apple~CloudDocs\" /Volumes/x/", &cfg()).is_deny());
    assert!(decide("icloud", "rsync -a --delete /Volumes/old/ ~/Documents/", &cfg()).is_deny());
    assert!(decide("icloud", "osascript -e 'tell application \"Finder\" to empty trash'", &cfg()).is_deny());
    let incident = "ssh host 'cd ~ && for f in Pictures Documents Desktop; do find \"$f\" -exec mv {} ~/.Trash/ \\; ; done'";
    assert!(decide("icloud", incident, &cfg()).is_deny());
    assert!(decide("icloud", "trash ~/Desktop/Screenshot.png", &cfg()).is_allow());
    assert!(decide("icloud", "mv ~/Documents/notes/one /Volumes/Archive/", &cfg()).is_allow());
    assert!(decide("icloud", "ls ~/Documents | head", &cfg()).is_allow());
    assert!(decide("icloud", "agent-mail send other \"please empty trash\"", &cfg()).is_allow());
    assert!(decide("icloud", "ICLOUD_WIPE_APPROVED=1 trash ~/Documents", &cfg()).is_allow());
}

#[test]
fn icloud_matches_the_live_hook_incident_cases() {
    let c = cfg();
    let incident = "ssh -o BatchMode=yes -o ConnectTimeout=8 owner@shared-mac.local 'cd ~ && stamp=$(date +%H%M%S); for f in Pictures Documents Desktop; do mkdir -p \"$HOME/.Trash/$f-desktop-$stamp\"; find \"$f\" -mindepth 1 -maxdepth 1 ! -name \".localized\" -exec mv {} \"$HOME/.Trash/$f-desktop-$stamp/\" \\; ; done'";
    assert!(decide("icloud", incident, &c).is_deny());
    let loop_mv = "cd ~ && for f in Pictures \"Library/Messages\" Documents Desktop Movies; do mv \"$f\" \"$HOME/.Trash/$(basename \"$f\")-m1\"; done";
    assert!(decide("icloud", loop_mv, &c).is_deny());
    let empty = "osascript -e 'tell application \"Finder\" to set warns before emptying of trash to false' -e 'tell application \"Finder\" to empty trash'";
    assert!(decide("icloud", empty, &c).is_deny());
    assert!(decide("icloud", "ssh m1 'osascript -e \"tell app \\\"Finder\\\" to empty the trash\"'", &c).is_deny());
    assert!(decide("icloud", "du -sh ~/Documents ~/Desktop; ls ~/Documents | head", &c).is_allow());
    assert!(decide("icloud", "tar -cf - -C ~ Documents | ssh m2 'cat > /Volumes/x/Documents.tar'", &c).is_allow());
    assert!(decide("icloud", "rsync -a ~/Documents/ /Volumes/Archive/Documents/", &c).is_allow());
    assert!(decide("icloud", "grep -n 'empty trash' notes.md", &c).is_allow());
    assert!(decide("icloud", "osascript -e 'tell application \"Finder\" to reveal POSIX file \"/tmp\"'", &c).is_allow());
}

#[test]
fn cd_outside_denies_absolute_cd_and_ignores_a_quoted_remote_cd() {
    let c = cfg();
    assert!(decide("cd-outside", "cd /etc", &c).is_deny());
    assert!(decide("cd-outside", "cd ~/Projects/example", &c).is_allow());
    assert!(decide("cd-outside", "cd /tmp/work", &c).is_allow());
    assert!(decide("cd-outside", "ssh host 'cd /etc && ls'", &c).is_allow());
    assert!(decide("cd-outside", "cd notes", &c).is_allow());
}

#[test]
fn cd_outside_email_substring_comes_from_config() {
    let mut c = cfg();
    c.email_substrings = vec!["person@example.com".into()];
    assert!(decide("cd-outside", "curl -A person@example.com https://example.com", &c).is_deny());
    assert!(decide("cd-outside", "curl https://example.com", &c).is_allow());
}

#[test]
fn push_asks_denies_force_and_honours_the_marker() {
    let c = cfg();
    assert!(decide("push", "git status", &c).is_allow());
    assert!(decide("push", "git push origin main", &c).is_ask());
    assert!(decide("push", "git push --force origin main", &c).is_deny());
    assert!(decide("push", "git push -f origin main", &c).is_deny());
    assert!(decide("push", "git push --force-with-lease origin main", &c).is_ask());
    assert!(decide("push", "AGENTKIT_PUSH_APPROVED=1 git push origin main", &c).is_allow());
    assert!(decide("push", "EXAMPLE_PUSH_APPROVED=1 git push origin main", &c).is_allow());
    assert!(decide("push", "git -C ../other push origin main", &c).is_ask());
    assert!(decide("push", "AGENTKIT_PUSH_APPROVED=1 git push --force origin main", &c).is_deny());
    assert!(decide("push", "EXAMPLE_PUSH_APPROVED=1 git push -f origin main", &c).is_deny());
    assert!(decide("push", "git -C ../other push --force origin main", &c).is_deny());
}

#[test]
fn offload_denies_heavy_only_when_a_worker_is_named() {
    let mut quiet = cfg();
    quiet.offload_worker = Some(String::new());
    assert!(decide("offload-heavy", "cargo test", &quiet).is_allow());
    let mut hot = cfg();
    hot.offload_worker = Some("worker".into());
    assert!(decide("offload-heavy", "cargo test", &hot).is_deny());
    assert!(decide("offload-heavy", "remote-run -- cargo test", &hot).is_allow());
    assert!(decide("offload-heavy", "HEAVY_SLOT_LOCAL=1 cargo test", &hot).is_allow());
    assert!(decide("offload-heavy", "echo hello", &hot).is_allow());
    assert!(decide("offload-heavy", "grep pytest notes.txt", &hot).is_allow());
}

#[test]
fn non_shell_tools_are_left_alone() {
    let c = cfg();
    let read = json!({"tool_name": "Read", "tool_input": {"file_path": "/etc/passwd"}});
    assert!(guard::decide_value("hard-delete", &read, &c).is_allow());
}

#[test]
fn neutral_and_provider_formats() {
    let d = decide("hard-delete", "rm -rf ~/important", &cfg());
    let neutral: Value = serde_json::from_str(&guard::format_neutral(&d)).unwrap();
    assert_eq!(neutral["decision"], "deny");
    let claude: Value = serde_json::from_str(&guard::format_claude(&d)).unwrap();
    assert_eq!(claude["hookSpecificOutput"]["permissionDecision"], "deny");
    let cursor: Value = serde_json::from_str(&guard::format_cursor(&d)).unwrap();
    assert_eq!(cursor["permission"], "deny");
    assert!(guard::format_claude(&Decision::allow()).is_empty());
}

#[test]
fn self_test_passes_on_the_default_rules() {
    let mut c = cfg();
    c.home = PathBuf::from("/var/tmp/fleet-guard-home");
    guard::self_test(&c).unwrap();
}
