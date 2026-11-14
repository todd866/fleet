//! One context block for every provider's session-start and prompt hook.

use serde_json::Value;
use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

pub struct Snapshot {
    pub now: String,
    pub capacity: String,
    pub mail: String,
    pub fleet: String,
}

pub fn render_default() -> String {
    render(&collect())
}

pub fn render(s: &Snapshot) -> String {
    format!(
        "* {:<8} {}\n* {:<8} {}\n* {:<8} {}\n* {:<8} {}\n",
        "time", s.now, "capacity", s.capacity, "mail", s.mail, "fleet", s.fleet
    )
}

fn collect() -> Snapshot {
    Snapshot {
        now: command_out("date", &["+%a %d %b %H:%M %Z"]).unwrap_or_else(|| "unknown".into()),
        capacity: capacity_line(),
        mail: mail_line(),
        fleet: fleet_line(),
    }
}

fn home() -> PathBuf {
    env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
}

fn command_out(bin: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(bin).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if text.is_empty() { None } else { Some(text) }
}

fn capacity_line() -> String {
    let bin = env::var_os("AGENTKIT_AI_CAPACITY")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".local/bin/ai-capacity"));
    if !bin.is_file() {
        return "unavailable".into();
    }
    let out = Command::new(&bin).arg("--json").output();
    let Ok(out) = out else { return "unavailable".into() };
    if !out.status.success() {
        return "unavailable".into();
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let Ok(v) = serde_json::from_str::<Value>(&text) else {
        return "unavailable".into();
    };
    let services = v.get("services").cloned().unwrap_or(v);
    let mut bits = Vec::new();
    if let Some(s) = pool_bits("codex", services.get("codex")) {
        bits.push(s);
    }
    if let Some(s) = pool_bits("claude", services.get("claude")) {
        bits.push(s);
    }
    if let Some(s) = pool_bits("cursor", services.get("cursor")) {
        bits.push(s);
    }
    if bits.is_empty() { "unavailable".into() } else { bits.join("  ") }
}

fn pool_bits(name: &str, node: Option<&Value>) -> Option<String> {
    let pools = node?.get("pools")?.as_array()?;
    let mut parts = Vec::new();
    for pool in pools {
        let id = pool.get("pool").and_then(|p| p.as_str()).unwrap_or("");
        let fresh = pool.get("freshness").and_then(|p| p.as_str()).unwrap_or("unknown");
        let short = id.rsplit(':').next().unwrap_or(id);
        if fresh != "fresh" {
            parts.push(format!("{short} ?"));
            continue;
        }
        if let Some(jobs) = pool.pointer("/estimate/jobsLeft").and_then(|n| n.as_i64()).or_else(|| {
            pool.pointer("/estimate/jobsLeft").and_then(|n| n.as_f64()).map(|f| f as i64)
        }) {
            parts.push(format!("{short} {jobs} jobs"));
            continue;
        }
        if let Some(pct) = pool.get("remainingPct").and_then(|n| n.as_f64()) {
            parts.push(format!("{short} {pct:.0}%"));
        } else {
            parts.push(format!("{short} ?"));
        }
    }
    if parts.is_empty() { None } else { Some(format!("{name} {}", parts.join(" "))) }
}

fn mail_line() -> String {
    let bin = env::var_os("AGENTKIT_AGENT_MAIL")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".local/bin/agent-mail"));
    if !bin.is_file() {
        return "unavailable".into();
    }
    let out = Command::new(&bin).arg("unread").output();
    let Ok(out) = out else { return "unavailable".into() };
    if !out.status.success() && out.stdout.is_empty() {
        return "unavailable".into();
    }
    let n = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .count();
    format!("{n} unread")
}

fn fleet_line() -> String {
    let path = env::var_os("AGENTKIT_FLEET_HEALTH")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".local/state/fleet/health.json"));
    let Ok(text) = fs::read_to_string(path) else {
        return "no report".into();
    };
    let Ok(v) = serde_json::from_str::<Value>(&text) else {
        return "no report".into();
    };
    let n = v.get("issues").and_then(|i| i.as_array()).map(|a| a.len()).unwrap_or(0);
    format!("{n} alerts")
}
