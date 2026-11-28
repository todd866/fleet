//! Site data lives outside the public repo, at `~/.config/agentkit` (a `--config DIR` flag
//! points the tests at another tree). The example tree in this crate is the sanitized shape.
//! Nothing here reads a path from the environment: a hook inherits its environment from the
//! agent it guards, so a variable that chose the config would let that agent choose it.

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
    /// agentkit's config directory when it is not `<home>/.config/agentkit` (a test tree).
    /// The guard-config guard protects it and `<home>/.config/agentkit` both.
    pub config_dir: Option<PathBuf>,
    /// agentkit's state folder when it is not `<home>/.local/state/agentkit` (a test tree).
    pub state_dir: Option<PathBuf>,
    /// The decision ledger when it is not `<state>/decisions.jsonl` (a test tree).
    pub ledger_file: Option<PathBuf>,
    /// The program that names the worker for a heavy command. `None` means
    /// `<home>/.local/bin/offload-target`.
    pub offload_probe: Option<PathBuf>,
    /// The programs the hooks run, besides `<home>/.local/bin/agentkit` and `agent-context`:
    /// the running hook's own executable and the path site config names. No agent writes them.
    pub guard_bins: Vec<PathBuf>,
}

pub const DEFAULT_SLOT_FILES: &[&str] = &["limit", "cool"];
pub const DEFAULT_APPROVAL_FILE_PATTERNS: &[&str] = &["APPROVE_*"];
pub const DEFAULT_IDRIVE_CLEANUP_PATTERNS: &[&str] = &[r"(?i)archive[\s_.-]*clean[\s_-]*up"];

/// The account's home folder from the user database, never from `$HOME`: the hook's
/// environment belongs to the agent it guards, and a `HOME` of the agent's choosing would move
/// the config, the ledger and every `~` the guards resolve.
pub fn account_home() -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    static HOME: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    HOME.get_or_init(|| {
        // SAFETY: getpwuid_r writes only into the buffers handed to it; `pw_dir` points into
        // `buf`, which outlives the copy made below.
        unsafe {
            let uid = libc::geteuid();
            let mut buf: Vec<libc::c_char> = vec![0; 4096];
            let mut pwd: libc::passwd = std::mem::zeroed();
            let mut result: *mut libc::passwd = std::ptr::null_mut();
            loop {
                let rc = libc::getpwuid_r(uid, &mut pwd, buf.as_mut_ptr(), buf.len(), &mut result);
                if rc == libc::ERANGE && buf.len() < (1 << 20) {
                    buf.resize(buf.len() * 4, 0);
                    continue;
                }
                if rc != 0 || result.is_null() || pwd.pw_dir.is_null() {
                    return None;
                }
                break;
            }
            let dir = std::ffi::CStr::from_ptr(pwd.pw_dir).to_bytes();
            if dir.is_empty() || dir[0] != b'/' {
                return None;
            }
            Some(PathBuf::from(std::ffi::OsStr::from_bytes(dir)))
        }
    })
    .clone()
}

/// Folders a hard delete is allowed in whatever `$TMPDIR` says.
pub const TEMP_ROOTS: &[&str] = &["/tmp/", "/private/tmp/", "/var/folders/", "/private/var/folders/"];

/// The temp roots as the disk has them, symlinks resolved (`/tmp` is `/private/tmp` on macOS).
pub fn canonical_temp_roots() -> &'static [PathBuf] {
    static ROOTS: std::sync::OnceLock<Vec<PathBuf>> = std::sync::OnceLock::new();
    ROOTS.get_or_init(|| {
        let mut out: Vec<PathBuf> = TEMP_ROOTS.iter().filter_map(|r| fs::canonicalize(r).ok()).collect();
        out.sort();
        out.dedup();
        out
    })
}

/// `$TMPDIR`, when it is a folder inside the system's temp roots. Any other value is the
/// caller's claim that some folder is disposable (`TMPDIR=$HOME` would make a delete anywhere
/// under home a temp delete), so it is not believed.
pub fn trusted_tmpdir(home: &Path) -> Option<PathBuf> {
    trusted_tmp(&env::var_os("TMPDIR").map(PathBuf::from)?, home)
}

