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
    let loop_mv = "cd ~ && for f in Pictures \"Library/Messages\" Documents Desktop Movies; do mv \"$f\" \"$HOME/.Trash/$(basename \"$f\")-bench\"; done";
    assert!(decide("icloud", loop_mv, &c).is_deny());
    let empty = "osascript -e 'tell application \"Finder\" to set warns before emptying of trash to false' -e 'tell application \"Finder\" to empty trash'";
    assert!(decide("icloud", empty, &c).is_deny());
    assert!(decide("icloud", "ssh bench 'osascript -e \"tell app \\\"Finder\\\" to empty the trash\"'", &c).is_deny());
    assert!(decide("icloud", "du -sh ~/Documents ~/Desktop; ls ~/Documents | head", &c).is_allow());
    assert!(decide("icloud", "tar -cf - -C ~ Documents | ssh lab 'cat > /Volumes/x/Documents.tar'", &c).is_allow());
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
        "ssh lab 'cd ~/x && curl -L -O https://example.org/big.zip'",
        "ssh -o ConnectTimeout=8 lab \"cd ~/x && rsync -a a/ b/\"",
        "ssh lab \"cd $D && rsync -a a/ b/ && npm ci\"",
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
    assert!(decide("hard-delete", "ssh bench 'rm -rf ~/x'", &c).is_deny());
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
        format!("disk-slot /Volumes/Disk /Volumes/Other -- rm -rf {home}"),
        format!("disk-slot --wait 60 \"/Volumes/A B\" /Volumes/Other -- rm -rf {home}"),
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

fn each(guard_name: &str, c: &GuardConfig, deny: &[&str], allow: &[&str]) {
    for cmd in deny {
        assert!(decide(guard_name, cmd, c).is_deny(), "{guard_name} should deny: {cmd}");
    }
    for cmd in allow {
        let d = decide(guard_name, cmd, c);
        assert!(d.is_allow(), "{guard_name} should allow: {cmd} ({})", d.reason);
    }
}

#[test]
fn disk_slot_is_judged_by_each_command_s_program_not_by_a_word_anywhere() {
    let c = cfg();
    each(
        "disk-slot",
        &c,
        &[
            // the word disk-slot somewhere else in the line is not a wrapper
            "echo disk-slot; cp /Volumes/X/a /Volumes/Y/b",
            "disk-slot status; cp /Volumes/X/a /Volumes/Y/b",
            "cp /Volumes/X/a /Volumes/Y/b # disk-slot",
            // a read-only verb as a word in a path or an argument is not the program
            "cp /Volumes/X/test/a /Volumes/X/b",
            "rsync -a /Volumes/X/du/ /Volumes/Y/ls/",
            "mv /Volumes/X/stat.txt /Volumes/X/df.txt",
            // diskutil is read-only only for its listing subcommands
            "diskutil eraseVolume APFS X /Volumes/X",
            "diskutil unmount /Volumes/X",
            "diskutil apfs deleteVolume /Volumes/X",
            // a read-only verb that writes through a redirect
            "ls /Volumes/X > /Volumes/Y/listing.txt",
            // as before
            "rsync -a ~/x/ /Volumes/Archive/x/",
            "cd /Volumes/X && make",
            "SRC=/Volumes/X/a; cp $SRC ~/b",
            "ssh host 'cp /Volumes/X/a /Volumes/Y/b'",
            "ssh host 'echo disk-slot; cp /Volumes/X/a /Volumes/Y/b'",
            "ssh host 'cat notes.txt' > /Volumes/X/notes.txt",
            "sh -c 'cp /Volumes/X/a /Volumes/Y/b'",
            "ls /Volumes/X & cp /Volumes/X/a /Volumes/Y/b",
            "disk-slot /Volumes/X -- cp /Volumes/X/a ~/b; cp /Volumes/X/c ~/d",
        ],
        &[
            "disk-slot /Volumes/Archive -- rsync -a ~/x/ /Volumes/Archive/x/",
            "disk-slot \"/Volumes/A B\" -- rsync -a ~/x/ \"/Volumes/A B/x/\" | tee log",
            "disk-slot \"/Volumes/A B\" -- rsync -a ~/x/ \"/Volumes/A B/x/\" 2>&1 | tee -a ~/log.txt",
            "ssh host 'disk-slot /Volumes/X -- cp /Volumes/X/a /Volumes/X/b'",
            "ssh -o BatchMode=yes host \"disk-slot /Volumes/X -- du -sh /Volumes/X/a\"",
            "disk-slot --wait 600 /Volumes/X /Volumes/Y -- sh -c 'cp /Volumes/X/a /Volumes/Y/b && sync'",
            "sudo disk-slot /Volumes/X -- cp /Volumes/X/a /Volumes/X/b",
            "HEAVY=1 disk-slot /Volumes/X -- cp /Volumes/X/a /Volumes/X/b",
            "disk-slot status",
            "disk-slot which /Volumes/X",
            "ls -la /Volumes/X",
            "ls /Volumes/",
            "stat -f %z /Volumes/X/a",
            "df -h /Volumes/X",
            "du -sh /Volumes/X/dir | sort -h",
            "test -d /Volumes/X && echo mounted",
            "[ -d /Volumes/X ] && echo mounted",
            "sudo ls /Volumes/X",
            "command ls /Volumes/X",
            "diskutil list",
            "diskutil info /Volumes/X",
            "diskutil apfs list /Volumes/X",
            "diskutil activity",
            "ssh host 'ls /Volumes/X; df -h /Volumes/X'",
            "ls /Volumes/X > /tmp/listing.txt",
            "echo hello",
            "curl https://example.org/Volumes/x",
        ],
    );
}

