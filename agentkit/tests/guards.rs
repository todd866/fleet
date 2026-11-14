use agentkit::guard::{self, Decision};
use agentkit::site::GuardConfig;
use serde_json::{json, Value};
use std::fs;
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
fn push_allows_plain_push_outside_gated_projects() {
    let c = cfg();
    assert!(decide("push", "git status", &c).is_allow());
    assert!(decide("push", "git push origin main", &c).is_allow());
    assert!(decide("push", "git -C ../other push origin main", &c).is_allow());
}

#[test]
fn push_gate_follows_a_cd_into_a_gated_project() {
    // A root with no symlink in it: /var is a link on macOS and the guard resolves real paths.
    let mut c = cfg();
    c.push_gate_projects = vec![PathBuf::from("/nonexistent/tester/Projects/example")];
    c.cwd = PathBuf::from("/nonexistent/tester/Projects/other");
    assert!(decide("push", "git push origin main", &c).is_allow());
    assert!(decide("push", "cd /nonexistent/tester/Projects/example && git push origin main", &c).is_ask());
    assert!(decide("push", "cd '/nonexistent/tester/Projects/example/sub dir' ; git push", &c).is_ask());
    assert!(decide("push", "cd /nonexistent/tester/Projects/elsewhere && git push origin main", &c).is_allow());
}

