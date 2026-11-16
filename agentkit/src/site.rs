//! Site data lives outside the public repo, at `~/.config/agentkit` or
//! `AGENTKIT_CONFIG`. The example tree in this crate is the sanitized shape.

use crate::guard::ALL_GUARD_NAMES;
use serde::Deserialize;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone)]
pub struct GuardConfig {
    pub home: PathBuf,
    pub cwd: PathBuf,
    pub tmpdir: Option<PathBuf>,
    pub air_hosts: Vec<String>,
    pub air_aliases: Vec<String>,
    pub allowed_cd: Vec<PathBuf>,
    pub email_substrings: Vec<String>,
    pub push_markers: Vec<String>,
    /// Paths (`~` expanded) where a plain `git push` asks unless a `push_markers` prefix is present.
    pub push_gate_projects: Vec<PathBuf>,
    /// Remote repositories (`owner/name`) whose clones and worktrees are gated wherever they live.
    pub push_gate_remotes: Vec<String>,
    /// `Some("")` means offload-target said stay. `None` means the probe was not run
    /// and the caller fills it. The library decision uses this string as the worker name.
    pub offload_worker: Option<String>,
    pub resolve_ssh: bool,
    /// heavy-slot's state directory. `None` means `<home>/.local/state/heavy-slot`, the
    /// directory heavy-slot itself uses when `HEAVY_SLOT_DIR` is not set.
    pub slot_state_dir: Option<PathBuf>,
    /// File names in that directory that only the fleet's own tools and the owner write.
    pub slot_files: Vec<String>,
    /// Names of owner-approval marker files. A trailing `*` matches any ending.
    pub approval_file_patterns: Vec<String>,
    /// Regular expressions that name the backup client's destructive cleanup.
    pub idrive_cleanup_patterns: Vec<String>,
    /// More private-key or credential files the secrets guard keeps from being printed or sent.
    pub secret_paths: Vec<String>,
}

pub const DEFAULT_SLOT_FILES: &[&str] = &["limit", "cool"];
pub const DEFAULT_APPROVAL_FILE_PATTERNS: &[&str] = &["APPROVE_*"];
pub const DEFAULT_IDRIVE_CLEANUP_PATTERNS: &[&str] = &[r"(?i)archive[\s_.-]*clean[\s_-]*up"];

impl Default for GuardConfig {
    fn default() -> Self {
        let home = env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/tmp"));
        let cwd = env::current_dir().unwrap_or_else(|_| home.clone());
        let tmpdir = env::var_os("TMPDIR").map(PathBuf::from);
        Self {
            allowed_cd: default_cd_roots(&home),
            home,
            cwd,
            tmpdir,
            air_hosts: Vec::new(),
            air_aliases: Vec::new(),
            email_substrings: Vec::new(),
            push_markers: vec!["AGENTKIT_PUSH_APPROVED=1".into()],
            push_gate_projects: Vec::new(),
            push_gate_remotes: Vec::new(),
            offload_worker: None,
            resolve_ssh: env::var("AGENTKIT_SKIP_SSH_RESOLVE").ok().as_deref() != Some("1"),
            slot_state_dir: None,
            slot_files: DEFAULT_SLOT_FILES.iter().map(|s| (*s).to_string()).collect(),
            approval_file_patterns: DEFAULT_APPROVAL_FILE_PATTERNS.iter().map(|s| (*s).to_string()).collect(),
            idrive_cleanup_patterns: DEFAULT_IDRIVE_CLEANUP_PATTERNS.iter().map(|s| (*s).to_string()).collect(),
            secret_paths: Vec::new(),
        }
    }
}

pub fn default_cd_roots(home: &Path) -> Vec<PathBuf> {
    vec![
        home.join("Projects"),
        home.join(".cursor/worktrees"),
        home.join(".local/state"),
        home.join(".claude"),
        PathBuf::from("/tmp"),
        PathBuf::from("/private/tmp"),
        PathBuf::from("/var/folders"),
    ]
}

impl GuardConfig {
    /// Where heavy-slot keeps `limit`, `cool`, its slot locks and its queue.
    pub fn slot_dir(&self) -> PathBuf {
        self.slot_state_dir
            .clone()
            .unwrap_or_else(|| self.home.join(".local/state/heavy-slot"))
    }

