//! Drift check. Exit 0 when the selected provider matches the site.
//! Writes `doctor.json` for fleet-health and mac-registry.

use crate::conformance::{self, conformance_ok};
use crate::generate::{self, Planned};
use crate::guard;
use crate::inventory::GAPS;
use crate::site::{self, GuardConfig, Site};
use crate::Paths;
use serde_json::{json, Value};
use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn run(
    paths: &Paths,
    provider: Option<&str>,
    conformance_only: bool,
    json_out: bool,
) -> Result<(i32, String), String> {
    let site = match Site::load(&paths.config) {
        Ok(s) => s,
        Err(e) => {
            let report = json!({
                "schema": 1,
                "generated": now(),
                "ok": false,
                "issues": [e],
                "providers": [],
                "conformance": [],
            });
            write_report(paths, &report)?;
            let text = if json_out {
                serde_json::to_string_pretty(&report).unwrap_or_default()
            } else {
                format!("! site       missing    {e}\n")
            };
            return Ok((1, text));
        }
    };
    let planned = generate::plan(paths, &site)?;
    let names: Vec<&str> = match provider {
        Some(p) if matches!(p, "claude" | "codex" | "cursor" | "agy") => vec![p],
        Some(p) => return Err(format!("unknown provider {p} (claude, codex, cursor, agy)")),
        None => site::active_providers(&paths.config, &site),
    };
    let mut cfg = guard_cfg(&paths.home, &paths.config, &site);
    cfg.offload_worker = Some("worker".into());
    let self_test = guard::self_test(&cfg);
    let memory_ok = paths.notebook().join("INDEX.md").is_file();
    let conformance_rows = conformance::run(paths, &planned, &cfg, &site);
    let mut providers = Vec::new();
    let mut issues: Vec<String> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    for project in &site.projects {
        match generate::resolve_under_home(&paths.home, &project.path) {
            Ok(dir) if project_tree_dirty(&dir) => {
                warnings.push(format!(
                    "project {}: working tree dirty at {}",
                    project.name,
                    dir.display()
                ));
            }
            Err(e) => warnings.push(format!("project {}: {e}", project.name)),
            _ => {}
        }
    }
    if let Err(e) = &self_test {
        issues.push(format!("guard self-test: {e}"));
    }
    // A guard switched off in site config: until 21 Nov 2026 one write of `disabled = [...]`
    // turned guards off and doctor stayed green. Now a disable the owner has accepted
    // (`agentkit doctor --accept-guards`, which records a hash of [guards]) is a warning that
    // names it; a disable with no accepted baseline, or after [guards] changed, fails.
    if !site.guards.disabled.is_empty() {
        let names = site.guards.disabled.join(", ");
        match guards_baseline_state(paths) {
            GuardsBaseline::Accepted => warnings.push(format!(
                "guards disabled by site config, accepted by the owner: {names}"
            )),
            GuardsBaseline::Changed => issues.push(format!(
                "guards disabled by site config ({names}) and [guards] changed since the owner accepted it; the owner reviews and runs `agentkit doctor --accept-guards`"
            )),
            GuardsBaseline::Missing => issues.push(format!(
                "guards disabled by site config ({names}) with no accepted baseline; the owner reviews and runs `agentkit doctor --accept-guards`"
            )),
        }
    }
    if !memory_ok {
        issues.push("memory: notebook INDEX.md is not readable".into());
    }
    if !conformance_ok(&conformance_rows) {
        issues.push("conformance: one or more provider×guard checks failed".into());
    }
    let mut lines = Vec::new();
    for name in &names {
        let row = provider_row(name, &planned, &site, memory_ok, self_test.is_ok());
        if !row.ok {
            for issue in &row.issues {
                issues.push(format!("{name}: {issue}"));
            }
        }
        lines.push(format_line(&row));
        providers.push(row);
    }
    let ok = issues.is_empty();
    let json_providers: Vec<Value> = providers.iter().map(|p| p.to_json()).collect();
    let report = json!({
        "schema": 1,
        "generated": now(),
        "ok": ok,
        "issues": issues,
        "warnings": warnings,
        "providers": json_providers,
        "conformance": conformance::to_json(&conformance_rows),
    });
    write_report(paths, &report)?;
    let text = if json_out {
        serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?
    } else if conformance_only {
        conformance::format_table(&conformance_rows)
    } else {
        let mut text = String::new();
        for line in lines {
            text.push_str(&line);
            text.push('\n');
        }
        for w in &warnings {
            text.push_str(&format!("? warning  {}\n", w));
        }
        text.push_str(&conformance::format_table(&conformance_rows));
        text
    };
    Ok((if ok { 0 } else { 1 }, text))
}