#[test]
fn push_gate_by_remote_covers_every_clone_but_not_a_similar_name() {
    let base = std::env::temp_dir().join(format!("agentkit-remote-gate-{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    let make = |name: &str, url: &str| -> PathBuf {
        let repo = base.join(name);
        fs::create_dir_all(repo.join(".git").join("objects")).unwrap();
        fs::write(
            repo.join(".git").join("config"),
            format!("[remote \"origin\"]\n\turl = {url}\n\tfetch = +refs/heads/*:refs/remotes/origin/*\n"),
        )
        .unwrap();
        repo
    };
    let gated_ssh = make("clone-a", "git@github.com:owner/spendy.git");
    let gated_https = make("clone-b", "https://github.com/owner/spendy");
    let similar = make("clone-c", "git@github.com:owner/spendy-open.git");
    let mut c = cfg();
    c.home = base.clone();
    c.allowed_cd = agentkit::site::default_cd_roots(&c.home);
    c.push_gate_remotes = vec!["owner/spendy".into()];
    for (dir, gated) in [(&gated_ssh, true), (&gated_https, true), (&similar, false)] {
        c.cwd = dir.clone();
        assert_eq!(decide("push", "git push origin main", &c).is_ask(), gated, "{}", dir.display());
    }
    c.cwd = similar.clone();
    let via_cd = format!("cd {} && git push", gated_ssh.display());
    assert!(decide("push", &via_cd, &c).is_ask());
    c.cwd = gated_ssh.clone();
    assert!(decide("push", "AGENTKIT_PUSH_APPROVED=1 git push", &c).is_allow());
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn push_denies_force_everywhere_and_asks_on_force_with_lease() {
    let c = cfg();
    assert!(decide("push", "git push --force origin main", &c).is_deny());
    assert!(decide("push", "git push -f origin main", &c).is_deny());
    assert!(decide("push", "git push --force-with-lease origin main", &c).is_ask());
    assert!(decide("push", "AGENTKIT_PUSH_APPROVED=1 git push --force origin main", &c).is_deny());
    assert!(decide("push", "EXAMPLE_PUSH_APPROVED=1 git push -f origin main", &c).is_deny());
    assert!(decide("push", "git -C ../other push --force origin main", &c).is_deny());
}

#[test]
fn push_asks_in_gated_projects_and_honours_markers() {
    let mut c = cfg();
    c.push_gate_projects = vec![PathBuf::from("/var/tmp/fleet-guard-home/Projects/example")];
    assert!(decide("push", "git push origin main", &c).is_ask());
    assert!(decide("push", "AGENTKIT_PUSH_APPROVED=1 git push origin main", &c).is_allow());
    assert!(decide("push", "EXAMPLE_PUSH_APPROVED=1 git push origin main", &c).is_allow());
}

#[test]
fn push_gates_linked_worktrees_of_a_gated_project() {
    let base = std::env::temp_dir().join("agentkit-push-gate");
    let _ = fs::remove_dir_all(&base);
    let main = base.join("Projects/gated");
    let linked = base.join(".cursor/worktrees/gated/feature");
    let main_git = main.join(".git");
    let worktrees = main_git.join("worktrees/wt");
    fs::create_dir_all(&worktrees).unwrap();
    fs::create_dir_all(&linked).unwrap();
    fs::write(
        main_git.join("HEAD"),
        "ref: refs/heads/main\n",
    )
    .unwrap();
    fs::write(
        linked.join(".git"),
        format!("gitdir: {}/worktrees/wt\n", main_git.display()),
    )
    .unwrap();
    fs::write(worktrees.join("commondir"), "../..\n").unwrap();

    let mut c = cfg();
    c.home = base.clone();
    c.cwd = linked.clone();
    c.push_gate_projects = vec![main.clone()];
    c.allowed_cd = agentkit::site::default_cd_roots(&c.home);

    assert!(decide("push", "git push origin main", &c).is_ask());
    assert!(decide("push", "git push origin main", &cfg()).is_allow());

    let _ = fs::remove_dir_all(&base);
}

#[test]
fn offload_denies_heavy_only_when_a_worker_is_named() {
    let mut quiet = cfg();
    quiet.offload_worker = Some(String::new());
    assert!(decide("offload-heavy", "cargo test", &quiet).is_allow());
    let mut hot = cfg();
    hot.offload_worker = Some("worker".into());
    assert!(decide("offload-heavy", "cargo test", &hot).is_deny());
    assert!(decide("offload-heavy", "./tests.sh", &hot).is_deny());
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
fn disk_slot_metered_agent_yolo_and_audio_guards() {
    let mut c = cfg();
    c.home = std::env::temp_dir().join("agentkit-guard-metered");
    let _ = fs::remove_dir_all(&c.home);
    fs::create_dir_all(&c.home).unwrap();
    assert!(decide("disk-slot", "rsync -a ~/x/ /Volumes/Archive/x/", &c).is_deny());
    assert!(decide("disk-slot", "disk-slot /Volumes/Archive -- rsync -a ~/x/ /Volumes/Archive/x/", &c).is_allow());
    let marker = c.home.join(".local/state/METERED_LINK");
    fs::create_dir_all(marker.parent().unwrap()).unwrap();
    fs::write(&marker, "test").unwrap();
    assert!(decide("metered-link", "rsync -avh big/ remote:/big/", &c).is_deny());
    let _ = fs::remove_file(&marker);
    assert!(decide("agent-yolo", "agent -p --force task", &c).is_deny());
    assert!(decide("agent-yolo", "cursor-worker -- echo hi", &c).is_allow());
    assert!(decide("test-audio", "afplay /tmp/x.wav", &c).is_deny());
    assert!(decide("test-audio", "AUDIBLE_APPROVED=1 afplay /tmp/x.wav", &c).is_allow());
}

#[test]
fn metered_link_fires_only_where_a_transfer_would_run() {
    let mut c = cfg();
    c.home = std::env::temp_dir().join("agentkit-guard-metered-position");
    let _ = fs::remove_dir_all(&c.home);
    let marker = c.home.join(".local/state/METERED_LINK");
    fs::create_dir_all(marker.parent().unwrap()).unwrap();
    fs::write(&marker, "test").unwrap();
    // Real transfers from this machine, in the positions a shell runs a command.
    for cmd in [
        "rsync -avh big/ remote:/big/",
        "cd /tmp && scp big.bin host:/x/",
        "npm ci",
        "heavy-slot -- npm ci",
        "caffeinate -i rsync -a a/ b:/a/",
        "timeout 600 aws s3 cp big.dump s3://bucket/",
        "aws s3 sync dir s3://bucket/dir",
        "sh -c 'wget https://example.org/big.iso'",
        "true\ndocker pull ubuntu",
        "curl -L -O https://example.org/big.zip",
        "curl -sLo /tmp/big.zip https://example.org/big.zip",
        "curl -L -C - -o \"Big File.zip\" \"https://example.org/big.zip\"",
        "curl -s https://example.org/big.tar.gz > /tmp/big.tar.gz",
        "curl -s https://example.org/big.tar.gz | tar xz",
        "curl -T big.bin https://example.org/upload",
        "curl --output big.zip https://example.org/big.zip",
        "bash <<EOF\nrsync -a a/ b:/a/\nEOF",
        "eval 'rsync -a a/ b:/a/'",
        "curl -s https://example.org/big.tar.gz 2>&1 | sudo tar xz",
        "curl -s https://example.org/big.bin | cat > big.bin",
        "curl -F file=@big.bin https://example.org/upload",
        "curl --data-binary @big.bin https://example.org/upload",
    ] {
        assert!(decide("metered-link", cmd, &c).is_deny(), "should deny: {cmd}");
    }
    // The words as text, a remote command, or a small request to standard output: on 12 Nov
    // 2026 the older "anywhere in the command" rule refused four read-only commands in one
    // investigation because a search pattern or a script body named a transfer tool.
    for cmd in [
        "grep -n 'ssh\\|timeout\\|curl' ~/.local/bin/offload-target | head",
        "strings bin | grep -E '^(git|ssh|curl|rsync)$' | sort",
        "python3 - <<'EOF'\nimport re\nLONG = re.compile(r\"npm ci|curl|rsync|scp\")\nprint(LONG)\nEOF",
        "cat > notes.md <<EOF\nnext: rsync the dump, then npm ci\nEOF",
        "echo 'run npm ci on the worker'",
        "git log --grep=rsync --oneline | head",
        "ssh m2 'cd ~/x && curl -L -O https://example.org/big.zip'",
        "ssh -o ConnectTimeout=8 m2 \"cd ~/x && rsync -a a/ b/\"",
        "ssh m2 \"cd $D && rsync -a a/ b/ && npm ci\"",
        "grep -c 'rsync' notes.txt",
        "curl -s -o /dev/null -w '%{http_code}\\n' https://example.org",
        "curl -sI https://example.org/figure.png | head -5",
        "curl -s -m 20 \"https://api.crossref.org/works/10.1/x\" | python3 -c 'import sys; print(len(sys.stdin.read()))'",
        "curl -s https://example.org/status 2>/dev/null | head -3",
        "curl -s https://example.org/status 2>&1 | head -3",
        "curl -s https://api.example.org/item | python3 -c 'import sys; print(sys.stdin.read()[:80])' > out.json",
        "curl -s -d '{\"q\": 1}' https://api.example.org/search | head",
        "METERED_TRANSFER_APPROVED=1 rsync -a a/ b:/a/",
        "ls ~/Projects/rsync-notes; cat curl.md",
    ] {
        assert!(decide("metered-link", cmd, &c).is_allow(), "should allow: {cmd}");
    }
    let _ = fs::remove_dir_all(&c.home);
}

#[test]
fn offload_probe_runs_only_for_a_heavy_command() {
    let base = std::env::temp_dir().join("agentkit-guard-lazy-probe");
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(base.join("home")).unwrap();
    let called = base.join("called");
    let probe = base.join("offload-target");
    fs::write(&probe, format!("#!/bin/sh\necho x >> '{}'\necho worker\n", called.display())).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&probe, fs::Permissions::from_mode(0o755)).unwrap();
    }
    // Only this test reads these two variables.
    unsafe {
        std::env::set_var("AGENTKIT_OFFLOAD_TARGET", &probe);
        std::env::set_var("AGENTKIT_CONFIG", base.join("no-config"));
    }
    // Loading the configuration must not start the probe: it can take 20 s when the worker
    // is slow to answer, and it used to run before every shell command.
    let mut c = agentkit::site::guard_config_from_env();
    c.home = base.join("home");
    assert!(c.offload_worker.is_none(), "the probe ran while loading the configuration");
    assert!(!called.exists());
    for cmd in ["echo hello", "git status", "ls /tmp", "remote-run -- cargo test"] {
        assert!(decide("offload-heavy", cmd, &c).is_allow(), "should allow: {cmd}");
        assert!(guard::decide_all(&bash(cmd), &c, guard::ALL_GUARD_NAMES).is_allow(), "all should allow: {cmd}");
    }
    assert!(!called.exists(), "the probe ran for a light command");
    assert!(decide("offload-heavy", "cargo test", &c).is_deny());
    assert!(called.exists(), "the probe did not run for a heavy command");
    unsafe {
        std::env::remove_var("AGENTKIT_OFFLOAD_TARGET");
        std::env::remove_var("AGENTKIT_CONFIG");
    }
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn test_audio_fires_only_where_a_player_would_run() {
    let c = cfg();
    // Real playback, in the positions a shell runs a command.
    for cmd in [
        "say hello",
        "cd /tmp && afplay x.wav",
        "make build; say done",
        "echo hi | say",
        "sudo afplay x.wav",
        "env LANG=C ffplay x.mp3",
        "sh -c 'say hi'",
        "true\nsay hi",
        "(cd x; mpg123 a.mp3)",
        "echo $(say hi)",
        "VOICE=Alex say hi",
        "/usr/bin/say hi",
        "osascript -e 'beep 2'",
    ] {
        assert!(decide("test-audio", cmd, &c).is_deny(), "should deny: {cmd}");
    }
    // The same words as text: prose in a here-document, a search pattern, a file name, a
    // shell function of the same name being defined.
    for cmd in [
        "cat >> notes.md <<'EOF'\nstatus files that say a job failed\nEOF",
        "python3 - <<'EOF'\nprint('we say so')\nsay = 1\nEOF\necho done",
        "grep -n \"afplay\" src/guard.rs",
        "git commit -m 'never say never'",
        "echo \"they say it works\"",
        "ls docs/say-what.md",
        "say() { echo \"$*\"; }",
        "rg ffplay -l",
    ] {
        assert!(decide("test-audio", cmd, &c).is_allow(), "should allow: {cmd}");
    }
}

#[test]
fn hard_delete_denies_ssh_m1_whole_tree_delete() {
    let c = cfg();
    assert!(decide("hard-delete", "ssh m1 'rm -rf ~/x'", &c).is_deny());
}

#[test]
fn self_test_passes_on_the_default_rules() {
    let mut c = cfg();
    c.home = PathBuf::from("/var/tmp/fleet-guard-home");
    guard::self_test(&c).unwrap();
}

#[test]
fn hard_delete_sees_through_ssh_arguments_wrapper_flag_values_and_shell_strings() {
    let c = cfg();
    let home = "/var/tmp/fleet-guard-home/Projects/keep";
    // ssh to a host that is not the exempt worker, command passed as separate arguments
    assert!(decide("hard-delete", &format!("ssh other-mac rm -rf {home}"), &c).is_deny());
    assert!(decide("hard-delete", &format!("ssh -o BatchMode=yes other-mac sudo rm -rf {home}"), &c).is_deny());
    assert!(decide("hard-delete", "ssh other-mac rm -rf /tmp/scratch", &c).is_allow());
    assert!(decide("hard-delete", "ssh other-mac ls -la", &c).is_allow());
    // a local delete after an ssh to the exempt worker is still local
    assert!(decide("hard-delete", &format!("ssh worker 'ls' && rm -rf {home}"), &c).is_deny());
    // wrapper flags that take a value
    for cmd in [
        format!("sudo -u root rm -rf {home}"),
        format!("nice -n 5 rm -rf {home}"),
        format!("env -u FOO rm -rf {home}"),
        format!("timeout 30 rm -rf {home}"),
        format!("timeout -s KILL 30 rm -rf {home}"),
        format!("caffeinate -t 60 rm -rf {home}"),
        format!("heavy-slot -- rm -rf {home}"),
        format!("disk-slot /Volumes/Disk -- rm -rf {home}"),
        format!("/usr/bin/sudo -u root /bin/rm -rf {home}"),
        format!("ls | xargs -I {{}} rm -rf {home}/{{}}"),
    ] {
        assert!(decide("hard-delete", &cmd, &c).is_deny(), "{cmd}");
    }
    for cmd in [
        "sudo -u root ls /var",
        "nice -n 5 cargo build",
        "timeout 30 rm -rf /tmp/scratch",
        "heavy-slot -- cargo test",
        "env FOO=1 echo rm",
    ] {
        assert!(decide("hard-delete", cmd, &c).is_allow(), "{cmd}");
    }
    // a command carried as one string
    assert!(decide("hard-delete", &format!("bash -c 'rm -rf {home}'"), &c).is_deny());
    assert!(decide("hard-delete", &format!("zsh -lc \"rm -rf {home}\""), &c).is_deny());
    assert!(decide("hard-delete", &format!("eval 'rm -rf {home}'"), &c).is_deny());
    assert!(decide("hard-delete", "bash -c 'rm -rf /tmp/scratch'", &c).is_allow());
    assert!(decide("hard-delete", "bash -c 'echo hello'", &c).is_allow());
}

#[test]
fn push_treats_plus_refspec_and_mirror_as_force_and_asks_on_remote_delete() {
    let c = cfg();
    assert!(decide("push", "git push origin +main", &c).is_deny());
    assert!(decide("push", "git push origin +HEAD:main", &c).is_deny());
    assert!(decide("push", "git push --mirror origin", &c).is_deny());
    assert!(decide("push", "AGENTKIT_PUSH_APPROVED=1 git push origin +main", &c).is_deny());
    assert!(decide("push", "git push origin --delete old-branch", &c).is_ask());
    assert!(decide("push", "git push -d origin old-branch", &c).is_ask());
    assert!(decide("push", "git push origin :old-branch", &c).is_ask());
    assert!(decide("push", "git push origin main", &c).is_allow());
    assert!(decide("push", "git push origin HEAD:main", &c).is_allow());
    assert!(decide("push", "git push -u origin feature/a+b", &c).is_allow());
}
