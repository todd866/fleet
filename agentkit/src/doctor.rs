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
        Some(p) if matches!(p, "claude" | "codex" | "cursor") => vec![p],
        Some(p) => return Err(format!("unknown provider {p} (claude, codex, cursor)")),
        None => vec!["claude", "codex", "cursor"],
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
            if text.contains("agent-context") {
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
