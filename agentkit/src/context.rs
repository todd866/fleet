//! One context block for every provider's session-start and prompt hook.

use crate::posture::{self, PostureFile, Tier};
use serde_json::Value;
use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

pub struct Snapshot {
    pub now: String,
    pub capacity: String,
    pub posture: String,
    pub mail: String,
    pub fleet: String,
    pub pointers: String,
}

pub fn render_default() -> String {
    render(&collect())
}

pub fn render(s: &Snapshot) -> String {
    let lines = vec![
        format!("* {:<8} {}", "time", s.now),
        format!("* {:<8} {}", "posture", s.posture),
        format!("* {:<8} {}", "capacity", s.capacity),
        format!("* {:<8} {}", "mail", s.mail),
        format!("* {:<8} {}", "fleet", s.fleet),
        format!("* {:<8} {}", "pointers", s.pointers),
    ];
    format!("{}\n", lines.join("\n"))
}

pub fn calling_provider() -> String {
    let args: Vec<String> = env::args().collect();
    if let Some(p) = args.get(1).filter(|s| !s.starts_with('-')) {
        return p.to_lowercase();
    }
    env::var("AGENTKIT_PROVIDER")
        .unwrap_or_else(|_| "unknown".into())
        .to_lowercase()
}

fn collect() -> Snapshot {
    let provider = calling_provider();
    let posture_file = load_posture_file();
    Snapshot {
        now: command_out("date", &["+%a %d %b %H:%M %Z"]).unwrap_or_else(|| "unknown".into()),
        capacity: capacity_block(),
        posture: posture_line(&provider, posture_file.as_ref()),
        mail: mail_line(),
        fleet: fleet_line(),
        pointers: pointers_line(),
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
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

fn posture_path() -> PathBuf {
    env::var_os("AGENTKIT_POSTURE")
        .map(PathBuf::from)
        .or_else(|| env::var_os("AGENTKIT_CLAUDE_POSTURE").map(PathBuf::from))
        .unwrap_or_else(|| home().join(".local/state/ai-budget/posture.json"))
}

fn load_posture_file() -> Option<PostureFile> {
    posture::read_file(&posture_path())
}

fn fmt_pool_short(key: &str, pool: &posture::PoolPosture) -> String {
    let pct = pool
        .pct
        .map(|p| format!(" {p:.0}%"))
        .unwrap_or_else(|| " ?".into());
    format!("{key} {}{}", pool.tier, pct)
}

fn posture_line(provider: &str, doc: Option<&PostureFile>) -> String {
    let Some(doc) = doc else {
        return "unavailable (run `agentkit posture write`)".into();
    };
    let keys = posture::provider_pool_keys(provider);
    let mut bits = Vec::new();
    for key in keys {
        if let Some(pool) = doc.pools.get(*key) {
            bits.push(fmt_pool_short(key, pool));
        }
    }
    if bits.is_empty() {
        bits.push(format!("{provider} unknown"));
    }
    let binding = binding_tier(doc, keys);
    let route = format!(
        "route: build {}, review {}",
        doc.route.build, doc.route.review
    );
  let handoff = if binding == Tier::Hold && doc.route.build != "unknown" && doc.route.build != provider {
        format!(" | HOLD — hand off to {}", doc.route.build)
    } else if binding == Tier::Hold {
        " | HOLD — one bounded job".into()
    } else {
        String::new()
    };
    format!("{} | {}{handoff}", bits.join(" · "), route)
}

fn binding_tier(doc: &PostureFile, keys: &[&str]) -> Tier {
    let mut worst = Tier::Full;
    let mut seen = false;
    for key in keys {
        let Some(pool) = doc.pools.get(*key) else {
            continue;
        };
        seen = true;
        let t = match pool.tier.as_str() {
            "FULL" => Tier::Full,
            "NORMAL" => Tier::Normal,
            "LEAN" => Tier::Lean,
            "HOLD" => Tier::Hold,
            _ => Tier::Unknown,
        };
        if t.rank() > worst.rank() {
            worst = t;
        }
    }
    if seen { worst } else { Tier::Unknown }
}

fn capacity_json() -> Option<Value> {
    let bin = env::var_os("AGENTKIT_AI_CAPACITY")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".local/bin/ai-capacity"));
    if !bin.is_file() {
        return None;
    }
    let out = Command::new(&bin).arg("--json").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    serde_json::from_str(&text).ok()
}

fn capacity_block() -> String {
    let Some(v) = capacity_json() else {
        return "unavailable".into();
    };
    let services = v.get("services").cloned().unwrap_or(v);
    let mut provider_bits = Vec::new();
    for name in ["codex", "claude", "cursor"] {
        if let Some(bit) = provider_capacity(name, services.get(name)).0 {
            provider_bits.push(bit);
        }
    }
    if provider_bits.is_empty() {
        return "unavailable".into();
    }
    provider_bits.join(" · ")
}