/// Judged by where the folder is, not by how its name is spelt: symlinks are resolved first,
/// so a link inside a temp root that points at the home folder (`/tmp/x -> ~`) is the home
/// folder and is refused. The resolved folder must sit inside a resolved temp root and must
/// not be, or hold, the home folder. A folder that does not exist cannot be placed and is not
/// believed. Returns the resolved path, so what is compared against it later is compared in
/// the same form.
pub fn trusted_tmp(raw: &Path, home: &Path) -> Option<PathBuf> {
    if !raw.is_absolute() || raw.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
        return None;
    }
    let real = fs::canonicalize(raw).ok()?;
    if !real.is_dir() || !canonical_temp_roots().iter().any(|root| real.starts_with(root)) {
        return None;
    }
    let real_home = fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf());
    if real_home.starts_with(&real) {
        return None;
    }
    Some(real)
}

impl Default for GuardConfig {
    fn default() -> Self {
        let home = account_home().unwrap_or_else(|| PathBuf::from("/var/empty"));
        let cwd = env::current_dir().unwrap_or_else(|_| home.clone());
        let tmpdir = trusted_tmpdir(&home);
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
            config_dir: None,
            state_dir: None,
            ledger_file: None,
            offload_probe: None,
            guard_bins: Vec::new(),
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

    /// agentkit's state folder: the ledger, its key, doctor's report, the accepted baseline.
    pub fn state(&self) -> PathBuf {
        self.state_dir.clone().unwrap_or_else(|| self.home.join(".local/state/agentkit"))
    }

    /// The decision ledger.
    pub fn ledger(&self) -> PathBuf {
        self.ledger_file.clone().unwrap_or_else(|| self.state().join("decisions.jsonl"))
    }

    /// Every program a generated hook runs, by absolute path: `agentkit` and the
    /// `agent-context` beside it, at the default place and wherever `guard_bins` says.
    pub fn hook_bins(&self) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for bin in std::iter::once(default_hook_bin(&self.home)).chain(self.guard_bins.iter().cloned()) {
            for path in [bin.clone(), bin.with_file_name("agent-context")] {
                if !out.contains(&path) {
                    out.push(path);
                }
            }
        }
        out
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

pub const ALL_PROVIDERS: &[&str] = &["claude", "codex", "cursor", "agy"];

#[derive(Debug, Clone, Deserialize)]
pub struct Site {
    #[serde(default = "one")]
    pub schema: u32,
    #[serde(default)]
    pub manage_model: bool,
    /// Providers managed by `agentkit sync` / `doctor`. When empty, all known providers
    /// except any listed in `paused-providers` are managed.
    #[serde(default)]
    pub providers: Vec<String>,
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
    /// Where the `agentkit` binary the hooks run is installed, when it is not
    /// `~/.local/bin/agentkit` (`~/` allowed). `agent-context` sits beside it. Generated hooks
    /// name both by absolute path, so nothing earlier on `PATH` can stand in for them.
    #[serde(default)]
    pub hook_bin: Option<String>,
}

fn one() -> u32 { 1 }

/// Where the hook binary is installed unless site config says otherwise.
pub fn default_hook_bin(home: &Path) -> PathBuf {
    home.join(".local/bin/agentkit")
}

impl Site {
    /// A site with nothing set: every guard on, built-in defaults. What a Mac with no
    /// `agentkit.toml` runs with.
    pub fn empty() -> Self {
        Site {
            schema: 1,
            manage_model: false,
            providers: vec![],
            projects: vec![],
            retired_cursor_rules: vec![],
            guards: GuardFile::default(),
            models: Models::default(),
            hook_bin: None,
        }
    }

    /// The absolute path generated hooks call. A relative `hook_bin` is an error: it would be
    /// looked up on `PATH`, which is what the absolute path is there to avoid.
    pub fn hook_bin_path(&self, home: &Path) -> Result<PathBuf, String> {
        let path = match self.hook_bin.as_deref() {
            None => default_hook_bin(home),
            Some(raw) => expand_home(raw, home),
        };
        // The path is written into hook commands a shell runs, unquoted.
        let text = path.to_string_lossy();
        let plain = text.chars().all(|c| c.is_ascii_alphanumeric() || "/._-+@:,".contains(c))
            && !text.split('/').any(|c| c == "..");
        if path.is_absolute() && path.file_name().is_some() && plain {
            Ok(path)
        } else {
            Err(format!(
                "the hook binary path {text:?} must be absolute (or start with ~/), with no spaces or shell characters; set `hook_bin` in agentkit.toml"
            ))
        }
    }
}

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

pub fn validate_providers(providers: &[String]) -> Result<(), String> {
    for name in providers {
        if !ALL_PROVIDERS.contains(&name.as_str()) {
            return Err(format!(
                "unknown provider {:?} in providers (known: {})",
                name,
                ALL_PROVIDERS.join(", ")
            ));
        }
    }
    Ok(())
}

pub fn paused_providers_in(config: &Path) -> Vec<String> {
    let Ok(text) = fs::read_to_string(config.join("paused-providers")) else {
        return Vec::new();
    };
    text.lines()
        .map(|l| l.split('#').next().unwrap_or("").trim().to_ascii_lowercase())
        .filter(|l| !l.is_empty())
        .collect()
}

pub fn active_providers(config: &Path, site: &Site) -> Vec<&'static str> {
    let paused = paused_providers_in(config);
    let base: Vec<&'static str> = if site.providers.is_empty() {
        ALL_PROVIDERS.to_vec()
    } else {
        ALL_PROVIDERS
            .iter()
            .copied()
            .filter(|p| site.providers.iter().any(|s| s == p))
            .collect()
    };
    base.into_iter()
        .filter(|p| !paused.iter().any(|q| q == p))
        .collect()
}

