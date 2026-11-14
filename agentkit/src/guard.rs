//! Standalone guard decisions. Each one reads a shell command (Claude
//! `tool_input.command`, Cursor `command`, or a bare string) and answers
//! allow, ask, or deny. Parse failures allow, matching the hooks these
//! replace: a broken stdin must not become a new way to stall a session.
//! The deny rules themselves are not relaxed.

use crate::site::GuardConfig;

/// Every guard name agentkit knows. Site `guards.disabled` must be a subset of this list.
pub const ALL_GUARD_NAMES: &[&str] = &[
    "hard-delete",
    "icloud",
    "offload-heavy",
    "cd-outside",
    "push",
    "disk-slot",
    "metered-link",
    "agent-yolo",
    "test-audio",
];
use fancy_regex::Regex;
use serde_json::Value;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub decision: &'static str,
    pub rule: String,
    pub reason: String,
}

impl Decision {
    pub fn allow() -> Self {
        Self { decision: "allow", rule: String::new(), reason: String::new() }
    }
    pub fn deny(rule: &str, reason: impl Into<String>) -> Self {
        Self { decision: "deny", rule: rule.to_string(), reason: reason.into() }
    }
    pub fn ask(rule: &str, reason: impl Into<String>) -> Self {
        Self { decision: "ask", rule: rule.to_string(), reason: reason.into() }
    }
    pub fn is_allow(&self) -> bool { self.decision == "allow" }
    pub fn is_deny(&self) -> bool { self.decision == "deny" }
    pub fn is_ask(&self) -> bool { self.decision == "ask" }
}

pub fn decide_json(name: &str, raw: &str, cfg: &GuardConfig) -> Result<Decision, ()> {
    let value: Value = if raw.trim().is_empty() {
        Value::Null
    } else {
        serde_json::from_str(raw).map_err(|_| ())?
    };
    Ok(decide_value(name, &value, cfg))
}

/// Strictest of several guard decisions (`deny` > `ask` > `allow`), with every reason attached.
pub fn combine_decisions(decisions: impl IntoIterator<Item = Decision>) -> Decision {
    let mut denies = Vec::new();
    let mut asks = Vec::new();
    for d in decisions {
        if d.is_deny() {
            denies.push(d);
        } else if d.is_ask() {
            asks.push(d);
        }
    }
    if !denies.is_empty() {
        let reason = denies
            .iter()
            .map(|d| format!("[agentkit:{}] {}", d.rule, d.reason))
            .collect::<Vec<_>>()
            .join(" ");
        let rule = denies[0].rule.clone();
        return Decision::deny(&rule, reason);
    }
    if !asks.is_empty() {
        let reason = asks
            .iter()
            .map(|d| format!("[agentkit:{}] {}", d.rule, d.reason))
            .collect::<Vec<_>>()
            .join(" ");
        let rule = asks[0].rule.clone();
        return Decision::ask(&rule, reason);
    }
    Decision::allow()
}

pub fn decide_all(value: &Value, cfg: &GuardConfig, enabled: &[&str]) -> Decision {
    if !is_shell(value) {
        return Decision::allow();
    }
    let decisions = enabled.iter().map(|name| decide_value(name, value, cfg));
    combine_decisions(decisions)
}

pub fn decide_value(name: &str, value: &Value, cfg: &GuardConfig) -> Decision {
    let adjusted = call_dir(value, cfg);
    let cfg = adjusted.as_ref().unwrap_or(cfg);
    if !is_shell(value) {
        return Decision::allow();
    }
    let command = command_of(value);
    match name {
        "hard-delete" => hard_delete(&command, cfg),
        "icloud" => icloud(&command),
        "offload-heavy" => offload(&command, cfg),
        "cd-outside" => cd_outside(&command, cfg),
        "push" => push(&command, cfg),
        "disk-slot" => disk_slot(&command, cfg),
        "metered-link" => metered_link(&command, cfg),
        "agent-yolo" => agent_yolo(&command),
        "test-audio" => test_audio(&command),
        _ => Decision::allow(),
    }
}

fn is_shell(value: &Value) -> bool {
    match value.get("tool_name").and_then(|t| t.as_str()) {
        None => true,
        Some("Bash" | "Shell" | "shell" | "shell_command" | "local_shell" | "exec_command") => true,
        Some(_) => false,
    }
}

/// Relative targets use the tool call's `cwd` when the payload has one.
/// Cursor user hooks are started in `~/.cursor`, which is not the shell's directory.
fn call_dir(value: &Value, cfg: &GuardConfig) -> Option<GuardConfig> {
    let raw = value.get("cwd").and_then(|c| c.as_str()).filter(|s| !s.is_empty())?;
    let mut local = cfg.clone();
    local.cwd = PathBuf::from(expand_path(raw, cfg));
    Some(local)
}

fn command_of(value: &Value) -> String {
    if let Some(s) = value.get("command").and_then(|c| c.as_str()) {
        return s.to_string();
    }
    value
        .get("tool_input")
        .and_then(|t| t.get("command"))
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .to_string()
}

pub fn format_neutral(d: &Decision) -> String {
    serde_json::json!({
        "decision": d.decision,
        "rule": d.rule,
        "reason": d.reason,
    })
    .to_string()
}

pub fn format_claude(d: &Decision) -> String {
    if d.is_allow() {
        return String::new();
    }
    serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": d.decision,
            "permissionDecisionReason": format!("[agentkit:{}] {}", d.rule, d.reason),
        }
    })
    .to_string()
}

