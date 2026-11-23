//! Provider-neutral posture from `ai-capacity --json` — same pace rule for every pool.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::Path;
use std::process::Command;

const PACE_HOLD: f64 = 0.5;
const PACE_LEAN: f64 = 0.8;
const PACE_NORMAL: f64 = 0.95;
const RESERVE_PCT: f64 = 15.0;
const PACE_ABSOLUTE_RESERVE: f64 = 2.0;
const WEEK_MIN: f64 = 7.0 * 24.0 * 60.0;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Tier {
    Full,
    Normal,
    Lean,
    Hold,
    #[serde(rename = "unknown")]
    Unknown,
}

impl Tier {
    pub fn as_str(&self) -> &'static str {
        match self {
            Tier::Full => "FULL",
            Tier::Normal => "NORMAL",
            Tier::Lean => "LEAN",
            Tier::Hold => "HOLD",
            Tier::Unknown => "unknown",
        }
    }

    pub fn rank(&self) -> i32 {
        match self {
            Tier::Full => 0,
            Tier::Normal => 1,
            Tier::Lean => 2,
            Tier::Hold => 3,
            Tier::Unknown => 9,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolPosture {
    pub tier: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pct: Option<f64>,
    #[serde(rename = "resetsAt", skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<String>,
    pub stale: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteLine {
    pub build: String,
    pub review: String,
    pub why: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostureFile {
    #[serde(rename = "asOf")]
    pub as_of: String,
    pub pools: HashMap<String, PoolPosture>,
    pub route: RouteLine,
}

pub fn default_posture_path(home: &Path) -> std::path::PathBuf {
    home.join(".local/state/ai-budget/posture.json")
}

fn parse_reset_iso(reset: &str) -> Option<f64> {
    let parsed = chrono_like_parse(reset)?;
    Some(parsed)
}

/// Minimal UTC parser for `resetAt` strings from ai-capacity (RFC3339).
fn chrono_like_parse(s: &str) -> Option<f64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    use std::time::{Duration, UNIX_EPOCH};
    let (date, rest) = s.split_once('T')?;
    let (time, tz) = rest.split_once('Z').map(|(t, _)| (t, "Z")).or_else(|| {
        rest.find('+')
            .or_else(|| rest.rfind('-').filter(|i| *i > 8))
            .map(|i| rest.split_at(i))
    })?;
    let parts: Vec<&str> = date.split('-').collect();
    if parts.len() != 3 {
        return None;
    }
    let y: i64 = parts[0].parse().ok()?;
    let mo: u32 = parts[1].parse().ok()?;
    let d: u32 = parts[2].parse().ok()?;
    let tparts: Vec<&str> = time.split(':').collect();
    if tparts.len() < 2 {
        return None;
    }
    let h: u32 = tparts[0].parse().ok()?;
    let m: u32 = tparts[1].parse().ok()?;
    let (sec, ms) = if tparts.len() > 2 {
        let ssec = tparts[2];
        let (s, frac) = ssec.split_once('.').unwrap_or((ssec, ""));
        let sec: u32 = s.parse().ok()?;
        let ms: u32 = if frac.is_empty() {
            0
        } else {
            let digits: String = frac.chars().take(3).collect();
            let padded = format!("{:0<3}", digits);
            padded.parse().unwrap_or(0)
        };
        (sec, ms)
    } else {
        (0, 0)
    };
    let _tz = tz;
    let days = days_from_civil(y, mo, d)?;
    let secs = days as i64 * 86400 + h as i64 * 3600 + m as i64 * 60 + sec as i64;
    let ms_total = secs * 1000 + ms as i64;
    let st = UNIX_EPOCH + Duration::from_millis(ms_total as u64);
    st.duration_since(UNIX_EPOCH).ok().map(|d| d.as_secs_f64())
}

fn days_from_civil(y: i64, m: u32, d: u32) -> Option<i64> {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy as i64;
    Some(era * 146097 + doe - 719468)
}

fn canonical_pool(service: &str, pool_id: &str) -> Option<String> {
    match (service, pool_id) {
        ("claude", "5h") => Some("claude:5h".into()),
        ("claude", "7d") => Some("claude:weekly".into()),
        ("codex", "codex:primary") | ("codex", "primary") => Some("codex:primary".into()),
        ("cursor", "api") => Some("cursor:api".into()),
        ("cursor", "cursor") => Some("cursor:grok-composer".into()),
        ("zai", "5h") => Some("zai:5h".into()),
        ("zai", "week") => Some("zai:weekly".into()),
        ("gemini", "5h") => Some("gemini:5h".into()),
        ("gemini", "week") => Some("gemini:weekly".into()),
        _ => None,
    }
}

fn window_minutes(pool: &Value) -> f64 {
    pool.get("windowMinutes")
        .and_then(|n| n.as_f64())
        .unwrap_or_else(|| {
            let id = pool.get("pool").and_then(|p| p.as_str()).unwrap_or("");
            if id.contains("5h") {
                5.0 * 60.0
            } else if id.contains("7d") || id.contains("weekly") || id.contains("primary") {
                WEEK_MIN
            } else {
                30.0 * 24.0 * 60.0
            }
        })
}

pub fn tier_from_pace(remaining_pct: f64, mins_to_reset: f64, window_mins: f64) -> Tier {
    if !remaining_pct.is_finite() || !mins_to_reset.is_finite() || mins_to_reset < 0.0 {
        return Tier::Unknown;
    }
    if window_mins <= 0.0 {
        return Tier::Unknown;
    }
    let expected = 100.0 * (mins_to_reset / window_mins).clamp(0.0, 1.0);
    if expected <= 0.0 {
        return Tier::Hold;
    }
    if remaining_pct <= RESERVE_PCT {
        return Tier::Hold;
    }
    if remaining_pct <= PACE_ABSOLUTE_RESERVE && mins_to_reset > 60.0 {
        return Tier::Hold;
    }
    let pace = remaining_pct / expected;
    if pace < PACE_HOLD {
        Tier::Hold
    } else if pace < PACE_LEAN {
        Tier::Lean
    } else if pace < PACE_NORMAL {
        Tier::Normal
    } else {
        Tier::Full
    }
}

fn pool_posture(pool: &Value, now_ms: f64) -> PoolPosture {
    let fresh = pool.get("freshness").and_then(|f| f.as_str()).unwrap_or("unknown");
    let stale_reading = fresh != "fresh";
    let remaining = pool.get("remainingPct").and_then(|n| n.as_f64());
    let reset_at = pool
        .get("resetAt")
        .and_then(|n| n.as_str())
        .map(|s| s.to_string());
    let mins_to_reset = reset_at
        .as_deref()
        .and_then(parse_reset_iso)
        .map(|epoch| (epoch * 1000.0 - now_ms) / 60_000.0);
    let tier = if stale_reading || remaining.is_none() {
        Tier::Unknown
    } else if let (Some(pct), Some(mins)) = (remaining, mins_to_reset) {
        tier_from_pace(pct, mins, window_minutes(pool))
    } else {
        Tier::Unknown
    };
    PoolPosture {
        tier: tier.as_str().into(),
        pct: remaining,
        resets_at: reset_at,
        stale: stale_reading || tier == Tier::Unknown,
    }
}

fn provider_binding_tier(pools: &HashMap<String, PoolPosture>, keys: &[&str]) -> Tier {
    let mut worst = Tier::Full;
    for key in keys {
        let Some(p) = pools.get(*key) else {
            return Tier::Unknown;
        };
        let t = match p.tier.as_str() {
            "FULL" => Tier::Full,
            "NORMAL" => Tier::Normal,
            "LEAN" => Tier::Lean,
            "HOLD" => Tier::Hold,
            _ => Tier::Unknown,
        };
        if t == Tier::Unknown {
            return Tier::Unknown;
        }
        if t.rank() > worst.rank() {
            worst = t;
        }
    }
    worst
}

/// Providers the owner has paused (one name per line in ~/.config/agentkit/paused-providers, `#` comments),
/// e.g. while a plan is downgraded. A paused provider is never chosen to build or review.
pub fn paused_providers() -> Vec<String> {
    let path = std::env::var("AGENTKIT_PAUSED_PROVIDERS").unwrap_or_else(|_| {
        format!("{}/.config/agentkit/paused-providers", std::env::var("HOME").unwrap_or_default())
    });
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|l| l.split('#').next().unwrap_or("").trim().to_lowercase())
        .filter(|l| !l.is_empty())
        .collect()
}

fn compute_route(pools: &HashMap<String, PoolPosture>) -> RouteLine {
    // Z.ai GLM Coding Plan first (owner, 19 Nov 2026: Max yearly), then Gemini AI Ultra
    // (owner, 23 Nov 2026: Claude & Codex paused; Gemini Ultra + Cursor + Z.ai primary),
    // then Cursor (preserving monthly Cursor allowance when Z.ai and Gemini are both FULL).
    let candidates: [(&str, &[&str]); 5] = [
        ("zai", &["zai:5h", "zai:weekly"]),
        ("gemini", &["gemini:5h", "gemini:weekly"]),
        ("cursor", &["cursor:api", "cursor:grok-composer"]),
        ("codex", &["codex:primary"]),
        ("claude", &["claude:5h", "claude:weekly"]),
    ];
    let mut scored: Vec<(String, i32)> = Vec::new();
    let paused = paused_providers();
    for (name, keys) in candidates {
        if paused.iter().any(|p| p == name) {
            continue;
        }
        let tier = provider_binding_tier(pools, keys);
        if tier == Tier::Unknown {
            continue;
        }
        scored.push((name.to_string(), tier.rank()));
    }
    scored.sort_by_key(|(_, r)| *r);
    let build = scored
        .first()
        .map(|(n, _)| n.clone())
        .unwrap_or_else(|| "unknown".into());
    let review = scored
        .iter()
        .find(|(n, _)| n != &build)
        .map(|(n, _)| n.clone())
        .unwrap_or_else(|| build.clone());
    let why = if build == "unknown" {
        "stale or unknown readings — one bounded job".into()
    } else {
        format!("best fresh pool headroom for build; review on {review}")
    };
    RouteLine {
        build,
        review,
        why,
    }
}

pub fn compute_from_capacity_json(capacity: &Value, now_ms: f64) -> PostureFile {
    let services = capacity
        .get("services")
        .cloned()
        .unwrap_or_else(|| capacity.clone());
    let mut pools = HashMap::new();
    for service in ["codex", "claude", "cursor", "zai", "gemini"] {
        let Some(node) = services.get(service) else {
            continue;
        };
        let Some(list) = node.get("pools").and_then(|p| p.as_array()) else {
            continue;
        };
        for pool in list {
            let pool_id = pool.get("pool").and_then(|p| p.as_str()).unwrap_or("");
            let Some(key) = canonical_pool(service, pool_id) else {
                continue;
            };
            pools.insert(key, pool_posture(pool, now_ms));
        }
    }
    let route = compute_route(&pools);
    let as_of = capacity
        .get("generatedAt")
        .and_then(|s| s.as_str())
        .unwrap_or("unknown")
        .to_string();
    PostureFile {
        as_of,
        pools,
        route,
    }
}

pub fn write_atomic(path: &Path, doc: &PostureFile) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string_pretty(doc).map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, text)?;
    fs::rename(tmp, path)?;
    Ok(())
}