#[test]
fn slot_holder_denies_sigkill_on_heavy_slot_by_name_or_by_a_queued_pid() {
    let mut c = cfg();
    c.home = std::env::temp_dir().join(format!("agentkit-guard-slot-holder-{}", std::process::id()));
    let _ = fs::remove_dir_all(&c.home);
    let queue = c.home.join(".local/state/heavy-slot/queue");
    fs::create_dir_all(&queue).unwrap();
    fs::write(queue.join("4242.json"), "{\"pid\":4242,\"state\":\"processing\"}\n").unwrap();
    each(
        "slot-holder",
        &c,
        &[
            "pkill -9 heavy-slot",
            "pkill -KILL -f 'heavy-slot -- cargo test'",
            "pkill --signal KILL heavy-slot",
            "pkill -SIGKILL -x heavy-slot",
            "killall -9 heavy-slot",
            "killall -KILL heavy-slot",
            "killall -s KILL heavy-slot",
            "sudo killall -9 heavy-slot",
            "kill -9 $(pgrep heavy-slot)",
            "kill -KILL `pgrep -f heavy-slot`",
            "kill -s KILL $(pgrep -f 'heavy-slot --')",
            "pgrep heavy-slot | xargs kill -9",
            "ps aux | grep heavy-slot | awk '{print $2}' | xargs kill -9",
            "ssh host 'pkill -9 heavy-slot'",
            "ssh host pkill -9 -f heavy-slot",
            "ssh -o BatchMode=yes host \"kill -9 \\$(pgrep heavy-slot)\"",
            "true && pkill -9 heavy-slot",
            // a pid heavy-slot has in its queue on this Mac
            "kill -9 4242",
            "kill -KILL 4242",
            "kill -s 9 111 4242",
            "kill -9 -4242",
        ],
        &[
            "heavy-slot release 4242",
            "kill 4242",
            "kill -TERM 4242",
            "kill -15 4242",
            "pkill heavy-slot",
            "pkill -TERM -f heavy-slot",
            "killall heavy-slot",
            // a pid that is not in heavy-slot's queue
            "kill -9 5151",
            "pkill -9 -f some-other-tool",
            "killall -9 Dock",
            "pgrep -fl heavy-slot",
            "heavy-slot status",
            "grep -rn 'pkill -9 heavy-slot' docs/",
            "echo 'never killall -9 heavy-slot'",
            "git commit -m 'slot-holder: deny pkill -9 heavy-slot'",
            // a numeric pid on another Mac cannot be attributed from here
            "ssh host 'kill -9 4242'",
        ],
    );
    let _ = fs::remove_dir_all(&c.home);
}

#[test]
fn slot_files_denies_shell_writes_to_limit_and_cool() {
    let c = cfg();
    each(
        "slot-files",
        &c,
        &[
            "echo 3 > ~/.local/state/heavy-slot/limit",
            "echo 70 >> $HOME/.local/state/heavy-slot/cool",
            "echo 3 >~/.local/state/heavy-slot/limit",
            "printf 2 > \"$HOME/.local/state/heavy-slot/limit\"",
            "echo 3 | tee ~/.local/state/heavy-slot/limit",
            "echo 3 | sudo tee -a /var/tmp/fleet-guard-home/.local/state/heavy-slot/cool >/dev/null",
            "sed -i '' 's/1/3/' ~/.local/state/heavy-slot/limit",
            "sed -i.bak s/1/3/ ~/.local/state/heavy-slot/limit",
            "perl -pi -e 's/1/3/' ~/.local/state/heavy-slot/limit",
            "cp /tmp/limit ~/.local/state/heavy-slot/limit",
            "cp /tmp/limit ~/.local/state/heavy-slot/",
            "mv /tmp/new-cool ~/.local/state/heavy-slot/cool",
            "mv ~/.local/state/heavy-slot/limit /tmp/limit.bak",
            "trash ~/.local/state/heavy-slot/cool",
            "truncate -s 0 ~/.local/state/heavy-slot/limit",
            "cd ~/.local/state/heavy-slot && echo 3 > limit",
            "python3 -c \"open('/var/tmp/fleet-guard-home/.local/state/heavy-slot/limit','w').write('3')\"",
            "python3 -c 'from pathlib import Path; Path.home().joinpath(\".local/state/heavy-slot/cool\").write_text(\"\")'",
            "perl -e 'open(F, \">\", \"$ENV{HOME}/.local/state/heavy-slot/limit\"); print F 3'",
            "python3 - <<'PY'\nimport os\nopen(os.path.expanduser('~/.local/state/heavy-slot/limit'), 'w').write('3')\nPY",
            "ssh host 'echo 3 > ~/.local/state/heavy-slot/limit'",
            "ssh host \"echo 3 | tee ~/.local/state/heavy-slot/limit\"",
            "true; echo 1 > ~/.local/state/heavy-slot/limit",
        ],
        &[
            "cat ~/.local/state/heavy-slot/limit",
            "cat ~/.local/state/heavy-slot/limit ~/.local/state/heavy-slot/cool 2>/dev/null",
            "ls -la ~/.local/state/heavy-slot/",
            "heavy-slot status",
            "grep -c . ~/.local/state/heavy-slot/cool",
            "ssh host 'cat ~/.local/state/heavy-slot/limit'",
            "echo 3 > /tmp/limit",
            "echo 3 > ~/Projects/example/limit",
            "cp ~/.local/state/heavy-slot/limit /tmp/limit.copy",
            "python3 -c \"print(open('/var/tmp/fleet-guard-home/.local/state/heavy-slot/limit').read())\"",
            "tail -5 ~/.local/state/heavy-slot/jobs.jsonl > /tmp/jobs.txt",
            "echo 'the limit and cool files live in ~/.local/state/heavy-slot/limit'",
            "sed -n 1p ~/.local/state/heavy-slot/limit",
        ],
    );
    let write = |tool: &str, path: &str| json!({"tool_name": tool, "tool_input": {"file_path": path, "content": "3"}});
    for tool in ["Write", "Edit", "MultiEdit"] {
        assert!(guard::decide_value("slot-files", &write(tool, "/var/tmp/fleet-guard-home/.local/state/heavy-slot/limit"), &c).is_deny(), "{tool}");
        assert!(guard::decide_value("slot-files", &write(tool, "~/.local/state/heavy-slot/cool"), &c).is_deny(), "{tool}");
        assert!(guard::decide_value("slot-files", &write(tool, "/var/tmp/fleet-guard-home/Projects/example/limit"), &c).is_allow(), "{tool}");
    }
    let read = json!({"tool_name": "Read", "tool_input": {"file_path": "/var/tmp/fleet-guard-home/.local/state/heavy-slot/limit"}});
    assert!(guard::decide_value("slot-files", &read, &c).is_allow());
    // the combined hook sees file tools too
    let all = guard::decide_all(&write("Write", "/var/tmp/fleet-guard-home/.local/state/heavy-slot/limit"), &c, guard::ALL_GUARD_NAMES);
    assert!(all.is_deny());
    assert!(guard::decide_all(&write("Write", "/var/tmp/fleet-guard-home/Projects/example/notes.md"), &c, guard::ALL_GUARD_NAMES).is_allow());
}