struct PoolSnap {
    label: String,
}

fn provider_capacity(name: &str, node: Option<&Value>) -> (Option<String>, Option<i64>) {
    let Some(pools) = node.and_then(|n| n.get("pools")).and_then(|p| p.as_array()) else {
        return (None, None);
    };
    let mut best: Option<PoolSnap> = None;
    let mut best_score = -1i64;
    for pool in pools {
        let id = pool.get("pool").and_then(|p| p.as_str()).unwrap_or("");
        let fresh = pool.get("freshness").and_then(|p| p.as_str()).unwrap_or("unknown");
        let short = id.rsplit(':').next().unwrap_or(id);
        let detail = if fresh != "fresh" {
            format!("{short} ?")
        } else if let Some(jobs) = pool
            .pointer("/estimate/jobsLeft")
            .and_then(|n| n.as_i64())
            .or_else(|| {
                pool.pointer("/estimate/jobsLeft")
                    .and_then(|n| n.as_f64())
                    .map(|f| f as i64)
            })
        {
            format!("{short} {jobs} jobs")
        } else if let Some(pct) = pool.get("remainingPct").and_then(|n| n.as_f64()) {
            format!("{short} {pct:.0}%")
        } else {
            format!("{short} ?")
        };
        let score = if fresh != "fresh" {
            0
        } else {
            pool.get("remainingPct")
                .and_then(|n| n.as_f64())
                .map(|p| p as i64)
                .or_else(|| {
                    pool.pointer("/estimate/jobsLeft")
                        .and_then(|n| n.as_i64())
                })
                .unwrap_or(0)
        };
        if score > best_score {
            best_score = score;
            best = Some(PoolSnap { label: detail });
        }
    }
    let Some(best) = best else {
        return (None, None);
    };
    (
        Some(format!("{name} {}", best.label)),
        Some(best_score),
    )
}

fn mail_line() -> String {
    let bin = env::var_os("AGENTKIT_AGENT_MAIL")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".local/bin/agent-mail"));
    if !bin.is_file() {
        return "unavailable".into();
    }
    let out = Command::new(&bin).arg("unread").output();
    let Ok(out) = out else {
        return "unavailable".into();
    };
    if !out.status.success() && out.stdout.is_empty() {
        return "unavailable".into();
    }
    let n = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .count();
    format!("{n} unread")
}

fn fleet_board_path() -> PathBuf {
    env::var_os("AGENTKIT_FLEET_BOARD")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".local/state/fleet/morning/board.json"))
}

fn fleet_line() -> String {
    let board = fleet_board_path();
    let path = if board.is_file() {
        board
    } else {
        let alt = home().join(".local/state/fleet/morning/summary.json");
        if alt.is_file() {
            alt
        } else {
            return fleet_health_fallback();
        }
    };
    let Ok(text) = fs::read_to_string(path) else {
        return fleet_health_fallback();
    };
    let Ok(v) = serde_json::from_str::<Value>(&text) else {
        return "no report".into();
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    fleet_summary(&v, now)
}

/// The fleet line: counts, where the failures are, and how old the report is.
///
/// A bare "28 fail 21 warn" on every prompt told an agent nothing it could act on, and nothing
/// about whether the report was this morning's or last week's (14 Nov 2026). Failures are grouped
/// by the board's `area` (job, backup, disk, ...), largest first, at most four named.
fn fleet_summary(v: &Value, now: u64) -> String {
    let fail = v.pointer("/counts/fail").and_then(|n| n.as_u64()).unwrap_or(0);
    let warn = v.pointer("/counts/warn").and_then(|n| n.as_u64()).unwrap_or(0);
    let mut areas: Vec<(String, u64)> = Vec::new();
    if let Some(rows) = v.get("rows").and_then(|r| r.as_array()) {
        for row in rows {
            if row.get("status").and_then(|s| s.as_str()) != Some("fail") {
                continue;
            }
            let area = row.get("area").and_then(|a| a.as_str()).unwrap_or("other").to_string();
            match areas.iter_mut().find(|(a, _)| *a == area) {
                Some((_, n)) => *n += 1,
                None => areas.push((area, 1)),
            }
        }
    }
    areas.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let mut out = format!("{fail} fail");
    if !areas.is_empty() {
        let named: Vec<String> = areas.iter().take(4).map(|(a, n)| format!("{a} {n}")).collect();
        let rest: u64 = areas.iter().skip(4).map(|(_, n)| n).sum();
        out.push_str(&format!(" ({}{})", named.join(", "), if rest > 0 { format!(", +{rest}") } else { String::new() }));
    }
    out.push_str(&format!(" {warn} warn"));
    if let Some(generated) = v.get("generated").and_then(|g| g.as_u64())
        && now > generated
    {
        let hours = (now - generated) / 3600;
        if hours >= 48 {
            out.push_str(&format!(" · report {} d old", hours / 24));
        } else if hours >= 3 {
            out.push_str(&format!(" · report {hours} h old"));
        }
    }
    out
}

fn fleet_health_fallback() -> String {
    let path = env::var_os("AGENTKIT_FLEET_HEALTH")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".local/state/fleet/health.json"));
    let Ok(text) = fs::read_to_string(path) else {
        return "no report".into();
    };
    let Ok(v) = serde_json::from_str::<Value>(&text) else {
        return "no report".into();
    };
    let n = v
        .get("issues")
        .and_then(|i| i.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    format!("{n} alerts (health)")
}

