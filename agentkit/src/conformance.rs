//! Per-provider guard conformance: installed, trusted, firing.

use crate::guard::{self, Decision};
use crate::generate::Planned;
use crate::site::{self, GuardConfig, Site};
use crate::Paths;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

pub const GUARD_NAMES: &[&str] = crate::guard::ALL_GUARD_NAMES;

#[derive(Debug, Clone)]
pub struct Cell {
    pub guard: String,
    pub installed: bool,
    pub trusted: bool,
    pub firing: bool,
    pub disabled: bool,
    pub detail: String,
}

pub fn run(paths: &Paths, planned: &[Planned], cfg: &GuardConfig, site: &Site) -> Vec<(String, Vec<Cell>)> {
    let mut out = Vec::new();
    for provider in ["claude", "codex", "cursor"] {
        let installed_map = installed_guards(planned, provider, site);
        let trusted_map = trusted_guards(paths, provider, &installed_map, site);
        let mut rows = Vec::new();
        for name in GUARD_NAMES {
            let site_disabled = !site::guard_enabled(name, &site.guards.disabled);
            if site_disabled {
                rows.push(Cell {
                    guard: (*name).into(),
                    installed: false,
                    trusted: false,
                    firing: false,
                    disabled: true,
                    detail: "disabled (site)".into(),
                });
                continue;
            }
            let installed = *installed_map.get(*name).unwrap_or(&false);
            let trusted = if provider == "codex" {
                *trusted_map.get(*name).unwrap_or(&!installed)
            } else {
                installed
            };
            let firing = if installed { firing_guard(name, cfg, &paths.home, site) } else { false };
            let mut detail = String::new();
            if !installed {
                detail = "not wired in hooks".into();
            } else if provider == "codex" && !trusted {
                detail = "Codex /hooks trust missing for this hook".into();
            } else if !firing {
                detail = "decision check failed".into();
            }
            rows.push(Cell {
                guard: (*name).into(),
                installed,
                trusted,
                firing,
                disabled: false,
                detail,
            });
        }
        out.push((provider.into(), rows));
    }
    out
}

fn installed_guards(planned: &[Planned], provider: &str, site: &Site) -> HashMap<String, bool> {
    let mut map: HashMap<String, bool> = GUARD_NAMES.iter().map(|g| (g.to_string(), false)).collect();
    for file in planned.iter().filter(|f| f.kind == "hooks" && f.providers.contains(&provider)) {
        let disk = fs::read_to_string(&file.path).unwrap_or_default();
        let text = if disk == file.contents { disk } else { file.contents.clone() };
        let all_needle = format!("hook {provider} all");
        if text.contains(&all_needle) {
            for name in GUARD_NAMES {
                if site::guard_enabled(name, &site.guards.disabled) {
                    map.insert((*name).into(), true);
                }
            }
            continue;
        }
        for name in GUARD_NAMES {
            let needle = format!("hook {provider} {name}");
            if text.contains(&needle) {
                map.insert((*name).into(), true);
            }
        }
    }
    map
}

fn trusted_guards(
    paths: &Paths,
    provider: &str,
    installed: &HashMap<String, bool>,
    site: &Site,
) -> HashMap<String, bool> {
    let mut map: HashMap<String, bool> = GUARD_NAMES.iter().map(|g| (g.to_string(), true)).collect();
    if provider != "codex" {
        return map;
    }
    let hooks_path = paths.home.join(".codex/hooks.json");
    let on_disk = fs::read_to_string(&hooks_path)
        .map(|t| t.contains("agentkit hook codex"))
        .unwrap_or(false);
    if !on_disk {
        return map;
    }
    let config_path = paths.home.join(".codex/config.toml");
    let hooks_text = fs::read_to_string(&hooks_path).unwrap_or_default();
    let config_text = fs::read_to_string(&config_path).unwrap_or_default();
    let Ok(hooks) = serde_json::from_str::<Value>(&hooks_text) else {
        for name in GUARD_NAMES {
            if *installed.get(*name).unwrap_or(&false) {
                map.insert((*name).into(), false);
            }
        }
        return map;
    };
  let mut by_guard: HashMap<String, bool> = HashMap::new();
    if let Some(pre) = hooks.get("hooks").and_then(|h| h.get("PreToolUse")).and_then(|p| p.as_array()) {
        for (_gi, group) in pre.iter().enumerate() {
            if let Some(list) = group.get("hooks").and_then(|h| h.as_array()) {
                for (hi, hook) in list.iter().enumerate() {
                    let cmd = hook.get("command").and_then(|c| c.as_str()).unwrap_or("");
                    if !cmd.contains("agentkit hook codex") {
                        continue;
                    }
                    let key = format!("pre_tool_use:{hi}:0");
                    let trusted = config_text.contains(&key) && config_text.lines().any(|l| {
                        l.contains("trusted_hash")
                    });
                    if cmd.contains("hook codex all") {
                        for name in GUARD_NAMES {
                            if site::guard_enabled(name, &site.guards.disabled) {
                                by_guard.insert((*name).into(), trusted);
                            }
                        }
                    }
                    for name in GUARD_NAMES {
                        if cmd.contains(&format!("hook codex {name}")) {
                            by_guard.insert((*name).into(), trusted);
                        }
                    }
                }
            }
        }
    }
    for name in GUARD_NAMES {
        if !*installed.get(*name).unwrap_or(&false) {
            continue;
        }
        map.insert(
            (*name).into(),
            *by_guard.get(*name).unwrap_or(&config_text.contains("trusted_hash")),
        );
    }
    map
}

