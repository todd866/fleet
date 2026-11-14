//! Drift check. Exit 0 when the selected provider matches the site.
//! Writes `doctor.json` for fleet-health and mac-registry.

use crate::generate::{self, Planned};
use crate::guard;
use crate::inventory::GAPS;
use crate::site::{GuardConfig, Site};
use crate::Paths;
use serde_json::{json, Value};
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn run(paths: &Paths, provider: Option<&str>) -> Result<(i32, String), String> {
    let site = match Site::load(&paths.config) {
        Ok(s) => s,
        Err(e) => {
            let text = format!("! site       missing    {e}\n");
            write_report(paths, &json!({
                "schema": 1,
                "generated": now(),
                "ok": false,
                "issues": [e],
                "providers": [],
            }))?;
            return Ok((1, text));
        }
    };
    let planned = generate::plan(paths, &site)?;
    let names: Vec<&str> = match provider {
        Some(p) if matches!(p, "claude" | "codex" | "cursor") => vec![p],
        Some(p) => return Err(format!("unknown provider {p} (claude, codex, cursor)")),
        None => vec!["claude", "codex", "cursor"],
    };
    let cfg = guard_cfg(&site);
    let self_test = guard::self_test(&cfg);
    let memory_ok = paths.notebook().join("INDEX.md").is_file();
    let mut providers = Vec::new();
    let mut issues: Vec<String> = Vec::new();
    if let Err(e) = &self_test {
        issues.push(format!("guard self-test: {e}"));
    }
    if !memory_ok {
        issues.push("memory: notebook INDEX.md is not readable".into());
    }
    let mut lines = Vec::new();
    for name in names {
        let row = provider_row(name, &planned, memory_ok, self_test.is_ok());
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
    write_report(paths, &json!({
        "schema": 1,
        "generated": now(),
        "ok": ok,
        "issues": issues,
        "providers": json_providers,
    }))?;
    let mut text = String::new();
    for line in lines {
        text.push_str(&line);
        text.push('\n');
    }
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

fn provider_row(name: &str, planned: &[Planned], memory_ok: bool, self_ok: bool) -> Row {
    let mut rule_bad = Vec::new();
    let mut guard_cmds = 0usize;
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
            for (guard, _, _) in [
                ("hard-delete", 0, false),
                ("icloud", 0, false),
                ("offload-heavy", 0, false),
                ("cd-outside", 0, false),
                ("push", 0, false),
            ] {
                guard_cmds += 1;
                let needle = format!("hook {name} {guard}");
                if disk.contains(&needle) {
                    guard_have += 1;
                }
            }
            if disk.contains("agent-context") {
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

fn guard_cfg(site: &Site) -> GuardConfig {
    let mut cfg = GuardConfig::default();
    cfg.air_hosts = site.guards.air_hosts.clone();
    cfg.air_aliases = site.guards.air_aliases.clone();
    cfg.resolve_ssh = false;
    if !site.guards.push_markers.is_empty() {
        cfg.push_markers = site.guards.push_markers.clone();
    }
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