struct Row {
    id: String,
    ok: bool,
    rules: String,
    guards: String,
    context: String,
    memory: String,
    issues: Vec<String>,
    gaps: Vec<String>,
}

impl Row {
    fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "ok": self.ok,
            "rules": self.rules,
            "guards": self.guards,
            "context": self.context,
            "memory": self.memory,
            "issues": self.issues,
            "gaps": self.gaps,
        })
    }
}

fn provider_row(name: &str, planned: &[Planned], site: &Site, memory_ok: bool, self_ok: bool) -> Row {
    let mut rule_bad = Vec::new();
    let enabled: Vec<&str> = conformance::GUARD_NAMES
        .iter()
        .copied()
        .filter(|g| site::guard_enabled(g, &site.guards.disabled))
        .collect();
    let guard_cmds = enabled.len();
    let mut guard_have = 0usize;
    let mut context_on = false;
    let mut hook_missing = false;
    for file in planned.iter().filter(|f| f.providers.contains(&name)) {
        let disk = fs::read_to_string(&file.path).unwrap_or_default();
        if file.kind == "rules" || file.kind == "model" {
            if disk != file.contents {
                rule_bad.push(file.path.display().to_string());
            }
        } else if file.kind == "hooks" {
            if disk != file.contents {
                hook_missing = true;
            }
            let text = if disk.is_empty() { &file.contents } else { &disk };
            let all_needle = format!("hook {name} all");
            if text.contains(&all_needle) {
                guard_have = enabled.len();
            } else {
                for guard in &enabled {
                    let needle = format!("hook {name} {guard}");
                    if text.contains(&needle) {
                        guard_have += 1;
                    }
                }
            }
            if text.contains("agent-context") || (name == "agy" && text.contains("agy-session-start")) {
                context_on = true;
            }
        }
    }
    let mut issues = Vec::new();
    let rules = if rule_bad.is_empty() {
        "current".into()
    } else {
        issues.push(format!("rules drift ({})", rule_bad.join(", ")));
        "drift".into()
    };
    let guards = if !self_ok {
        issues.push("guard self-test failed".into());
        "self-test fail".into()
    } else if guard_have == guard_cmds && guard_cmds > 0 {
        format!("{guard_have}/{guard_cmds}")
    } else {
        issues.push(format!("guards wired {guard_have}/{guard_cmds}"));
        format!("{guard_have}/{guard_cmds}")
    };
    let context = if context_on {
        "on".into()
    } else {
        issues.push("context hook missing".into());
        "off".into()
    };
    let memory = if memory_ok {
        "ok".into()
    } else {
        issues.push("notebook missing".into());
        "missing".into()
    };
    if hook_missing && guard_have == guard_cmds && context_on {
        issues.push("hook file differs from the generated hook file".into());
    }
    let gaps = GAPS
        .iter()
        .filter(|g| g.provider == "all" || g.provider.eq_ignore_ascii_case(name) || g.provider.contains(name_title(name)))
        .map(|g| format!("{}: {}", g.provider, g.gap))
        .collect();
    let ok = issues.is_empty();
    Row { id: name.to_string(), ok, rules, guards, context, memory, issues, gaps }
}

fn name_title(name: &str) -> &'static str {
    match name {
        "claude" => "Claude",
        "codex" => "Codex",
        "cursor" => "Cursor",
        "agy" => "Antigravity",
        _ => "",
    }
}

fn format_line(row: &Row) -> String {
    let mark = if row.ok { "*" } else { "!" };
    format!(
        "{mark} {:<8} rules {:<8} guards {:<14} context {:<3} memory {}",
        row.id, row.rules, row.guards, row.context, row.memory
    )
}

fn project_tree_dirty(dir: &std::path::Path) -> bool {
    if !dir.is_dir() {
        return false;
    }
    Command::new("git")
        .args(["-C", &dir.to_string_lossy(), "status", "--porcelain"])
        .output()
        .map(|o| o.status.success() && !o.stdout.is_empty())
        .unwrap_or(false)
}