fn pointers_line() -> String {
    let reg = home().join(".local/state/mac-sync/registry/REGISTRY.md");
    let place = home().join(".config/fleet/placement.toml");
    let work = home()
        .join(".local/state/mac-sync/registry/MACOS-WORKAROUNDS.md");
    let mut bits = Vec::new();
    bits.push(if reg.is_file() {
        "registry REGISTRY.md"
    } else {
        "registry missing"
    });
    bits.push(if place.is_file() {
        "placement placement.toml"
    } else {
        "placement missing"
    });
    bits.push(if work.is_file() {
        "workarounds MACOS-WORKAROUNDS.md"
    } else {
        "workarounds missing"
    });
    bits.join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::posture::{PoolPosture, PostureFile, RouteLine};
    use serde_json::json;
    use std::collections::HashMap;

    #[test]
    fn render_fixture_block() {
        let s = Snapshot {
            now: "Mon 10 Nov 17:00 AEDT".into(),
            posture: "cursor:api FULL 56% | route: build cursor, review codex".into(),
            capacity: "codex primary 27% · cursor api 250 jobs".into(),
            mail: "0 unread".into(),
            fleet: "2 fail 1 warn".into(),
            pointers: "registry REGISTRY.md · placement placement.toml · workarounds MACOS-WORKAROUNDS.md"
                .into(),
        };
        let out = render(&s);
        assert!(out.contains("* posture"));
        assert!(out.contains("route: build cursor"));
    }

    #[test]
    fn hold_adds_handoff_for_another_provider() {
        let mut pools = HashMap::new();
        pools.insert(
            "claude:weekly".into(),
            PoolPosture {
                tier: "HOLD".into(),
                pct: Some(5.0),
                resets_at: None,
                stale: false,
            },
        );
        let doc = PostureFile {
            as_of: "t".into(),
            pools,
            route: RouteLine {
                build: "cursor".into(),
                review: "codex".into(),
                why: String::new(),
            },
        };
        let line = posture_line("claude", Some(&doc));
        assert!(line.contains("hand off to cursor"));
    }

    #[test]
    fn stale_pools_get_unknown_posture() {
        let pools = json!({"pools":[{"pool":"primary","freshness":"stale","remainingPct":50.0}]});
        let (bit, score) = provider_capacity("codex", Some(&pools));
        assert!(bit.unwrap().contains('?'));
        assert_eq!(score, Some(0));
    }

    #[test]
    fn fleet_counts_parse_fixture() {
        let v: Value = serde_json::from_str(r#"{"counts":{"fail":3,"warn":2}}"#).unwrap();
        let fail = v.pointer("/counts/fail").and_then(|n| n.as_u64()).unwrap();
        assert_eq!(fail, 3);
        assert_eq!(fleet_summary(&v, 0), "3 fail 2 warn");
    }

    #[test]
    fn fleet_line_says_where_the_failures_are_and_how_old_the_report_is() {
        let v = json!({"counts": {"fail": 7, "warn": 1}, "generated": 1000, "rows": [
            {"status": "fail", "area": "job"}, {"status": "fail", "area": "job"}, {"status": "fail", "area": "job"},
            {"status": "fail", "area": "backup"}, {"status": "fail", "area": "disk"}, {"status": "fail", "area": "thermal"},
            {"status": "fail", "area": "project"}, {"status": "warn", "area": "job"}, {"status": "pass", "area": "disk"}]});
        assert_eq!(fleet_summary(&v, 1000 + 600), "7 fail (job 3, backup 1, disk 1, project 1, +1) 1 warn");
        assert_eq!(fleet_summary(&v, 1000 + 9 * 3600), "7 fail (job 3, backup 1, disk 1, project 1, +1) 1 warn · report 9 h old");
        assert_eq!(fleet_summary(&v, 1000 + 72 * 3600), "7 fail (job 3, backup 1, disk 1, project 1, +1) 1 warn · report 3 d old");
        assert_eq!(fleet_summary(&json!({"ok": true, "counts": {"fail": 0, "warn": 0}}), 5), "0 fail 0 warn");
    }
}