fn firing_guard(guard_name: &str, cfg: &GuardConfig, session_home: &Path, site: &Site) -> bool {
    let mut cfg = cfg.clone();
    if guard_name == "metered-link" {
        cfg.home = session_home.to_path_buf();
    } else {
        cfg.home = PathBuf::from("/Users/agentkit-conformance");
    }
    cfg.cwd = cfg.home.join("Projects/example");
    cfg.allowed_cd = crate::site::default_cd_roots(&cfg.home);
    cfg.offload_worker = Some("worker".into());
    let marker = cfg.home.join(".local/state/METERED_LINK");
    let _metered_backup = marker.exists();
    if guard_name == "metered-link" {
        if let Some(parent) = marker.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let _ = fs::write(&marker, "agentkit-conformance\n");
    }
    if !site.guards.push_gate_remotes.is_empty() {
        cfg.push_gate_remotes = site.guards.push_gate_remotes.clone();
    }
    if !site.guards.push_gate_projects.is_empty() {
        cfg.push_gate_projects = site
            .guards
            .push_gate_projects
            .iter()
            .map(|p| site::expand_home(p, &cfg.home))
            .collect();
    }
    let ok = fixture_cases(guard_name)
        .iter()
        .all(|(raw, expect)| {
            let decision = guard::decide_json(guard_name, raw, &cfg).unwrap_or_else(|_| guard::Decision::allow());
            matches_expect(&decision, expect.as_str())
        });
    if guard_name == "metered-link" && marker.exists() {
        let _ = fs::remove_file(&marker);
    }
    ok
}

fn matches_expect(d: &Decision, expect: &str) -> bool {
    match expect {
        "deny" => d.is_deny(),
        "ask" => d.is_ask(),
        "allow" => d.is_allow(),
        _ => false,
    }
}

fn fixture_cases(name: &str) -> Vec<(String, String)> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("tests/fixtures/guards/{name}.json"));
    let Ok(text) = fs::read_to_string(&path) else {
        return legacy_cases(name);
    };
    let Ok(v) = serde_json::from_str::<Value>(&text) else {
        return legacy_cases(name);
    };
    let mut out = Vec::new();
    if let Some(arr) = v.get("cases").and_then(|c| c.as_array()) {
        for case in arr {
            let raw = case.get("input").map(|i| i.to_string()).unwrap_or_default();
            let expect = case
                .get("expect")
                .and_then(|e| e.as_str())
                .unwrap_or("allow")
                .to_string();
            out.push((raw, expect));
        }
    }
    if out.is_empty() { legacy_cases(name) } else { out }
}