impl Site {
    pub fn load(dir: &Path) -> Result<Self, String> {
        let path = dir.join("agentkit.toml");
        let text = fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
        Self::parse(&path, &text)
    }

    fn parse(path: &Path, text: &str) -> Result<Self, String> {
        let site: Site = toml::from_str(text).map_err(|e| format!("parse {}: {e}", path.display()))?;
        if site.schema != 1 {
            return Err(format!("unsupported schema {} (this agentkit reads 1)", site.schema));
        }
        validate_providers(&site.providers)?;
        validate_disabled(&site.guards.disabled)?;
        Ok(site)
    }

    /// For a hook: `None` when there is no `agentkit.toml` at all (every guard runs with its
    /// defaults), an error when one is there and cannot be read or understood. A hook that
    /// went on with defaults then would silently drop the site's push gates, secret paths and
    /// worker names, so the caller refuses instead.
    pub fn load_if_present(dir: &Path) -> Result<Option<Self>, String> {
        let path = dir.join("agentkit.toml");
        match fs::read_to_string(&path) {
            Ok(text) => Self::parse(&path, &text).map(Some),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("read {}: {e}", path.display())),
        }
    }
}

/// The site and the guard configuration a hook runs with, from the folders in `paths` and
/// nothing else. The only things read from the environment are the session's working folder
/// (`CLAUDE_PROJECT_DIR`, used when the tool call carries none), a `TMPDIR` inside the system
/// temp roots, and `AGENTKIT_SKIP_SSH_RESOLVE`, which can only make a guard stricter.
pub fn load_for_hook(paths: &crate::Paths) -> Result<(Site, GuardConfig), String> {
    let mut cfg = GuardConfig::default();
    cfg.home = paths.home.clone();
    cfg.tmpdir = trusted_tmpdir(&cfg.home);
    cfg.allowed_cd = default_cd_roots(&cfg.home);
    if let Some(dir) = env::var_os("CLAUDE_PROJECT_DIR").filter(|d| !d.is_empty()) {
        cfg.cwd = PathBuf::from(dir);
    }
    cfg.config_dir = Some(paths.config.clone());
    cfg.state_dir = Some(paths.state.clone());
    cfg.ledger_file = paths.ledger.clone();
    if let Ok(exe) = env::current_exe() {
        cfg.guard_bins.push(exe);
    }
    let site = match Site::load_if_present(&paths.config)? {
        Some(site) => {
            apply_guards(&mut cfg, &paths.config, &site.guards);
            cfg.guard_bins.push(site.hook_bin_path(&paths.home)?);
            site
        }
        None => {
            let legacy = paths.config.join("guards.toml");
            match fs::read_to_string(&legacy) {
                Ok(text) => {
                    let guards: GuardFile =
                        toml::from_str(&text).map_err(|e| format!("parse {}: {e}", legacy.display()))?;
                    validate_disabled(&guards.disabled)?;
                    apply_guards(&mut cfg, &paths.config, &guards);
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(format!("read {}: {e}", legacy.display())),
            }
            Site::empty()
        }
    };
    // `offload_worker` stays unset here. The probe can take 20 s when the worker is slow to
    // answer; the offload-heavy guard runs it only for a command that is actually heavy.
    Ok((site, cfg))
}

pub(crate) fn apply_guards(cfg: &mut GuardConfig, config: &Path, guards: &GuardFile) {
    cfg.config_dir = Some(config.to_path_buf());
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

pub(crate) fn probe_offload(cfg: &GuardConfig) -> Result<String, ()> {
    let bin = cfg.offload_probe.clone().unwrap_or_else(|| cfg.home.join(".local/bin/offload-target"));
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