pub fn format_cursor(d: &Decision) -> String {
    serde_json::json!({
        "permission": d.decision,
        "agent_message": if d.is_allow() { String::new() } else { format!("[agentkit:{}] {}", d.rule, d.reason) },
        "user_message": if d.is_deny() { format!("Blocked ({}).", d.rule) } else { String::new() },
    })
    .to_string()
}

struct Rex(&'static Regex);

impl Rex {
    fn is_match(&self, text: &str) -> bool {
        self.0.is_match(text).unwrap_or(false)
    }
    fn split<'a>(&self, text: &'a str) -> Vec<&'a str> {
        let mut out = Vec::new();
        let mut last = 0;
        for found in self.0.find_iter(text).flatten() {
            out.push(&text[last..found.start()]);
            last = found.end();
        }
        out.push(&text[last..]);
        out
    }
    fn replace(&self, text: &str, rep: &str) -> String {
        self.0.replace(text, rep).into_owned()
    }
    fn replace_all(&self, text: &str, rep: &str) -> String {
        self.0.replace_all(text, rep).into_owned()
    }
    fn captures<'t>(&self, text: &'t str) -> Option<fancy_regex::Captures<'t>> {
        self.0.captures(text).ok().flatten()
    }
    fn captures_iter<'t>(&self, text: &'t str) -> Vec<fancy_regex::Captures<'t>> {
        self.0.captures_iter(text).filter_map(|c| c.ok()).collect()
    }
}

fn compiled(pattern: &str) -> Rex {
    static CACHE: OnceLock<std::sync::Mutex<std::collections::HashMap<String, &'static Regex>>> =
        OnceLock::new();
    let cache = CACHE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    if let Some(existing) = cache.lock().expect("regex cache").get(pattern).copied() {
        return Rex(existing);
    }
    let owned: &'static Regex = Box::leak(Box::new(Regex::new(pattern).expect("guard regex")));
    cache.lock().expect("regex cache").insert(pattern.to_string(), owned);
    Rex(owned)
}

const TEMP_ROOTS: &[&str] = &["/tmp/", "/private/tmp/", "/var/folders/", "/private/var/folders/"];
const DELETERS: &[&str] = &["rm", "rmdir", "unlink", "shred", "srm"];
const WRAPPERS: &[&str] = &[
    "sudo", "command", "builtin", "nohup", "time", "env", "xargs", "exec", "nice", "timeout",
    "caffeinate", "heavy-slot", "disk-slot", "power-pause", "ionice", "taskpolicy", "stdbuf",
];
/// Flags of a wrapper that take a separate value. Dropping the flag without its value leaves
/// the value where the program name should be, and the deleter behind it is never examined.
const WRAPPER_VALUE_FLAGS: &[(&str, &[&str])] = &[
    ("sudo", &["-u", "-g", "-C", "-h", "-p", "-U", "-D", "-R", "-T", "-r", "-t"]),
    ("env", &["-u", "-C", "-S", "-P"]),
    ("nice", &["-n"]),
    ("xargs", &["-n", "-I", "-L", "-P", "-s", "-E", "-d", "-a", "-J", "-R"]),
    ("timeout", &["-s", "-k"]),
    ("caffeinate", &["-t", "-w"]),
    ("ionice", &["-c", "-n", "-p"]),
    ("taskpolicy", &["-c", "-t", "-l", "-p"]),
    ("stdbuf", &["-i", "-o", "-e"]),
    ("disk-slot", &["--wait", "--label"]),
    ("heavy-slot", &["--label"]),
];
/// Wrappers whose first plain argument is their own (a duration, a path), not the command.
const WRAPPER_POSITIONAL: &[&str] = &["timeout", "disk-slot"];
const SHELLS: &[&str] = &["sh", "bash", "zsh", "dash", "ksh", "fish"];
const KEYWORDS: &[&str] = &[
    "then", "do", "else", "elif", "if", "while", "until", "!", "{", "}", "case", "in",
];