    pub fn is_air(&self, host: &str) -> bool {
        let name = host.rsplit('@').next().unwrap_or(host).to_ascii_lowercase();
        if self.air_aliases.iter().any(|a| a.eq_ignore_ascii_case(&name))
            || self.air_hosts.iter().any(|a| a.eq_ignore_ascii_case(&name))
        {
            return true;
        }
        if !self.resolve_ssh {
            return false;
        }
        let output = Command::new("ssh")
            .args(["-G", &name])
            .output();
        let Ok(output) = output else { return false };
        let text = String::from_utf8_lossy(&output.stdout);
        let real = text
            .lines()
            .find(|l| l.starts_with("hostname "))
            .and_then(|l| l.split_whitespace().nth(1))
            .unwrap_or("")
            .to_ascii_lowercase();
        self.air_hosts.iter().any(|a| a.eq_ignore_ascii_case(&real))
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Site {
    #[serde(default = "one")]
    pub schema: u32,
    #[serde(default)]
    pub manage_model: bool,
    #[serde(default)]
    pub projects: Vec<Project>,
    /// File names under `~/.cursor/rules` that the generated global rule replaces. Each is
    /// rewritten as a stub that is never auto-applied. Site-specific, so it lives in config.
    #[serde(default)]
    pub retired_cursor_rules: Vec<String>,
    #[serde(default)]
    pub guards: GuardFile,
    #[serde(default)]
    pub models: Models,
}

fn one() -> u32 { 1 }

#[derive(Debug, Clone, Deserialize)]
pub struct Project {
    pub name: String,
    pub path: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct GuardFile {
    #[serde(default)]
    pub air_hosts: Vec<String>,
    #[serde(default)]
    pub air_aliases: Vec<String>,
    #[serde(default)]
    pub allowed_cd: Vec<String>,
    #[serde(default)]
    pub push_markers: Vec<String>,
    #[serde(default)]
    pub push_gate_projects: Vec<String>,
    #[serde(default)]
    pub push_gate_remotes: Vec<String>,
    /// Optional file of substrings that must not appear in a command. The file
    /// stays in the private config; nothing from it is written into the repo.
    #[serde(default)]
    pub email_substrings_file: Option<String>,
    /// heavy-slot's state directory, when it is not `~/.local/state/heavy-slot`.
    #[serde(default)]
    pub slot_state_dir: Option<String>,
    /// Files in it that are not edited by hand. Default: `limit`, `cool`.
    #[serde(default)]
    pub slot_files: Vec<String>,
    /// Owner-approval marker file names. Default: `APPROVE_*`.
    #[serde(default)]
    pub approval_file_patterns: Vec<String>,
    /// Regular expressions naming the backup client's cleanup that deletes from the backup.
    #[serde(default)]
    pub idrive_cleanup_patterns: Vec<String>,
    /// Extra private-key or credential files for the secrets guard (`~/` allowed).
    #[serde(default)]
    pub secret_paths: Vec<String>,
    /// Guards omitted from generated hooks (e.g. `push` when sessions run without prompts).
    #[serde(default)]
    pub disabled: Vec<String>,
}

pub fn guard_enabled(name: &str, disabled: &[String]) -> bool {
    !disabled.iter().any(|d| d == name)
}

pub fn validate_disabled(disabled: &[String]) -> Result<(), String> {
    for name in disabled {
        if !ALL_GUARD_NAMES.contains(&name.as_str()) {
            return Err(format!(
                "unknown guard {:?} in [guards] disabled (known: {})",
                name,
                ALL_GUARD_NAMES.join(", ")
            ));
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Models {
    #[serde(default)]
    pub claude_model: String,
    #[serde(default)]
    pub claude_effort: String,
    #[serde(default)]
    pub codex_model: String,
    #[serde(default)]
    pub codex_effort: String,
    #[serde(default)]
    pub cursor_model: String,
}

impl Site {
    pub fn load(dir: &Path) -> Result<Self, String> {
        let path = dir.join("agentkit.toml");
        let text = fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
        let site: Site = toml::from_str(&text).map_err(|e| format!("parse {}: {e}", path.display()))?;
        if site.schema != 1 {
            return Err(format!("unsupported schema {} (this agentkit reads 1)", site.schema));
        }
        validate_disabled(&site.guards.disabled)?;
        Ok(site)
    }
}

pub fn guard_config_from_env() -> GuardConfig {
    let mut cfg = GuardConfig::default();
    if let Some(dir) = env::var_os("CLAUDE_PROJECT_DIR").filter(|d| !d.is_empty()) {
        cfg.cwd = PathBuf::from(dir);
    }
    let config = env::var_os("AGENTKIT_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|| cfg.home.join(".config/agentkit"));
    // heavy-slot's own override; a site value below takes precedence.
    if let Some(dir) = env::var_os("HEAVY_SLOT_DIR").filter(|d| !d.is_empty()) {
        cfg.slot_state_dir = Some(PathBuf::from(dir));
    }
    if let Ok(site) = Site::load(&config) {
        apply_guards(&mut cfg, &config, &site.guards);
    } else if let Ok(text) = fs::read_to_string(config.join("guards.toml")) {
        if let Ok(guards) = toml::from_str::<GuardFile>(&text) {
            apply_guards(&mut cfg, &config, &guards);
        }
    }
    // `offload_worker` stays unset here. The probe can take 20 s when the worker is slow to
    // answer; the offload-heavy guard runs it only for a command that is actually heavy.
    cfg
}

pub(crate) fn apply_guards(cfg: &mut GuardConfig, config: &Path, guards: &GuardFile) {
    cfg.air_hosts = guards.air_hosts.clone();
    cfg.air_aliases = guards.air_aliases.clone();
    if !guards.allowed_cd.is_empty() {
        cfg.allowed_cd = guards
            .allowed_cd
            .iter()
            .map(|p| expand_home(p, &cfg.home))
            .collect();
    }
    if !guards.push_markers.is_empty() {
        cfg.push_markers = guards.push_markers.clone();
    }
    if !guards.push_gate_projects.is_empty() {
        cfg.push_gate_projects = guards
            .push_gate_projects
            .iter()
            .map(|p| expand_home(p, &cfg.home))
            .collect();
    }
    if !guards.push_gate_remotes.is_empty() {
        cfg.push_gate_remotes = guards.push_gate_remotes.clone();
    }
    if let Some(dir) = guards.slot_state_dir.as_deref().filter(|d| !d.is_empty()) {
        cfg.slot_state_dir = Some(expand_home(dir, &cfg.home));
    }
    if !guards.slot_files.is_empty() {
        cfg.slot_files = guards.slot_files.clone();
    }
    if !guards.approval_file_patterns.is_empty() {
        cfg.approval_file_patterns = guards.approval_file_patterns.clone();
    }
    if !guards.idrive_cleanup_patterns.is_empty() {
        cfg.idrive_cleanup_patterns = guards.idrive_cleanup_patterns.clone();
    }
    cfg.secret_paths = guards.secret_paths.clone();
    if let Some(file) = &guards.email_substrings_file {
        let path = if Path::new(file).is_absolute() {
            PathBuf::from(file)
        } else {
            config.join(file)
        };
        if let Ok(text) = fs::read_to_string(path) {
            cfg.email_substrings = text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).map(str::to_string).collect();
        }
    }
}

pub fn expand_home(path: &str, home: &Path) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        home.join(rest)
    } else if path == "~" {
        home.to_path_buf()
    } else {
        PathBuf::from(path)
    }
}

pub(crate) fn probe_offload(home: &Path) -> Result<String, ()> {
    let bin = env::var_os("AGENTKIT_OFFLOAD_TARGET")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".local/bin/offload-target"));
    if !bin.is_file() {
        return Ok(String::new());
    }
    let output = Command::new(&bin).output().map_err(|_| ())?;
    if !output.status.success() {
        return Ok(String::new());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub fn rules_global(config: &Path) -> Result<String, String> {
    let always = config.join("rules/core-always.md");
    if always.is_file() {
        return fs::read_to_string(&always).map_err(|e| format!("rules/core-always.md: {e}"));
    }
    fs::read_to_string(config.join("rules/global.md")).map_err(|e| format!("rules/global.md: {e}"))
}

pub fn rules_for_provider(config: &Path, provider: &str) -> Result<String, String> {
    let core = rules_global(config)?;
    let appendix = config.join(format!("rules/appendix-{provider}.md"));
    if appendix.is_file() {
        let extra = fs::read_to_string(&appendix).map_err(|e| format!("rules/appendix-{provider}.md: {e}"))?;
        if extra.trim().is_empty() {
            return Ok(core);
        }
        return Ok(format!("{}\n\n{}", core.trim_end(), extra.trim_end()));
    }
    Ok(core)
}

pub fn rules_project(config: &Path, name: &str) -> Result<String, String> {
    let path = config.join("rules/projects").join(format!("{name}.md"));
    fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))
}

pub fn skill_dirs(config: &Path) -> Vec<PathBuf> {
    let root = config.join("skills");
    let mut out = Vec::new();
    let Ok(rd) = fs::read_dir(&root) else { return out };
    for ent in rd.flatten() {
        if ent.path().join("SKILL.md").is_file() {
            out.push(ent.path());
        }
    }
    out.sort();
    out
}
