use serde_json::Value;
use std::fs;
use std::path::Path;

pub fn median_expected_s(jobs_path: &Path, label: &str, now: i64) -> Option<u32> {
    let Ok(content) = fs::read_to_string(jobs_path) else {
        return None;
    };
    let cutoff = now - 30 * 24 * 3600;
    let mut durations: Vec<u32> = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(lbl) = v.get("label").and_then(|x| x.as_str()) else {
            continue;
        };
        if lbl != label {
            continue;
        }
        let start = v.get("start").and_then(|x| x.as_i64()).unwrap_or(0);
        if start < cutoff {
            continue;
        }
        let end = v.get("end").and_then(|x| x.as_i64()).unwrap_or(start);
        let dur = (end - start).max(0) as u32;
        durations.push(dur);
    }
    if durations.is_empty() {
        return None;
    }
    durations.sort_unstable();
    let mid = durations.len() / 2;
    if durations.len() % 2 == 1 {
        Some(durations[mid])
    } else {
        Some((durations[mid - 1] + durations[mid]) / 2)
    }
}