#[test]
fn approval_files_are_the_owner_s_to_create() {
    let c = cfg();
    each(
        "approval-files",
        &c,
        &[
            "touch APPROVE_EXAMPLE",
            "touch ~/.local/state/example/APPROVE_EXAMPLE",
            "touch \"/Volumes/Some Disk/APPROVE_WIPE\"",
            "echo yes > /tmp/APPROVE_EXAMPLE",
            "date >> ~/x/APPROVE_THING",
            ": > APPROVE_X",
            "echo ok | tee /tmp/work/APPROVE_EXAMPLE",
            "cp /dev/null ~/x/APPROVE_EXAMPLE",
            "cp /tmp/APPROVE_EXAMPLE ~/x/",
            "mv /tmp/marker ~/x/APPROVE_EXAMPLE",
            "install -m 644 /dev/null ~/x/APPROVE_EXAMPLE",
            "ln -s /tmp/y ~/x/APPROVE_EXAMPLE",
            "mkdir -p ~/x/APPROVE_EXAMPLE",
            "ssh host touch /Users/agentkit-someone/APPROVE_EXAMPLE",
            "ssh host 'touch ~/APPROVE_EXAMPLE'",
            "ssh -o BatchMode=yes host \"cd ~/x && touch APPROVE_EXAMPLE\"",
            "sudo touch /var/db/APPROVE_EXAMPLE",
            "sh -c 'touch APPROVE_EXAMPLE'",
            "true && touch APPROVE_EXAMPLE",
            "python3 -c \"open('APPROVE_EXAMPLE','w').close()\"",
            "python3 -c 'from pathlib import Path; Path(\"APPROVE_EXAMPLE\").touch()'",
        ],
        &[
            "ls -la ~/x/APPROVE_EXAMPLE",
            "test -e ~/x/APPROVE_EXAMPLE && echo approved",
            "[ -f APPROVE_EXAMPLE ] || echo 'not approved'",
            "ssh host 'ls ~/APPROVE_EXAMPLE'",
            "grep -rn APPROVE_EXAMPLE docs/",
            "echo 'the owner creates APPROVE_EXAMPLE himself'",
            "git commit -m 'never touch APPROVE_EXAMPLE'",
            "cat ~/x/APPROVE_EXAMPLE",
            "touch ~/x/approved.txt",
            "touch ~/x/NOT_APPROVE_X",
            "trash ~/x/APPROVE_EXAMPLE",
        ],
    );
    let write = |tool: &str, path: &str| json!({"tool_name": tool, "tool_input": {"file_path": path, "content": ""}});
    assert!(guard::decide_value("approval-files", &write("Write", "/var/tmp/fleet-guard-home/x/APPROVE_EXAMPLE"), &c).is_deny());
    assert!(guard::decide_value("approval-files", &write("Edit", "/var/tmp/fleet-guard-home/x/APPROVE_EXAMPLE/note"), &c).is_deny());
    assert!(guard::decide_value("approval-files", &write("Write", "/var/tmp/fleet-guard-home/x/approve.md"), &c).is_allow());
    assert!(guard::decide_all(&write("Write", "/var/tmp/fleet-guard-home/x/APPROVE_EXAMPLE"), &c, guard::ALL_GUARD_NAMES).is_deny());
}

#[test]
fn idrive_archive_cleanup_is_never_started_from_a_shell() {
    let c = cfg();
    each(
        "idrive-cleanup",
        &c,
        &[
            "osascript -e 'tell application \"System Events\" to tell process \"IDrive\" to click menu item \"Archive Cleanup…\" of menu \"File\" of menu bar 1'",
            "osascript <<'EOF'\ntell application \"System Events\" to tell process \"IDrive\"\n  click menu item \"Archive Cleanup\" of menu 1 of menu bar item \"IDrive\" of menu bar 1\nend tell\nEOF",
            "/Applications/IDrive.app/Contents/MacOS/IDrive --archive-cleanup",
            "perl archive_cleanup.pl",
            "./Archive_Cleanup.pl --now",
            "cd ~/BackupForLinux/scripts && ./archive_cleanup.pl",
            "open 'idrive://archive-cleanup'",
            "ssh host \"osascript -e 'tell application \\\"IDrive\\\" to archive cleanup'\"",
            "ssh host perl /opt/BackupForLinux/scripts/archive_cleanup.pl",
            "sh -c 'perl archive_cleanup.pl'",
        ],
        &[
            "grep -rn -i 'archive cleanup' docs/",
            "echo 'never run Archive Cleanup'",
            "git commit -m 'idrive-cleanup: deny Archive Cleanup'",
            "cat docs/STORAGE-PLAN.md | grep -c 'Archive Cleanup'",
            "ls ~/Library/Logs/IDrive | grep -i cleanup",
            "open -a IDrive",
            "osascript -e 'tell application \"IDrive\" to activate'",
            "agent-mail send host \"do not run Archive Cleanup\"",
        ],
    );
    let mut site = cfg();
    site.idrive_cleanup_patterns = vec![r"(?i)prune[\s_-]*remote".into(), "(unclosed".into()];
    assert!(decide("idrive-cleanup", "osascript -e 'tell app \"Backup\" to prune remote'", &site).is_deny());
    assert!(decide("idrive-cleanup", "perl archive_cleanup.pl", &site).is_allow());
}