fn hard_delete(cmd: &str, cfg: &GuardConfig) -> Decision {
    let cmd = strip_file_heredocs(cmd);
    if let Some((host, rest)) = ssh_parts(&cmd) {
        if cfg.is_air(&host) && rest.len() == 1 {
            return Decision::allow();
        }
        // ssh joins its trailing arguments with spaces before the remote shell sees them, so
        // `ssh host rm -rf dir` is the same command as `ssh host 'rm -rf dir'`.
        if !cfg.is_air(&host) && !rest.is_empty() {
            if let Decision { decision: "deny", reason, .. } = hard_delete(&rest.join(" "), cfg) {
                return Decision::deny(
                    "hard-delete",
                    format!("{reason} (over ssh to {host})"),
                );
            }
        }
    }
    let inline = compiled(
        r"shutil\.rmtree|os\.(remove|unlink|rmdir|removedirs)\s*\(|Path\([^)]*\)\.unlink|\.unlink\(\s*(missing_ok)?|fs\.(rm|rmSync|unlink|unlinkSync|rmdir|rmdirSync)\s*\(",
    );
    if inline.is_match(&cmd) {
        return Decision::deny(
            "hard-delete",
            "BLOCKED hard delete: inline script delete (shutil.rmtree / os.remove / fs.rm ...). Move it to the Trash with `trash <path>`. Hard deletes are allowed only inside /tmp, /private/tmp, /var/folders, $TMPDIR or a scratchpad directory.",
        );
    }
    for seg in segments(&cmd) {
        let mut argv = shlex::split(seg).unwrap_or_else(|| seg.split_whitespace().map(str::to_string).collect());
        while !argv.is_empty()
            && (is_assignment(&argv[0]) || WRAPPERS.contains(&base_name(&argv[0])) || KEYWORDS.contains(&argv[0].as_str()))
        {
            let wrapper = base_name(&argv[0]).to_string();
            argv.remove(0);
            let value_flags = WRAPPER_VALUE_FLAGS
                .iter()
                .find(|(name, _)| *name == wrapper)
                .map(|(_, flags)| *flags)
                .unwrap_or(&[]);
            while !argv.is_empty() && argv[0].starts_with('-') && argv[0] != "--" {
                let flag = argv.remove(0);
                if value_flags.contains(&flag.as_str()) && !argv.is_empty() {
                    argv.remove(0);
                }
            }
            if WRAPPER_POSITIONAL.contains(&wrapper.as_str())
                && !argv.is_empty()
                && argv[0] != "--"
            {
                argv.remove(0);
            }
            if !argv.is_empty() && argv[0] == "--" {
                argv.remove(0);
            }
        }
        if argv.is_empty() {
            continue;
        }
        let prog = Path::new(&argv[0])
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(&argv[0])
            .to_string();
        // `sh -c '...'` and `eval '...'` carry a whole command in one argument.
        if SHELLS.contains(&prog.as_str()) {
            let mut inner = None;
            let mut i = 1;
            while i < argv.len() {
                let a = &argv[i];
                if a.starts_with('-') && !a.starts_with("--") && a.contains('c') {
                    inner = argv.get(i + 1).cloned();
                    break;
                }
                if !a.starts_with('-') {
                    break;
                }
                i += 1;
            }
            if let Some(inner) = inner {
                let d = hard_delete(&inner, cfg);
                if d.is_deny() {
                    return d;
                }
            }
        } else if prog == "eval" && argv.len() > 1 {
            let d = hard_delete(&argv[1..].join(" "), cfg);
            if d.is_deny() {
                return d;
            }
        }
        if DELETERS.contains(&prog.as_str()) {
            let targets: Vec<&str> = argv[1..].iter().filter(|a| !a.starts_with('-')).map(String::as_str).collect();
            if targets.is_empty() || !targets.iter().all(|t| is_temp(t, cfg)) {
                return Decision::deny(
                    "hard-delete",
                    format!("BLOCKED hard delete: `{prog}` on a non-temp path. Move it to the Trash with `trash <path>`. Hard deletes are allowed only inside /tmp, /private/tmp, /var/folders, $TMPDIR or a scratchpad directory."),
                );
            }
        } else if prog == "find"
            && (argv.iter().any(|a| a == "-delete")
                || argv.windows(2).any(|w| {
                    w[0] == "-exec" && DELETERS.contains(&Path::new(&w[1]).file_name().and_then(|s| s.to_str()).unwrap_or(""))
                }))
        {
            let root = argv[1..]
                .iter()
                .find(|a| !a.starts_with('-') && !a.starts_with('('));
            if root.is_none_or(|r| !is_temp(r, cfg)) {
                return Decision::deny(
                    "hard-delete",
                    "BLOCKED hard delete: `find -delete` / `find -exec rm` outside a temp dir. Move paths to the Trash instead.",
                );
            }
        } else if prog == "git"
            && argv.iter().any(|a| a == "clean")
            && argv.iter().any(|a| a.starts_with('-') && a.trim_start_matches('-').contains('f'))
        {
            return Decision::deny(
                "hard-delete",
                "BLOCKED hard delete: `git clean -f` permanently deletes untracked files. Move them to the Trash instead.",
            );
        }
    }
    Decision::allow()
}

fn base_name(token: &str) -> &str {
    Path::new(token).file_name().and_then(|s| s.to_str()).unwrap_or(token)
}

fn is_assignment(token: &str) -> bool {
    compiled(r"^[A-Za-z_][A-Za-z0-9_]*=").is_match(token)
}

fn segments(cmd: &str) -> Vec<&str> {
    compiled(r"&&|\|\||;|\||\n|\$\(|`|\(|\)")
        .split(cmd)
        .into_iter()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect()
}

fn is_temp(path: &str, cfg: &GuardConfig) -> bool {
    let mut roots: Vec<String> = TEMP_ROOTS.iter().map(|s| (*s).to_string()).collect();
    if let Some(tmp) = &cfg.tmpdir {
        let mut real = resolve_path(&tmp.to_string_lossy(), cfg);
        if !real.ends_with('/') {
            real.push('/');
        }
        roots.push(real);
    }
    let mut resolved = resolve_path(&expand_path(path, cfg), cfg);
    if !resolved.ends_with('/') {
        resolved.push('/');
    }
    roots.iter().any(|r| resolved.starts_with(r)) || resolved.contains("/scratchpad/")
}

fn expand_path(path: &str, cfg: &GuardConfig) -> String {
    let trimmed = path.trim().trim_matches(|c| c == '\'' || c == '"');
    if let Some(rest) = trimmed.strip_prefix("~/") {
        return cfg.home.join(rest).to_string_lossy().into_owned();
    }
    if trimmed == "~" {
        return cfg.home.to_string_lossy().into_owned();
    }
    trimmed.to_string()
}

fn resolve_path(path: &str, cfg: &GuardConfig) -> String {
    let raw = if path.starts_with('/') {
        PathBuf::from(path)
    } else {
        cfg.cwd.join(path)
    };
    let logical = logical_normalize(&raw);
    canonicalize_prefix(&logical).to_string_lossy().into_owned()
}

fn logical_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::RootDir => out.push("/"),
            Component::CurDir => {}
            Component::ParentDir => { out.pop(); }
            Component::Normal(s) => out.push(s),
            Component::Prefix(p) => out.push(p.as_os_str()),
        }
    }
    out
}

