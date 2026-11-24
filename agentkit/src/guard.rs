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
    "slot-holder",
    "slot-files",
    "approval-files",
    "idrive-cleanup",
    "secrets",
    "guard-config",
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
    if let Some(path) = read_target(value) {
        // A file-reading tool call: only the secrets guard reads its path.
        return if enabled.contains(&"secrets") { secrets_read(&path, cfg) } else { Decision::allow() };
    }
    if !is_shell(value) {
        // A file-writing tool call: only the two path guards answer, from the path alone. The
        // content is never read, and a missing or malformed path allows.
        if file_target(value).is_none() {
            return Decision::allow();
        }
        let decisions = FILE_TOOL_GUARDS
            .iter()
            .filter(|name| enabled.contains(name))
            .map(|name| decide_value(name, value, cfg));
        return combine_decisions(decisions);
    }
    let decisions = enabled.iter().map(|name| decide_value(name, value, cfg));
    combine_decisions(decisions)
}

pub fn decide_value(name: &str, value: &Value, cfg: &GuardConfig) -> Decision {
    let adjusted = call_dir(value, cfg);
    let cfg = adjusted.as_ref().unwrap_or(cfg);
    if let Some(path) = read_target(value) {
        return if name == "secrets" { secrets_read(&path, cfg) } else { Decision::allow() };
    }
    if !is_shell(value) {
        // A file-writing tool call carries the path it writes. Two guards protect paths, not
        // commands, so they read it; every other guard only reads shell commands.
        return match file_target(value) {
            Some(path) => match name {
                "slot-files" => slot_files_path(&path, cfg),
                "approval-files" => approval_files_path(&path, cfg),
                "guard-config" => guard_config_file(&path, value, cfg),
                _ => Decision::allow(),
            },
            None => Decision::allow(),
        };
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
        "slot-holder" => slot_holder(&command, cfg),
        "slot-files" => slot_files(&command, cfg),
        "approval-files" => approval_files(&command, cfg),
        "idrive-cleanup" => idrive_cleanup(&command, cfg),
        "secrets" => secrets(&command, cfg),
        "guard-config" => guard_config(&command, cfg),
        _ => Decision::allow(),
    }
}

/// The guards that judge a file-writing tool call. Every other guard reads shell commands only.
const FILE_TOOL_GUARDS: &[&str] = &["slot-files", "approval-files", "guard-config"];

fn tool_name_of(value: &Value) -> Option<&str> {
    value
        .get("tool_name")
        .and_then(|t| t.as_str())
        .or_else(|| value.get("toolCall").and_then(|tc| tc.get("name")).and_then(|t| t.as_str()))
}

fn tool_input_of(value: &Value) -> Option<&Value> {
    value
        .get("tool_input")
        .or_else(|| value.get("toolCall").and_then(|tc| tc.get("args")))
}

/// The path a file-writing tool call (Claude `Write`/`Edit`/`MultiEdit`/`NotebookEdit`, Antigravity `write_to_file`/`write_file`/`replace_file_content`/`multi_replace_file_content`/`edit_file`) names.
fn file_target(value: &Value) -> Option<String> {
    let tool = tool_name_of(value)?;
    if !matches!(
        tool,
        "Write" | "Edit" | "MultiEdit" | "NotebookEdit"
            | "write_to_file" | "write_file" | "replace_file_content" | "multi_replace_file_content" | "edit_file"
    ) {
        return None;
    }
    let input = tool_input_of(value)?;
    input
        .get("file_path")
        .or_else(|| input.get("notebook_path"))
        .or_else(|| input.get("TargetFile"))
        .or_else(|| input.get("Path"))
        .and_then(|p| p.as_str())
        .filter(|p| !p.is_empty())
        .map(str::to_string)
}

fn is_shell(value: &Value) -> bool {
    match tool_name_of(value) {
        None => true,
        Some("Bash" | "Shell" | "shell" | "shell_command" | "local_shell" | "exec_command" | "run_command") => true,
        Some(_) => false,
    }
}

/// Relative targets use the tool call's `cwd` when the payload has one.
/// Cursor user hooks are started in `~/.cursor`, which is not the shell's directory.
fn call_dir(value: &Value, cfg: &GuardConfig) -> Option<GuardConfig> {
    let raw = value
        .get("cwd")
        .and_then(|c| c.as_str())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            tool_input_of(value)
                .and_then(|i| i.get("Cwd"))
                .and_then(|c| c.as_str())
                .filter(|s| !s.is_empty())
        })
        .or_else(|| {
            value
                .get("workspacePaths")
                .and_then(|w| w.as_array())
                .and_then(|a| a.first())
                .and_then(|c| c.as_str())
                .filter(|s| !s.is_empty())
        })?;
    let mut local = cfg.clone();
    local.cwd = PathBuf::from(expand_path(raw, cfg));
    Some(local)
}

fn command_of(value: &Value) -> String {
    if let Some(s) = value.get("command").and_then(|c| c.as_str()) {
        return s.to_string();
    }
    tool_input_of(value)
        .and_then(|t| t.get("command").or_else(|| t.get("CommandLine")))
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .to_string()
}