#[test]
fn emptying_the_trash_is_denied_on_every_host() {
    let c = cfg();
    each(
        "icloud",
        &c,
        &[
            // the exempt worker may hard-delete, but not empty its Trash
            "ssh worker 'rm -rf ~/.Trash/*'",
            "ssh worker rm -rf '~/.Trash/*'",
            "ssh worker 'rm -rf ~/.Trash'",
            "ssh worker 'rm -rf ~/.Trash/* ~/.Trash/.[!.]*'",
            "ssh worker 'cd ~/.Trash && rm -rf *'",
            "ssh worker 'find ~/.Trash -mindepth 1 -delete'",
            "ssh worker 'sudo rm -rf /Users/agentkit-someone/.Trash/*'",
            "ssh other.example 'rm -rf ~/.Trash/*'",
            "rm -rf ~/.Trash/*",
            "rm -rf \"$HOME/.Trash/\"*",
            "find ~/.Trash -type f -delete",
            "sudo rm -rf /Volumes/Disk/.Trashes/*",
        ],
        &[
            "ssh worker 'ls ~/.Trash | head'",
            "ssh worker 'du -sh ~/.Trash'",
            "ls ~/.Trash",
            "mv ~/.Trash/report.pdf ~/Desktop/report.pdf",
            "ssh worker 'rm -rf ~/build/cache'",
            "grep -rn 'rm -rf ~/.Trash' docs/",
            "ICLOUD_WIPE_APPROVED=1 rm -rf ~/.Trash/*",
        ],
    );
    // the worker's hard-delete exemption itself is unchanged
    assert!(decide("hard-delete", "ssh worker 'rm -rf ~/build/cache'", &c).is_allow());
    let all = guard::decide_all(&bash("ssh worker 'rm -rf ~/.Trash/*'"), &c, guard::ALL_GUARD_NAMES);
    assert!(all.is_deny());
}

#[test]
fn metered_link_covers_package_restores_clones_and_model_pulls() {
    let mut c = cfg();
    c.home = std::env::temp_dir().join(format!("agentkit-guard-metered-restores-{}", std::process::id()));
    let _ = fs::remove_dir_all(&c.home);
    let marker = c.home.join(".local/state/METERED_LINK");
    let restores = [
        "npm install",
        "npm install left-pad",
        "cd app && npm install --no-audit",
        "brew install ffmpeg",
        "brew upgrade",
        "brew fetch llvm",
        "pip install torch",
        "pip3 install -r requirements.txt",
        "python3 -m pip install torch",
        "uv pip install torch",
        "uv sync",
        "cargo fetch",
        "cargo install ripgrep",
        "git clone https://example.org/big/repo.git",
        "git -C ~/Projects clone https://example.org/big/repo.git",
        "git lfs pull",
        "git lfs fetch --all",
        "ollama pull llama3",
        "huggingface-cli download org/model",
        "hf download org/model",
        "heavy-slot -- uv sync",
        "sh -c 'pip install torch'",
    ];
    // not metered: nothing is refused
    for cmd in restores {
        assert!(decide("metered-link", cmd, &c).is_allow(), "not metered, should allow: {cmd}");
    }
    fs::create_dir_all(marker.parent().unwrap()).unwrap();
    fs::write(&marker, "test").unwrap();
    each(
        "metered-link",
        &c,
        &restores,
        &[
            "git pull --ff-only",
            "git fetch origin",
            "git push origin main",
            "git status",
            "git lfs ls-files",
            "npm run build",
            "npm test",
            "brew list",
            "brew info ffmpeg",
            "pip list",
            "pip show torch",
            "uv run pytest",
            "cargo build",
            "cargo test -p agentkit",
            "ollama list",
            "hf whoami",
            "echo 'pip install torch'",
            "grep -rn 'git clone' README.md",
            "ssh host 'brew install ffmpeg'",
            "METERED_TRANSFER_APPROVED=1 npm install",
        ],
    );
    let _ = fs::remove_dir_all(&c.home);
}

