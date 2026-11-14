//! Standalone guard decisions. Each one reads a shell command (Claude
//! `tool_input.command`, Cursor `command`, or a bare string) and answers
//! allow, ask, or deny. Parse failures allow, matching the hooks these
//! replace: a broken stdin must not become a new way to stall a session.
//! The deny rules themselves are not relaxed.

use crate::site::GuardConfig;
use fancy_regex::Regex;
use serde_json::Value;
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
    "sudo", "command", "builtin", "nohup", "time", "env", "xargs", "exec", "nice",
];
const KEYWORDS: &[&str] = &[
    "then", "do", "else", "elif", "if", "while", "until", "!", "{", "}", "case", "in",
];

fn hard_delete(cmd: &str, cfg: &GuardConfig) -> Decision {
    let cmd = strip_file_heredocs(cmd);
    if let Some((host, rest)) = ssh_parts(&cmd) {
        if cfg.is_air(&host) && rest.len() == 1 {
            return Decision::allow();
        }
        if !cfg.is_air(&host) && rest.len() == 1 {
            if let Decision { decision: "deny", reason, .. } = hard_delete(&rest[0], cfg) {
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
            && (is_assignment(&argv[0]) || WRAPPERS.contains(&argv[0].as_str()) || KEYWORDS.contains(&argv[0].as_str()))
        {
            argv.remove(0);
            while !argv.is_empty() && argv[0].starts_with('-') && argv[0] != "--" {
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
    let worker = cfg.offload_worker.clone().unwrap_or_default();
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
    Decision::ask(
        "push",
        "A push can publish or spend a build. Confirm the project's gate is green and the owner approved this push, then prefix AGENTKIT_PUSH_APPROVED=1.",
    )
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
    let ask_push = push("git push origin main", cfg);
    if !ask_push.is_ask() {
        return Err("push guard did not ask on git push".into());
    }
    let deny_force = push("git push --force origin main", cfg);
    if !deny_force.is_deny() {
        return Err("push guard allowed git push --force".into());
    }
    Ok(())
}