fn legacy_cases(name: &str) -> Vec<(String, String)> {
    match name {
        "hard-delete" => vec![
            (r#"{"tool_name":"Bash","tool_input":{"command":"rm -rf ~/Documents/x"}}"#.into(), "deny".into()),
            (r#"{"tool_name":"Bash","tool_input":{"command":"rm -rf /tmp/agentkit-ok"}}"#.into(), "allow".into()),
            (r#"{"tool_name":"Bash","tool_input":{"command":"ssh m1 'rm -rf ~/x'"}}"#.into(), "deny".into()),
        ],
        "icloud" => vec![
            (r#"{"tool_name":"Bash","tool_input":{"command":"trash ~/Documents"}}"#.into(), "deny".into()),
            (r#"{"tool_name":"Bash","tool_input":{"command":"ICLOUD_WIPE_APPROVED=1 trash ~/Documents"}}"#.into(), "allow".into()),
            (r#"{"tool_name":"Bash","tool_input":{"command":"trash ~/Desktop/one.png"}}"#.into(), "allow".into()),
        ],
        "offload-heavy" => vec![
            (r#"{"tool_name":"Bash","tool_input":{"command":"cargo test"}}"#.into(), "deny".into()),
            (r#"{"tool_name":"Bash","tool_input":{"command":"./tests.sh"}}"#.into(), "deny".into()),
            (r#"{"tool_name":"Bash","tool_input":{"command":"remote-run -- cargo test"}}"#.into(), "allow".into()),
        ],
        "cd-outside" => vec![
            (r#"{"tool_name":"Bash","tool_input":{"command":"cd /etc && ls"}}"#.into(), "deny".into()),
            (r#"{"tool_name":"Bash","tool_input":{"command":"cd ~/Projects"}}"#.into(), "allow".into()),
        ],
        "push" => vec![
            (r#"{"tool_name":"Bash","tool_input":{"command":"git push origin main"}}"#.into(), "allow".into()),
            (r#"{"tool_name":"Bash","tool_input":{"command":"git push --force origin main"}}"#.into(), "deny".into()),
            (r#"{"tool_name":"Bash","tool_input":{"command":"git push --force-with-lease origin main"}}"#.into(), "ask".into()),
            (r#"{"tool_name":"Bash","tool_input":{"command":"git status"}}"#.into(), "allow".into()),
        ],
        "disk-slot" => vec![
            (r#"{"tool_name":"Bash","tool_input":{"command":"rsync -a ~/x/ /Volumes/Archive/x/"}}"#.into(), "deny".into()),
            (r#"{"tool_name":"Bash","tool_input":{"command":"disk-slot /Volumes/Archive -- rsync -a ~/x/ /Volumes/Archive/x/"}}"#.into(), "allow".into()),
        ],
        "metered-link" => vec![
            (r#"{"tool_name":"Bash","tool_input":{"command":"rsync -avh big/ remote:/big/"}}"#.into(), "deny".into()),
            (r#"{"tool_name":"Bash","tool_input":{"command":"echo ok"}}"#.into(), "allow".into()),
        ],
        "agent-yolo" => vec![
            (r#"{"tool_name":"Bash","tool_input":{"command":"agent -p --force fix"}}"#.into(), "deny".into()),
            (r#"{"tool_name":"Bash","tool_input":{"command":"cursor-worker -- echo hi"}}"#.into(), "allow".into()),
        ],
        "test-audio" => vec![
            (r#"{"tool_name":"Bash","tool_input":{"command":"afplay /tmp/tone.wav"}}"#.into(), "deny".into()),
            (r#"{"tool_name":"Bash","tool_input":{"command":"AUDIBLE_APPROVED=1 afplay /tmp/tone.wav"}}"#.into(), "allow".into()),
            (r#"{"tool_name":"Bash","tool_input":{"command":"echo quiet"}}"#.into(), "allow".into()),
        ],
        _ => Vec::new(),
    }
}

pub fn format_table(rows: &[(String, Vec<Cell>)]) -> String {
    let mut out = String::new();
    out.push_str("conformance (installed / trusted / firing)\n");
    for (provider, cells) in rows {
        for cell in cells {
            let mark = if cell.disabled {
                "-"
            } else if cell.installed && cell.trusted && cell.firing {
                "*"
            } else {
                "!"
            };
            let inst = if cell.installed { "yes" } else { "no" };
            let trust = if cell.trusted { "yes" } else { "no" };
            let fire = if cell.firing { "yes" } else { "no" };
            out.push_str(&format!(
                "{mark} {:<8} {:<14} installed {:<3} trusted {:<3} firing {:<3} {}\n",
                provider, cell.guard, inst, trust, fire, cell.detail
            ));
        }
    }
    out
}

pub fn to_json(rows: &[(String, Vec<Cell>)]) -> Value {
    let mut providers = Vec::new();
    for (id, cells) in rows {
        let mut guards = Vec::new();
        for cell in cells {
            guards.push(json!({
                "name": cell.guard,
                "installed": cell.installed,
                "trusted": cell.trusted,
                "firing": cell.firing,
                "disabled": cell.disabled,
                "detail": cell.detail,
            }));
        }
        providers.push(json!({ "id": id, "guards": guards }));
    }
    json!({ "providers": providers })
}

pub fn conformance_ok(rows: &[(String, Vec<Cell>)]) -> bool {
    rows.iter()
        .flat_map(|(_, cells)| cells.iter())
        .all(|c| c.disabled || (c.installed && c.trusted && c.firing))
}