#[test]
fn hard_delete_reads_a_quoted_search_pattern_or_message_as_text() {
    let c = cfg();
    let home = "/var/tmp/fleet-guard-home/Projects/keep";
    each(
        "hard-delete",
        &c,
        &[
            // real deletes carried in a string a shell runs
            &format!("sh -c \"rm -rf {home}\""),
            &format!("bash -c 'cd /tmp; rm -rf {home}'"),
            &format!("bash -lc 'true | rm -rf {home}'"),
            &format!("eval 'true; rm -rf {home}'"),
            &format!("ssh other-mac 'cd /tmp; rm -rf {home}'"),
            // text handed on to something that runs it
            &format!("echo 'cd /tmp; rm -rf {home}' | sh"),
            &format!("printf 'x; rm -rf {home}\\n' | bash"),
            // a delete after the text command, and one inside a substitution
            &format!("grep -n 'a\\|b' notes.txt; rm -rf {home}"),
            &format!("grep -n 'x' notes.txt | xargs rm -rf"),
            &format!("echo \"$(true; rm -rf {home})\""),
            &format!("git commit -m \"x `true; rm -rf {home}`\""),
            &format!("grep \"it's\" \"$(rm -rf {home})\" \"it's\" notes.txt"),
            &format!("grep 'a;b' notes.txt && rm -rf {home}"),
            // an unbalanced quote or a here-document is not trusted as text
            &format!("echo <<EOF\nit's\nEOF\nrm -rf {home}\necho 'x'"),
            &format!("grep 'a\nb'; rm -rf {home}"),
        ],
        &[
            "grep -n 'foo\\|rm -rf\\|bar' src/guard.rs",
            "grep -rn \"rm -rf ~/important; echo\" docs/",
            "rg 'find . -delete|git clean -fd' -l",
            "grep -E -e '(^|;) *rm -rf /' notes.txt",
            "grep --regexp='a; rm -rf b' -rn docs/",
            "git grep -n -e 'x; rm -rf y' -- src",
            "git log -S 'x; rm -rf y' --oneline",
            "git commit -m 'hard-delete: rm -rf x; rm y is denied'",
            "git commit -q -m \"guard: deny a | rm -rf b\"",
            "git log --grep='x; rm -rf y' --oneline",
        ],
    );
}

#[test]
fn hard_delete_text_is_only_trusted_on_a_line_that_is_one_simple_command() {
    let c = cfg();
    let home = "/var/tmp/fleet-guard-home/Projects/keep";
    each(
        "hard-delete",
        &c,
        &[
            // echo and printf are never text: something else on the line can run what they print
            "echo 'import os; os.remove(\"/Users/agentkit-me/secret\")' > /tmp/x.py && python3 /tmp/x.py",
            &format!("eval \"$(echo 'true; rm -rf {home}')\""),
            &format!("sh <(echo 'true; rm -rf {home}')"),
            &format!("x=$(printf 'true; rm -rf %s' {home}); eval \"$x\""),
            &format!("source /dev/stdin <<< 'true; rm -rf {home}'"),
            &format!("echo 'true; rm -rf {home}' > /tmp/s.sh; sh /tmp/s.sh"),
            &format!("echo 'true; rm -rf {home}'"),
            &format!("printf '%s\\n' 'a | rm -rf {home}'"),
            // a search or a message is text only when it is the whole line
            &format!("grep -n 'a; rm -rf {home}' notes.txt | head"),
            &format!("grep -n 'a; rm -rf {home}' notes.txt > /tmp/out; sh /tmp/out"),
            &format!("git commit -m 'x; rm -rf {home}' && git push"),
            &format!("grep 'x' <(echo 'a; rm -rf {home}')"),
            &format!("grep 'a; rm -rf {home}' <<< text"),
            // options that run a command, config overrides, pager variables, wrappers
            &format!("git grep --open-files-in-pager='true; rm -rf {home}' x"),
            &format!("git grep -O'sh -c \"true; rm -rf {home}\"' x"),
            &format!("rg --pre 'sh -c \"true; rm -rf {home}\"' x"),
            &format!("git -c core.pager='true; rm -rf {home}' log --grep='x'"),
            &format!("git -c alias.z='!true; rm -rf {home}' z"),
            &format!("GIT_PAGER='true; rm -rf {home}' git log --grep='x'"),
            &format!("PAGER='true; rm -rf {home}' git log --grep='x'"),
            &format!("git log --grep='x' --output='a; rm -rf {home}'"),
            &format!("git commit -m 'msg' --exec-path='a; rm -rf {home}'"),
            &format!("sudo grep -e 'x; rm -rf {home}' notes.txt"),
            // only the pattern or the message is emptied, never another argument
            &format!("grep -e 'x' 'notes; rm -rf {home}'"),
            &format!("git commit -m 'msg' 'path; rm -rf {home}'"),
            &format!("git log --grep='x' --format='a; rm -rf {home}'"),
            // a here-document whose text something else can run
            &format!("eval \"$(cat <<EOF\nrm -rf {home}\nEOF\n)\""),
            &format!("cat <<EOF | sh\nrm -rf {home}\nEOF"),
            &format!("cat <<EOF\nrm -rf {home}\nEOF"),
            &format!("source /dev/stdin <<EOF\nrm -rf {home}\nEOF"),
            &format!("tee /tmp/s.sh <<EOF | sh\nrm -rf {home}\nEOF"),
        ],
        &[
            "cat > /tmp/script.sh <<EOF\nrm -rf ~/important\nEOF\n",
            "cat >> notes.md <<'EOF'\nnever rm -rf ~/important\nEOF",
            "git commit -m \"$(cat <<'EOF'\nhard-delete: rm -rf x is denied\n\nrm -rf ~/important stays denied\nEOF\n)\"",
        ],
    );
}