fn canonicalize_prefix(path: &Path) -> PathBuf {
    let mut acc = PathBuf::new();
    let mut pending: Vec<Component> = Vec::new();
    let mut seen_missing = false;
    for c in path.components() {
        if seen_missing {
            pending.push(c);
            continue;
        }
        let mut next = acc.clone();
        next.push(c);
        if next.exists() {
            acc = std::fs::canonicalize(&next).unwrap_or(next);
        } else {
            seen_missing = true;
            pending.push(c);
        }
    }
    for c in pending {
        acc.push(c);
    }
    acc
}

fn find_heredoc(line: &str) -> Option<(usize, usize, String)> {
    // No backreference: the regex crate does not support them. Quoted and bare
    // delimiters are separate alternatives, quoted first.
    let heredoc = compiled(
        r#"(<<-?\s*)(?:'([A-Za-z_][A-Za-z0-9_]*)'|"([A-Za-z_][A-Za-z0-9_]*)"|([A-Za-z_][A-Za-z0-9_]*))"#,
    );
    let caps = heredoc.captures(line)?;
    let full = caps.get(0)?;
    let delim = caps.get(2).or_else(|| caps.get(3)).or_else(|| caps.get(4))?;
    Some((full.start(), full.end(), delim.as_str().to_string()))
}

fn strip_file_heredocs(cmd: &str) -> String {
    let lines: Vec<&str> = cmd.split('\n').collect();
    let mut out: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        out.push(line);
        if let Some((start, end, delim)) = find_heredoc(line) {
            let head = &line[..start];
            let tail = &line[end..];
            let writes_file = compiled(r"(^|[;&|(]\s*)(cat|tee)\b").is_match(head.trim())
                && !compiled(r"\|\s*((ba|z)?sh|python3?|node|ruby|perl|osascript)\b").is_match(tail);
            if writes_file {
                let mut j = i + 1;
                while j < lines.len() && lines[j].trim() != delim {
                    j += 1;
                }
                if j < lines.len() {
                    out.push(lines[j]);
                }
                i = j;
            }
        }
        i += 1;
    }
    out.join("\n")
}

const SSH_OPTS_WITH_ARG: &str = "BbcDEeFIiJLlmOoPpQRSWw";

fn ssh_parts(cmd: &str) -> Option<(String, Vec<String>)> {
    let argv = shlex::split(cmd)?;
    if argv.is_empty() {
        return None;
    }
    let base = Path::new(&argv[0]).file_name()?.to_str()?;
    if base != "ssh" {
        return None;
    }
    let mut i = 1;
    while i < argv.len() && argv[i].starts_with('-') {
        let flag = &argv[i];
        i += if flag.chars().count() == 2 && SSH_OPTS_WITH_ARG.contains(flag.chars().nth(1)?) {
            2
        } else {
            1
        };
    }
    if i >= argv.len() {
        return None;
    }
    Some((argv[i].clone(), argv[i + 1..].to_vec()))
}