pub fn read_file(path: &Path) -> Option<PostureFile> {
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

pub fn refresh_from_ai_capacity(home: &Path, capacity_bin: &Path, out_path: &Path) -> Result<PostureFile, String> {
    if !capacity_bin.is_file() {
        return Err(format!("ai-capacity missing at {}", capacity_bin.display()));
    }
    let out = Command::new(capacity_bin)
        .arg("--json")
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err("ai-capacity --json failed".into());
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let v: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64() * 1000.0)
        .unwrap_or(0.0);
    let doc = compute_from_capacity_json(&v, now_ms);
    write_atomic(out_path, &doc).map_err(|e| e.to_string())?;
    let _ = home;
    Ok(doc)
}

pub fn provider_pool_keys(provider: &str) -> &'static [&'static str] {
    match provider {
        "claude" => &["claude:5h", "claude:weekly"],
        "codex" => &["codex:primary"],
        "cursor" => &["cursor:api", "cursor:grok-composer"],
        "zai" => &["zai:5h", "zai:weekly"],
        "gemini" => &["gemini:5h", "gemini:weekly"],
        _ => &[],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn pace_tiers_match_the_documented_bands() {
        assert_eq!(tier_from_pace(80.0, 3.5 * 24.0 * 60.0, WEEK_MIN), Tier::Full);
        assert_eq!(tier_from_pace(35.0, 3.5 * 24.0 * 60.0, WEEK_MIN), Tier::Lean);
        assert_eq!(tier_from_pace(10.0, 6.0 * 24.0 * 60.0, WEEK_MIN), Tier::Hold);
        assert_eq!(tier_from_pace(10.0, 1.0, WEEK_MIN), Tier::Hold);
    }

    #[test]
    fn fixture_capacity_maps_pools_and_route() {
        let cap = json!({
            "generatedAt": "2026-11-10T07:00:00.000Z",
            "services": {
                "codex": {"pools": [{"pool":"codex:primary","remainingPct":24,"resetAt":"2026-11-16T12:49:54.000Z","freshness":"fresh","windowMinutes":10080}]},
                "claude": {"pools": [
                    {"pool":"5h","remainingPct":45,"resetAt":"2026-11-10T13:00:00.000Z","freshness":"fresh","windowMinutes":300},
                    {"pool":"7d","remainingPct":5,"resetAt":"2026-11-10T13:00:00.000Z","freshness":"fresh","windowMinutes":10080}
                ]},
                "cursor": {"pools": [
                    {"pool":"api","remainingPct":56,"resetAt":"2026-12-03T12:24:31.000Z","freshness":"fresh","windowMinutes":43200},
                    {"pool":"cursor","remainingPct":64,"resetAt":"2026-12-03T12:24:31.000Z","freshness":"fresh","windowMinutes":43200}
                ]}
            }
        });
        let now = parse_reset_iso("2026-11-10T07:00:00.000Z").unwrap() * 1000.0;
        let doc = compute_from_capacity_json(&cap, now);
        assert_eq!(doc.as_of, "2026-11-10T07:00:00.000Z");
        assert!(doc.pools.contains_key("claude:weekly"));
        assert!(doc.pools.contains_key("cursor:api"));
        assert_eq!(doc.pools["claude:weekly"].tier, "HOLD");
        assert!(!doc.route.build.is_empty());
    }

    #[test]
    fn stale_pool_is_unknown_not_spare() {
        let cap = json!({
            "services": {
                "claude": {"pools": [{"pool":"5h","remainingPct":90,"freshness":"stale"}]}
            }
        });
        let doc = compute_from_capacity_json(&cap, 0.0);
        assert_eq!(doc.pools["claude:5h"].tier, "unknown");
        assert!(doc.pools["claude:5h"].stale);
    }

    #[test]
    fn gemini_capacity_maps_5h_and_week_pools() {
        let cap = json!({
            "generatedAt": "2026-11-20T11:00:00.000Z",
            "services": {
                "gemini": {"pools": [
                    {"pool":"5h","remainingPct":80,"resetAt":"2026-11-20T15:00:00.000Z","freshness":"fresh","windowMinutes":300},
                    {"pool":"week","remainingPct":70,"resetAt":"2026-11-26T08:00:00.000Z","freshness":"fresh","windowMinutes":10080}
                ]}
            }
        });
        let now = parse_reset_iso("2026-11-20T11:00:00.000Z").unwrap() * 1000.0;
        let doc = compute_from_capacity_json(&cap, now);
        assert!(doc.pools.contains_key("gemini:5h"), "{:?}", doc.pools.keys());
        assert!(doc.pools.contains_key("gemini:weekly"), "{:?}", doc.pools.keys());
        assert_ne!(doc.pools["gemini:5h"].tier, "unknown");
        assert_ne!(doc.pools["gemini:weekly"].tier, "unknown");
    }

    #[test]
    fn provider_pool_keys_lists_gemini_windows() {
        assert_eq!(
            provider_pool_keys("gemini"),
            &["gemini:5h", "gemini:weekly"][..]
        );
    }

    #[test]
    fn gemini_is_routable_when_it_has_more_headroom_than_zai() {
        let cap = json!({
            "generatedAt": "2026-11-20T11:00:00.000Z",
            "services": {
                "gemini": {"pools": [
                    {"pool":"5h","remainingPct":80,"resetAt":"2026-11-20T15:00:00.000Z","freshness":"fresh","windowMinutes":300},
                    {"pool":"week","remainingPct":70,"resetAt":"2026-11-26T08:00:00.000Z","freshness":"fresh","windowMinutes":10080}
                ]},
                "zai": {"pools": [
                    {"pool":"5h","remainingPct":5,"resetAt":"2026-11-20T15:00:00.000Z","freshness":"fresh","windowMinutes":300},
                    {"pool":"week","remainingPct":5,"resetAt":"2026-11-26T08:00:00.000Z","freshness":"fresh","windowMinutes":10080}
                ]}
            }
        });
        let now = parse_reset_iso("2026-11-20T11:00:00.000Z").unwrap() * 1000.0;
        let doc = compute_from_capacity_json(&cap, now);
        assert_eq!(doc.pools["zai:5h"].tier, "HOLD");
        assert_eq!(doc.route.build, "gemini");
    }
}