#[test]
fn hard_delete_fixture_denials_never_flip() {
    // Every case in the shipped fixture keeps its decision: no change to how text is read may
    // turn a denied delete into an allowed one.
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/guards/hard-delete.json");
    let doc: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    let mut c = cfg();
    c.home = PathBuf::from("/Users/agentkit-conformance");
    c.cwd = c.home.join("Projects/example");
    c.air_aliases.clear();
    c.air_hosts.clear();
    let cases = doc["cases"].as_array().unwrap();
    let mut denied = 0;
    for case in cases {
        let d = guard::decide_value("hard-delete", &case["input"], &c);
        assert_eq!(d.decision, case["expect"].as_str().unwrap(), "{}", case["input"]);
        if d.is_deny() {
            denied += 1;
        }
    }
    assert!(denied > 0, "the fixture has no deny case");
    // The deny commands the older tests in this file pin, as one list.
    let home = "/var/tmp/fleet-guard-home/Projects/keep";
    let c = cfg();
    for cmd in [
        "rm -rf ~/important".to_string(),
        "rm -rf /tmp/../etc/passwd".to_string(),
        "find ~/Projects -delete".to_string(),
        "git clean -fd".to_string(),
        "python3 -c 'import os; os.remove(\"/tmp/x\")'".to_string(),
        "ssh other.example 'rm -rf /var/tmp/fleet-guard-home/file'".to_string(),
        "ssh worker 'rm -rf /tmp/x' && rm -rf ~/important".to_string(),
        "bash <<EOF\nrm -rf ~/important\nEOF\n".to_string(),
        "ssh bench 'rm -rf ~/x'".to_string(),
        format!("ssh other-mac rm -rf {home}"),
        format!("sudo -u root rm -rf {home}"),
        format!("timeout -s KILL 30 rm -rf {home}"),
        format!("heavy-slot -- rm -rf {home}"),
        format!("disk-slot /Volumes/Disk -- rm -rf {home}"),
        format!("ls | xargs -I {{}} rm -rf {home}/{{}}"),
        format!("bash -c 'rm -rf {home}'"),
        format!("zsh -lc \"rm -rf {home}\""),
        format!("eval 'rm -rf {home}'"),
    ] {
        assert!(decide("hard-delete", &cmd, &c).is_deny(), "flipped to allow: {cmd}");
    }
}

fn nested_sh(cmd: &str, layers: usize) -> String {
    let mut out = cmd.to_string();
    for _ in 0..layers {
        out = format!("sh -c '{}'", out.replace('\'', "'\\''"));
    }
    out
}