fn icloud(cmd: &str) -> Decision {
    if cmd.contains("ICLOUD_WIPE_APPROVED=1") {
        return Decision::allow();
    }
    if compiled(r"^\s*(?:\S*/)?agent-mail\s+send\b").is_match(cmd) {
        return Decision::allow();
    }
    let root_names = r"(?:Documents|Desktop|Library/Mobile Documents/com~apple~CloudDocs|Library/Mobile\\ Documents/com~apple~CloudDocs)";
    let users_home: String = ['/', 'U', 's', 'e', 'r', 's', '/'].iter().collect();
    let prefix = format!(r#"(?:~/|\$HOME/|\$\{{HOME\}}/|{users_home}[^/\s\"']+/)"#);
    let end = r#"(?=/?\*?(?:["'\s;)|&]|$))"#;
    let root_ref = compiled(&format!(r#"[\"']?{prefix}{root_names}{end}"#));
    let bare_ref = compiled(&format!(
        r#"(?:(?<=[\s\"'])|^)(?:Documents|Desktop){end}"#
    ));
    let has_root = |text: &str| root_ref.is_match(text) || bare_ref.is_match(text);
    if cmd.contains("osascript") && compiled(r"(?i)\bempty\s+(?:the\s+)?trash\b").is_match(cmd) {
        return Decision::deny(
            "icloud",
            "BLOCKED iCloud-wide delete risk: emptying the Trash is permanent. On a Mac with iCloud Desktop & Documents it also purges iCloud Recently Deleted. Empty it only with the owner's go-ahead, then prefix ICLOUD_WIPE_APPROVED=1.",
        );
    }
    let loop_re = compiled(r"\bfor\s+([A-Za-z_][A-Za-z0-9_]*)\s+in\s+([^;\n]*)");
    let tainted: Vec<String> = loop_re
        .captures_iter(cmd)
        .into_iter()
        .filter(|c| has_root(c.get(2).map(|m| m.as_str()).unwrap_or("")))
        .map(|c| c.get(1).map(|m| m.as_str().to_string()).unwrap_or_default())
        .collect();
    let verb = compiled(
        r#"(?:^|[\s;&|(`"'])(?:sudo\s+)?(?:mv|trash|rm|rmdir|srm|unlink)\s|-exec\s+(?:mv|rm|trash)\b|\s-delete\b"#,
    );
    let rsync_delete = compiled(r"\brsync\b[^;&|]*--delete");
    let seps = compiled(r"&&|\|\||;|\n|\|");
    for seg in seps.split(cmd) {
        let verb_hit = verb.is_match(seg);
        let rsync_hit = rsync_delete.is_match(seg);
        if !verb_hit && !rsync_hit {
            continue;
        }
        if rsync_hit && !verb_hit {
            let last = seg.split_whitespace().last().unwrap_or("");
            if has_root(&format!(" {last}")) {
                return Decision::deny(
                    "icloud",
                    "BLOCKED iCloud-wide delete risk: `rsync --delete` into a whole Desktop/Documents/iCloud Drive folder. Sync a subfolder, or prefix ICLOUD_WIPE_APPROVED=1 after confirming iCloud Desktop & Documents is off.",
                );
            }
            continue;
        }
        if has_root(seg) && !loop_re.is_match(seg) {
            return Decision::deny(
                "icloud",
                "BLOCKED iCloud-wide delete risk: moving or deleting a whole Desktop/Documents/iCloud Drive folder or all its contents. Move single items. A whole-folder wipe needs iCloud Desktop & Documents confirmed off and ICLOUD_WIPE_APPROVED=1.",
            );
        }
        for v in &tainted {
            let pat = format!(r"\$\{{?{v}\b");
            if compiled(&pat).is_match(seg) {
                return Decision::deny(
                    "icloud",
                    "BLOCKED iCloud-wide delete risk: moving or deleting a whole Desktop/Documents folder inside a loop. Move single items instead.",
                );
            }
        }
    }
    Decision::allow()
}

fn offload(cmd: &str, cfg: &GuardConfig) -> Decision {
    if !heavy(cmd) || compiled(r"\bremote-run\b|\bssh\b|\bheavy-slot\b|HEAVY_SLOT_LOCAL=1").is_match(cmd) {
        return Decision::allow();
    }
    // Probe only now, for a heavy command: before 12 Nov 2026 the probe ran while loading the
    // configuration, so every shell command paid for it and stalled when the worker was slow.
    let worker = match &cfg.offload_worker {
        Some(worker) => worker.clone(),
        None => crate::site::probe_offload(&cfg.home).unwrap_or_default(),
    };
    if worker.is_empty() {
        return Decision::allow();
    }
    Decision::deny(
        "offload-heavy",
        format!("This machine should stay cool, and remote-run worker '{worker}' can take the job. Use `remote-run -- <command>`. Only if the job truly needs this machine, prefix HEAVY_SLOT_LOCAL=1 and say why."),
    )
}

fn heavy(cmd: &str) -> bool {
    let heavy_re = compiled(
        r"^(\S*/)?(tests\.sh|build\.sh|sanitize\.sh|xcodebuild|swift\s+(build|test)|cargo\s+(build|test)|dotnet\s+(build|test)|npm\s+(run\s+)?(test|build)|npx\s+(vitest|playwright|next\s+build|jest)|vitest|playwright\s+test|pytest|(\S*/)?python3?(\.\d+)?\s+(-m\s+pytest|\S*native-gate\.py\s+run|\S*run-python-tests\.py)|clang\b.*\bTests/test_\S*.*)(\s|$)",
    );
    let segs = compiled(r"&&|\|\||[;&|\n]|\$\(|`");
    let lead = compiled(r"^(\s*(\w+=\S*|time|nohup|caffeinate(\s+-\w+)*|env|exec|then|do|else|\(|\{)\s+)+");
    for seg in segs.split(cmd) {
        let seg = lead.replace(seg.trim(), "");
        if heavy_re.is_match(seg.trim()) {
            return true;
        }
    }
    false
}

fn cd_outside(cmd: &str, cfg: &GuardConfig) -> Decision {
    for email in &cfg.email_substrings {
        if !email.is_empty() && cmd.contains(email) {
            return Decision::deny(
                "email-in-command",
                "The owner's address is in this command and would raise a privacy confirmation. Remove it.",
            );
        }
    }
    let local = compiled(r"'[^']*'").replace_all(cmd, "''");
    let cd_re = compiled(r#"(?:^|[;&|(]\s*)cd\s+("?)([^"\s;&|)]+)\1"#);
    for caps in cd_re.captures_iter(&local) {
        let raw = caps.get(2).map(|m| m.as_str()).unwrap_or("");
        if raw.starts_with('$') || (!raw.starts_with('/') && !raw.starts_with('~')) {
            continue;
        }
        let target = if raw == "~" {
            cfg.home.clone()
        } else if let Some(rest) = raw.strip_prefix("~/") {
            cfg.home.join(rest)
        } else {
            PathBuf::from(raw)
        };
        let target = logical_normalize(&target);
        let text = target.to_string_lossy();
        let allowed = cfg.allowed_cd.iter().any(|root| {
            let root = logical_normalize(root);
            let root_s = root.to_string_lossy();
            text == root_s || text.starts_with(&(root_s.to_string() + "/"))
        });
        if !allowed {
            return Decision::deny(
                "cd-outside",
                format!("`cd {raw}` leaves the allowed working directories and can raise a permission prompt. Use an absolute path, `git -C <dir>`, or `npm --prefix <dir>`."),
            );
        }
    }
    Decision::allow()
}

fn escape_literal(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if !c.is_ascii_alphanumeric() && c != '_' {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn push(cmd: &str, cfg: &GuardConfig) -> Decision {
    let flat = compiled(r"\s+").replace_all(cmd.trim(), " ");
    let git_opt = compiled(
        r#"\bgit(?:\s+(?:-[Cc]\s+(?:"[^"]*"|'[^']*'|\S+)|--(?:git-dir|work-tree|namespace|exec-path|config-env)(?:=|\s+)(?:"[^"]*"|'[^']*'|\S+)|--[a-z][\w-]*(?:=(?:"[^"]*"|'[^']*'|\S+))?|-[pP]))+(?=\s+[a-z])"#,
    );
    let c = git_opt.replace_all(&flat, "git");
    if !compiled(r"\bgit\s+push\b").is_match(&c) {
        return Decision::allow();
    }
    if compiled(r"\bgit\s+push\b[^|;&]*(--force(?!-with-lease)\b|(?:^|\s)-[A-Za-z]*f[A-Za-z]*(?=\s|$))").is_match(&c)
    {
        return Decision::deny(
            "force-push",
            "Force-push rewrites shared remote history. Use `--force-with-lease` only with the owner's approval; a plain -f or --force stays denied.",
        );
    }
    // `+ref`, `+src:dst` and --mirror overwrite the remote exactly as --force does.
    if compiled(r"\bgit\s+push\b[^|;&]*\s(?:\+[^\s|;&]+|--mirror\b)").is_match(&c) {
        return Decision::deny(
            "force-push",
            "A `+refspec` or `--mirror` push overwrites remote history, the same as --force. Use `--force-with-lease` only with the owner's approval.",
        );
    }
    // Deleting a remote branch is routine but not undoable from here: confirm it.
    if compiled(r"\bgit\s+push\b[^|;&]*\s(?:--delete\b|-d\b|:[^\s|;&]+)").is_match(&c) {
        return Decision::ask(
            "push-delete",
            "This push deletes a branch or tag on the remote. Confirm it is merged or backed up.",
        );
    }
    if compiled(r"--force-with-lease\b").is_match(&c) {
        return Decision::ask(
            "force-with-lease",
            "Force-with-lease still rewrites the remote branch. Confirm this is your branch and the owner approved.",
        );
    }
    let mut markers = cfg.push_markers.clone();
    if !markers.iter().any(|m| m == "AGENTKIT_PUSH_APPROVED=1") {
        markers.push("AGENTKIT_PUSH_APPROVED=1".into());
    }
    for marker in &markers {
        let pat = format!(r"(^|[\s;&|]){}\b", escape_literal(marker));
        if compiled(&pat).is_match(&c) || compiled(&pat).is_match(&flat) {
            return Decision::allow();
        }
    }
    if !push_targets_gated_project(cmd, cfg) {
        return Decision::allow();
    }
    Decision::ask(
        "push",
        "A push can publish or spend a build. Confirm the project's gate is green and the owner approved this push, then prefix AGENTKIT_PUSH_APPROVED=1.",
    )
}

fn push_targets_gated_project(cmd: &str, cfg: &GuardConfig) -> bool {
    if cfg.push_gate_projects.is_empty() && cfg.push_gate_remotes.is_empty() {
        return false;
    }
    let mut dirs = vec![cfg.cwd.clone()];
    dirs.extend(git_option_paths(cmd, cfg));
    let mut check = Vec::new();
    for d in dirs {
        check.extend(repository_locations(&d));
    }
    check.iter().any(|p| {
        path_under_any_gate(p, &cfg.push_gate_projects) || remote_is_gated(p, &cfg.push_gate_remotes)
    })
}

/// True when the repository at `location` (a working dir, a `.git` dir or a common dir) has a
/// remote whose URL names one of the gated `owner/name` repositories. Read from the config
/// file; no subprocess. `owner/app` does not match `owner/app-open`.
fn remote_is_gated(location: &Path, remotes: &[String]) -> bool {
    if remotes.is_empty() {
        return false;
    }
    let config = if location.join("config").is_file() && location.join("objects").is_dir() {
        location.join("config")
    } else {
        location.join(".git").join("config")
    };
    let Ok(text) = fs::read_to_string(&config) else {
        return false;
    };
    text.lines().any(|line| {
        let Some(url) = line.trim().strip_prefix("url") else {
            return false;
        };
        let url = url.trim_start().trim_start_matches('=').trim();
        let url = url.strip_suffix(".git").unwrap_or(url).trim_end_matches('/');
        remotes.iter().any(|gate| {
            let gate = gate.trim().trim_end_matches(".git").trim_matches('/');
            !gate.is_empty()
                && url.ends_with(gate)
                && url[..url.len() - gate.len()].ends_with(|c| c == '/' || c == ':')
        })
    })
}

fn path_under_any_gate(path: &Path, gates: &[PathBuf]) -> bool {
    let here = logical_normalize(path);
    let here_s = here.to_string_lossy();
    gates.iter().any(|gate| {
        let root = logical_normalize(gate);
        let root_s = root.to_string_lossy();
        here_s == root_s || here_s.starts_with(&format!("{root_s}/"))
    })
}

fn git_option_paths(cmd: &str, cfg: &GuardConfig) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let grab = |caps: fancy_regex::Captures| -> Option<String> {
        caps.get(1)
            .or_else(|| caps.get(2))
            .or_else(|| caps.get(3))
            .map(|m| m.as_str().to_string())
    };
    for pattern in [
        r#"\bgit(?:\s+\S+)*\s+-[Cc]\s+(?:"([^"]+)"|'([^']+)'|(\S+))"#,
        r#"\bgit(?:\s+\S+)*\s+--git-dir(?:=|\s+)(?:"([^"]+)"|'([^']+)'|(\S+))"#,
        r#"\bgit(?:\s+\S+)*\s+--work-tree(?:=|\s+)(?:"([^"]+)"|'([^']+)'|(\S+))"#,
        // `cd <project> && git push`: the push runs where the cd went, not in the session's cwd.
        r#"(?:^|[;&|(]\s*)cd\s+(?:"([^"]+)"|'([^']+)'|([^\s;&|)]+))"#,
    ] {
        let re = compiled(pattern);
        for caps in re.captures_iter(cmd) {
            if let Some(raw) = grab(caps) {
                let resolved = resolve_to_pathbuf(&raw, cfg);
                paths.push(resolved);
            }
        }
    }
    paths
}

fn resolve_to_pathbuf(raw: &str, cfg: &GuardConfig) -> PathBuf {
    PathBuf::from(resolve_path(&expand_path(raw, cfg), cfg))
}

/// Working dirs and main-repo roots reachable from a path (linked worktrees included).
fn repository_locations(start: &Path) -> Vec<PathBuf> {
    let start = if start.is_file() {
        start.parent().map(Path::to_path_buf).unwrap_or_else(|| start.to_path_buf())
    } else {
        start.to_path_buf()
    };
    let mut locs = vec![start.clone()];
    let Some(git_meta) = locate_git_meta(&start) else {
        return locs;
    };
    let repo_root = git_meta.parent().unwrap_or(&start);
    if let Some(common) = read_git_common_dir(repo_root, &git_meta) {
        locs.push(common.clone());
        if common.file_name().and_then(|s| s.to_str()) == Some(".git") {
            if let Some(parent) = common.parent() {
                locs.push(parent.to_path_buf());
            }
        }
    }
    locs
}

fn locate_git_meta(start: &Path) -> Option<PathBuf> {
    let mut cur = start.to_path_buf();
    loop {
        let git = cur.join(".git");
        if git.is_dir() || git.is_file() {
            return Some(git);
        }
        if !cur.pop() {
            break;
        }
    }
    None
}

fn read_git_common_dir(repo_root: &Path, git_meta: &Path) -> Option<PathBuf> {
    if git_meta.is_dir() {
        return Some(git_meta.to_path_buf());
    }
    let text = fs::read_to_string(git_meta).ok()?;
    let gitdir = text.lines().find_map(|l| l.strip_prefix("gitdir: ").map(str::trim))?;
    let gitdir = PathBuf::from(gitdir);
    let gitdir = if gitdir.is_absolute() {
        gitdir
    } else {
        repo_root.join(gitdir)
    };
    let commondir = gitdir.join("commondir");
    if commondir.is_file() {
        let rel = fs::read_to_string(&commondir).ok()?.trim().to_string();
        let common = gitdir.join(rel);
        return Some(logical_normalize(&common));
    }
    Some(gitdir)
}

fn disk_slot(cmd: &str, cfg: &GuardConfig) -> Decision {
    if compiled(r"\bdisk-slot\b").is_match(cmd) {
        return Decision::allow();
    }
    let hits_volume = compiled(r#"(?i)(?:^|[\s"'=(])/(?:Volumes|private/Volumes)/"#).is_match(cmd)
        || compiled(r#"\s~?/?Volumes/"#).is_match(cmd);
    if !hits_volume {
        return Decision::allow();
    }
    let exempt = compiled(
        r"\b(ls|stat|df|du|diskutil|test|disk-slot\s+(status|which))\b",
    );
    for seg in compiled(r"&&|\|\||;|\n|\|").split(cmd) {
        let seg = seg.trim();
        if !compiled(r#"/Volumes/"#).is_match(seg) {
            continue;
        }
        if exempt.is_match(seg) {
            continue;
        }
        return Decision::deny(
            "disk-slot",
            "Disk work on /Volumes/* must run as `disk-slot PATH [PATH ...] -- <command>` so one physical disk is not thrashed.",
        );
    }
    let _ = cfg;
    Decision::allow()
}

fn metered_link(cmd: &str, cfg: &GuardConfig) -> Decision {
    let marker = cfg.home.join(".local/state/METERED_LINK");
    if !marker.is_file() {
        return Decision::allow();
    }
    if compiled(r"(?i)(REMOTE_RUN_ALLOW_METERED=1|METERED_TRANSFER_APPROVED=1)").is_match(cmd) {
        return Decision::allow();
    }
    // A transfer tool only counts where this machine's shell would run it. Here-document
    // bodies and quoted arguments are text (a search pattern, a note, a script body, the
    // remote half of an ssh command), except the quoted script of `sh -c`.
    let heredoc = compiled(r#"(?s)<<-?\s*["']?(\w+)["']?[^\n]*\n.*?\n\s*\1\b"#);
    // A here-document fed to a shell is a script this machine runs: keep its body.
    let shell_heredoc = compiled(r"(?m)(?:^|[;&|(]\s*)(?:\S*/)?(?:ba|z|da)?sh\b[^\n;&|]*<<");
    let text = if shell_heredoc.is_match(cmd) { cmd.to_string() } else { heredoc.replace_all(cmd, "<<HEREDOC") };
    let text = compiled(r#"(\bssh\s[^;&|\n"']*)"[^"]*""#).replace_all(&text, "$1\"\"");
    let text = compiled(r#"(?<!c\s)(?<!eval\s)'[^']*'"#).replace_all(&text, "''");
    let text = compiled(r#"(?<!c\s)(?<!eval\s)"[^"`$]*""#).replace_all(&text, "\"\"");
    const POSITION: &str = r#"(?m)(?:^|[;&|(`]|\$\(|\s--\s|\beval\s+["']?|\b(?:ba|z|da)?sh\s+(?:-\w+\s+)*-\w*c\s+["'])\s*(?:(?:sudo|nohup|exec|command|time|caffeinate(?:\s+-\w+)*|timeout\s+\S+|xargs(?:\s+-\S+)*)\s+|env\s+(?:-\S+\s+)*(?:\w+=\S*\s+)*)*(?:\w+=\S*\s+)*(?:\S*/)?"#;
    let transfer = compiled(&format!(
        r"{POSITION}(rsync|scp|sftp|wget|aws\s+s3\s+(?:cp|sync|mv)|gcloud\s+storage\s+cp|azcopy|tar\s+-c[f]?|docker\s+(?:pull|push)|npm\s+ci|pnpm\s+install|yarn\s+install)\b"
    ));
    // curl to standard output is a small request (a status check, an API call). It is a
    // transfer when it saves, resumes or uploads a file, or feeds an unpacker.
    let curl = compiled(&format!(r"{POSITION}curl\b((?:>&|[^;&|\n])*)((?:\|(?:>&|[^;&|\n])*)*)"));
    let curl_file = compiled(
        r#"(?:^|\s)(?:-[A-Za-z]*[OCTF]\b|--remote-name\S*|--continue-at|--upload-file|--form\b|(?:-d|--data\S*|--json)\s*["']?@|(?:-[A-Za-z]*o|--output)[=\s]\s*(?!/dev/null)\S)|(?<![0-9&>])>>?\s*(?!/dev/null|&)\S"#,
    );
    // A consumer that passes the bytes on or unpacks them; a filter such as head or python does not count.
    let unpacker = compiled(r"\|\s*(?:(?:sudo|env|xargs|nohup|command)\s+(?:-\S+\s+)*)*(?:\S*/)?(?:tar|bsdtar|unzip|gunzip|zstd|tee|cat|dd|pv|sh|bash|zsh)\b");
    let curl_transfer = curl.captures_iter(&text).iter().any(|c| {
        c.get(1).is_some_and(|args| curl_file.is_match(args.as_str()))
            || c.get(2).is_some_and(|next| unpacker.is_match(next.as_str()))
    });
    if transfer.is_match(&text) || curl_transfer {
        return Decision::deny(
            "metered-link",
            "A metered link is active (~/.local/state/METERED_LINK). Do not start a large transfer; use a fibre worker or prefix METERED_TRANSFER_APPROVED=1 after the owner approves.",
        );
    }
    Decision::allow()
}

fn agent_yolo(cmd: &str) -> Decision {
    let flat = cmd.trim();
    if compiled(r#"(^|[\s/])(agent)(\s+|$)"#).is_match(flat)
        && compiled(r"(?:^|\s)(--force|--yolo)\b").is_match(flat)
        && !compiled(r"\bcursor-(worker|opinion)\b").is_match(flat)
    {
        return Decision::deny(
            "agent-yolo",
            "Do not run raw `agent -p` with --force or --yolo in a live checkout. Use `cursor-worker` or `cursor-opinion`.",
        );
    }
    Decision::allow()
}

fn test_audio(cmd: &str) -> Decision {
    if compiled(r"(?i)(SELECTOR_DECK_SILENT=1|AUDIO_QA_SILENT=1|AUDIBLE_APPROVED=1)").is_match(cmd) {
        return Decision::allow();
    }
    // A player only counts where the shell would run it: at the start of a command (after
    // `;`, `&`, `|`, `(`, `$(`, a backtick, a new line, or `sh -c "`), optionally behind
    // `sudo`, `env VAR=…`, `nohup`, `exec`, `command` or `time`. The word inside prose, a
    // quoted search pattern or a here-document body is text, not playback: on 11 Nov 2026 the
    // older "anywhere after a space" rule refused a notes file containing the verb "say".
    let heredoc = compiled(r#"(?s)<<-?\s*["']?(\w+)["']?[^\n]*\n.*?\n\s*\1\b"#);
    let without_bodies = heredoc.replace_all(cmd, "<<HEREDOC");
    let player = compiled(
        r#"(?im)(?:^|[;&|(`]|\$\(|-c\s+["'])\s*(?:(?:sudo|nohup|exec|command|time)\s+|env\s+(?:-\S+\s+)*(?:\w+=\S*\s+)*)*(?:\w+=\S*\s+)*(?:/usr/bin/)?(afplay|say|mpg123|ffplay|paplay)\b(?!\s*\()"#,
    );
    let osascript_sound = compiled(r"(?i)osascript\b.*\b(play|beep|sound)");
    if player.is_match(&without_bodies) || osascript_sound.is_match(&without_bodies) {
        return Decision::deny(
            "test-audio",
            "Audible playback is denied by default. Prefix AUDIBLE_APPROVED=1 only when the owner asked to hear it in this session (see audio-doctor register). SELECTOR_DECK_SILENT=1 still silences deck checks.",
        );
    }
    Decision::allow()
}

/// Known-bad and known-safe commands the doctor runs. A weakened guard fails the doctor.
pub fn self_test(cfg: &GuardConfig) -> Result<(), String> {
    // An absolute path outside temp, not `~/...`: a test HOME can sit inside $TMPDIR,
    // and a home path there is a temp path by the rule.
    let deny_rm = hard_delete("rm -rf /var/agentkit-self-test/important", cfg);
    if !deny_rm.is_deny() {
        return Err(format!("hard-delete allowed rm outside temp ({})", deny_rm.decision));
    }
    let allow_tmp = hard_delete("rm -rf /tmp/agentkit-self-test", cfg);
    if !allow_tmp.is_allow() {
        return Err("hard-delete blocked a /tmp delete".into());
    }
    let deny_trash = icloud("trash ~/Documents");
    if !deny_trash.is_deny() {
        return Err("icloud allowed trash ~/Documents".into());
    }
    let allow_file = icloud("trash ~/Desktop/Screenshot.png");
    if !allow_file.is_allow() {
        return Err("icloud blocked trash of one Desktop file".into());
    }
    let allow_push = push("git push origin main", cfg);
    if !allow_push.is_allow() {
        return Err(format!("push guard blocked plain git push outside gated projects ({})", allow_push.decision));
    }
    let deny_force = push("git push --force origin main", cfg);
    if !deny_force.is_deny() {
        return Err("push guard allowed git push --force".into());
    }
    Ok(())
}