pub fn format_agy(d: &Decision) -> String {
    if d.is_allow() {
        serde_json::json!({ "decision": "allow" }).to_string()
    } else {
        serde_json::json!({
            "decision": d.decision,
            "reason": format!("[agentkit:{}] {}", d.rule, d.reason),
        })
        .to_string()
    }
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

/// A pattern from site config: an invalid one is skipped, never a panic in a hook.
fn try_compiled(pattern: &str) -> Option<Rex> {
    Regex::new(pattern).ok()?;
    Some(compiled(pattern))
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

const HARD_DELETE_REASON: &str = "Move it to the Trash with `trash <path>`. Hard deletes are allowed only inside /tmp, /private/tmp, /var/folders, $TMPDIR or a scratchpad directory.";

/// Three readings of the line; any one denying denies. The first splits at every separator,
/// quoted or not (so `echo 'x; rm -rf y'` is read as a delete). The second walks the commands
/// the shell would run (`units_deep`): ssh past the first word, text a shell is handed,
/// `rsync --delete`, every `find` root, a path moved into temp and deleted there. The third
/// reads whole statements and refuses a program word it cannot read (`$(echo rm)`, `$X`).
/// Until 21 Nov 2026 only the first existed, and those shapes were allowed.
fn hard_delete(cmd: &str, cfg: &GuardConfig) -> Decision {
    hard_delete_at(cmd, cfg, 0)
}

fn hard_delete_at(cmd: &str, cfg: &GuardConfig, depth: usize) -> Decision {
    let d = hard_delete_legacy(cmd, cfg);
    if d.is_deny() {
        return d;
    }
    let text = neutralize_text_args(&strip_file_heredocs(cmd));
    let mut air = AirCache::for_line(&text);
    let d = hard_delete_units(&text, cfg, &mut air);
    if d.is_deny() {
        return d;
    }
    hard_delete_statements(&text, cfg, &mut air, depth)
}

/// `is_air` may run `ssh -G`; one answer per host per call. A host reached on this line with
/// an option that can send the connection elsewhere (`-o HostName=…`, `-F`, `-J`, `-l`,
/// `user@`), or whose remote text runs ssh again, is not the worker for this line: until
/// 21 Nov 2026 `ssh -o HostName=other worker 'rm -rf ~/x'` matched the worker's alias.
#[derive(Default)]
struct AirCache {
    known: std::collections::HashMap<String, bool>,
    redirected: std::collections::HashSet<String>,
}

/// Does this ssh command line carry an option that can change where it connects?
fn ssh_redirects(argv: &[String]) -> bool {
    let Some((host, rest)) = ssh_parts_argv(argv) else { return true };
    // Only options that cannot change where it connects keep the worker exemption:
    // -t -T -q -v -4 -6 -C -n and -p PORT. Anything else (-o, -F, -J, -l, -i, -S, ...) loses it.
    let opts_end = argv.iter().position(|a| *a == host).unwrap_or(argv.len());
    let mut redirect_opt = false;
    let mut i = 1;
    while i < opts_end {
        let a = argv[i].as_str();
        i += 1;
        if a == "-p" {
            i += 1;
            continue;
        }
        let Some(body) = a.strip_prefix('-') else { redirect_opt = true; break };
        let port = body.strip_prefix('p').is_some_and(|v| !v.is_empty() && v.chars().all(|c| c.is_ascii_digit()));
        if !port && (body.is_empty() || body.starts_with('-') || !body.chars().all(|c| "tTqv46Cn".contains(c))) {
            redirect_opt = true;
            break;
        }
    }
    redirect_opt || host.contains('@') || rest.iter().any(|r| compiled(r"(?:^|[\s;&|(`'\x22])ssh\b").is_match(r))
}

impl AirCache {
    fn for_line(cmd: &str) -> Self {
        let mut cache = AirCache::default();
        for u in units_deep(cmd) {
            if u.prog() == "ssh" && !u.later && ssh_redirects(&u.argv) {
                if let Some((host, _)) = ssh_parts_argv(&u.argv) {
                    cache.redirected.insert(host.rsplit('@').next().unwrap_or(&host).to_string());
                    cache.redirected.insert(host);
                }
            }
        }
        cache
    }

    fn is_air(&mut self, host: &str, cfg: &GuardConfig) -> bool {
        let bare = host.rsplit('@').next().unwrap_or(host);
        if self.redirected.contains(host) || self.redirected.contains(bare) {
            return false;
        }
        if let Some(v) = self.known.get(host) {
            return *v;
        }
        let v = cfg.is_air(host);
        self.known.insert(host.to_string(), v);
        v
    }
}

fn hard_delete_deny(what: &str, host: Option<&str>) -> Decision {
    let over = host.map(|h| format!(" (over ssh to {h})")).unwrap_or_default();
    Decision::deny("hard-delete", format!("BLOCKED hard delete: {what}{over}. {HARD_DELETE_REASON}"))
}

/// Programs that run the words after them as a command (beyond the wrappers already stripped).
const EXECUTORS: &[&str] = &[
    "watch", "parallel", "flock", "chronic", "unbuffer", "doas", "chroot", "sg", "fd", "ssh-agent", "script",
];

fn deletes_with_find(argv: &[String]) -> bool {
    argv.iter().any(|a| a == "-delete")
        || argv.windows(2).any(|w| {
            matches!(w[0].as_str(), "-exec" | "-execdir" | "-ok" | "-okdir") && DELETERS.contains(&base_name(&w[1]))
        })
}

/// The start points of a `find`: the words before its first expression (after -H/-L/-P).
fn find_roots(argv: &[String]) -> Vec<&str> {
    let mut out = Vec::new();
    for a in argv.iter().skip(1) {
        if out.is_empty() && matches!(a.as_str(), "-H" | "-L" | "-P" | "-E" | "-X" | "-d" | "-s" | "-x") {
            continue;
        }
        if a.starts_with('-') || a.starts_with('(') || a == "!" {
            break;
        }
        out.push(a.as_str());
    }
    if out.is_empty() {
        out.push(".");
    }
    out
}

/// `--delete`, `--del`, `--delete-after`, `--delete-excluded`, ... (not `--remove-source-files`,
/// which removes from the source what was copied).
fn rsync_deletes(argv: &[String]) -> bool {
    argv.iter().skip(1).any(|a| a.starts_with("--del"))
}

/// The second reading: every command `units_deep` finds, judged by what it was read from.
fn hard_delete_units(cmd: &str, cfg: &GuardConfig, air: &mut AirCache) -> Decision {
    let all = units_deep(cmd);
    if any_unparsed(&all) && compiled(r"\b(?:rm|rmdir|unlink|shred|srm)\b|-delete\b|--del").is_match(cmd) {
        return hard_delete_deny("a delete on a line that cannot be read with certainty", None);
    }
    let mut launder: Option<String> = None;
    let mut temp_delete = false;
    for u in &all {
        let remote = u.remote.as_deref();
        if let Some(host) = remote
            && air.is_air(host, cfg)
        {
            // The worker's own files: it may delete its build output. Only the remote text is
            // exempt; a local `"$(...)"` around it has no remote and is judged here.
            continue;
        }
        if u.carrier || u.argv.is_empty() {
            continue;
        }
        // `git rm`, `docker rm`: a later word is a program only after a known executor.
        if u.later && !EXECUTORS.contains(&u.parent.as_str()) {
            continue;
        }
        let prog = u.prog();
        let operands = u.operands();
        if DELETERS.contains(&prog) {
            if operands.is_empty() || !operands.iter().all(|t| is_temp(t, cfg)) {
                return hard_delete_deny(&format!("`{prog}` on a non-temp path"), remote);
            }
            temp_delete = true;
        } else if prog == "find" && deletes_with_find(&u.argv) {
            if find_roots(&u.argv).iter().any(|r| !is_temp(r, cfg)) {
                return hard_delete_deny("`find -delete` / `find -exec rm` outside a temp dir (every start point counts)", remote);
            }
            temp_delete = true;
        } else if prog == "rsync" && rsync_deletes(&u.argv) {
            // The destination is not always the last word (`... dest/ --exclude /tmp/x`), so
            // every operand after the first must be temp or on the worker.
            // `-e 'ssh -o HostName=…'` can send it to another host than the name says.
            // rsync also takes its connection program from the environment.
            let own_rsh = u.argv.iter().any(|a| a == "-e" || a.starts_with("--rsh") || (a.starts_with('-') && !a.starts_with("--") && a.contains('e')))
                || ["RSYNC_RSH", "RSYNC_CONNECT_PROG", "RSYNC_PROXY"].iter().any(|n| cmd.contains(n) || u.wrappers.iter().any(|w| w.contains(n)));
            let mut safe = |w: &str| match w.split_once(':') {
                Some((host, path)) if !host.contains('/') && !host.is_empty() => {
                    (!own_rsh && !host.contains('@') && air.is_air(host, cfg))
                        || (path.starts_with('/') && is_temp(path, cfg))
                }
                _ => !w.is_empty() && is_temp(w, cfg),
            };
            let to_temp = operands.len() >= 2 && operands[1..].iter().all(|w| safe(w));
            if !to_temp {
                return hard_delete_deny(
                    "`rsync --delete` removes from a non-temp destination whatever the source lacks",
                    remote,
                );
            }
            temp_delete = true;
        } else if matches!(prog, "mv" | "ln") && !u.guessed && operands.len() >= 2 {
            // Any temp operand beside a non-temp one: the destination may come first (`mv -t /tmp x`).
            if operands.iter().any(|w| is_temp(w, cfg)) && operands.iter().any(|w| !is_temp(w, cfg)) {
                launder = Some(prog.to_string());
            }
        }
    }
    if let (Some(prog), true) = (launder, temp_delete) {
        return hard_delete_deny(
            &format!("`{prog}` of a non-temp path into a temp dir on a line that deletes there"),
            None,
        );
    }
    Decision::allow()
}

/// One statement of a command line: its text and the separator in front of it.
struct Statement {
    text: String,
    before: String,
}

/// Index just past the `)` that closes the substitution whose body starts at `i`.
fn subst_end(b: &[u8], mut i: usize) -> usize {
    let mut depth = 1usize;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 1,
            b'\'' => {
                i += 1;
                while i < b.len() && b[i] != b'\'' {
                    i += 1;
                }
            }
            b'"' => {
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    if b[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
            }
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return i + 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    b.len()
}

/// Splits a line into statements at top-level separators, keeping `$(...)`, `<(...)`,
/// backticks, `${...}` and quotes inside the word they belong to, so `$(echo rm) -rf x` is one
/// statement whose program word is `$(echo`. Also returns the bodies of command substitutions
/// (not arithmetic), which the shell runs first.
fn statements(cmd: &str) -> (Vec<Statement>, Vec<String>) {
    let b = cmd.as_bytes();
    let mut out = Vec::new();
    let mut subs = Vec::new();
    let (mut i, mut start) = (0usize, 0usize);
    let mut before = String::new();
    let mut in_double = false;
    while i < b.len() {
        let c = b[i];
        if c == b'\\' {
            i += 2;
            continue;
        }
        if c == b'\'' && !in_double {
            i += 1;
            while i < b.len() && b[i] != b'\'' {
                i += 1;
            }
            i += 1;
            continue;
        }
        if c == b'"' {
            in_double = !in_double;
            i += 1;
            continue;
        }
        let rest = &b[i..];
        if rest.starts_with(b"$((") {
            i = subst_end(b, i + 3);
            continue;
        }
        if rest.starts_with(b"$(") || (!in_double && (rest.starts_with(b"<(") || rest.starts_with(b">("))) {
            let end = subst_end(b, i + 2);
            let close = if end > i + 2 && b.get(end - 1) == Some(&b')') { end - 1 } else { end };
            subs.push(cmd[i + 2..close.max(i + 2)].to_string());
            i = end;
            continue;
        }
        if rest.starts_with(b"${") {
            while i < b.len() && b[i] != b'}' {
                i += 1;
            }
            i += 1;
            continue;
        }
        if c == b'`' {
            let open = i + 1;
            i += 1;
            while i < b.len() && b[i] != b'`' {
                if b[i] == b'\\' {
                    i += 1;
                }
                i += 1;
            }
            subs.push(cmd[open.min(b.len())..i.min(b.len())].to_string());
            i += 1;
            continue;
        }
        if in_double {
            i += 1;
            continue;
        }
        let len = if rest.starts_with(b"&&") || rest.starts_with(b"||") || rest.starts_with(b"|&") || rest.starts_with(b";;") {
            2
        } else if matches!(c, b';' | b'|' | b'\n' | b'(' | b')') {
            1
        } else if c == b'&' && !(i > 0 && matches!(b[i - 1], b'<' | b'>')) && b.get(i + 1) != Some(&b'>') {
            1
        } else {
            0
        };
        if len == 0 {
            i += 1;
            continue;
        }
        out.push(Statement { text: cmd[start..i].to_string(), before: before.clone() });
        before = cmd[i..i + len].to_string();
        i += len;
        start = i;
    }
    out.push(Statement { text: cmd[start.min(cmd.len())..].to_string(), before });
    (out, subs)
}

/// The statement after its leading `NAME=value` words, each read as one shell word: quotes,
/// `$(...)`, backticks and `${...}` in the value stay inside it.
fn skip_assignments(text: &str) -> &str {
    let mut rest = text.trim_start();
    loop {
        let b = rest.as_bytes();
        let name_end = b.iter().position(|c| !(c.is_ascii_alphanumeric() || *c == b'_')).unwrap_or(b.len());
        if name_end == 0 || b[0].is_ascii_digit() || b.get(name_end) != Some(&b'=') {
            return rest;
        }
        let mut i = name_end + 1;
        while i < b.len() && !b[i].is_ascii_whitespace() {
            match b[i] {
                b'\\' => i += 2,
                b'\'' => {
                    i += 1;
                    while i < b.len() && b[i] != b'\'' {
                        i += 1;
                    }
                    i += 1;
                }
                b'"' => {
                    i += 1;
                    while i < b.len() && b[i] != b'"' {
                        if b[i] == b'\\' {
                            i += 1;
                        } else if b[i..].starts_with(b"$(") {
                            i = subst_end(b, i + 2);
                            continue;
                        }
                        i += 1;
                    }
                    i += 1;
                }
                b'`' => {
                    i += 1;
                    while i < b.len() && b[i] != b'`' {
                        i += if b[i] == b'\\' { 2 } else { 1 };
                    }
                    i += 1;
                }
                b'$' if b.get(i + 1) == Some(&b'(') => i = subst_end(b, i + 2),
                b'$' if b.get(i + 1) == Some(&b'{') => {
                    while i < b.len() && b[i] != b'}' {
                        i += 1;
                    }
                    i += 1;
                }
                _ => i += 1,
            }
        }
        rest = rest[i.min(rest.len())..].trim_start();
    }
}

/// `NAME=value` on the line, literal values only (quotes removed).
fn literal_assignments(cmd: &str) -> Vec<(String, String)> {
    compiled(r#"(?:^|[\s;&|(])(?:export\s+|local\s+|declare\s+)?([A-Za-z_][A-Za-z0-9_]*)=('[^']*'|"[^"$`]*"|[^\s;&|()'"$`]*)(?=[\s;&|()]|$)"#)
        .captures_iter(cmd)
        .iter()
        .filter_map(|c| {
            let v = c.get(2)?.as_str();
            Some((c.get(1)?.as_str().to_string(), v.trim_matches(|q| q == '\'' || q == '"').to_string()))
        })
        .collect()
}

/// A program word as the shell would see it, when the line alone tells.
fn expand_program(word: &str, cfg: &GuardConfig, vars: &[(String, String)]) -> String {
    let mut w = expand_vars(word, cfg);
    for (name, value) in vars {
        w = w.replace(&format!("${{{name}}}"), value).replace(&format!("${name}"), value);
    }
    w
}

/// The third reading: whole statements, not split at `$(`. A program word the guard cannot
/// read (`$(echo rm)`, `$X`, `{rm,-rf,x}`, a glob) is refused, since it may be `rm`; a variable
/// assigned a literal on the same line is substituted and the statement judged again. Text a
/// shell reads on stdin (`echo ... | sh`, `sh <<< ...`) is judged as a command line.
fn hard_delete_statements(cmd: &str, cfg: &GuardConfig, air: &mut AirCache, depth: usize) -> Decision {
    if depth > MAX_UNIT_DEPTH {
        return Decision::allow();
    }
    let (flat, docs) = split_heredocs(cmd);
    for doc in &docs {
        if heredoc_is_data(&doc.head, &doc.tail) {
            continue;
        }
        let mut head = Vec::new();
        collect_units(&doc.head, None, &[], 0, &Walk { deep: false, sole: false, strict: false }, &mut head);
        let to_shell = head.iter().any(|u| SHELLS.contains(&u.prog()));
        let to_remote = head
            .iter()
            .find_map(|u| if u.prog() == "ssh" { ssh_parts_argv(&u.argv).map(|(h, _)| h) } else { None });
        if to_shell || to_remote.as_deref().is_some_and(|h| !air.is_air(h, cfg)) {
            let d = hard_delete_statements(&doc.body, cfg, air, depth + 1);
            if d.is_deny() {
                return d;
            }
        }
    }
    let vars = literal_assignments(&flat);
    let case_line = compiled(r"(?:^|[\s;&|(])case\s").is_match(&flat);
    let (stmts, subs) = statements(&flat);
    for body in &subs {
        let d = hard_delete_statements(body, cfg, air, depth + 1);
        if d.is_deny() {
            return d;
        }
    }
    // The earlier stages of the current pipeline: `echo ... | cat | sh` feeds sh from echo.
    let mut pipeline: Vec<Vec<String>> = Vec::new();
    for st in &stmts {
        let whole = st.text.trim();
        let first_raw = whole.split_whitespace().next().unwrap_or("").to_string();
        // Leading `NAME=value` words are dropped as whole shell words first: shlex would split
        // `L=$(ls -td /x/* | head -1)` into pieces and leave `/x/*` where the program goes.
        let text = skip_assignments(whole);
        let mut argv = shlex::split(text).unwrap_or_else(|| text.split_whitespace().map(str::to_string).collect());
        strip_wrappers(&mut argv);
        if !matches!(st.before.as_str(), "|" | "|&") {
            pipeline.clear();
        }
        let feeders = pipeline.clone();
        pipeline.push(argv.clone());
        let Some(word) = argv.first() else { continue };
        if first_raw == "case" || matches!(word.as_str(), "in" | "esac" | "for" | "select" | "function") {
            continue;
        }
        let prog_word = expand_program(word, cfg, &vars);
        let unreadable: &[char] = if case_line { &['$', '`', '{'] } else { &['$', '`', '{', '*', '?'] };
        if prog_word.contains(unreadable) {
            return hard_delete_deny(
                &format!("the program word `{word}` is computed when the line runs, so the guard cannot tell it is not `rm`. Write the program name itself"),
                None,
            );
        }
        if prog_word != *word && depth < MAX_UNIT_DEPTH {
            let rest: Vec<String> = argv[1..].iter().map(|a| quote_word(a)).collect();
            let d = hard_delete_at(&format!("{prog_word} {}", rest.join(" ")), cfg, depth + 1);
            if d.is_deny() {
                return d;
            }
        }
        let prog = base_name(&prog_word).to_string();
        // Operands whole, with a substitution glued on: `rm -rf /tmp/$(echo ../x)` is not temp.
        if DELETERS.contains(&prog.as_str()) {
            let targets: Vec<&String> = argv[1..].iter().filter(|a| !a.starts_with('-')).collect();
            if targets.iter().any(|t| !is_temp(t, cfg)) {
                return hard_delete_deny(&format!("`{prog}` on a path that is not certainly temp"), None);
            }
        }
        if SHELLS.contains(&prog.as_str()) {
            if let Some(inner) = shell_c_arg(&argv) {
                let d = hard_delete_statements(&inner, cfg, air, depth + 1);
                if d.is_deny() {
                    return d;
                }
                continue;
            }
            // A shell reading its script on stdin: a here-string, or a pipe into it.
            let mut fed: Vec<String> = Vec::new();
            if let Some(at) = argv.iter().position(|a| a == "<<<") {
                fed.extend(argv.get(at + 1).cloned());
            }
            let script_file = argv[1..].iter().any(|a| !a.starts_with('-') && a != "<<<")
                && !argv.iter().any(|a| a == "<<<");
            if matches!(st.before.as_str(), "|" | "|&") && !script_file {
                for feed in &feeders {
                    fed.push(feed.iter().skip(1).cloned().collect::<Vec<_>>().join(" "));
                }
            }
            for text in fed {
                if depth < MAX_UNIT_DEPTH {
                    let d = hard_delete_at(&text, cfg, depth + 1);
                    if d.is_deny() {
                        return d;
                    }
                }
            }
        } else if prog == "eval" && argv.len() > 1 {
            let d = hard_delete_statements(&argv[1..].join(" "), cfg, air, depth + 1);
            if d.is_deny() {
                return d;
            }
        } else if prog == "ssh"
            && let Some((host, rest)) = ssh_parts_argv(&argv)
            && !rest.is_empty()
            && !air.is_air(&host, cfg)
        {
            let d = hard_delete_statements(&rest.join(" "), cfg, air, depth + 1);
            if d.is_deny() {
                return Decision::deny("hard-delete", format!("{} (over ssh to {host})", d.reason));
            }
        }
    }
    Decision::allow()
}

fn hard_delete_legacy(cmd: &str, cfg: &GuardConfig) -> Decision {
    let cmd = strip_file_heredocs(cmd);
    // A search pattern or a commit message is text. Until 14 Nov 2026 a grep whose pattern named
    // a delete command between `\|` separators was refused as a delete.
    let cmd = neutralize_text_args(&cmd);
    if let Some((host, rest)) = ssh_parts(&cmd) {
        if cfg.is_air(&host) && rest.len() == 1 {
            // The worker's remote text is its own: splitting it here at quoted separators would
            // refuse `ssh worker 'cd b && rm -rf cache'`. The other two readings still judge what
            // runs locally around it (`"$(...)"`); until 21 Nov 2026 this allowed the whole line.
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
        strip_wrappers(&mut argv);
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

/// Drops what stands in front of the program: `VAR=value`, shell keywords, and wrappers such as
/// `sudo -u root`, `timeout 30`, `disk-slot PATH ... --`, with their own flags and values.
/// Returns the wrappers it removed.
fn strip_wrappers(argv: &mut Vec<String>) -> Vec<String> {
    let mut seen = Vec::new();
    while !argv.is_empty()
        && (is_assignment(&argv[0]) || WRAPPERS.contains(&base_name(&argv[0])) || KEYWORDS.contains(&argv[0].as_str()))
    {
        let wrapper = base_name(&argv[0]).to_string();
        if !is_assignment(&argv[0]) && WRAPPERS.contains(&wrapper.as_str()) {
            seen.push(wrapper.clone());
        }
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
        // `disk-slot PATH PATH ... -- command`: every path before the `--` is the wrapper's.
        if wrapper == "disk-slot"
            && let Some(at) = argv.iter().position(|a| a == "--")
        {
            argv.drain(..at);
        }
        if !argv.is_empty() && argv[0] == "--" {
            argv.remove(0);
        }
    }
    seen
}

/// One stretch of a command line between shell separators that stand outside quotes.
struct Piece<'a> {
    text: &'a str,
    /// The separator that ended it (`;`, `|`, `&&`, `$(`, ...); empty for the last piece.
    sep: &'a str,
    /// False when the line ended inside a quote.
    closed: bool,
}

/// Splits a command line where the shell would: at `&&`, `||`, `;`, `|`, `&`, a new line,
/// `$(`, a backtick or a parenthesis, but only outside quotes. A separator inside a quoted
/// argument is that argument's text.
fn pieces(cmd: &str) -> Vec<Piece<'_>> {
    let b = cmd.as_bytes();
    let mut out = Vec::new();
    let (mut i, mut start) = (0usize, 0usize);
    let (mut single, mut double) = (false, false);
    while i < b.len() {
        let c = b[i];
        if single {
            single = c != b'\'';
            i += 1;
            continue;
        }
        if c == b'\\' {
            i += 2;
            continue;
        }
        if double {
            double = c != b'"';
            i += 1;
            continue;
        }
        if c == b'\'' {
            single = true;
            i += 1;
            continue;
        }
        if c == b'"' {
            double = true;
            i += 1;
            continue;
        }
        let rest = &b[i..];
        let len = if rest.starts_with(b"&&") || rest.starts_with(b"||") || rest.starts_with(b"$(") {
            2
        } else if matches!(c, b';' | b'|' | b'\n' | b'`' | b'(' | b')') {
            1
        } else if c == b'&' && !(i > 0 && matches!(b[i - 1], b'<' | b'>')) && b.get(i + 1) != Some(&b'>') {
            // a lone `&` (background), not `2>&1`, `<&3` or `&>file`
            1
        } else {
            0
        };
        if len == 0 {
            i += 1;
            continue;
        }
        out.push(Piece { text: &cmd[start..i], sep: &cmd[i..i + len], closed: true });
        i += len;
        start = i;
    }
    let start = start.min(cmd.len());
    out.push(Piece { text: &cmd[start..], sep: "", closed: !single && !double });
    // bash's $'...' and $"..." follow other rules (in $'...' a backslash escapes a quote), so the
    // quote tracking above can be wrong anywhere after one: what looks like text may be a command.
    // The whole line counts as unreadable, which each guard refuses when the line is about its subject.
    if has_dollar_quote(cmd) {
        for piece in &mut out {
            piece.closed = false;
        }
    }
    out
}

/// The line as the shell would join it: quote marks and backslashes removed. `APPR""OVE_X` and
/// `lim\it` name the same files as `APPROVE_X` and `limit`, so a guard that first asks "is my
/// subject named on this line at all?" asks it of this form too.
fn without_quoting(cmd: &str) -> String {
    cmd.chars().filter(|c| !matches!(c, '\'' | '"' | '\\')).collect()
}

/// Whether the line uses bash's `$'...'` (ANSI-C) or `$"..."` (locale) quoting.
fn has_dollar_quote(cmd: &str) -> bool {
    cmd.contains("$'") || cmd.contains("$\"")
}

/// Replaces quoted arguments with empty quotes. A double-quoted string that holds `$(` or a
/// backtick is kept: the shell runs what is inside it.
fn blank_quotes(text: &str) -> String {
    let b = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0usize;
    let mut plain = 0usize;
    while i < b.len() {
        let c = b[i];
        if c == b'\\' {
            i += 2;
            continue;
        }
        if c != b'\'' && c != b'"' {
            i += 1;
            continue;
        }
        let open = i;
        i += 1;
        while i < b.len() && b[i] != c {
            if c == b'"' && b[i] == b'\\' {
                i += 1;
            }
            i += 1;
        }
        let close = i.min(b.len());
        i = (close + 1).min(b.len());
        let inner = &text[(open + 1).min(close)..close];
        let runs = c == b'"' && (inner.contains("$(") || inner.contains('`'));
        if !runs {
            out.push_str(&text[plain..open]);
            out.push(c as char);
            out.push(c as char);
            plain = i;
        }
    }
    out.push_str(&text[plain.min(text.len())..]);
    out
}

/// The words of a command line that is one simple command and nothing else: no separator
/// outside quotes, and nowhere on the line a substitution, a process substitution, a
/// here-document, a here-string or a new line. Only on such a line is a quoted argument
/// certain to stay text: nothing else is there to run it.
fn simple_command(cmd: &str) -> Option<Vec<String>> {
    let line = cmd.trim();
    if line.is_empty() || ["\n", "\r", "$(", "`", "<(", ">(", "<<"].iter().any(|mark| line.contains(mark)) {
        return None;
    }
    if has_dollar_quote(line) {
        return None;
    }
    let parts = pieces(line);
    if parts.len() != 1 || !parts[0].closed {
        return None;
    }
    shlex::split(line).filter(|argv| !argv.is_empty())
}

const GREP_LONG: &[&str] = &[
    "--regexp", "--include", "--exclude", "--exclude-dir", "--color", "--colour", "--line-number",
    "--ignore-case", "--recursive", "--count", "--files-with-matches", "--files-without-match",
    "--extended-regexp", "--fixed-strings", "--invert-match", "--word-regexp", "--only-matching",
    "--quiet", "--no-messages", "--context", "--after-context", "--before-context", "--null",
    "--with-filename", "--no-filename", "--max-count", "--binary-files", "--text", "--perl-regexp",
    "--line-buffered", "--glob", "--iglob", "--type", "--type-not", "--smart-case", "--no-heading",
    "--hidden", "--no-ignore", "--files", "--json", "--multiline", "--pcre2", "--max-depth",
    "--follow", "--sort", "--heading", "--column", "--case-sensitive", "--line-regexp",
];
const GIT_GREP_LONG: &[&str] = &[
    "--ignore-case", "--line-number", "--count", "--files-with-matches", "--name-only",
    "--extended-regexp", "--fixed-strings", "--perl-regexp", "--word-regexp", "--invert-match",
    "--cached", "--untracked", "--no-index", "--recurse-submodules", "--max-depth", "--and", "--or",
    "--not", "--all-match", "--quiet", "--heading", "--break", "--show-function", "--context",
    "--after-context", "--before-context", "--threads", "--color", "--no-color", "--only-matching",
    "--column", "--text", "--full-name", "--null",
];
/// Long options that take their value as the next word.
const SEARCH_LONG_VALUE: &[&str] = &[
    "--include", "--exclude", "--exclude-dir", "--context", "--after-context", "--before-context",
    "--max-count", "--glob", "--iglob", "--type", "--type-not", "--max-depth", "--sort", "--threads",
];
const GIT_MESSAGE_LONG: &[&str] = &[
    "--message", "--all", "--amend", "--no-edit", "--quiet", "--signoff", "--allow-empty",
    "--allow-empty-message", "--no-verify", "--only", "--include", "--verbose", "--no-gpg-sign",
    "--reset-author", "--annotate", "--force", "--dry-run", "--short",
];
const GIT_LOG_LONG: &[&str] = &[
    "--grep", "--oneline", "--all", "--author", "--since", "--until", "--after", "--before",
    "--format", "--pretty", "--stat", "--name-only", "--name-status", "--graph", "--decorate",
    "--no-merges", "--merges", "--reverse", "--max-count", "--follow", "--date", "--abbrev-commit",
    "--patch", "--no-patch", "--first-parent", "--invert-grep", "--regexp-ignore-case",
    "--all-match", "--fixed-strings", "--extended-regexp", "--perl-regexp", "--branches", "--tags",
    "--remotes", "--shortstat", "--numstat", "--no-color", "--color",
];

fn long_name(arg: &str) -> &str {
    arg.split('=').next().unwrap_or(arg)
}

/// Marks the pattern of a search command: the value of `-e` / `--regexp`, or else the first
/// plain word. None when an option is not on the known list (it might run a command).
fn search_inert(
    argv: &[String],
    start: usize,
    known_long: &[&str],
    value_shorts: &str,
    refused_shorts: &str,
    inert: &mut [bool],
) -> Option<()> {
    let mut pattern_given = false;
    let mut first_word = None;
    let mut options_done = false;
    let mut i = start;
    while i < argv.len() {
        let a = argv[i].as_str();
        i += 1;
        if options_done || !a.starts_with('-') || a == "-" {
            first_word.get_or_insert(i - 1);
        } else if a == "--" {
            options_done = true;
        } else if a.starts_with("--") {
            let name = long_name(a);
            if !known_long.contains(&name) {
                return None;
            }
            if name == "--regexp" {
                pattern_given = true;
                if a.contains('=') {
                    inert[i - 1] = true;
                } else if i < argv.len() {
                    inert[i] = true;
                    i += 1;
                }
            } else if SEARCH_LONG_VALUE.contains(&name) && !a.contains('=') {
                i += 1;
            }
        } else {
            let body = &a[1..];
            if body.chars().any(|c| refused_shorts.contains(c)) {
                return None;
            }
            if let Some(at) = body.find(|c| value_shorts.contains(c)) {
                let letter = body.as_bytes()[at] as char;
                if letter == 'e' || letter == 'f' {
                    pattern_given = true;
                }
                // the value is the next word unless it is attached (`-A3`)
                if at + 1 == body.len() {
                    if letter == 'e' && i < argv.len() {
                        inert[i] = true;
                    }
                    i += 1;
                }
            }
        }
    }
    if !pattern_given && let Some(at) = first_word {
        inert[at] = true;
    }
    Some(())
}

/// Which words of a command are text to it and to everything else: the pattern of `grep` /
/// `rg` / `git grep`, the message of `git commit` / `git tag`, the `--grep`, `-S`, `-G` value of
/// `git log`. Nothing else is marked: a file name, a format, any other option's value stays as
/// written. None when the command is not one of these, has anything in front of the program
/// (`sudo`, `PAGER=...`), a git option before the subcommand (`-c`, `-C`), or an option that is
/// not on the known list: such an option may run a command (`rg --pre`, `git grep -O`).
fn inert_tokens(argv: &[String]) -> Option<Vec<bool>> {
    let prog = argv.first()?;
    if is_assignment(prog) {
        return None;
    }
    let mut inert = vec![false; argv.len()];
    match base_name(prog) {
        "grep" | "egrep" | "fgrep" => search_inert(argv, 1, GREP_LONG, "efmABCdD", "", &mut inert)?,
        "rg" => search_inert(argv, 1, GREP_LONG, "efgtTmABCjMrEd", "", &mut inert)?,
        "git" => match argv.get(1)?.as_str() {
            "grep" => search_inert(argv, 2, GIT_GREP_LONG, "efABCm", "O", &mut inert)?,
            "commit" | "tag" => {
                let mut i = 2;
                while i < argv.len() {
                    let a = argv[i].as_str();
                    i += 1;
                    if a.starts_with("--") {
                        if !GIT_MESSAGE_LONG.contains(&long_name(a)) {
                            return None;
                        }
                        if a == "--message" {
                            if i < argv.len() {
                                inert[i] = true;
                            }
                            i += 1;
                        } else if a.starts_with("--message=") {
                            inert[i - 1] = true;
                        }
                    } else if a.starts_with('-') && a.len() > 1 {
                        let body = &a[1..];
                        // -F, -C, -c, -t take a file or a commit; keep those lines as written
                        if body.chars().any(|c| "FCct".contains(c)) {
                            return None;
                        }
                        if body.ends_with('m') {
                            if i < argv.len() {
                                inert[i] = true;
                            }
                            i += 1;
                        }
                    }
                }
            }
            "log" => {
                let mut i = 2;
                while i < argv.len() {
                    let a = argv[i].as_str();
                    i += 1;
                    if a == "--grep" || a == "-S" || a == "-G" {
                        if i < argv.len() {
                            inert[i] = true;
                        }
                        i += 1;
                    } else if a.starts_with("--grep=") {
                        inert[i - 1] = true;
                    } else if a.starts_with("--") {
                        if a != "--" && !GIT_LOG_LONG.contains(&long_name(a)) {
                            return None;
                        }
                    } else if a.starts_with('-') && a.len() > 1 {
                        let body = &a[1..];
                        if body == "n" {
                            i += 1;
                        } else if !(body.chars().all(|c| c.is_ascii_digit()) || matches!(body, "p" | "i" | "E" | "F" | "P" | "q" | "u")) {
                            return None;
                        }
                    }
                }
            }
            _ => return None,
        },
        _ => return None,
    }
    Some(inert)
}

/// A word as the shell would need it written.
fn quote_word(word: &str) -> String {
    let plain = !word.is_empty()
        && word.chars().all(|c| c.is_ascii_alphanumeric() || "_@%+=:,./-".contains(c));
    if plain { word.to_string() } else { format!("'{}'", word.replace('\'', "'\\''")) }
}

/// The line with its search pattern or commit message emptied, when the whole line is one
/// simple `grep` / `rg` / `git commit|tag|log|grep` command; otherwise the line unchanged.
///
/// 14 Nov 2026, first version: it emptied every quoted argument of those commands and of
/// `echo` / `printf`, piece by piece. That hid text which something else on the line could run
/// (`echo '...' > x.py && python3 x.py`, `eval "$(echo ...)"`, `rg --pre '...'`). Now `echo`
/// and `printf` are never text, a line with anything else on it is read as written, and only
/// the pattern or message value is emptied. A false denial is cheaper than a bypass.
fn neutralize_text_args(cmd: &str) -> String {
    let Some(argv) = simple_command(cmd) else { return cmd.to_string() };
    let Some(inert) = inert_tokens(&argv) else { return cmd.to_string() };
    if !inert.iter().any(|x| *x) {
        return cmd.to_string();
    }
    argv.iter()
        .zip(&inert)
        .map(|(word, inert)| match (*inert, word.split_once('=')) {
            (false, _) => quote_word(word),
            (true, Some((name, _))) if name.starts_with("--") => format!("{name}="),
            (true, _) => "''".to_string(),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// One command the shell would run.
struct Unit {
    /// Its words after wrappers; empty for a bare assignment or `disk-slot status`.
    argv: Vec<String>,
    /// The text of its piece of the command line, wrappers and redirections included.
    text: String,
    /// Wrappers in front of it, and in front of any `sh -c` / `ssh` that carries it.
    wrappers: Vec<String>,
    /// The host, when it runs inside an `ssh` command.
    remote: Option<String>,
    /// True for `sh -c '...'`, `eval '...'` and `ssh host '...'`: the commands inside are
    /// listed as their own units.
    carrier: bool,
    /// The separator that ended its piece (`|`, `$(`, ...).
    sep: String,
    /// True when it could not be read: nested too deep, or a quote that never closes. A guard
    /// whose subject is on the line denies when it sees one.
    unparsed: bool,
    /// Read from text that is only guessed to be a command (a quoted word, a script string).
    guessed: bool,
    /// One of the speculative units that start at a later word of another command; `parent`
    /// is that command's program.
    later: bool,
    parent: String,
}

impl Unit {
    fn prog(&self) -> &str {
        self.argv.first().map(|a| base_name(a)).unwrap_or("")
    }
    /// Arguments that are not flags.
    fn operands(&self) -> Vec<&str> {
        self.argv.iter().skip(1).filter(|a| !a.starts_with('-')).map(String::as_str).collect()
    }
}

/// How many layers of `sh -c`, `ssh`, `eval` and carried text are followed. Deeper than this
/// is recorded as unparsed, not skipped: until 14 Nov 2026 five layers of `sh -c` around a
/// copy between volumes left no unit for the guard to judge, and it allowed.
const MAX_UNIT_DEPTH: usize = 6;

#[derive(Clone, Copy)]
struct Walk {
    /// Also read text another program may run: quoted words with spaces in them, every later
    /// word of a command that is not a known text reader, string literals in a script body.
    deep: bool,
    /// The whole line is one simple command, so what `echo` prints goes nowhere.
    sole: bool,
    /// The text is known to be shell (the line itself, `sh -c`, `ssh`, a shell here-document),
    /// so a quote that never closes means it was not read. Text that is only guessed to be a
    /// command (a quoted word, a string in a script) is prose more often than not.
    strict: bool,
}

/// Programs that only read the files they are given: no later word of theirs is a command.
const READERS: &[&str] = &[
    "cat", "ls", "head", "tail", "less", "more", "wc", "stat", "file", "test", "[", "[[", "diff", "cd",
    "trash", "du", "df", "sort", "uniq", "cut", "tr", "basename", "dirname", "realpath", "readlink",
    "shasum", "md5",
];

/// Every command in a command line, with the ones carried by `sh -c`, `eval`, `ssh`, `"$(...)"`
/// and here-documents. For a guard that denies by default and exempts a few programs.
fn units(cmd: &str) -> Vec<Unit> {
    walk_units(cmd, false)
}

/// The same, and text that some other program on the line may run is read as a command too.
/// For a guard that denies named commands: a false denial is cheaper than a bypass.
fn units_deep(cmd: &str) -> Vec<Unit> {
    walk_units(cmd, true)
}

fn any_unparsed(units: &[Unit]) -> bool {
    units.iter().any(|u| u.unparsed)
}

fn walk_units(cmd: &str, deep: bool) -> Vec<Unit> {
    let mut out = Vec::new();
    let walk = Walk { deep, sole: simple_command(cmd).is_some(), strict: true };
    let (flat, docs) = split_heredocs(cmd);
    collect_units(&flat, None, &[], 0, &walk, &mut out);
    let inner = Walk { deep, sole: false, strict: true };
    let guess = Walk { strict: false, ..inner };
    for doc in docs {
        if heredoc_is_data(&doc.head, &doc.tail) {
            continue;
        }
        // What the body is depends on the command it feeds: the last one on its line.
        let mut head = Vec::new();
        collect_units(&doc.head, None, &[], 0, &Walk { deep: false, sole: false, strict: false }, &mut head);
        let wrappers = head.last().map(|u| u.wrappers.clone()).unwrap_or_default();
        let remote = head.iter().find_map(|u| u.remote.clone());
        let feeds_shell = head.iter().any(|u| SHELLS.contains(&u.prog()) || u.prog() == "ssh");
        let script = !feeds_shell && head.iter().any(|u| runs_inline_script(u.prog()));
        if script {
            // Python, Perl, AppleScript: not shell. The body stands as one unit under the
            // head's wrappers, and its string literals are read as possible commands.
            out.push(Unit {
                argv: Vec::new(),
                text: doc.body.clone(),
                wrappers: wrappers.clone(),
                remote: remote.clone(),
                carrier: false,
                sep: String::new(),
                unparsed: false,
                guessed: true,
                later: false,
                parent: String::new(),
            });
            if deep {
                let literal = compiled(r#""((?:[^"\\\n]|\\.)*)"|'((?:[^'\\\n]|\\.)*)'"#);
                for line in doc.body.lines() {
                    let words: Vec<String> = literal
                        .captures_iter(line)
                        .iter()
                        .filter_map(|c| c.get(1).or_else(|| c.get(2)).map(|m| m.as_str().replace("\\\"", "\"")))
                        .collect();
                    for word in words.iter().filter(|w| w.contains(char::is_whitespace)) {
                        collect_units(word, remote.as_deref(), &wrappers, 1, &guess, &mut out);
                    }
                    if words.len() > 1 {
                        // subprocess.run(["touch", "name"])
                        collect_units(&words.join(" "), remote.as_deref(), &wrappers, 1, &guess, &mut out);
                    }
                }
            }
        } else {
            // A shell, ssh, `source`, a pipe into something: the body is commands.
            collect_units(&doc.body, remote.as_deref(), &wrappers, 1, &inner, &mut out);
        }
    }
    out
}

fn collect_units(cmd: &str, remote: Option<&str>, inherited: &[String], depth: usize, walk: &Walk, out: &mut Vec<Unit>) {
    if depth > MAX_UNIT_DEPTH {
        out.push(Unit {
            argv: Vec::new(),
            text: cmd.to_string(),
            wrappers: inherited.to_vec(),
            remote: remote.map(str::to_string),
            carrier: false,
            sep: String::new(),
            unparsed: true,
            guessed: !walk.strict,
            later: false,
            parent: String::new(),
        });
        return;
    }
    for piece in pieces(cmd) {
        let text = piece.text.trim();
        if text.is_empty() {
            continue;
        }
        for inner in quoted_substitutions(text) {
            collect_units(&inner, remote, inherited, depth + 1, walk, out);
        }
        let split = shlex::split(text);
        let unparsed = walk.strict && (split.is_none() || !piece.closed);
        let mut argv = split.unwrap_or_else(|| text.split_whitespace().map(str::to_string).collect());
        let mut wrappers = inherited.to_vec();
        wrappers.extend(strip_wrappers(&mut argv));
        let prog = argv.first().map(|a| base_name(a).to_string()).unwrap_or_default();
        let mut carrier = false;
        let mut carried: Option<String> = None;
        if SHELLS.contains(&prog.as_str()) {
            if let Some(inner) = shell_c_arg(&argv) {
                carrier = true;
                collect_units(&inner, remote, &wrappers, depth + 1, walk, out);
                carried = Some(inner);
            }
        } else if prog == "eval" && argv.len() > 1 {
            carrier = true;
            collect_units(&argv[1..].join(" "), remote, &wrappers, depth + 1, walk, out);
        } else if prog == "ssh"
            && let Some((host, rest)) = ssh_parts_argv(&argv)
            && !rest.is_empty()
        {
            carrier = true;
            let inner = rest.join(" ");
            collect_units(&inner, Some(&host), &wrappers, depth + 1, walk, out);
            carried = Some(inner);
        }
        if walk.deep {
            let inert = match prog.as_str() {
                // mail is data for another agent; what echo prints goes nowhere on a sole line
                "agent-mail" => Some(vec![true; argv.len()]),
                "echo" | "printf" if walk.sole && depth == 0 => Some(vec![true; argv.len()]),
                _ => inert_tokens(&argv),
            };
            for (i, word) in argv.iter().enumerate() {
                if inert.as_ref().is_some_and(|marks| marks[i]) || carried.as_deref() == Some(word.as_str()) {
                    continue;
                }
                if !word.contains(char::is_whitespace) {
                    continue;
                }
                // `watch 'cmd'`, `su -c 'cmd'`, `python -c 'os.system("cmd")'`, `-c alias.x='!cmd'`
                let guess = Walk { strict: false, ..*walk };
                collect_units(word.trim_start_matches('!'), remote, &wrappers, depth + 1, &guess, out);
                if let Some((_, value)) = word.split_once('=') {
                    collect_units(value.trim_start_matches('!'), remote, &wrappers, depth + 1, &guess, out);
                }
            }
            if inert.is_none() && !carrier && !READERS.contains(&prog.as_str()) {
                // `watch cmd args`, `find ... -exec cmd args`: any later word may be a program.
                for i in 1..argv.len().min(64) {
                    if argv[i].starts_with('-') {
                        continue;
                    }
                    out.push(Unit {
                        argv: argv[i..].to_vec(),
                        text: text.to_string(),
                        wrappers: wrappers.clone(),
                        remote: remote.map(str::to_string),
                        carrier: false,
                        sep: piece.sep.to_string(),
                        unparsed: false,
                        guessed: !walk.strict,
                        later: true,
                        parent: prog.clone(),
                    });
                }
            }
        }
        out.push(Unit {
            argv,
            text: text.to_string(),
            wrappers,
            remote: remote.map(str::to_string),
            carrier,
            sep: piece.sep.to_string(),
            unparsed,
            guessed: !walk.strict,
            later: false,
            parent: String::new(),
        });
    }
}

/// The script of `sh -c '...'` (also `bash -lc`, `zsh -ic`).
fn shell_c_arg(argv: &[String]) -> Option<String> {
    let mut i = 1;
    while i < argv.len() {
        let a = &argv[i];
        if a.starts_with('-') && !a.starts_with("--") && a.contains('c') {
            return argv.get(i + 1).cloned();
        }
        if !a.starts_with('-') {
            return None;
        }
        i += 1;
    }
    None
}

/// What follows `$(` or a backtick inside a double-quoted string: the shell runs it.
fn quoted_substitutions(text: &str) -> Vec<String> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 2,
            b'\'' => {
                i += 1;
                while i < b.len() && b[i] != b'\'' {
                    i += 1;
                }
                i += 1;
            }
            b'"' => {
                let open = i + 1;
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    if b[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
                let close = i.min(b.len());
                let inner = &text[open.min(close)..close];
                for mark in ["$(", "`"] {
                    for (at, _) in inner.match_indices(mark) {
                        out.push(inner[at + mark.len()..].to_string());
                    }
                }
                i += 1;
            }
            _ => i += 1,
        }
    }
    out
}

/// Targets of `>` and `>>` in a piece of a command line (not `2>&1`).
fn redirect_targets(text: &str) -> Vec<String> {
    // One word may be built from quoted and bare parts (`/tmp/a"b c"d`); the shell joins them.
    compiled(r#">{1,2}\|?\s*((?:"[^"]*"|'[^']*'|\\.|[^\s;&|()<>'"\\]+)+)"#)
        .captures_iter(text)
        .iter()
        .filter_map(|c| c.get(1).map(|m| without_quoting(m.as_str())))
        .collect()
}

/// A path as the shell would see it: quotes off, `$HOME` and `~` expanded.
fn expand_vars(token: &str, cfg: &GuardConfig) -> String {
    let home = cfg.home.to_string_lossy();
    let t = token.trim().trim_matches(|c| c == '\'' || c == '"');
    let t = t.replace("${HOME}", &home).replace("$HOME", &home);
    expand_path(&t, cfg)
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
    // `$TMPDIR` and `$HOME` are known; any other `$`, a backtick or a brace is expanded by the
    // shell after the guard has looked, and `..` climbs out. Until 21 Nov 2026
    // `/tmp/{a,../Users/agentkit-me/x}` and `/tmp/$X` passed as temp.
    let mut path = path.trim().trim_matches(|c| c == '\'' || c == '"').to_string();
    if let Some(tmp) = &cfg.tmpdir {
        let tmp = tmp.to_string_lossy();
        let tmp = tmp.trim_end_matches('/');
        path = path.replace("${TMPDIR}", tmp).replace("$TMPDIR", tmp);
    }
    let path = expand_vars(&path, cfg);
    // `{}` too: xargs -I puts its input there, which may be `../..`.
    if path.contains(['$', '`', '{', '}']) || path.split('/').any(|c| c == "..") {
        return false;
    }
    let path = path.as_str();
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
    // `<<< word` is a here-string, not a here-document ending at `word`.
    if line[..full.start()].ends_with('<') {
        return None;
    }
    let delim = caps.get(2).or_else(|| caps.get(3)).or_else(|| caps.get(4))?;
    Some((full.start(), full.end(), delim.as_str().to_string()))
}

/// Is a here-document's body data that nothing on the line can run? Only two shapes are:
/// `cat` or `tee` as a statement of its own with its output sent to a file (`cat > f <<EOF`),
/// and a message handed to `git commit|tag`, `gh` or `agent-mail` as `"$(cat <<EOF`. Anything
/// else (a pipe after it, `eval "$(cat <<EOF`, `source /dev/stdin <<EOF`, a bare `cat <<EOF`
/// whose output an enclosing construct may run) is read as commands.
fn heredoc_is_data(head: &str, tail: &str) -> bool {
    if tail.contains('|') || tail.contains("$(") || tail.contains('`') || tail.contains(';') || tail.contains('&') {
        return false;
    }
    if compiled(r#"^\s*(?:git\s+(?:commit|tag)|gh|agent-mail)\s[^;&|`$()<>]*"\$\(cat\s*$"#).is_match(head) {
        return true;
    }
    let parts = pieces(head);
    let Some(last) = parts.last() else { return false };
    if !last.closed {
        return false;
    }
    if parts.len() > 1 && !matches!(parts[parts.len() - 2].sep, ";" | "&&" | "||" | "\n") {
        return false;
    }
    // A message read from stdin: `git commit -F -`, `git tag -F -`, `--file=-`.
    if compiled(r#"^\s*git\s+(?:commit|tag)\b[^;&|`$()<>]*\s(?:-[A-Za-z]*F\s*-|--file[= ]-)\s*$"#).is_match(last.text) {
        return true;
    }
    let Some(argv) = shlex::split(last.text) else { return false };
    if !argv.first().is_some_and(|prog| matches!(prog.as_str(), "cat" | "tee")) {
        return false;
    }
    // its output goes to a file, so the body is file content
    let to_file = |text: &str| redirect_targets(text).iter().any(|t| !t.starts_with('&'));
    to_file(last.text) || to_file(tail)
}

struct Heredoc {
    /// The line up to `<<`.
    head: String,
    /// The rest of that line after the delimiter.
    tail: String,
    body: String,
}

/// The command line without its here-document bodies, and the bodies with the lines they hang on.
fn split_heredocs(cmd: &str) -> (String, Vec<Heredoc>) {
    if !cmd.contains("<<") {
        return (cmd.to_string(), Vec::new());
    }
    let lines: Vec<&str> = cmd.split('\n').collect();
    let mut flat: Vec<&str> = Vec::new();
    let mut docs = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        flat.push(line);
        if let Some((start, end, delim)) = find_heredoc(line) {
            let mut j = i + 1;
            while j < lines.len() && lines[j].trim() != delim {
                j += 1;
            }
            docs.push(Heredoc {
                head: line[..start].to_string(),
                tail: line[end..].to_string(),
                body: lines[i + 1..j.min(lines.len())].join("\n"),
            });
            i = j;
        }
        i += 1;
    }
    (flat.join("\n"), docs)
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
            if heredoc_is_data(head, tail) {
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
    ssh_parts_argv(&shlex::split(cmd)?)
}

fn ssh_parts_argv(argv: &[String]) -> Option<(String, Vec<String>)> {
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

/// The line with its paths in one spelling: `//` as `/`, `/./` as `/`, `dir/..` dropped, and a
/// quoted `"$HOME"` bare, so `~/Documents//`, `~/./Documents`, `~/Documents/x/..` and
/// `"$HOME"/Documents` read as the folder they name. Until 21 Nov 2026 they passed.
fn icloud_normalize(cmd: &str) -> String {
    let mut t = cmd.to_string();
    for (from, to) in [("\"$HOME\"", "$HOME"), ("\"${HOME}\"", "$HOME"), ("'$HOME'", "$HOME"), ("${HOME}", "$HOME")] {
        t = t.replace(from, to);
    }
    let slashes = compiled(r"/{2,}");
    let dot = compiled(r#"/\.(?=/|["'\s;)|&]|$)"#);
    let up = compiled(r#"/(?!\.\.(?:[/"'\s;)|&]|$))[^/\s"';|&()]+/\.\.(?=[/"'\s;)|&]|$)"#);
    for _ in 0..16 {
        let next = up.replace_all(&dot.replace_all(&slashes.replace_all(&t, "/"), ""), "");
        if next == t {
            break;
        }
        t = next;
    }
    t
}

/// Where a fleet tool sits in a command: as the program, or as the script an interpreter runs
/// (`zsh ~/.local/bin/empty-bin`, `python3 -I ~/.local/bin/cred _values`).
fn tool_position(u: &Unit, tool: &str) -> Option<usize> {
    let first = u.argv.first()?;
    if is_tool(first, tool) {
        return Some(0);
    }
    // An interpreter's options take values (`python -W x`, `-X x`, `-m mod`, `sh -o opt`), so
    // the script word is not simply the first non-option. Any word that is the tool counts as
    // it: a false refusal is cheaper than running it with what follows.
    if SHELLS.contains(&u.prog()) || runs_inline_script(u.prog()) {
        return u.argv.iter().skip(1).position(|a| is_tool(a, tool)).map(|i| i + 1);
    }
    None
}

/// The file operands of `sed` / `perl -p` / `ruby`: without the values of `-e`/`-f`/`-i`
/// (BSD `-i ''` takes a suffix word) and, when no `-e`/`-f` is given, without the script word.
fn script_tool_files(argv: &[String]) -> Vec<&str> {
    let mut files = Vec::new();
    let mut script_given = false;
    let mut unsure = false;
    let mut i = 1;
    while i < argv.len() {
        let a = argv[i].as_str();
        i += 1;
        if matches!(a, "-e" | "-f" | "--expression" | "--file") {
            script_given = true;
            i += 1;
        } else if a.starts_with("--expression=") || a.starts_with("--file=") {
            script_given = true; // the script is inline; the next word is an operand
        } else if a == "-i" && argv.get(i).is_some_and(|v| v.is_empty()) {
            i += 1;
        } else if a.starts_with('-') && !a.starts_with("--") && a.len() > 1 {
            let body = &a[1..];
            if body.starts_with('i') && body.len() > 1 {
                // `-ie`, `-i.bak`: a suffix glued to -i; with e/f in it the reading is unsure
                if body[1..].contains(['e', 'f']) {
                    unsure = true;
                }
            } else if body.ends_with(['e', 'f']) {
                // `-pe`, `-ne`, `-f`: the code or script file is the next word
                script_given = true;
                i += 1;
            } else if body.contains(['e', 'f']) {
                // `-es/a/b/`, `-pes/x/y/`: the script is attached; nothing more to skip
                script_given = true;
            }
        } else if !a.starts_with('-') {
            files.push(a);
        }
    }
    // Only when it is certain that the first plain word is the script is it dropped; otherwise
    // every plain word may be a file, as before.
    if !script_given && !unsure && !files.is_empty() {
        files.remove(0);
    }
    files
}

/// Could the line run the tool at all: its name on the line, or a path word that resolves to it.
fn names_tool(cmd: &str, tool: &str) -> bool {
    let bare = without_quoting(cmd);
    cmd.contains(tool)
        || bare.contains(tool)
        || bare
            .split(|c: char| c.is_whitespace() || ";&|()`<>".contains(c))
            .any(|w| w.contains('/') && is_tool(w, tool))
}

/// The word names the tool: by its base name, or by what it resolves to on disk (a symlink
/// or copy under another name).
fn is_tool(word: &str, tool: &str) -> bool {
    if base_name(word) == tool {
        return true;
    }
    if !word.contains('/') {
        return false;
    }
    let path = match (word.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => PathBuf::from(home).join(rest),
        _ => PathBuf::from(word),
    };
    fs::canonicalize(path).ok().is_some_and(|p| p.file_name().is_some_and(|n| n == tool))
}

/// `empty-bin` empties the whole Bin unless told otherwise; that is the owner's action (18 Nov
/// 2026). An agent may run it only with `--verified-offloads` or `--dry-run`/`-n`, here or over
/// ssh, under any wrapper. Fed its arguments by `xargs`, or a line that cannot be read, is refused.
fn empty_bin(cmd: &str) -> Option<Decision> {
    if !names_tool(cmd, "empty-bin") {
        return None;
    }
    let deny = || {
        Some(Decision::deny(
            "icloud",
            "BLOCKED: `empty-bin` with no option empties the whole Bin, which is the owner's action. The agent path is `empty-bin --verified-offloads` (only items the offload tool verified), or `--dry-run` to look.",
        ))
    };
    let all = units_deep(cmd);
    if any_unparsed(&all) {
        return deny();
    }
    let vars = literal_assignments(cmd);
    for u in &all {
        if u.later && !EXECUTORS.contains(&u.parent.as_str()) {
            continue;
        }
        let Some(at) = tool_position(u, "empty-bin") else { continue };
        let args: Vec<String> = u.argv[at + 1..]
            .iter()
            .map(|a| vars.iter().fold(a.clone(), |t, (n, v)| t.replace(&format!("${{{n}}}"), v).replace(&format!("${n}"), v)))
            .collect();
        let ok = args.iter().any(|a| matches!(a.as_str(), "--verified-offloads" | "--dry-run" | "-n"));
        if !ok || u.wrappers.iter().any(|w| w == "xargs") || args.iter().any(|a| a.contains(['$', '`'])) {
            return deny();
        }
    }
    None
}

fn icloud(cmd: &str) -> Decision {
    if let Some(d) = empty_bin(cmd) {
        return d;
    }
    // The owner's go-ahead is a leading assignment on a lone command, never a word anywhere on
    // the line: until 21 Nov 2026 `ls # ICLOUD_WIPE_APPROVED=1` in a comment switched it off.
    if simple_command(cmd).is_some_and(|argv| argv[0] == "ICLOUD_WIPE_APPROVED=1") {
        return Decision::allow();
    }
    let cmd = &icloud_normalize(cmd);
    // Mail to another agent is text, when the mail is the whole line. Until 14 Nov 2026 a line
    // that merely began with `agent-mail send` was allowed whatever followed it.
    if simple_command(cmd).is_some_and(|argv| base_name(&argv[0]) == "agent-mail") {
        return Decision::allow();
    }
    let root_names = r"(?:Documents|Desktop|Library/Mobile Documents/com~apple~CloudDocs|Library/Mobile\\ Documents/com~apple~CloudDocs)";
    let users_home: String = ['/', 'U', 's', 'e', 'r', 's', '/'].iter().collect();
    let prefix = format!(r#"(?:~/|\$HOME/|\$\{{HOME\}}/|{users_home}[^/\s\"']+/)"#);
    // The folder, or a glob of everything directly in it (`*`, `?*`, `[!.]*`, `.*`, `.[!.]*`).
    let end = r#"(?=/?(?:\*|\?+\*?|\.?\[[^\]/\s]*\]\*?|\.\*|\.\?\?\*)?(?:["'\s;)|&]|$))"#;
    let root_ref = compiled(&format!(r#"[\"']?{prefix}{root_names}{end}"#));
    let bare_ref = compiled(&format!(
        r#"(?:(?<=[\s\"'])|^)(?:Documents|Desktop){end}"#
    ));
    let has_root = |text: &str| root_ref.is_match(text) || bare_ref.is_match(text);
    if cmd.contains("osascript") && compiled(r"(?i)\bempty\s+(?:the\s+)?trash\b").is_match(cmd) {
        return Decision::deny(
            "icloud",
            "BLOCKED iCloud-wide delete risk: emptying the Trash is permanent. On a Mac with iCloud Desktop & Documents it also purges iCloud Recently Deleted. Empty it only with the owner's go-ahead, then start a lone command with ICLOUD_WIPE_APPROVED=1 (the first word, not elsewhere on the line).",
        );
    }
    if empties_trash(cmd) {
        return Decision::deny(
            "icloud",
            "BLOCKED: emptying the Trash is permanent, on every Mac, the workers included (with iCloud Desktop & Documents it also purges iCloud Recently Deleted). Leave the Trash alone; to free space, report its size and let the owner empty it. With his go-ahead in this session, start a lone command with ICLOUD_WIPE_APPROVED=1 (the first word, not elsewhere on the line).",
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
    let rsync_delete = compiled(r"\brsync\b[^;&|]*--del");
    let seps = compiled(r"&&|\|\||;|\n|\|");
    for seg in seps.split(cmd) {
        let verb_hit = verb.is_match(seg);
        let rsync_hit = rsync_delete.is_match(seg);
        if !verb_hit && !rsync_hit {
            continue;
        }
        if rsync_hit && !verb_hit {
            // The destination is not always the last word (`... ~/Documents/ --exclude x`): any
            // operand after the first (the source) that is a root counts.
            let words = shlex::split(seg).unwrap_or_else(|| seg.split_whitespace().map(str::to_string).collect());
            let at = words.iter().position(|w| base_name(w) == "rsync").map(|i| i + 1).unwrap_or(0);
            let operands: Vec<&String> = words[at..].iter().filter(|w| !w.starts_with('-')).collect();
            if operands.iter().skip(1).any(|w| has_root(&format!(" {w}"))) {
                return Decision::deny(
                    "icloud",
                    "BLOCKED iCloud-wide delete risk: `rsync --delete` into a whole Desktop/Documents/iCloud Drive folder. Sync a subfolder, or start a lone command with ICLOUD_WIPE_APPROVED=1 (the first word, not elsewhere on the line) after confirming iCloud Desktop & Documents is off.",
                );
            }
            continue;
        }
        if has_root(seg) && !loop_re.is_match(seg) {
            return Decision::deny(
                "icloud",
                "BLOCKED iCloud-wide delete risk: moving or deleting a whole Desktop/Documents/iCloud Drive folder or all its contents. Move single items. A whole-folder wipe needs iCloud Desktop & Documents confirmed off and a lone command that starts with ICLOUD_WIPE_APPROVED=1.",
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

/// True when a command deletes the Trash itself or everything in it, here or over ssh. The
/// worker exemption of the hard-delete guard does not reach this: a worker may delete its own
/// build output, but its Trash can hold the owner's iCloud files.
fn empties_trash(cmd: &str) -> bool {
    if !cmd.contains(".Trash") {
        return false;
    }
    // The Trash, or a glob of all of it: `~/.Trash`, `~/.Trash/*`, `.Trash/.[!.]*`, `/Volumes/X/.Trashes/501/*`.
    let whole = compiled(r"(?:^|/)\.Trash(?:es(?:/\d+)?)?(?:/?|/(?:\*|\.\*|\.\[[^/]*|\.\?\?\*|\{[^/]*\}))$");
    let inside = compiled(r"(?:^|/)\.Trash(?:es)?(?:/|$)");
    let clean = |t: &str| t.trim().trim_matches(|c| c == '\'' || c == '"').to_string();
    let mut in_trash = false;
    let all = units_deep(cmd);
    if any_unparsed(&all) && compiled(r"\b(?:rm|rmdir|unlink|shred|srm|find)\b").is_match(cmd) {
        return true;
    }
    for u in all {
        let prog = u.prog();
        let operands: Vec<String> = u.operands().iter().map(|t| clean(t)).collect();
        if prog == "cd" {
            in_trash = operands.first().is_some_and(|t| inside.is_match(t.trim_end_matches('/')) && whole.is_match(t));
            continue;
        }
        let everything_here = |t: &String| matches!(t.as_str(), "*" | "." | "./" | "./*" | ".*" | ".[!.]*" | "..?*");
        if DELETERS.contains(&prog)
            && operands.iter().any(|t| whole.is_match(t) || (in_trash && everything_here(t)))
        {
            return true;
        }
        let find_deletes = prog == "find"
            && (u.argv.iter().any(|a| a == "-delete")
                || u.argv.windows(2).any(|w| w[0] == "-exec" && DELETERS.contains(&base_name(&w[1]))));
        if find_deletes
            && operands.first().is_some_and(|root| inside.is_match(root) || (in_trash && everything_here(root)))
        {
            return true;
        }
    }
    false
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
    let mut markers = cfg.push_markers.clone();
    if !markers.iter().any(|m| m == "AGENTKIT_PUSH_APPROVED=1") {
        markers.push("AGENTKIT_PUSH_APPROVED=1".into());
    }
    // Deleting a remote branch is routine but not undoable from here: confirm it. The refusal
    // tells the caller to rerun with the owner's marker, so the marker answers it, but only as
    // an assignment on that very `git push`, on a line that is that one command. A marker
    // elsewhere on a compound line approves nothing here.
    if compiled(r"\bgit\s+push\b[^|;&]*\s(?:--delete\b|-d\b|:[^\s|;&]+)").is_match(&c) {
        // One plain command: no separator, substitution, grouping or redirection, and no lease.
        let lone = !compiled(r"[|;&`(){}<>\n\r]|\$\(").is_match(cmd.trim())
            && !compiled(r"--force-with-lease\b").is_match(&c);
        let prefixed = markers.iter().any(|marker| {
            let pat = format!(
                r"^\s*(?:[A-Za-z_][A-Za-z0-9_]*=\S*\s+)*{}\s+(?:[A-Za-z_][A-Za-z0-9_]*=\S*\s+)*git\s+push\b",
                escape_literal(marker)
            );
            compiled(&pat).is_match(&c)
        });
        if lone && prefixed {
            return Decision::allow();
        }
        return Decision::ask(
            "push-delete",
            "This push deletes a branch or tag on the remote. Confirm it is merged or backed up and that the owner approved; then run it alone as `AGENTKIT_PUSH_APPROVED=1 git push ... --delete ...`.",
        );
    }
    // Force-with-lease rewrites history: no marker answers it.
    if compiled(r"--force-with-lease\b").is_match(&c) {
        return Decision::ask(
            "force-with-lease",
            "Force-with-lease still rewrites the remote branch. Confirm this is your branch and the owner approved.",
        );
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
    // Fail closed: the session's cwd always counts, with one exception. A lone, plain
    // `[VAR=x ...] git -C <existing dir> push ...` (no separator, subshell, substitution,
    // redirection or comment) provably runs the push in that directory, so that directory
    // alone decides. Every other shape (a `cd` in a subshell, `cd -`, `--work-tree`, a second
    // command) keeps the cwd in (security review, 15 Nov 2026).
    let mut dirs = match lone_git_c_push_dir(cmd, cfg) {
        Some(dir) => vec![dir],
        None => vec![cfg.cwd.clone()],
    };
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

/// The directory of a lone, plain `git -C <literal dir> push ...`, when `<dir>` exists.
/// Anything else returns None, so the caller keeps the session cwd: a compound line
/// (separators, subshells, substitution, redirection, comments, newlines), any environment
/// prefix (GIT_DIR or GIT_WORK_TREE can move the repository behind the -C), any other git
/// option before `push`, and any path the shell would rewrite or keep literal where the guard
/// would not (`$`, `\`, glob characters, `~`).
fn lone_git_c_push_dir(cmd: &str, cfg: &GuardConfig) -> Option<PathBuf> {
    let line = cmd.trim();
    if compiled(r"[|;&`(){}<>#\n\r$\\*?\[\]~]").is_match(line) {
        return None;
    }
    let re = compiled(r#"^git\s+-C\s+(?:"([^"]+)"|'([^']+)'|([^\s"']+))\s+push\b"#);
    let caps = re.captures(line)?;
    let raw = caps.get(1).or_else(|| caps.get(2)).or_else(|| caps.get(3))?.as_str();
    let dir = resolve_to_pathbuf(raw, cfg);
    dir.is_dir().then_some(dir)
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

/// Disk work on an external volume runs under `disk-slot`. Each command on the line is judged
/// by its own program. Until 14 Nov 2026 the word `disk-slot` anywhere on the line allowed all
/// of it (`echo disk-slot; cp /Volumes/X/a /Volumes/Y/b`), and a read-only verb anywhere in a
/// command exempted it (`cp /Volumes/X/test/a ...`, `diskutil eraseVolume ...`).
fn disk_slot(cmd: &str, _cfg: &GuardConfig) -> Decision {
    let hits_volume = compiled(r#"(?i)(?:^|[\s"'=(])/(?:Volumes|private/Volumes)/"#).is_match(cmd)
        || compiled(r#"\s~?/?Volumes/"#).is_match(cmd);
    if !hits_volume {
        return Decision::allow();
    }
    let on_volume = |text: &str| text.contains("/Volumes/");
    // With bash's $'...' quoting on the line nothing can say where a command ends, so the
    // wrapper cannot be trusted to cover what touches the volume.
    if has_dollar_quote(cmd) {
        return disk_slot_deny();
    }
    let all = units(cmd);
    // Something on a line that names a volume could not be read (nested too deep, a quote
    // that never closes): it is not known to be wrapped or read-only.
    if all.iter().any(|u| u.unparsed && !u.wrappers.iter().any(|w| w == "disk-slot")) {
        return disk_slot_deny();
    }
    for u in all {
        if u.wrappers.iter().any(|w| w == "disk-slot") {
            continue;
        }
        let writes_volume = |text: &str| redirect_targets(text).iter().any(|t| on_volume(t));
        if u.carrier {
            // `ssh host '...'` and `sh -c '...'`: the commands inside are judged on their own.
            // What is left here is a redirect on the carrier itself.
            if writes_volume(&blank_quotes(&u.text)) {
                return disk_slot_deny();
            }
            continue;
        }
        if !on_volume(&u.text) {
            continue;
        }
        let read_only = match u.prog() {
            "ls" | "stat" | "df" | "du" | "test" | "[" | "[[" => true,
            "diskutil" => {
                let sub: Vec<&str> = u.argv.iter().skip(1).map(String::as_str).collect();
                matches!(sub.as_slice(), ["list", ..] | ["info", ..] | ["activity", ..] | ["apfs", "list", ..])
            }
            _ => false,
        };
        if !read_only || writes_volume(&u.text) {
            return disk_slot_deny();
        }
    }
    Decision::allow()
}

fn disk_slot_deny() -> Decision {
    Decision::deny(
        "disk-slot",
        "Disk work on /Volumes/* must run as `disk-slot PATH [PATH ...] -- <command>` so one physical disk is not thrashed. Each command on the line needs it; only ls, stat, df, du, test and diskutil list/info are exempt.",
    )
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
        r"{POSITION}(rsync|scp|sftp|wget|aws\s+s3\s+(?:cp|sync|mv)|gcloud\s+storage\s+cp|azcopy|tar\s+-c[f]?|docker\s+(?:pull|push)|npm\s+(?:ci|install)|pnpm\s+install|yarn\s+install|brew\s+(?:install|upgrade|fetch)|(?:python3?(?:\.\d+)?\s+-m\s+)?pip3?(?:\.\d+)?\s+install|uv\s+(?:pip\s+install|sync)|cargo\s+(?:fetch|install)|git(?:\s+-[Cc]\s+\S+)*\s+(?:clone|lfs\s+(?:pull|fetch))|ollama\s+pull|huggingface-cli\s+download|hf\s+download)\b"
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
            "A metered link is active (~/.local/state/METERED_LINK). Do not start a large transfer, package restore, clone or model pull; use a fibre worker or prefix METERED_TRANSFER_APPROVED=1 after the owner approves.",
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

const SLOT_TOOL: &str = "heavy-slot";

fn is_kill_signal(name: &str) -> bool {
    name == "9" || name.eq_ignore_ascii_case("KILL") || name.eq_ignore_ascii_case("SIGKILL")
}

/// (sends SIGKILL, plain operands) for `kill`, `pkill` or `killall`. A `-N` after the signal is a
/// process group for `kill`, so it is returned as an operand.
fn kill_args(argv: &[String]) -> (bool, Vec<String>) {
    let mut sigkill = false;
    let mut signal_seen = false;
    let mut operands = Vec::new();
    let mut i = 1;
    while i < argv.len() {
        let a = argv[i].as_str();
        i += 1;
        if matches!(a, "-s" | "-n" | "--signal") {
            sigkill |= argv.get(i).is_some_and(|v| is_kill_signal(v));
            signal_seen = true;
            i += 1;
        } else if let Some(v) = a.strip_prefix("--signal=") {
            sigkill |= is_kill_signal(v);
            signal_seen = true;
        } else if let Some(body) = a.strip_prefix('-').filter(|b| !b.is_empty() && !b.starts_with('-')) {
            if is_kill_signal(body) && !signal_seen {
                sigkill = true;
                signal_seen = true;
            } else if signal_seen && body.chars().all(|c| c.is_ascii_digit()) {
                operands.push(body.to_string());
            } else if body.chars().all(|c| c.is_ascii_alphanumeric()) && !signal_seen {
                // another signal (`-TERM`, `-15`), or a flag such as pkill's `-f`
                signal_seen = body.chars().all(|c| c.is_ascii_digit()) || body.chars().all(|c| c.is_ascii_uppercase());
            }
        } else if a != "--" {
            operands.push(a.to_string());
        }
    }
    (sigkill, operands)
}

/// SIGKILL on heavy-slot loses the job it holds and leaves its queue entry behind. Denied when
/// the command names heavy-slot (`pkill -9 heavy-slot`, `kill -9 $(pgrep heavy-slot)`), here or
/// over ssh, and when a numeric pid has an entry in this Mac's heavy-slot queue: that is a file
/// lookup, no process is started. A numeric pid on another Mac cannot be attributed from here
/// and is left alone.
fn slot_holder(cmd: &str, cfg: &GuardConfig) -> Decision {
    if !compiled(r"\b(?:p?kill|killall)\b").is_match(cmd) {
        return Decision::allow();
    }
    let finds_tool = compiled(&format!(
        r"\b(?:pgrep|pidof|grep)\b[^;&\n]*{}",
        escape_literal(SLOT_TOOL)
    ))
    .is_match(cmd);
    let deny = || {
        Decision::deny(
            "slot-holder",
            "SIGKILL on heavy-slot loses the job and leaves its queue entry behind. To free the slot and keep the job: `heavy-slot release <pid>`. To stop the job: plain `kill <pid>` (TERM), which heavy-slot passes on.",
        )
    };
    let dir = cfg.slot_dir();
    let queue = dir.join("queue");
    // The line names heavy-slot or its state directory: a pid computed on it may be a holder's.
    let home_relative = dir.strip_prefix(&cfg.home).ok().map(|rel| rel.to_string_lossy().into_owned());
    let names_slot = cmd.contains(SLOT_TOOL)
        || cmd.contains(&*dir.to_string_lossy())
        || home_relative.as_deref().is_some_and(|rel| !rel.is_empty() && cmd.contains(rel));
    let all = units_deep(cmd);
    if any_unparsed(&all) && compiled(r"(?i)(?:-9\b|KILL)").is_match(cmd) {
        return deny();
    }
    for u in all {
        let prog = u.prog();
        if !matches!(prog, "kill" | "pkill" | "killall") {
            continue;
        }
        let (sigkill, operands) = kill_args(&u.argv);
        if !sigkill {
            continue;
        }
        let named = match prog {
            "kill" => {
                // No literal pid: `$(...)`, a backtick, `$VAR`, or pids piped in through xargs.
                let computed = operands.is_empty()
                    || matches!(u.sep.as_str(), "$(" | "`")
                    || operands.iter().any(|o| !o.chars().all(|c| c.is_ascii_digit()));
                finds_tool
                    || (names_slot && computed)
                    || (u.remote.is_none()
                        && operands.iter().any(|pid| {
                            pid.chars().all(|c| c.is_ascii_digit()) && queue.join(format!("{pid}.json")).is_file()
                        }))
            }
            _ => operands.iter().any(|o| o.contains(SLOT_TOOL)),
        };
        if named {
            return deny();
        }
    }
    Decision::allow()
}

/// Text of an inline script that changes a file, in the languages agents reach for.
fn script_writes(cmd: &str) -> bool {
    compiled(
        r#"open\s*\([^)]*,\s*["'](?:[wax]|r\+)|,\s*["'](?:[wax]\+?b?|r\+b?)["']\s*\)|["']>>?|write_text|write_bytes|\.write\s*\(|writeFile|appendFile|\.touch\s*\(|truncate|unlink|os\.remove|os\.rename|os\.replace|shutil\.|mknod|mkdir|File\.(?:write|open)"#,
    )
    .is_match(cmd)
}

fn runs_inline_script(prog: &str) -> bool {
    ["python", "perl", "ruby", "node", "osascript"].iter().any(|p| prog.starts_with(p))
}

/// `sed -i`, `perl -pi`: a short flag cluster with `i` in it, or `--in-place`.
fn edits_in_place(argv: &[String]) -> bool {
    argv.iter().skip(1).any(|a| {
        a == "--in-place"
            || a.starts_with("--in-place=")
            || (a.starts_with('-') && !a.starts_with("--") && a.contains('i'))
    })
}

fn slot_file_names(cfg: &GuardConfig) -> Vec<&str> {
    cfg.slot_files.iter().map(String::as_str).filter(|n| !n.is_empty() && !n.contains('/')).collect()
}

/// Is this path one of heavy-slot's hand-off files? By resolved path on this Mac, and by its
/// last two components anywhere (`.../heavy-slot/limit`), which is all that is known over ssh.
fn slot_file_hit(token: &str, cfg: &GuardConfig) -> bool {
    let t = expand_vars(token, cfg);
    if t.is_empty() {
        return false;
    }
    let dir = cfg.slot_dir();
    let dir_name = dir.file_name().and_then(|n| n.to_str()).unwrap_or(SLOT_TOOL);
    slot_file_names(cfg).iter().any(|name| {
        t == format!("{dir_name}/{name}")
            || t.ends_with(&format!("/{dir_name}/{name}"))
            || resolve_path(&t, cfg) == resolve_path(&dir.join(name).to_string_lossy(), cfg)
    })
}

fn slot_files_deny() -> Decision {
    Decision::deny(
        "slot-files",
        "heavy-slot's `limit` and `cool` files are not edited by hand. Leave them; if a slot looks full on a cool, idle Mac, that is a fault to look into (`heavy-slot status`) and report, and `heavy-slot --now` is the owner's to ask for.",
    )
}

fn slot_files_path(path: &str, cfg: &GuardConfig) -> Decision {
    if slot_file_hit(path, cfg) { slot_files_deny() } else { Decision::allow() }
}

/// Shell writes to heavy-slot's `limit` and `cool`: a redirect, `tee`, `sed -i`, `cp`/`mv` onto
/// them, removing them, or an inline script that opens them for writing. Reading them is fine.
fn slot_files(cmd: &str, cfg: &GuardConfig) -> Decision {
    let dir = cfg.slot_dir();
    let dir_name = dir.file_name().and_then(|n| n.to_str()).unwrap_or(SLOT_TOOL).to_string();
    let here = PathBuf::from(resolve_path(&cfg.cwd.to_string_lossy(), cfg));
    let dir_real = PathBuf::from(resolve_path(&dir.to_string_lossy(), cfg));
    if !cmd.contains(&dir_name) && !without_quoting(cmd).contains(&dir_name) && !here.starts_with(&dir_real) {
        return Decision::allow();
    }
    let names = slot_file_names(cfg);
    let named_in_text = compiled(&format!(
        r"{}/(?:{})\b",
        escape_literal(&dir_name),
        names.iter().map(|n| escape_literal(n)).collect::<Vec<_>>().join("|")
    ))
    .is_match(cmd);
    let mut local = cfg.clone();
    let all = units_deep(cmd);
    // Fail closed only when the line is about these files, not for every use of the tool's name.
    let about_files = named_in_text
        || here.starts_with(&dir_real)
        || (cmd.contains(&format!("/{dir_name}"))
            && compiled(&format!(
                r"\b(?:{})\b",
                names.iter().map(|n| escape_literal(n)).collect::<Vec<_>>().join("|")
            ))
            .is_match(cmd));
    if about_files && any_unparsed(&all) {
        return slot_files_deny();
    }
    for u in all {
        let prog = u.prog();
        let operands = u.operands();
        if prog == "cd" {
            if let Some(target) = operands.first() {
                local.cwd = PathBuf::from(resolve_path(&expand_vars(target, &local), &local));
            }
            continue;
        }
        let hit = |t: &str| slot_file_hit(t, &local);
        let is_dir = |t: &str| resolve_path(&expand_vars(t, &local), &local) == dir_real.to_string_lossy();
        let any = operands.iter().any(|t| hit(t));
        let onto = operands.last().is_some_and(|dest| {
            hit(dest)
                || (is_dir(dest)
                    && operands[..operands.len() - 1]
                        .iter()
                        .any(|src| names.contains(&base_name(src.trim_end_matches('/')))))
        });
        let wrote = redirect_targets(&u.text).iter().any(|t| hit(t))
            || match prog {
                "tee" | "truncate" | "mv" | "trash" | "rm" | "unlink" | "shred" | "srm" => any,
                "sed" | "gsed" => any && edits_in_place(&u.argv),
                "cp" | "install" | "ln" | "rsync" | "ditto" => onto,
                "dd" => u.argv.iter().any(|a| a.strip_prefix("of=").is_some_and(|t| hit(t))),
                _ => false,
            }
            || (runs_inline_script(prog)
                && ((any && edits_in_place(&u.argv)) || (named_in_text && script_writes(cmd))));
        if wrote {
            return slot_files_deny();
        }
    }
    Decision::allow()
}

/// Does a path have a component that is an owner-approval marker (`APPROVE_*` by default)?
fn approval_name_hit(token: &str, cfg: &GuardConfig) -> bool {
    let t = token.trim().trim_matches(|c| c == '\'' || c == '"');
    t.split('/').any(|part| {
        cfg.approval_file_patterns.iter().any(|pat| match pat.strip_suffix('*') {
            Some(prefix) => !prefix.is_empty() && part.starts_with(prefix),
            None => !pat.is_empty() && part == pat,
        })
    })
}

fn approval_files_deny() -> Decision {
    Decision::deny(
        "approval-files",
        "An `APPROVE_*` marker file is the owner's approval and only he creates it. Do not create, copy or move one; tell the owner what is ready and which file he would create.",
    )
}

fn approval_files_path(path: &str, cfg: &GuardConfig) -> Decision {
    if approval_name_hit(path, cfg) { approval_files_deny() } else { Decision::allow() }
}

/// Creating an owner-approval marker from a shell: `touch`, a redirect, `tee`, `cp`, `mv`,
/// `install`, `ln`, `mkdir`, an inline script, here or over ssh. Reading or testing for one is fine.
fn approval_files(cmd: &str, cfg: &GuardConfig) -> Decision {
    let literals: Vec<&str> = cfg
        .approval_file_patterns
        .iter()
        .map(|p| p.trim_end_matches('*'))
        .filter(|p| !p.is_empty())
        .collect();
    let bare = without_quoting(cmd);
    if !literals.iter().any(|l| cmd.contains(l) || bare.contains(l)) {
        return Decision::allow();
    }
    let named_in_text = cfg.approval_file_patterns.iter().any(|pat| {
        let body = match pat.strip_suffix('*') {
            Some(prefix) if !prefix.is_empty() => format!(r"{}[\w.-]*", escape_literal(prefix)),
            Some(_) => return false,
            None => format!(r"{}(?![\w.-])", escape_literal(pat)),
        };
        compiled(&format!(r#"(?:^|[/"'\s(=,]){body}"#)).is_match(cmd)
    });
    let all = units_deep(cmd);
    if any_unparsed(&all) {
        return approval_files_deny();
    }
    for u in all {
        let prog = u.prog();
        let any = u.operands().iter().any(|t| approval_name_hit(t, cfg));
        let wrote = redirect_targets(&u.text).iter().any(|t| approval_name_hit(t, cfg))
            || match prog {
                "touch" | "tee" | "cp" | "mv" | "install" | "ln" | "mkdir" | "rsync" | "ditto" | "truncate" => any,
                "dd" => u.argv.iter().any(|a| a.strip_prefix("of=").is_some_and(|t| approval_name_hit(t, cfg))),
                _ => false,
            }
            || (runs_inline_script(prog) && named_in_text && script_writes(cmd));
        if wrote {
            return approval_files_deny();
        }
    }
    Decision::allow()
}

/// What a protected path is to the guard-config guard.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Guarded {
    /// No agent writes it: agentkit's site config, the Codex and Cursor hook files.
    Whole,
    /// Claude's user settings: an edit that leaves `hooks` and `disableAllHooks` alone is fine.
    ClaudeSettings,
    /// Codex's config: an edit that leaves its `hooks` trust table and hook features alone is fine.
    CodexConfig,
}

/// agentkit's config directories: the one this hook loaded, and the default under home.
fn agentkit_config_dirs(cfg: &GuardConfig) -> Vec<String> {
    let mut dirs = vec![resolve_path(&cfg.home.join(".config/agentkit").to_string_lossy(), cfg)];
    if let Some(dir) = &cfg.config_dir {
        let d = resolve_path(&dir.to_string_lossy(), cfg);
        if !dirs.contains(&d) {
            dirs.push(d);
        }
    }
    dirs
}

/// The files the guards are wired through, resolved, with what each one is.
fn guarded_files(cfg: &GuardConfig) -> Vec<(String, Guarded)> {
    let at = |rel: &str| resolve_path(&cfg.home.join(rel).to_string_lossy(), cfg);
    vec![
        (at(".claude/settings.json"), Guarded::ClaudeSettings),
        (at(".claude/settings.local.json"), Guarded::ClaudeSettings),
        (at(".codex/hooks.json"), Guarded::Whole),
        (at(".codex/config.toml"), Guarded::CodexConfig),
        (at(".cursor/hooks.json"), Guarded::Whole),
        (at(".gemini/config/hooks.json"), Guarded::Whole),
    ]
}

/// Is a resolved path one the guard-config guard protects? In agentkit's config directory
/// everything is, except what agents are meant to write there: the shared notebook (`memory/`),
/// `skills/`, the prose rules (`rules/*.md`, `rules/projects/`) and `paused-providers`.
fn guarded_kind(full: &str, cfg: &GuardConfig) -> Option<Guarded> {
    let full = full.trim_end_matches('/');
    for dir in agentkit_config_dirs(cfg) {
        if full == dir {
            return Some(Guarded::Whole);
        }
        if let Some(rel) = full.strip_prefix(&format!("{dir}/")) {
            let open = rel.starts_with("memory/")
                || rel == "memory"
                || rel.starts_with("skills/")
                || rel == "skills"
                || rel == "paused-providers"
                || rel.starts_with("rules/projects/")
                || (rel.starts_with("rules/") && rel.ends_with(".md"));
            return if open { None } else { Some(Guarded::Whole) };
        }
    }
    if let Some(k) = guarded_files(cfg).into_iter().find(|(p, _)| p == full).map(|(_, k)| k) {
        return Some(k);
    }
    // The decision ledger and its HMAC key (crate::ledger): an agent that could replace them
    // (a FIFO, a symlink, a truncation) could stall the hook or erase its own record.
    if ledger_files(cfg).iter().any(|f| full == f || full.starts_with(&format!("{f}."))) {
        return Some(Guarded::Whole);
    }
    for dir in ledger_dirs(cfg) {
        if full == dir {
            return Some(Guarded::Whole);
        }
        if full.starts_with(&format!("{dir}/")) {
            return Some(Guarded::Whole);
        }
    }
    // The same files in any directory: a project's `.claude/settings.json` with
    // `disableAllHooks` switches the guards off for every session there.
    if full.ends_with("/.claude/settings.json") || full.ends_with("/.claude/settings.local.json") {
        return Some(Guarded::ClaudeSettings);
    }
    if full.ends_with("/.codex/config.toml") {
        return Some(Guarded::CodexConfig);
    }
    if full.ends_with("/.codex/hooks.json")
        || full.ends_with("/.cursor/hooks.json")
        || full.ends_with("/.gemini/config/hooks.json")
    {
        return Some(Guarded::Whole);
    }
    // doctor's record of the owner's accepted [guards] section (`agentkit doctor --accept-guards`).
    if full.ends_with("/agentkit/guards-accepted.json") {
        return Some(Guarded::Whole);
    }
    None
}

/// The ledger's protected folders and files, from the writer's own functions (crate::ledger).
fn ledger_dirs(cfg: &GuardConfig) -> Vec<String> {
    crate::ledger::protected_paths(&cfg.home).0.iter().map(|d| resolve_path(&d.to_string_lossy(), cfg)).collect()
}

fn ledger_files(cfg: &GuardConfig) -> Vec<String> {
    crate::ledger::protected_paths(&cfg.home).1.iter().map(|d| resolve_path(&d.to_string_lossy(), cfg)).collect()
}

/// The whole protected set: the ledger's files and folders, resolved.
fn ledger_protected(cfg: &GuardConfig) -> Vec<String> {
    let mut all = ledger_files(cfg);
    all.extend(ledger_dirs(cfg));
    all
}

/// True when `top` is a protected ledger path or a folder above one, wherever it lives. Home
/// and the folders above it are left to the other rules (a copy into `~` is not a replacement
/// of `~/.local/state`); everything between home and the ledger counts.
fn ledger_ancestor_hit(top: &str, cfg: &GuardConfig) -> bool {
    let top = top.trim_end_matches('/');
    if top.is_empty() {
        return false;
    }
    let home = resolve_path(&cfg.home.to_string_lossy(), cfg);
    let home = home.trim_end_matches('/');
    let at_or_above_home = top == home || home.starts_with(&format!("{top}/"));
    ledger_protected(cfg).iter().any(|p| {
        let p = p.trim_end_matches('/');
        p == top || (p.starts_with(&format!("{top}/")) && !at_or_above_home)
    }) || same_inode_as_ledger(top, cfg)
}

/// The ledger or its key under another name: a hard link made earlier shares the inode.
fn same_inode_as_ledger(path: &str, cfg: &GuardConfig) -> bool {
    use std::os::unix::fs::MetadataExt;
    let Ok(m) = fs::metadata(path) else { return false };
    if !m.is_file() {
        return false;
    }
    ledger_files(cfg).iter().any(|f| fs::metadata(f).is_ok_and(|n| n.dev() == m.dev() && n.ino() == m.ino()))
}

/// Programs that only read and have no option that writes a file or runs a program. Every
/// other program given the ledger, its key, its folder (or an ancestor, or the ledger's inode)
/// in any argument is treated as a write. (`rg` is not here: `--pre` runs a program.)
const LEDGER_READERS: &[&str] = &["cat", "head", "tail", "wc", "ls", "stat", "grep", "jq"];

fn is_readonly_ledger_unit(u: &Unit) -> bool {
    let prog = u.prog();
    if LEDGER_READERS.contains(&prog) {
        return true;
    }
    prog == "find"
        && !u.argv.iter().any(|a| {
            matches!(
                a.as_str(),
                "-delete" | "-exec" | "-execdir" | "-ok" | "-okdir" | "-fls" | "-fprint" | "-fprint0" | "-fprintf"
            )
        })
}

/// The paths a word may name: the word itself and the value of `--opt=VALUE` or `name=VALUE`
/// (`of=` for dd). Short options with a glued value (`-oVALUE`) are judged by substring in
/// `ledger_write_hit`, never by slicing.
fn word_paths(word: &str) -> Vec<String> {
    let mut out = vec![word.to_string()];
    if let Some((_, v)) = word.split_once('=') {
        out.push(v.to_string());
    }
    out.retain(|w| !w.is_empty());
    out
}

/// Names that, appearing anywhere in a word, may point at the ledger: each protected path's
/// base name and the paths themselves.
fn ledger_name_parts(cfg: &GuardConfig) -> Vec<String> {
    let mut out = Vec::new();
    for p in ledger_protected(cfg) {
        if let Some(name) = std::path::Path::new(&p).file_name() {
            out.push(name.to_string_lossy().into_owned());
        }
        out.push(p);
    }
    out.extend(ledger_needles(cfg));
    out.retain(|w| !w.is_empty());
    out.sort();
    out.dedup();
    out
}

/// The value glued to a short option (`-C~/x` -> `~/x`), cut on a character boundary.
fn glued_value(word: &str) -> Option<&str> {
    if !word.starts_with('-') || word.starts_with("--") {
        return None;
    }
    let mut chars = word.char_indices().skip(1);
    chars.next()?;
    let (i, _) = chars.next()?;
    word.get(i..).filter(|v| !v.is_empty())
}

/// A command whose structure the cwd tracking cannot follow: subshells, groups, compound
/// statements, functions, substitutions, here-docs. Any cd in it leaves the folder unknown.
fn cwd_untrackable(cmd: &str) -> bool {
    if cmd.contains(['(', ')', '{', '}', '`', '<']) {
        return true;
    }
    cmd.split(|c: char| c.is_whitespace() || c == ';' || c == '&' || c == '|')
        .any(|w| matches!(w, "if" | "then" | "else" | "elif" | "fi" | "while" | "until" | "for" | "do" | "done" | "case" | "esac" | "function" | "select"))
}

/// Stat-only check, before any fast path: every word of every unit (and every redirect target),
/// resolved against the working folder as unconditional `cd`/`pushd`/`popd` move it. A hit on
/// the ledger's files or folders, an ancestor, or the ledger's inode denies unless the program
/// only reads. A single-dash word, or any relative word once the folder is unknown (a `cd -`,
/// a `cd` after `||`, in a pipe or a subshell, or to a target that cannot be resolved), is a hit
/// when it contains a protected name. Defence in depth only: a same-user process can still
/// rewrite the file, so the ledger is shipped every minute and the archive host's sequence
/// check is the real control.
fn ledger_write_hit(cmd: &str, cfg: &GuardConfig) -> bool {
    // First, whole-command rules over every cd/pushd at any depth (tracked or not).
    // (a) A target that resolves into or above the ledger's folder, with any program that is
    //     not a reader (or any redirect), refuses the command.
    // (b) A target known only at run time (`$(…)`, backticks, `cd -`, `+N`, a glob, an
    //     unexpanded variable) refuses it only when the command also names a protected file,
    //     folder or ancestor, or hands a glob, `.` or `..` to a non-reader or a redirect, or a
    //     bare relative name to a redirect or a file-changing program (`: > *`,
    //     `truncate -s0 decisions.jsonl`). Ordinary `cd "$dir" && make` stays allowed.
    let units = units_deep(cmd);
    let mut known_risky = false;
    let mut runtime_cd = false;
    for u in units.iter().filter(|u| u.remote.is_none() && matches!(u.prog(), "cd" | "pushd")) {
        if u.text.contains('`') || u.text.contains("$(") || u.sep == "$(" || u.sep == "`" {
            runtime_cd = true;
            continue;
        }
        match u.argv.get(1).map(String::as_str) {
            None => {}
            Some(t) if t == "-" || t.starts_with('+') || (t.starts_with('-') && t.len() > 1) => runtime_cd = true,
            Some(t) => {
                let e = expand_vars(t, cfg);
                if e.contains(['$', '`', '*', '?', '[']) {
                    runtime_cd = true;
                } else {
                    let full = resolve_path(&e, cfg);
                    if in_ledger_dir(&full, cfg) || ledger_ancestor_hit(&full, cfg) {
                        known_risky = true;
                    }
                }
            }
        }
    }
    let is_writer = |u: &Unit| {
        let prog = u.prog();
        !prog.is_empty() && !is_readonly_ledger_unit(u) && !matches!(prog, "cd" | "pushd" | "popd")
    };
    let any_redirect = units.iter().any(|u| !redirect_targets(&u.text).is_empty());
    if known_risky && (any_redirect || units.iter().any(is_writer)) {
        return true;
    }
    if runtime_cd {
        const CHANGERS: &[&str] = &["rm", "rmdir", "unlink", "mv", "cp", "ln", "link", "truncate", "tee", "dd", "shred",
                                    "chmod", "chflags", "chown", "touch", "mkfifo", "install", "rsync", "sed", "perl", "srm", ":"];
        let parts = ledger_name_parts(cfg);
        let names_protected = units.iter().any(|u| u.argv.iter().chain(std::iter::once(&u.text)).any(|w| parts.iter().any(|p| w.contains(p.as_str()))));
        let vague = |w: &str| w.contains(['*', '?', '[']) || w == "." || w == ".." || w == "./" || w == "../";
        let bare = |w: &str| !w.starts_with('-') && !w.contains('/') && !w.contains('=') && !w.is_empty();
        let hands_vague = units.iter().any(|u| {
            let prog = u.prog();
            let targets = redirect_targets(&u.text);
            targets.iter().any(|t| vague(t) || bare(t))
                || (is_writer(u) && u.argv.iter().skip(1).any(|w| vague(w)))
                || (CHANGERS.contains(&prog) && u.argv.iter().skip(1).any(|w| bare(w)))
        });
        if names_protected || hands_vague {
            return true;
        }
    }
    let mut local = cfg.clone();
    let mut stack: Vec<PathBuf> = Vec::new();
    let mut cwd_known = true;
    let parts = ledger_name_parts(cfg);
    let named = |w: &str| parts.iter().any(|p| w.contains(p.as_str()));
    let untrackable = cwd_untrackable(cmd);
    let hit = |t: &str, c: &GuardConfig, known: bool| {
        if t.starts_with('-') && !t.starts_with("--") {
            if named(t) {
                return true;
            }
            if let Some(v) = glued_value(t) {
                let full = resolve_path(&expand_vars(v, c), c);
                if ledger_ancestor_hit(&full, c) || in_ledger_dir(&full, c) {
                    return true;
                }
            }
        }
        if !known && !t.starts_with('/') && !t.starts_with('~') && named(t) {
            return true;
        }
        word_paths(t).iter().any(|w| {
            let full = resolve_path(&expand_vars(w, c), c);
            ledger_ancestor_hit(&full, c) || in_ledger_dir(&full, c)
        })
    };
    let mut prev_sep = String::new();
    for u in units_deep(cmd) {
        let sep_before = std::mem::replace(&mut prev_sep, u.sep.clone());
        if u.remote.is_some() {
            continue;
        }
        let prog = u.prog();
        if matches!(prog, "cd" | "pushd" | "popd") {
            // Tracked only as a plain top-level unit joined by ; && or a newline; anything else
            // (carried by sh -c/eval/ssh, wrapped, inside any compound syntax) fails closed.
            let unconditional = !untrackable && !u.guessed && !u.later && !u.carrier && u.wrappers.is_empty()
                && matches!(sep_before.as_str(), "" | ";" | "&&" | "\n")
                && matches!(u.sep.as_str(), "" | ";" | "&&" | "\n");
            let target = u.argv.get(1).map(String::as_str);
            let plain = |t: &str| !(t == "-" || t.starts_with('+') || (t.starts_with('-') && t.len() > 1) || t.contains(['$', '`', '*', '?']));
            if !unconditional {
                cwd_known = false;
            } else if prog == "popd" {
                match stack.pop() {
                    Some(back) if target.is_none() => local.cwd = back,
                    _ => cwd_known = false,
                }
            } else {
                match target {
                    Some(t) if plain(t) => {
                        if prog == "pushd" {
                            stack.push(local.cwd.clone());
                        }
                        local.cwd = PathBuf::from(resolve_path(&expand_vars(t, &local), &local));
                    }
                    Some(_) => cwd_known = false,
                    None => local.cwd = local.home.clone(),
                }
            }
            continue;
        }
        if redirect_targets(&u.text).iter().any(|t| hit(t, &local, cwd_known)) {
            return true;
        }
        if is_readonly_ledger_unit(&u) {
            continue;
        }
        if u.argv.iter().skip(1).any(|w| hit(w, &local, cwd_known)) {
            return true;
        }
    }
    false
}

fn in_ledger_dir(full: &str, cfg: &GuardConfig) -> bool {
    ledger_dirs(cfg).iter().any(|d| full == d || full.starts_with(&format!("{d}/")))
}

/// Words that must send a command past the keyword fast path: every protected path, each of its
/// ancestors below home (absolute and `~/` forms), and their base names.
fn ledger_needles(cfg: &GuardConfig) -> Vec<String> {
    let home = resolve_path(&cfg.home.to_string_lossy(), cfg);
    let home = home.trim_end_matches('/').to_string();
    let mut out = Vec::new();
    for p in ledger_protected(cfg) {
        let mut cur = std::path::PathBuf::from(p.trim_end_matches('/'));
        loop {
            let s = cur.to_string_lossy().into_owned();
            if s.is_empty() || s == "/" || s == home || home.starts_with(&format!("{s}/")) {
                break;
            }
            if let Some(rel) = s.strip_prefix(&format!("{home}/")) {
                out.push(format!("~/{rel}"));
            }
            if let Some(name) = cur.file_name() {
                out.push(name.to_string_lossy().into_owned());
            }
            out.push(s);
            if !cur.pop() {
                break;
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// The folders hook files live in.
const HOOK_DIRS: &[&str] = &[".claude", ".codex", ".cursor"];

/// How many keys named `disableAllHooks` (any case, escapes decoded) a JSON text has at any
/// depth; None when it does not parse.
fn json_disable_count(text: &str) -> Option<usize> {
    fn walk(v: &Value) -> usize {
        match v {
            Value::Object(m) => m
                .iter()
                .map(|(k, child)| usize::from(k.eq_ignore_ascii_case("disableAllHooks")) + walk(child))
                .sum(),
            Value::Array(a) => a.iter().map(walk).sum(),
            _ => 0,
        }
    }
    if text.trim().is_empty() {
        return Some(0);
    }
    serde_json::from_str::<Value>(text).ok().map(|v| walk(&v))
}

/// For text that is not strict JSON (comments, a partial file): `\uXXXX` escapes decoded,
/// backslashes and case dropped, then the name counted.
fn loose_disable_count(text: &str) -> usize {
    let mut out = String::with_capacity(text.len());
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '\\' && i + 5 < chars.len() && chars[i + 1] == 'u' {
            let hex: String = chars[i + 2..i + 6].iter().collect();
            if let Some(c) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                out.push(c);
                i += 6;
                continue;
            }
        }
        if chars[i] != '\\' {
            out.push(chars[i]);
        }
        i += 1;
    }
    out.to_ascii_lowercase().matches("disableallhooks").count()
}

fn disable_count(text: &str) -> usize {
    json_disable_count(text).unwrap_or_else(|| loose_disable_count(text))
}

/// Words that name the guard config or a hook file, for a path the guard cannot resolve.
const GUARDED_NAMES: &[&str] = &[
    ".claude", ".codex", ".cursor", ".config", "agentkit.toml", "guards.toml", "settings.json", "settings.local.json",
    "hooks.json", "config.toml",
];

/// A write target with a glob, an unresolved `$` or a backtick: could it land on a protected
/// path? When what is literal names one, or its literal start lies on the way into or inside a
/// protected folder (`~/.c*/...`, `~/.claude/sett*`), or it is relative and the shell is in one.
fn guarded_dynamic(token: &str, cfg: &GuardConfig) -> bool {
    let e = expand_vars(token, cfg);
    let Some(at) = e.find(['*', '?', '[', '$', '`']) else { return false };
    let prefix = &e[..at];
    let homes = [cfg.home.to_string_lossy().into_owned(), resolve_path(&cfg.home.to_string_lossy(), cfg)];
    // The names are looked for in what the token itself spells, not in the home it expands to.
    let own = homes
        .iter()
        .find_map(|h| prefix.strip_prefix(h.trim_end_matches('/')))
        .unwrap_or(prefix);
    // Inside a worktree under `.claude/worktrees/NAME/` only what follows the worktree counts.
    // `.` and `//` are collapsed first; a `..` anywhere could climb back out of the worktree,
    // so with one present the worktree is not trusted.
    let mut lit = own.to_string();
    loop {
        let next = lit.replace("//", "/").replace("/./", "/");
        if next == lit {
            break;
        }
        lit = next;
    }
    if let Some(i) = lit.find("/.claude/worktrees/") {
        // the whole token, glob tail included (`…/worktrees/a/*/../../settings.json`)
        if lit.split('/').any(|c| c == "..") || e.split('/').any(|c| c == "..") {
            return true;
        }
        let rest = &lit[i + "/.claude/worktrees/".len()..];
        lit = rest.find('/').map(|j| rest[j..].to_string()).unwrap_or_default();
    }
    if GUARDED_NAMES.iter().any(|n| lit.contains(n) || e[at..].contains(n)) {
        return true;
    }
    // A component of the glob tail that could still become a hook folder (`*/.cl*/x`, `*/.?odex`).
    let hook_folders = [".claude", ".codex", ".cursor", ".config"];
    let tail_partial = e[at..].split('/').skip(1).any(|c| {
        let literal = &c[..c.find(['*', '?', '[', '$', '`']).unwrap_or(c.len())];
        literal.starts_with('.') && hook_folders.iter().any(|f| f.starts_with(literal))
    });
    if tail_partial {
        return true;
    }
    // A hook folder as a literal component at any depth (`x/.claude/sett*`), or the component
    // the glob or variable sits in could still become one (`proj/.c*/…`, `proj/.*`).
    let comp_start = lit.rfind('/').map(|j| j + 1).unwrap_or(0);
    let glob_comp = &lit[comp_start..];
    let folders = [".claude", ".codex", ".cursor", ".config"];
    let whole_comp = lit[..comp_start].split('/').any(|c| folders.contains(&c));
    // the component the glob or variable sits in could still become one of them
    let partial = glob_comp.starts_with('.') && folders.iter().any(|f| f.starts_with(glob_comp));
    if whole_comp || partial {
        return true;
    }
    // Each root with the length of the part that is not yet inside it (its home, or its parent).
    let mut roots: Vec<(String, usize)> = Vec::new();
    for h in &homes {
        let h = h.trim_end_matches('/');
        for r in [".claude", ".codex", ".cursor", ".config"] {
            roots.push((format!("{h}/{r}"), h.len() + 1));
        }
    }
    for d in agentkit_config_dirs(cfg) {
        let parent = d.rfind('/').unwrap_or(0) + 1;
        roots.push((d, parent));
    }
    let abs = if prefix.is_empty() {
        resolve_path(&cfg.cwd.to_string_lossy(), cfg)
    } else if prefix.starts_with('/') {
        prefix.to_string()
    } else {
        format!("{}/{prefix}", cfg.cwd.to_string_lossy().trim_end_matches('/'))
    };
    if prefix.is_empty() {
        return roots.iter().any(|(r, _)| abs == *r || abs.starts_with(&format!("{r}/")));
    }
    roots.iter().any(|(r, outside)| abs.starts_with(r.as_str()) || (r.starts_with(abs.as_str()) && abs.len() > *outside))
}

/// For a shell write: the path is protected, or is a folder under home that holds one
/// (`mv ~/.codex /tmp/x`).
fn guarded_shell_hit(token: &str, cfg: &GuardConfig) -> bool {
    let t = expand_vars(token, cfg);
    if t.is_empty() {
        return false;
    }
    let full = resolve_path(&t, cfg);
    if guarded_kind(&full, cfg).is_some() || ledger_ancestor_hit(&full, cfg) {
        return true;
    }
    // A `.claude`, `.codex` or `.cursor` folder itself, in any directory: replacing, moving or
    // removing it changes the hooks of everything under it.
    if HOOK_DIRS.contains(&base_name(full.trim_end_matches('/'))) {
        return true;
    }
    let home = resolve_path(&cfg.home.to_string_lossy(), cfg);
    let top = full.trim_end_matches('/');
    if top.len() <= home.len() || !top.starts_with(&format!("{home}/")) {
        return false;
    }
    let inside = format!("{top}/");
    guarded_files(cfg).iter().any(|(p, _)| p.starts_with(&inside))
        || agentkit_config_dirs(cfg).iter().any(|d| d.starts_with(&inside))
        || ledger_dirs(cfg).iter().chain(ledger_files(cfg).iter()).any(|d| d.starts_with(&inside))
}

const GUARD_CONFIG_REASON: &str = "agentkit's site config and the hook files that run the guards are not written by agents: a change there can switch every guard off. Site config is the owner's to change; `agentkit sync --apply` regenerates the hook files from it. For other Claude or Codex settings, use the Edit tool on the file and leave its hooks (and disableAllHooks) as they are; that is allowed.";

fn guard_config_deny() -> Decision {
    Decision::deny("guard-config", GUARD_CONFIG_REASON)
}

/// The file as a Write, Edit or MultiEdit would leave it; None when that cannot be known (the
/// edit's text is not in the file, so the tool will fail).
fn edited_contents(value: &Value, disk: &str) -> Option<String> {
    let tool = tool_name_of(value)?;
    let input = tool_input_of(value)?;
    let apply = |text: &str, edit: &Value| -> Option<String> {
        let old = edit
            .get("old_string")
            .or_else(|| edit.get("TargetContent"))
            .or_else(|| edit.get("Target"))
            .and_then(|s| s.as_str())?;
        let new = edit
            .get("new_string")
            .or_else(|| edit.get("ReplacementContent"))
            .or_else(|| edit.get("Replacement"))
            .and_then(|s| s.as_str())
            .unwrap_or("");
        if old.is_empty() || !text.contains(old) {
            return None;
        }
        let all = edit
            .get("replace_all")
            .or_else(|| edit.get("AllowMultiple"))
            .and_then(|b| b.as_bool())
            .unwrap_or(false);
        Some(if all { text.replace(old, new) } else { text.replacen(old, new, 1) })
    };
    match tool {
        "Write" | "write_to_file" | "write_file" => input
            .get("content")
            .or_else(|| input.get("CodeContent"))
            .or_else(|| input.get("Content"))
            .and_then(|c| c.as_str())
            .map(str::to_string),
        "Edit" | "replace_file_content" => apply(disk, input),
        "edit_file" => {
            let old = input.get("Target").and_then(|s| s.as_str())?;
            let new = input.get("Replacement").and_then(|s| s.as_str()).unwrap_or("");
            if old.is_empty() || !disk.contains(old) { return None; }
            Some(disk.replace(old, new))
        }
        "MultiEdit" | "multi_replace_file_content" => {
            let mut text = disk.to_string();
            let list = input.get("edits").or_else(|| input.get("ReplacementChunks"))?.as_array()?;
            for edit in list {
                text = apply(&text, edit)?;
            }
            Some(text)
        }
        _ => None,
    }
}

/// Every key of a Codex config whose path, at any depth, names hooks or features (the trust
/// table, `[features]`, `[profiles.x.features]`), and the top-level profile selector.
fn codex_hook_keys(v: &toml::Value) -> std::collections::BTreeMap<String, String> {
    fn walk(v: &toml::Value, path: &str, hit: bool, out: &mut std::collections::BTreeMap<String, String>) {
        match v.as_table() {
            Some(t) => {
                if hit {
                    out.insert(path.to_string(), "{table}".into());
                }
                for (k, child) in t {
                    let lower = k.to_ascii_lowercase();
                    let next = if path.is_empty() { k.clone() } else { format!("{path}.{k}") };
                    walk(child, &next, hit || lower.contains("hook") || lower.contains("feature"), out);
                }
            }
            None => {
                if hit {
                    out.insert(path.to_string(), v.to_string());
                }
            }
        }
    }
    let mut out = std::collections::BTreeMap::new();
    walk(v, "", false, &mut out);
    if let Some(p) = v.get("profile") {
        out.insert("profile".into(), p.to_string());
    }
    out
}

/// A Write, Edit or MultiEdit of a protected file, or of any JSON file that it would give a
/// `disableAllHooks` it did not have.
fn guard_config_file(path: &str, value: &Value, cfg: &GuardConfig) -> Decision {
    let full = resolve_path(&expand_vars(path, cfg), cfg);
    let kind = guarded_kind(&full, cfg);
    if kind == Some(Guarded::Whole) {
        return guard_config_deny();
    }
    if kind.is_none() && !full.to_ascii_lowercase().ends_with(".json") {
        return Decision::allow();
    }
    let disk = fs::read_to_string(&full).unwrap_or_default();
    if kind.is_none() {
        // Claude reads settings from more places than the guard knows (managed settings,
        // a settings file passed by flag); a new `disableAllHooks` key is refused in any JSON.
        // Counted on parsed keys at every depth, so `"disableAllHooks"` is the same key;
        // text that is not strict JSON (a tsconfig with comments) is counted loosely instead.
        let tool = tool_name_of(value).unwrap_or("");
        let added = match edited_contents(value, &disk) {
            Some(after) => disable_count(&after) > disable_count(&disk),
            None => tool == "NotebookEdit" && loose_disable_count(&value.to_string()) > 0,
        };
        return if added { guard_config_deny() } else { Decision::allow() };
    }
    let Some(kind) = kind else { return Decision::allow() };
    let Some(after) = edited_contents(value, &disk) else {
        // NotebookEdit, or an edit that will not apply: nothing a hook needs to allow here.
        let tool = tool_name_of(value).unwrap_or("");
        return if matches!(tool, "Edit" | "MultiEdit" | "replace_file_content" | "multi_replace_file_content" | "edit_file") { Decision::allow() } else { guard_config_deny() };
    };
    let same = match kind {
        Guarded::ClaudeSettings => {
            let before: Value = serde_json::from_str(&disk).unwrap_or(Value::Null);
            match serde_json::from_str::<Value>(&after) {
                Ok(next) => {
                    let off = next.get("disableAllHooks").is_some_and(|v| !matches!(v, Value::Bool(false) | Value::Null));
                    // `disableAllHooks` as a key at any depth, any case, escapes decoded.
                    let added = json_disable_count(&after).unwrap_or(usize::MAX) > disable_count(&disk);
                    !off && !added && next.get("hooks") == before.get("hooks")
                }
                Err(_) => false,
            }
        }
        Guarded::CodexConfig => {
            let before: Option<toml::Value> = toml::from_str(&disk).ok();
            match toml::from_str::<toml::Value>(&after) {
                Ok(next) => before.as_ref().map(codex_hook_keys).unwrap_or_default() == codex_hook_keys(&next),
                Err(_) => false,
            }
        }
        Guarded::Whole => false,
    };
    if same { Decision::allow() } else { guard_config_deny() }
}

/// Shell writes to the guard config and hook files: a redirect, `tee`, `sed -i`, `cp`/`mv`/`ln`
/// onto them, moving or removing them (or a folder that holds them), `chmod`, a `git`
/// checkout over them, an inline script that writes them. Reading them is fine.
fn guard_config(cmd: &str, cfg: &GuardConfig) -> Decision {
    let bare = without_quoting(cmd);
    let dirs = agentkit_config_dirs(cfg);
    let mut needles: Vec<String> = [
        ".config", "agentkit.toml", "guards.toml", "settings.json", "settings.local.json", "hooks.json", "config.toml",
        ".claude", ".codex", ".cursor", "accept-guards", "guards-accepted",
        "decisions.jsonl", "ledger.key", "state/agentkit", ".local",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect();
    needles.extend(cfg.config_dir.iter().filter_map(|d| d.file_name().map(|n| n.to_string_lossy().into_owned())));
    needles.extend(ledger_needles(cfg));
    let here = resolve_path(&cfg.cwd.to_string_lossy(), cfg);
    let home = resolve_path(&cfg.home.to_string_lossy(), cfg);
    let roots: Vec<String> = [".claude", ".codex", ".cursor", ".config"]
        .iter()
        .map(|r| format!("{home}/{r}"))
        .chain(dirs.iter().cloned())
        .chain(ledger_dirs(cfg))
        .collect();
    let in_root = |d: &str| roots.iter().any(|r| d == r || d.starts_with(&format!("{r}/")));
    // A glob or a variable can name these files without any of the words (`~/.c*/a*/a*.toml`).
    let dynamic = cmd.contains(['*', '?', '[', '$', '`']);
    // A copy can carry a hook folder in its source without naming one (`rsync -a /tmp/x/ ~/`).
    let copies = compiled(r"\b(?:cp|rsync|ditto|install|rclone|mv|ln|gcp)\b").is_match(cmd);
    // Before any fast path: a write aimed at the ledger, its key, its folder, or the ledger
    // under another name (a hard link made earlier, found by inode).
    if ledger_write_hit(cmd, cfg) {
        return guard_config_deny();
    }
    if !dynamic && !copies && !needles.iter().any(|n| cmd.contains(n.as_str()) || bare.contains(n.as_str())) && !in_root(&here) {
        return Decision::allow();
    }
    let all = units_deep(cmd);
    let vars = literal_assignments(cmd);
    let subst = |t: &str| vars.iter().fold(t.to_string(), |acc, (n, v)| acc.replace(&format!("${{{n}}}"), v).replace(&format!("${n}"), v));
    let mut local = cfg.clone();
    let mut stack: Vec<PathBuf> = Vec::new();
    let named_in_text = |text: &str, c: &GuardConfig| file_words(text, &vars).iter().any(|w| guarded_shell_hit(w, c));
    let accepts = bare.contains("accept-guards");
    if any_unparsed(&all) && (named_in_text(cmd, &local) || in_root(&here) || accepts) {
        return guard_config_deny();
    }
    for u in &all {
        let prog = u.prog();
        let operands = u.operands();
        // `agentkit doctor --accept-guards` is the owner's acceptance of a guard switched off;
        // an agent running it would launder its own config change. A search for the words, a
        // commit message or mail that names it is text.
        let computed_doctor_flag = prog == "agentkit"
            && u.argv.iter().any(|a| a == "doctor")
            && (u.argv.iter().any(|a| subst(a).contains(['$', '`'])) || matches!(u.sep.as_str(), "$(" | "`"));
        if computed_doctor_flag
            || (accepts
                && !matches!(prog, "grep" | "egrep" | "rg" | "git" | "agent-mail")
                && u.argv.iter().any(|a| subst(a).contains("accept-guards")))
        {
            return Decision::deny(
                "guard-config",
                "`agentkit doctor --accept-guards` is the owner's step: it accepts guards switched off in site config. Do not run it; tell the owner what doctor reports and that the acceptance is his to give.",
            );
        }
        if matches!(prog, "cd" | "pushd" | "popd") && u.remote.is_none() {
            if prog == "popd" {
                if let Some(back) = stack.pop() {
                    local.cwd = back;
                }
            } else if let Some(target) = operands.first() {
                if prog == "pushd" {
                    stack.push(local.cwd.clone());
                }
                local.cwd = PathBuf::from(resolve_path(&expand_vars(&subst(target), &local), &local));
            }
            continue;
        }
        // A literal assignment on the line is substituted; a target the guard still cannot
        // resolve (a glob, `$`, a backtick) counts when it could land on a protected path.
        let hit = |t: &str| {
            let t = subst(t);
            guarded_shell_hit(&t, &local) || guarded_dynamic(&t, &local)
        };
        // Output and extract-directory options aimed at a protected path: `curl -o`,
        // `unzip -d`, `tar -C ... -x`, `sort -o`. Programs that only read are left out.
        let out_flag = (!READERS.contains(&prog) || prog == "sort")
            && !matches!(prog, "git" | "ssh" | "grep" | "rg" | "find" | "du" | "jq" | "agentkit")
            && u.argv.iter().enumerate().skip(1).any(|(i, a)| {
                let value = if matches!(a.as_str(), "-o" | "-O" | "-d" | "-C" | "--output" | "--output-document" | "--output-dir" | "--directory") {
                    u.argv.get(i + 1).map(String::as_str)
                } else if let Some((name, v)) = a.split_once('=') {
                    matches!(name, "--output" | "--output-document" | "--output-dir" | "--directory").then_some(v)
                } else if a.starts_with("-o") && !a.starts_with("--") && a.len() > 2 {
                    Some(&a[2..])
                } else {
                    None
                };
                value.is_some_and(|v| hit(v))
            });
        let any = operands.iter().any(|t| hit(t));
        // Copy-type writers. The destination is `-t DIR` / `--target-directory[=]DIR` when
        // given; otherwise any operand after the first may be it (flags and their values move
        // the last word). What each source would leave behind is judged too: a `.claude`
        // folder (or one inside the source) landing where one already is, or on a protected
        // path, is a write to hooks (`cp -r /tmp/x/.claude proj/`, `rsync -a /tmp/x/ ~/`).
        let onto = {
            let mut target: Option<String> = None;
            let mut i = 1;
            while i < u.argv.len() {
                let a = u.argv[i].as_str();
                if a == "-t" || a == "--target-directory" {
                    target = u.argv.get(i + 1).cloned();
                    i += 1;
                } else if let Some(v) = a.strip_prefix("--target-directory=") {
                    target = Some(v.to_string());
                } else if let Some(v) = a.strip_prefix("-t").filter(|v| !v.is_empty() && !a.starts_with("--")) {
                    target = Some(v.to_string());
                }
                i += 1;
            }
            let (sources, dests): (Vec<String>, Vec<String>) = match &target {
                Some(t) => (operands.iter().filter(|o| **o != t.as_str()).map(|s| s.to_string()).collect(), vec![t.clone()]),
                None if operands.len() >= 2 => (
                    operands[..operands.len() - 1].iter().map(|s| s.to_string()).collect(),
                    operands[1..].iter().map(|s| s.to_string()).collect(),
                ),
                None => (Vec::new(), operands.iter().map(|s| s.to_string()).collect()),
            };
            dests.iter().any(|d| hit(d))
                || dests.iter().any(|dest| {
                    let d = resolve_path(&expand_vars(&subst(dest), &local), &local);
                    let d = d.trim_end_matches('/');
                    sources.iter().filter(|s| *s != dest).any(|src| {
                        let raw = subst(src);
                        let contents = raw.ends_with('/') || raw.ends_with("/.");
                        let s = resolve_path(&expand_vars(&raw, &local), &local);
                        let s = s.trim_end_matches('/');
                        let base = base_name(s);
                        // Where the source, and any hook folder inside it, would land.
                        let mut landings: Vec<String> = Vec::new();
                        if !contents {
                            landings.push(format!("{d}/{base}"));
                        }
                        for h in HOOK_DIRS.iter().chain([".config"].iter()) {
                            if Path::new(&format!("{s}/{h}")).exists() {
                                landings.push(format!("{d}/{h}"));
                                if !contents {
                                    landings.push(format!("{d}/{base}/{h}"));
                                }
                            }
                        }
                        let home = resolve_path(&local.home.to_string_lossy(), &local);
                        let configs = agentkit_config_dirs(&local);
                        landings.iter().any(|to| {
                            let parent = to.rfind('/').map(|j| &to[..j]).unwrap_or("");
                            // a protected file, a hook folder merged into one that is there (or
                            // made at home), or over agentkit's config folder or its parent
                            guarded_kind(to, &local).is_some()
                                || (HOOK_DIRS.contains(&base_name(to)) && (Path::new(to).exists() || parent == home))
                                || configs.iter().any(|c| c == to || c.starts_with(&format!("{to}/")))
                        })
                    })
                })
        };
        let git_rewrites = prog == "git"
            && u.argv.iter().any(|a| {
                matches!(a.as_str(), "checkout" | "restore" | "reset" | "stash" | "apply" | "am" | "pull" | "merge" | "rebase" | "clean" | "mv" | "rm" | "switch" | "cherry-pick" | "revert")
            })
            && (any
                || u.argv.windows(2).any(|w| w[0] == "-C" && dirs.iter().any(|d| resolve_path(&expand_vars(&subst(&w[1]), &local), &local) == *d))
                || (u.remote.is_none() && dirs.iter().any(|d| local.cwd.to_string_lossy() == *d || resolve_path(&local.cwd.to_string_lossy(), &local).starts_with(&format!("{d}/")))));
        let wrote = redirect_targets(&u.text).iter().any(|t| hit(t))
            || out_flag
            || git_rewrites
            || match prog {
                "tee" | "truncate" | "mv" | "trash" | "rm" | "rmdir" | "unlink" | "shred" | "srm" | "sponge" | "chmod"
                | "chflags" | "chown" | "xattr" | "mkfifo" | "mknod" => any,
                // the files, not the script: `sed -i '' '1978s/.*/x/' f` names one file
                "sed" | "gsed" | "perl" | "ruby" => edits_in_place(&u.argv) && script_tool_files(&u.argv).iter().any(|t| hit(t)),
                // A link or copy whose SOURCE is the ledger, its key or its state folder makes a
                // second name for it (`ln decisions.jsonl notes.jsonl && : > notes.jsonl`).
                "ln" | "link" | "cp" | "gcp" | "install" | "rsync" | "ditto" | "rclone"
                    if operands.iter().any(|t| {
                        let full = resolve_path(&expand_vars(&subst(t), &local), &local);
                        ledger_ancestor_hit(&full, &local) || guarded_kind(&full, &local).is_some_and(|_| in_ledger_dir(&full, &local))
                    }) => true,
                "cp" | "install" | "ln" | "rsync" | "ditto" | "rclone" => onto,
                "link" => any,
                "dd" => u.argv.iter().any(|a| a.strip_prefix("of=").is_some_and(|t| hit(t))),
                _ => false,
            }
            || (runs_inline_script(prog) && script_writes(&u.text) && named_in_text(&u.text, &local));
        if wrote {
            return guard_config_deny();
        }
    }
    Decision::allow()
}

/// The backup client's cleanup that deletes from the backup whatever is no longer on the Mac,
/// which is exactly what an offload leaves behind. Denied when a command would start it: an
/// AppleScript that names it, the client's own program or an `open` with it as an argument, or
/// a script of that name. The patterns are site config (`guards.idrive_cleanup_patterns`).
/// Naming it in a search, a note or a message is text.
fn idrive_cleanup(cmd: &str, cfg: &GuardConfig) -> Decision {
    let patterns: Vec<Rex> = cfg.idrive_cleanup_patterns.iter().filter_map(|p| try_compiled(p)).collect();
    let named = |text: &str| patterns.iter().any(|p| p.is_match(text));
    if !named(cmd) && !named(&without_quoting(cmd)) {
        return Decision::allow();
    }
    let deny = || {
        Decision::deny(
            "idrive-cleanup",
            "IDrive's Archive Cleanup deletes from the backup whatever is no longer on this Mac, which is what an offload leaves behind. Never run it. To free IDrive space, write up what should go and ask the owner.",
        )
    };
    let all = units_deep(cmd);
    if any_unparsed(&all) {
        return deny();
    }
    for u in all {
        let Some(program) = u.argv.first() else { continue };
        let prog = u.prog();
        // Mail that names it is text; only that command is skipped, not the rest of the line.
        if prog == "agent-mail" {
            continue;
        }
        let script = u.operands().first().copied().unwrap_or("");
        let starts = (prog == "osascript" && (named(&u.text) || u.text.contains("<<")))
            || named(program)
            || ((SHELLS.contains(&prog) || runs_inline_script(prog)) && named(script))
            || ((prog == "open" || program.to_ascii_lowercase().contains("idrive")) && named(&u.text));
        if starts {
            return deny();
        }
    }
    Decision::allow()
}

/// The path a file-reading tool call names: Claude `Read` (`file_path`) / `Grep` (`path`), or Antigravity `view_file`/`read_file`/`grep_search`.
fn read_target(value: &Value) -> Option<String> {
    let tool = tool_name_of(value)?;
    let input = tool_input_of(value)?;
    let get = |f: &str| input.get(f).and_then(|p| p.as_str()).filter(|p| !p.is_empty()).map(str::to_string);
    match tool {
        "Read" => get("file_path"),
        "view_file" | "read_file" => get("AbsolutePath").or_else(|| get("Path")),
        // A Grep whose glob aims at ssh's folder or a key name reads keys wherever it starts.
        // Grep (ripgrep) skips dot folders unless its glob reaches into one. A glob with a
        // segment that can match a dot name, or a key name, is judged as a search of ~/.ssh.
        "Grep" => match get("glob") {
            Some(g) if g.split(['/', ',', '{', '}']).any(|seg| {
                seg.starts_with(['.', '[', '?']) || seg.contains("id_") || seg.contains("ssh")
            }) => Some("~/.ssh".into()),
            _ => Some(get("path").unwrap_or_else(|| ".".into())),
        },
        "grep_search" => Some(get("SearchPath").unwrap_or_else(|| ".".into())),
        _ => None,
    }
}

const SECRETS_REASON: &str = "Private keys are used in place, never read: ssh, git, scp and ssh-add use them as they are. For what a key is, run `ssh-keygen -lf KEY` (fingerprint) or `ssh-keygen -y -f KEY` (its public half). To put a key on another fleet Mac, scp or rsync it there with no -o/-F/-J/-S/-e options; that is allowed.";

fn ssh_dir(cfg: &GuardConfig) -> String {
    resolve_path(&cfg.home.join(".ssh").to_string_lossy(), cfg)
}

/// A word as a path: quotes gone, `$HOME` / `~` expanded, made absolute against `cwd`.
fn key_path(word: &str, cfg: &GuardConfig) -> String {
    let raw = word.trim().trim_matches(|c| c == '\'' || c == '"');
    let raw = raw.strip_prefix("$HOME/").or_else(|| raw.strip_prefix("${HOME}/"))
        .map(|rest| cfg.home.join(rest).to_string_lossy().into_owned())
        .unwrap_or_else(|| if raw == "$HOME" { cfg.home.to_string_lossy().into_owned() } else { raw.to_string() });
    resolve_path(&expand_path(&raw, cfg), cfg)
}

/// A private key file: anything in ~/.ssh that is not public material or ssh's own lists, an
/// `id_<type>` key anywhere, or a site path in `guards.secret_paths`.
fn is_private_key(path: &str, cfg: &GuardConfig) -> bool {
    if path.trim().is_empty() {
        return false;
    }
    let full = key_path(path, cfg);
    let base = base_name(&full);
    let public = base.ends_with(".pub") || base.starts_with("known_hosts") || base.starts_with("authorized_keys")
        || base == "config" || base.starts_with("config.") || base == "agent" || base == "environment";
    let ssh = ssh_dir(cfg);
    if let Some(rest) = full.strip_prefix(&format!("{ssh}/")) {
        return !public && !rest.starts_with("agent/");
    }
    // ssh-keygen's own names (`id_ed25519`, `id_rsa_work`, ...), not any word that starts with id_.
    let key_type = ["id_rsa", "id_dsa", "id_ecdsa", "id_ed25519", "id_xmss"].iter().any(|t| {
        base.strip_prefix(t).is_some_and(|rest| rest.is_empty() || rest.starts_with(['_', '-', '.']))
    });
    if key_type && !public {
        return true;
    }
    cfg.secret_paths.iter().any(|p| {
        let want = key_path(p, cfg);
        full == want || full.starts_with(&format!("{}/", want.trim_end_matches('/')))
    })
}

/// A word names a key, or the folder that holds them, or a glob that could reach one.
fn names_secret(word: &str, cfg: &GuardConfig) -> bool {
    if is_private_key(word, cfg) {
        return true;
    }
    let ssh = ssh_dir(cfg);
    let full = key_path(word, cfg);
    if full == ssh || cfg.secret_paths.iter().any(|p| key_path(p, cfg) == full) {
        return true; // the whole folder: a recursive reader or an archiver takes every key in it
    }
    if !word.contains(['*', '?', '[']) {
        return false;
    }
    // The shell expands a glob against the disk, so the disk decides: dot files included,
    // which is stricter than the shell.
    // A glob too long to judge is refused, not waved through: the hook must answer in time.
    if word.chars().count() > MAX_GLOB_LEN {
        return true;
    }
    // A glob that matches the folder itself (`.ss?`) takes every key in it too.
    glob_matches(&full, 200).iter().any(|m| is_private_key(m, cfg) || resolve_path(m, cfg) == ssh)
}

/// Longest glob word the secrets guard matches against the disk.
const MAX_GLOB_LEN: usize = 512;

enum GlobTok {
    Star,
    Any,
    Class(bool, Vec<char>),
    Lit(char),
    /// A `[` with no usable `]`: matches nothing.
    Never,
}

fn glob_tokens(pat: &[char]) -> Vec<GlobTok> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < pat.len() {
        match pat[i] {
            '*' => {
                // A run of stars is one star.
                if !matches!(out.last(), Some(GlobTok::Star)) {
                    out.push(GlobTok::Star);
                }
                i += 1;
            }
            '?' => {
                out.push(GlobTok::Any);
                i += 1;
            }
            '[' => match pat[i..].iter().position(|&x| x == ']') {
                Some(end) if end > 1 => {
                    let set = &pat[i + 1..i + end];
                    let (neg, set) = if set[0] == '!' || set[0] == '^' { (true, &set[1..]) } else { (false, set) };
                    out.push(GlobTok::Class(neg, set.to_vec()));
                    i += end + 1;
                }
                _ => {
                    out.push(GlobTok::Never);
                    i += 1;
                }
            },
            c => {
                out.push(GlobTok::Lit(c));
                i += 1;
            }
        }
    }
    out
}

fn glob_tok_matches(tok: &GlobTok, c: char) -> bool {
    match tok {
        GlobTok::Any => true,
        GlobTok::Lit(p) => *p == c,
        GlobTok::Class(neg, set) => {
            let mut hit = false;
            let mut i = 0;
            while i < set.len() {
                if i + 2 < set.len() && set[i + 1] == '-' {
                    hit |= set[i] <= c && c <= set[i + 2];
                    i += 3;
                } else {
                    hit |= set[i] == c;
                    i += 1;
                }
            }
            hit != *neg
        }
        GlobTok::Star | GlobTok::Never => false,
    }
}

/// Shell-style glob match of one path segment (`*`, `?`, `[...]`). Iterative, with one
/// backtrack point, so time is at most pattern × name: until 21 Nov 2026 it recursed on every
/// `*` and `/etc/*****…z` held the hook past its timeout, which lets the command run.
fn seg_match(pat: &[char], s: &[char]) -> bool {
    let toks = glob_tokens(pat);
    let (mut p, mut i) = (0usize, 0usize);
    let mut star: Option<(usize, usize)> = None;
    while i < s.len() {
        if p < toks.len() && matches!(toks[p], GlobTok::Star) {
            star = Some((p, i));
            p += 1;
        } else if p < toks.len() && glob_tok_matches(&toks[p], s[i]) {
            p += 1;
            i += 1;
        } else if let Some((sp, si)) = star {
            p = sp + 1;
            i = si + 1;
            star = Some((sp, si + 1));
        } else {
            return false;
        }
    }
    while p < toks.len() && matches!(toks[p], GlobTok::Star) {
        p += 1;
    }
    p == toks.len()
}

/// Paths an absolute glob matches on disk, dot files included, at most `cap` per level.
fn glob_matches(pattern: &str, cap: usize) -> Vec<String> {
    let mut found = vec![PathBuf::from("/")];
    for part in pattern.split('/').filter(|p| !p.is_empty()) {
        let mut next = Vec::new();
        for base in &found {
            if !part.contains(['*', '?', '[']) {
                let joined = base.join(part);
                if joined.exists() {
                    next.push(joined);
                }
                continue;
            }
            let pat: Vec<char> = part.chars().collect();
            for e in fs::read_dir(base).into_iter().flatten().flatten() {
                let name: Vec<char> = e.file_name().to_string_lossy().chars().collect();
                if seg_match(&pat, &name) {
                    next.push(base.join(e.file_name()));
                }
            }
        }
        next.truncate(cap);
        found = next;
    }
    found.into_iter().map(|p| p.to_string_lossy().into_owned()).collect()
}

fn secrets_read(path: &str, cfg: &GuardConfig) -> Decision {
    if names_secret(path, cfg) {
        Decision::deny("secrets", SECRETS_REASON)
    } else {
        Decision::allow()
    }
}

/// What a command reads on stdin: the word after each `<` (not `<<` or `<(`).
fn redirect_sources(text: &str) -> Vec<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r#"(?<![<0-9])[0-9]?<(?![<(&])\s*([^\s;&|)]+)"#).unwrap());
    re.captures_iter(&without_quoting(text)).flatten().filter_map(|c| Some(c.get(1)?.as_str().to_string())).collect()
}

/// `NAME=value` assignments on the line, so a later `$NAME` / `${NAME}` reads as its value.
fn assignments(text: &str) -> Vec<(String, String)> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"(?:^|[\s;&|(])(?:export\s+)?([A-Za-z_][A-Za-z0-9_]*)=([^\s;&|)]+)").unwrap());
    re.captures_iter(text).flatten().filter_map(|c| Some((c.get(1)?.as_str().to_string(), c.get(2)?.as_str().to_string()))).collect()
}

/// The words of a command that may name a file: quotes and backslashes removed, assigned
/// variables substituted, then split on everything a path cannot hold unquoted, so
/// `open("~/.ssh/k")`, `f=@k`, `<k` and `~/.s''sh/k` all leave the bare path.
fn file_words(text: &str, vars: &[(String, String)]) -> Vec<String> {
    let mut t = without_quoting(text).replace("${HOME}", "$HOME");
    for (name, value) in vars {
        t = t.replace(&format!("${{{name}}}"), value).replace(&format!("${name}"), value);
    }
    t.split(|c: char| c.is_whitespace() || "(){},;|&<>=@`".contains(c))
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>()
}

/// `cred` subcommands an agent may type: lookups by name and store upkeep. `_values` prints
/// every stored password as JSON, `fill` puts one in another program's argv, and any other
/// `_`-prefixed or unknown subcommand is internal; those are refused. (chrome-bg reads cred's
/// logins in-process and does not shell out.)
const CRED_ALLOWED: &[&str] = &["list", "add", "import", "rm", "-h", "--help"];

fn cred_dump(cmd: &str) -> Option<Decision> {
    if !names_tool(cmd, "cred") {
        return None;
    }
    let deny = || {
        Some(Decision::deny(
            "secrets",
            "BLOCKED: that `cred` subcommand prints or hands out passwords. Agents use a login by NAME (`cred list` shows them); chrome-bg and the browser tools put the value in without the model seeing it.",
        ))
    };
    let all = units_deep(cmd);
    let vars = literal_assignments(cmd);
    let mentions = all.iter().any(|u| tool_position(u, "cred").is_some());
    if mentions && any_unparsed(&all) {
        return deny();
    }
    for u in &all {
        if u.later && !EXECUTORS.contains(&u.parent.as_str()) {
            continue;
        }
        let Some(at) = tool_position(u, "cred") else { continue };
        let sub = u.argv.get(at + 1).map(|a| {
            vars.iter().fold(a.clone(), |t, (n, v)| t.replace(&format!("${{{n}}}"), v).replace(&format!("${n}"), v))
        });
        let fed = u.wrappers.iter().any(|w| w == "xargs");
        match sub {
            // bare `cred` prints its usage, unless xargs supplies the subcommand
            None => {
                if fed {
                    return deny();
                }
            }
            Some(s) if CRED_ALLOWED.contains(&s.as_str()) && !fed => {}
            Some(_) => return deny(),
        }
    }
    None
}

/// Is `full` the ssh folder's parent or above (the home, `/Users`, `/`)?
fn holds_ssh_dir(full: &str, cfg: &GuardConfig) -> bool {
    let ssh = ssh_dir(cfg);
    let top = full.trim_end_matches('/');
    top.is_empty() || ssh.starts_with(&format!("{top}/"))
}

/// For an archiver or a recursive reader: the config to read its members with (tar's `-C`
/// directory as cwd), and whether a member it reads holds ~/.ssh (`~`, `.` under `-C ~`, `/`).
fn tree_reader(u: &Unit, cfg: &GuardConfig, vars: &[(String, String)]) -> (Option<GuardConfig>, bool) {
    let prog = u.prog();
    let argv = &u.argv;
    let subst = |a: &str| vars.iter().fold(a.replace("${HOME}", "$HOME"), |t, (n, v)| t.replace(&format!("${{{n}}}"), v).replace(&format!("${n}"), v));
    let holds = |w: &str, c: &GuardConfig| holds_ssh_dir(&key_path(&subst(w), c), c);
    let short_flags = |a: &str| a.starts_with('-') && !a.starts_with("--") && a.len() > 1;
    if matches!(prog, "tar" | "bsdtar" | "gtar") {
        let mut dir: Option<String> = None;
        let mut members: Vec<&str> = Vec::new();
        let mut i = 1;
        while i < argv.len() {
            let a = argv[i].as_str();
            i += 1;
            let cluster = if short_flags(a) {
                Some(&a[1..])
            } else if i == 2 && !a.contains('/') && a.chars().all(|c| c.is_ascii_alphabetic()) {
                Some(a) // old style: `tar cf x dir`
            } else {
                None
            };
            if let Some(body) = cluster {
                // Each value flag in the cluster takes the next word, or the rest of the cluster.
                for (at, c) in body.char_indices() {
                    if matches!(c, 'f' | 'C' | 'T' | 'X' | 'b' | 's' | 'I') {
                        let attached = &body[at + c.len_utf8()..];
                        let value = if attached.is_empty() {
                            let v = argv.get(i).cloned();
                            i += 1;
                            v
                        } else {
                            Some(attached.to_string())
                        };
                        if c == 'C' {
                            dir = value;
                        }
                        if !attached.is_empty() {
                            break;
                        }
                    }
                }
                continue;
            }
            if let Some(v) = a.strip_prefix("--directory=") {
                dir = Some(v.to_string());
                continue;
            }
            if a == "--directory" || a == "--file" || a == "--exclude" || a == "--files-from" {
                if a == "--directory" {
                    dir = argv.get(i).cloned();
                }
                i += 1;
                continue;
            }
            if a.starts_with('-') {
                continue;
            }
            members.push(a);
        }
        let member_cfg = dir.map(|d| {
            let mut c = cfg.clone();
            c.cwd = PathBuf::from(key_path(&subst(&d), cfg));
            c
        });
        let base = member_cfg.as_ref().unwrap_or(cfg);
        let tree = members.iter().any(|m| holds(m, base) || names_secret(&subst(m), base));
        return (member_cfg, tree);
    }
    let has_short = |letters: &[char]| argv.iter().skip(1).any(|a| short_flags(a) && a[1..].contains(letters));
    let has_long = |names: &[&str]| argv.iter().skip(1).any(|a| names.contains(&long_name(a)));
    let ops = u.operands();
    let (recursive, read): (bool, Vec<&str>) = match prog {
        "zip" => (has_short(&['r', 'R']) || has_long(&["--recurse-paths"]), ops.iter().skip(1).copied().collect()),
        "7z" | "7za" | "7zz" => (true, ops.iter().skip(2).copied().collect()),
        "ditto" | "rsync" | "rclone" | "restic" | "borg" | "kopia" | "pax" => {
            (true, ops[..ops.len().saturating_sub(1)].to_vec())
        }
        "cp" | "scp" | "gcp" => (
            has_short(&['r', 'R', 'a']) || has_long(&["--recursive", "--archive"]),
            ops[..ops.len().saturating_sub(1)].to_vec(),
        ),
        "grep" | "egrep" | "fgrep" | "ggrep" => (has_short(&['r', 'R']) || has_long(&["--recursive", "--dereference-recursive"]), ops.clone()),
        "rg" | "ag" => (has_short(&['u', '.']) || has_long(&["--hidden", "--unrestricted"]), ops.clone()),
        "find" => (
            argv.iter().any(|a| matches!(a.as_str(), "-exec" | "-execdir" | "-ok" | "-okdir" | "-fprint" | "-fls")),
            find_roots(argv),
        ),
        _ => (false, Vec::new()),
    };
    (None, recursive && read.iter().any(|w| holds(w, cfg)))
}

/// Exfiltration of keys, not their use. Any command that names a private key (or ~/.ssh, or a
/// glob that reaches one) is denied unless its program uses keys in place: ssh, git, ssh-add,
/// ssh-keygen, chmod, ls, stat, trash; cp/mv that stay inside ~/.ssh; scp/rsync/sftp to a fleet
/// Mac with no option that can redirect the connection. Also a decrypted keychain dump. Deny
/// only, never ask. A tripwire for an agent talked into it, not a sandbox: a script file the
/// agent writes and then runs is not read.
fn secrets(cmd: &str, cfg: &GuardConfig) -> Decision {
    const USERS: &[&str] = &[
        "ssh", "ssh-add", "ssh-keygen", "ssh-copy-id", "git", "chmod", "chown", "chflags", "ls", "stat", "test", "[",
        "[[", "file", "trash", "echo", "printf", "agent-mail", "cd", "pushd", "wc",
    ];
    const COPIERS: &[&str] = &["scp", "rsync", "sftp"];
    let deny = || Decision::deny("secrets", SECRETS_REASON);
    if let Some(d) = cred_dump(cmd) {
        return d;
    }
    let all = units(cmd);
    let vars = assignments(&without_quoting(cmd));
    let mut local = cfg.clone();
    if any_unparsed(&all) && file_words(cmd, &vars).iter().any(|w| names_secret(w, cfg)) {
        return deny();
    }
    for u in &all {
        if u.carrier {
            continue;
        }
        let prog = u.prog();
        if prog == "security" && u.argv.iter().any(|a| a == "dump-keychain") && u.argv.iter().any(|a| a.starts_with('-') && !a.starts_with("--") && a.contains('d')) {
            return Decision::deny("secrets", "A decrypted keychain dump prints every password on this Mac. Read the one item a job needs with `security find-generic-password -s SERVICE -w`.");
        }
        if matches!(prog, "cd" | "pushd") && u.remote.is_none() {
            if let Some(dir) = u.operands().first() {
                local.cwd = PathBuf::from(key_path(dir, &local));
            }
        }
        let words = file_words(&u.text, &vars);
        // `tar -C ~ -cf x .ssh` names `.ssh` relative to its -C directory; `tar -cf x ~`,
        // `grep -r k ~`, `zip -r x ~` take the whole home, ~/.ssh included.
        let (member_cfg, tree) = tree_reader(u, &local, &vars);
        let named = words.iter().any(|w| names_secret(w, &local))
            || member_cfg.as_ref().is_some_and(|m| words.iter().any(|w| names_secret(w, m)));
        if !named && !tree {
            continue;
        }
        // A key read on stdin (`prog < key`, `$(<key)`) is data whatever the program is.
        let fed = redirect_sources(&u.text).iter().any(|w| {
            file_words(w, &vars).iter().any(|x| names_secret(x, &local))
        });
        if fed {
            return deny();
        }
        if prog.is_empty() {
            continue;
        }
        // git uses a key through ssh (`-c core.sshCommand=...`, GIT_SSH_COMMAND, a message
        // that names one); a key as a path argument (`git add`, `hash-object`) is read as data.
        if prog == "git" {
            let subst = |a: &str| vars.iter().fold(a.replace("${HOME}", "$HOME"), |t, (n, v)| t.replace(&format!("${{{n}}}"), v).replace(&format!("${n}"), v));
            let as_path = |a: &String| !a.contains(char::is_whitespace)
                && file_words(&subst(a), &[]).iter().any(|p| names_secret(p, &local));
            let from_stdin = u.argv.iter().any(|a| a == "--stdin-paths" || a == "--stdin" || a.starts_with("--pathspec-from-file"));
            if u.argv.iter().skip(1).any(as_path) || from_stdin {
                return deny();
            }
            continue;
        }
        if USERS.contains(&prog) {
            continue;
        }
        let ssh = format!("{}/", ssh_dir(&local));
        // Within ~/.ssh, and the copy is still a key by name: `cp k k.pub; cat k.pub` is not.
        if matches!(prog, "cp" | "mv") {
            let ops = u.operands();
            let inside = ops.iter().all(|w| key_path(w, &local).starts_with(&ssh));
            let dest = ops.last().map(|w| key_path(w, &local)).unwrap_or_default();
            let dest_is_dir = dest.ends_with('/') || Path::new(&dest).is_dir();
            // Each file the copy leaves behind must still be a key by name.
            let results: Vec<String> = if dest_is_dir {
                ops[..ops.len().saturating_sub(1)].iter()
                    .map(|w| format!("{}/{}", dest.trim_end_matches('/'), base_name(&key_path(w, &local))))
                    .collect()
            } else {
                vec![dest.clone()]
            };
            if inside && ops.len() >= 2 && results.iter().all(|r| is_private_key(r, &local)) {
                continue;
            }
            return deny();
        }
        if COPIERS.contains(&prog) {
            let redirect = u.argv.iter().skip(1).any(|a| {
                a.starts_with("--rsh") || a.starts_with("--rsync-path") || a.starts_with("--proxy")
                    || (a.starts_with('-') && !a.starts_with("--") && a.contains(['o', 'F', 'J', 'S', 'e']))
            });
            let fleet = |w: &str| {
                let host = w.split_once(':').map(|(h, _)| h.rsplit('@').next().unwrap_or(h));
                host.is_some_and(|h| cfg.air_aliases.iter().chain(cfg.air_hosts.iter()).any(|a| a == h))
            };
            // rsync reads its connection program from the environment too.
            let env_redirect = ["RSYNC_RSH", "RSYNC_CONNECT_PROG", "RSYNC_PROXY"].iter().any(|n| {
                cmd.contains(n) || u.wrappers.iter().any(|w| w.contains(n)) || vars.iter().any(|(k, _)| k == n)
            });
            let ops = u.operands();
            let dest = ops.last().copied().unwrap_or("");
            if !redirect && !env_redirect && u.remote.is_none() && ops.len() >= 2 && dest.contains(':') && fleet(dest) {
                continue;
            }
        }
        return deny();
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
    // From a neutral directory: the doctor may be started inside a push-gated project, where a
    // plain push correctly asks. This case checks the ungated rule, so it must not inherit cwd.
    let mut ungated = cfg.clone();
    ungated.cwd = PathBuf::from("/var/agentkit-self-test/ungated");
    let allow_push = push("git push origin main", &ungated);
    if !allow_push.is_allow() {
        return Err(format!("push guard blocked plain git push outside gated projects ({})", allow_push.decision));
    }
    let deny_force = push("git push --force origin main", cfg);
    if !deny_force.is_deny() {
        return Err("push guard allowed git push --force".into());
    }
    let expect = |guard: &str, decision: Decision, deny: bool, what: &str| -> Result<(), String> {
        if decision.is_deny() == deny {
            Ok(())
        } else {
            Err(format!("{guard} {} {what}", if deny { "allowed" } else { "blocked" }))
        }
    };
    expect("icloud", icloud("ssh worker.example 'rm -rf ~/.Trash/*'"), true, "emptying the Trash over ssh")?;
    expect("disk-slot", disk_slot("echo disk-slot; cp /Volumes/A/x /Volumes/B/x", cfg), true, "a copy between volumes behind the word disk-slot")?;
    expect("disk-slot", disk_slot("disk-slot /Volumes/A -- cp /Volumes/A/x /Volumes/A/y", cfg), false, "a wrapped copy")?;
    expect("slot-holder", slot_holder("pkill -9 heavy-slot", cfg), true, "pkill -9 heavy-slot")?;
    expect("slot-holder", slot_holder("heavy-slot release 123", cfg), false, "heavy-slot release")?;
    let limit = cfg.slot_dir().join("limit");
    expect("slot-files", slot_files(&format!("echo 9 > '{}'", limit.display()), cfg), true, "a redirect onto the limit file")?;
    expect("slot-files", slot_files(&format!("cat '{}'", limit.display()), cfg), false, "reading the limit file")?;
    expect("approval-files", approval_files("touch /var/agentkit-self-test/APPROVE_EXAMPLE", cfg), true, "touch of an approval marker")?;
    expect("approval-files", approval_files("ls /var/agentkit-self-test/APPROVE_EXAMPLE", cfg), false, "listing an approval marker")?;
    // With the built-in pattern: a site may have replaced it with its own client's wording.
    let mut stock = cfg.clone();
    stock.idrive_cleanup_patterns = crate::site::DEFAULT_IDRIVE_CLEANUP_PATTERNS.iter().map(|s| (*s).to_string()).collect();
    expect("idrive-cleanup", idrive_cleanup("perl archive_cleanup.pl", &stock), true, "the archive cleanup script")?;
    expect("idrive-cleanup", idrive_cleanup("grep -i 'archive cleanup' notes.md", &stock), false, "a search for the words")?;
    expect("secrets", secrets("cat ~/.ssh/id_ed25519", cfg), true, "printing a private key")?;
    expect("secrets", secrets("ssh-keygen -y -f ~/.ssh/id_ed25519", cfg), false, "reading a key's public half")?;
    expect("secrets", secrets("tar -C ~ -cf /tmp/agentkit-self-test.tar .ssh", cfg), true, "archiving ~/.ssh from -C ~")?;
    let site_file = cfg.home.join(".config/agentkit/agentkit.toml");
    expect("guard-config", guard_config(&format!("echo x >> '{}'", site_file.display()), cfg), true, "a write to the site config")?;
    expect("guard-config", guard_config(&format!("cat '{}'", site_file.display()), cfg), false, "reading the site config")?;
    expect("hard-delete", hard_delete("$(echo rm) -rf /var/agentkit-self-test/important", cfg), true, "a computed program word")?;
    expect("hard-delete", hard_delete("rsync -a --delete /tmp/agentkit-self-test/ /var/agentkit-self-test/important/", cfg), true, "rsync --delete outside temp")?;
    Ok(())
}