/// bash's `$'...'` quoting lets `\'` stand for a quote inside the string, so `$'\''` is one
/// complete word. A reader that does not know that sees a quote opening after it and takes the
/// rest of the line for text. Such a line is unreadable here, and unreadable is refused when the
/// line is about the guard's subject.
#[test]
fn ansi_c_quoting_is_unreadable_not_text() {
    let c = cfg();
    for (guard_name, cmd) in [
        ("disk-slot", r#"echo $'\''; cp /Volumes/A/x /Volumes/B/y; echo 'done"#),
        ("disk-slot", r#"echo $'\''; cp /Volumes/A/x /Volumes/B/y #'"#),
        // the wrapper covers only the command it wraps, and here nobody can say which that is
        ("disk-slot", r#"disk-slot /Volumes/A -- echo $'\''; cp /Volumes/A/x /Volumes/B/y #'"#),
        ("slot-holder", r#"echo $'\''; pkill -9 heavy-slot #'"#),
        ("slot-files", r#"echo $'\''; echo 3 > ~/.local/state/heavy-slot/limit #'"#),
        ("approval-files", r#"echo $'\''; touch APPROVE_EXAMPLE #'"#),
        ("idrive-cleanup", r#"agent-mail send host $'\''; perl archive_cleanup.pl #'"#),
        ("icloud", r#"agent-mail send host $'\''; rm -rf ~/.Trash/* #'"#),
        ("icloud", r#"agent-mail send host $'\''; trash ~/Documents #'"#),
        ("hard-delete", r#"grep -n $'\''; rm -rf /Users/agentkit-me/x #' notes.txt"#),
        ("hard-delete", r#"git commit -m $'\''; rm -rf /Users/agentkit-me/x #'"#),
    ] {
        assert!(decide(guard_name, cmd, &c).is_deny(), "{guard_name}: {cmd}");
    }
    // the quoting alone is not a reason to refuse
    for guard_name in ["disk-slot", "slot-holder", "slot-files", "approval-files", "idrive-cleanup", "icloud", "hard-delete"] {
        assert!(decide(guard_name, r#"printf $'a\tb\n'"#, &c).is_allow(), "{guard_name}: plain ANSI-C string");
    }
}

#[test]
fn a_command_too_deep_or_too_broken_to_read_is_denied_when_the_line_is_relevant() {
    let mut c = cfg();
    c.home = std::env::temp_dir().join(format!("agentkit-guard-unparsed-{}", std::process::id()));
    let _ = fs::remove_dir_all(&c.home);
    let queue = c.home.join(".local/state/heavy-slot/queue");
    fs::create_dir_all(&queue).unwrap();
    fs::write(queue.join("4242.json"), "{}\n").unwrap();
    for layers in [1, 3, 5, 6, 7, 9] {
        for (guard_name, cmd) in [
            ("disk-slot", "cp /Volumes/A/x /Volumes/B/y"),
            ("slot-holder", "pkill -9 heavy-slot"),
            ("slot-holder", "kill -9 4242"),
            ("slot-files", "echo 3 > ~/.local/state/heavy-slot/limit"),
            ("approval-files", "touch APPROVE_EXAMPLE"),
            ("idrive-cleanup", "perl archive_cleanup.pl"),
            ("icloud", "rm -rf ~/.Trash/*"),
        ] {
            let nested = nested_sh(cmd, layers);
            assert!(decide(guard_name, &nested, &c).is_deny(), "{guard_name}, {layers} layers: {nested}");
        }
        // nesting alone is not a reason to refuse
        for guard_name in ["disk-slot", "slot-holder", "slot-files", "approval-files", "idrive-cleanup", "icloud"] {
            let nested = nested_sh("echo hello", layers);
            assert!(decide(guard_name, &nested, &c).is_allow(), "{guard_name}, {layers} layers of echo");
        }
    }
    // a wrapped command stays allowed however deep it is carried
    assert!(decide("disk-slot", &format!("disk-slot /Volumes/A -- {}", nested_sh("cp /Volumes/A/x /Volumes/A/y", 9)), &c).is_allow());
    // an unbalanced quote: the rest of the line cannot be read
    each(
        "disk-slot",
        &c,
        &["ls 'x; cp /Volumes/A/x /Volumes/B/y", "sh -c \"ls 'x\"; cp /Volumes/A/x /Volumes/B/y"],
        &["ls \"/Volumes/My Disk\"", "echo \"it's fine\""],
    );
    each("approval-files", &c, &["true 'x; touch APPROVE_EXAMPLE"], &["echo \"it's fine\""]);
    let _ = fs::remove_dir_all(&c.home);
}

#[test]
fn disk_slot_judges_here_document_bodies_by_the_command_they_feed() {
    let c = cfg();
    each(
        "disk-slot",
        &c,
        &[
            "python3 - <<'EOF'\nimport shutil\nshutil.copy('/Volumes/A/x', '/Volumes/B/y')\nEOF",
            "bash <<EOF\ncp /Volumes/A/x /Volumes/B/y\nEOF",
            "source /dev/stdin <<EOF\ncp /Volumes/A/x /Volumes/B/y\nEOF",
            "cat <<EOF | sh\ncp /Volumes/A/x /Volumes/B/y\nEOF",
            "ssh host bash <<EOF\ncp /Volumes/A/x /Volumes/B/y\nEOF",
            "bash <<< 'cp /Volumes/A/x /Volumes/B/y'",
            "disk-slot /Volumes/A -- true\nbash <<EOF\ncp /Volumes/A/x /Volumes/B/y\nEOF",
        ],
        &[
            "disk-slot /Volumes/A -- bash <<EOF\ncp /Volumes/A/x /Volumes/A/y\nsync\nEOF",
            "disk-slot /Volumes/A -- python3 - <<'EOF'\nimport shutil\nshutil.copy('/Volumes/A/x', '/Volumes/A/y')\nEOF",
            "cat > ~/notes.md <<'EOF'\nthe copy is on /Volumes/A/x\nEOF",
            "git commit -m \"$(cat <<'EOF'\ndisk-slot: /Volumes/A/x needs the wrapper\nEOF\n)\"",
        ],
    );
}

#[test]
fn slot_holder_denies_a_sigkill_whose_target_is_computed_from_heavy_slot_s_state() {
    let c = cfg();
    each(
        "slot-holder",
        &c,
        &[
            "kill -9 $(cat ~/.local/state/heavy-slot/queue/*.json | head -1 | tr -dc 0-9)",
            "kill -9 `ls ~/.local/state/heavy-slot/queue | head -1 | cut -d. -f1`",
            "kill -KILL \"$(basename ~/.local/state/heavy-slot/queue/*.json .json)\"",
            "P=$(ls /var/tmp/fleet-guard-home/.local/state/heavy-slot/queue | head -1); kill -9 ${P%.json}",
            "PID=$(heavy-slot status | awk '/pid/ {print $5}'); kill -9 $PID",
            "ls ~/.local/state/heavy-slot/queue | cut -d. -f1 | xargs kill -9",
            "heavy-slot status | awk '{print $5}' | xargs -n1 kill -KILL",
            "ssh host 'kill -9 $(cat ~/.local/state/heavy-slot/queue/*.json | tr -dc 0-9)'",
            "for p in $(ls ~/.local/state/heavy-slot/queue); do kill -9 ${p%.json}; done",
        ],
        &[
            "kill -9 $SOME_PID",
            "kill -9 $(pgrep -f my-dev-server)",
            "kill -9 \"$(cat /tmp/server.pid)\"",
            "lsof -ti :3000 | xargs kill -9",
            "kill $(cat ~/.local/state/heavy-slot/queue/*.json | tr -dc 0-9)",
            "heavy-slot status; kill -9 5151",
        ],
    );
}

#[test]
fn text_another_program_would_run_is_read_as_a_command() {
    let mut c = cfg();
    c.home = std::env::temp_dir().join(format!("agentkit-guard-carried-{}", std::process::id()));
    let _ = fs::remove_dir_all(&c.home);
    fs::create_dir_all(&c.home).unwrap();
    each(
        "approval-files",
        &c,
        &[
            "echo 'touch APPROVE_EXAMPLE' | sh",
            "eval \"$(echo 'touch APPROVE_EXAMPLE')\"",
            "bash <<< 'touch APPROVE_EXAMPLE'",
            "source /dev/stdin <<EOF\ntouch APPROVE_EXAMPLE\nEOF",
            "eval \"$(cat <<EOF\ntouch APPROVE_EXAMPLE\nEOF\n)\"",
            "find . -name x -exec touch APPROVE_EXAMPLE \\;",
            "watch -n 60 touch APPROVE_EXAMPLE",
            "watch 'touch APPROVE_EXAMPLE'",
            "su root -c 'touch APPROVE_EXAMPLE'",
            "python3 -c 'import os; os.system(\"touch APPROVE_EXAMPLE\")'",
            "python3 - <<'EOF'\nimport subprocess\nsubprocess.run([\"touch\", \"APPROVE_EXAMPLE\"])\nEOF",
            "osascript -e 'do shell script \"touch ~/APPROVE_EXAMPLE\"'",
            "agent-mail send host \"hi\"; touch APPROVE_EXAMPLE",
            "agent-mail send host \"$(touch APPROVE_EXAMPLE)\"",
            "git commit -m 'x' && touch APPROVE_EXAMPLE",
            "git -c alias.z='!touch APPROVE_EXAMPLE' z",
            "rg --pre 'touch APPROVE_EXAMPLE' x",
        ],
        &[
            "echo 'touch APPROVE_EXAMPLE is for the owner'",
            "git commit -m 'approval-files: touch APPROVE_EXAMPLE is denied'",
            "git commit -m \"$(cat <<'EOF'\napproval-files: denied\n\ntouch APPROVE_EXAMPLE\nEOF\n)\"",
            "grep -rn 'touch APPROVE_EXAMPLE' docs/",
            "agent-mail send host \"do not touch APPROVE_EXAMPLE\"",
            "cat > notes.md <<'EOF'\ntouch APPROVE_EXAMPLE\nEOF",
        ],
    );
    each(
        "slot-holder",
        &c,
        &[
            "watch 'pkill -9 heavy-slot'",
            "echo 'pkill -9 heavy-slot' | sh",
            "python3 - <<'EOF'\nimport os\nos.system(\"pkill -9 heavy-slot\")\nEOF",
            "agent-mail send host \"hi\"; pkill -9 heavy-slot",
            "nohup sh -c 'sleep 1; killall -9 heavy-slot' &",
        ],
        &["agent-mail send host \"never pkill -9 heavy-slot\"", "echo 'never killall -9 heavy-slot'"],
    );
    each(
        "slot-files",
        &c,
        &[
            "su root -c 'echo 3 > ~/.local/state/heavy-slot/limit'",
            "echo 'echo 3 > ~/.local/state/heavy-slot/limit' | sh",
            "agent-mail send host \"hi\"; echo 3 > ~/.local/state/heavy-slot/limit",
            "bash <<EOF\necho 3 > ~/.local/state/heavy-slot/limit\nEOF",
        ],
        &["agent-mail send host \"limit is 3: cat ~/.local/state/heavy-slot/limit\""],
    );
    each(
        "idrive-cleanup",
        &c,
        &[
            "agent-mail send host \"hi\"; perl archive_cleanup.pl",
            "agent-mail send host \"hi\" && osascript -e 'tell application \"IDrive\" to archive cleanup'",
            "agent-mail send host \"$(perl archive_cleanup.pl)\"",
            "echo 'perl archive_cleanup.pl' | sh",
            "watch 'perl archive_cleanup.pl'",
        ],
        &["agent-mail send host \"do not run Archive Cleanup; grep the log\""],
    );
    each(
        "icloud",
        &c,
        &[
            "agent-mail send host \"hi\"; trash ~/Documents",
            "agent-mail send host \"hi\" && osascript -e 'tell application \"Finder\" to empty trash'",
            "agent-mail send host \"$(trash ~/Documents)\"",
            "agent-mail send host \"hi\"; ssh worker 'rm -rf ~/.Trash/*'",
            "ssh worker \"osascript -e 'do shell script \\\"rm -rf ~/.Trash/*\\\"'\"",
            "ssh worker 'echo \"rm -rf ~/.Trash/*\" | sh'",
            "ssh worker bash <<EOF\nrm -rf ~/.Trash/*\nEOF",
        ],
        &[
            "agent-mail send other \"please empty trash\"",
            "agent-mail send other \"do not trash ~/Documents\"",
        ],
    );
    let _ = fs::remove_dir_all(&c.home);
}

#[test]
fn a_file_tool_call_is_judged_by_its_path_alone() {
    let c = cfg();
    let all = |v: Value| guard::decide_all(&v, &c, guard::ALL_GUARD_NAMES);
    // The content is never read as a command, whatever it says.
    let content = "rm -rf ~/important\nrsync -a ~/x /Volumes/A/\ntouch APPROVE_EXAMPLE\npkill -9 heavy-slot\necho 3 > ~/.local/state/heavy-slot/limit\n";
    for tool in ["Write", "Edit", "MultiEdit", "NotebookEdit"] {
        let ok = json!({"tool_name": tool, "tool_input": {"file_path": "/var/tmp/fleet-guard-home/Projects/example/run.sh", "content": content, "new_string": content, "command": content}});
        assert!(all(ok).is_allow(), "{tool}");
    }
    // Only slot-files and approval-files answer for a file tool.
    let limit = json!({"tool_name": "Write", "tool_input": {"file_path": "/var/tmp/fleet-guard-home/.local/state/heavy-slot/limit", "content": "3"}});
    for name in guard::ALL_GUARD_NAMES {
        let d = guard::decide_value(name, &limit, &c);
        assert_eq!(d.is_deny(), *name == "slot-files", "{name}");
    }
    assert!(all(limit.clone()).is_deny());
    assert!(guard::decide_all(&limit, &c, &["hard-delete", "icloud", "approval-files"]).is_allow(), "a disabled guard still answered");
    let notebook = json!({"tool_name": "NotebookEdit", "tool_input": {"notebook_path": "/var/tmp/fleet-guard-home/x/APPROVE_EXAMPLE.ipynb"}});
    assert!(all(notebook).is_deny());
    // A missing or malformed path allows: a broken payload must not stall a session.
    for input in [
        json!({}),
        json!({"file_path": null}),
        json!({"file_path": 7}),
        json!({"file_path": ""}),
        json!({"file_path": ["/var/tmp/fleet-guard-home/.local/state/heavy-slot/limit"]}),
        json!({"file_path": {"path": "APPROVE_EXAMPLE"}}),
        json!("APPROVE_EXAMPLE"),
        json!(null),
    ] {
        let v = json!({"tool_name": "Write", "tool_input": input});
        assert!(all(v.clone()).is_allow(), "{v}");
    }
    assert!(all(json!({"tool_name": "Write"})).is_allow());
    // Other tools are not file writes, whatever their input names.
    let read = json!({"tool_name": "Read", "tool_input": {"file_path": "/var/tmp/fleet-guard-home/.local/state/heavy-slot/limit"}});
    assert!(all(read).is_allow());
}