fn guard_cfg(home: &Path, config: &Path, site: &Site) -> GuardConfig {
    let mut cfg = GuardConfig::default();
    cfg.home = home.to_path_buf();
    cfg.air_hosts = site.guards.air_hosts.clone();
    cfg.air_aliases = site.guards.air_aliases.clone();
    cfg.resolve_ssh = false;
    site::apply_guards(&mut cfg, config, &site.guards);
    cfg
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn write_report(paths: &Paths, value: &Value) -> Result<(), String> {
    fs::create_dir_all(&paths.state).map_err(|e| e.to_string())?;
    let mut text = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
    text.push('\n');
    fs::write(paths.state.join("doctor.json"), text).map_err(|e| e.to_string())
}

/// Where the owner's accepted `[guards]` baseline lives. guard-config denies agents writing it.
pub const GUARDS_BASELINE_FILE: &str = "guards-accepted.json";

enum GuardsBaseline {
    Accepted,
    Changed,
    Missing,
}

/// The site's `[guards]` section in one canonical text (keys sorted), whatever its layout.
fn guards_section(paths: &Paths) -> Result<String, String> {
    let path = paths.config.join("agentkit.toml");
    let text = fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let doc: toml::Value = toml::from_str(&text).map_err(|e| format!("parse {}: {e}", path.display()))?;
    let guards = doc.get("guards").cloned().unwrap_or(toml::Value::Table(Default::default()));
    let mut wrapper = toml::map::Map::new();
    wrapper.insert("guards".into(), sort_toml(&guards));
    toml::to_string(&toml::Value::Table(wrapper)).map_err(|e| e.to_string())
}

fn sort_toml(v: &toml::Value) -> toml::Value {
    match v {
        toml::Value::Table(t) => {
            let mut keys: Vec<&String> = t.keys().collect();
            keys.sort();
            let mut out = toml::map::Map::new();
            for k in keys {
                out.insert(k.clone(), sort_toml(&t[k]));
            }
            toml::Value::Table(out)
        }
        toml::Value::Array(a) => toml::Value::Array(a.iter().map(sort_toml).collect()),
        other => other.clone(),
    }
}

fn sha256_hex(text: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(text.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

fn read_baseline(paths: &Paths) -> Option<Value> {
    let text = fs::read_to_string(paths.state.join(GUARDS_BASELINE_FILE)).ok()?;
    serde_json::from_str(&text).ok()
}

fn guards_baseline_state(paths: &Paths) -> GuardsBaseline {
    let Ok(section) = guards_section(paths) else { return GuardsBaseline::Changed };
    match read_baseline(paths).and_then(|b| b.get("sha256").and_then(|h| h.as_str()).map(str::to_string)) {
        Some(h) if h == sha256_hex(&section) => GuardsBaseline::Accepted,
        Some(_) => GuardsBaseline::Changed,
        None => GuardsBaseline::Missing,
    }
}

/// `agentkit doctor --accept-guards`: the owner's acceptance of the current `[guards]`
/// section. Prints what changed since the last acceptance and records the new hash. The
/// guard-config guard refuses agents running it, and it records nothing unless `confirm` (the
/// caller's interactive check: both ends a terminal, the owner typed `accept`) says yes.
pub fn accept_guards(paths: &Paths, confirm: impl FnOnce(&str) -> bool) -> Result<String, String> {
    let section = guards_section(paths)?;
    let hash = sha256_hex(&section);
    let previous = read_baseline(paths);
    let old_section = previous.as_ref().and_then(|b| b.get("section")).and_then(|s| s.as_str()).unwrap_or("");
    let mut out = String::new();
    if previous.as_ref().and_then(|b| b.get("sha256")).and_then(|h| h.as_str()) == Some(hash.as_str()) {
        out.push_str("[guards] unchanged since it was last accepted.\n");
    } else {
        out.push_str(if previous.is_none() { "No accepted baseline yet. [guards] is:\n" } else { "[guards] changed since it was last accepted:\n" });
        let old: Vec<&str> = old_section.lines().collect();
        let new: Vec<&str> = section.lines().collect();
        for line in &old {
            if !new.contains(line) {
                out.push_str(&format!("- {line}\n"));
            }
        }
        for line in &new {
            if !old.contains(line) {
                out.push_str(&format!("+ {line}\n"));
            }
        }
    }
    if !confirm(&out) {
        return Err("not accepted; nothing recorded".into());
    }
    fs::create_dir_all(&paths.state).map_err(|e| e.to_string())?;
    let record = json!({"sha256": hash, "section": section, "accepted": now()});
    let mut text = serde_json::to_string_pretty(&record).map_err(|e| e.to_string())?;
    text.push('\n');
    fs::write(paths.state.join(GUARDS_BASELINE_FILE), text).map_err(|e| e.to_string())?;
    out.push_str(&format!("Recorded {} in {}.\n", &hash[..12], paths.state.join(GUARDS_BASELINE_FILE).display()));
    Ok(out)
}
