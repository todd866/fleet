//! Incremental, idempotent ship of events and daily history.
//! The caller decides the directory. The CLI only points it at an encrypted volume.

use crate::store::{EventRow, HistRow};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub events_through_id: i64,
    pub history_days: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct EventLine {
    id: i64,
    t: i64,
    path: String,
    delta_bytes: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct HistLine {
    day: String,
    path: String,
    total_bytes: i64,
}

fn vol_dir(archive: &Path, host: &str, volume: &str) -> PathBuf {
    archive.join(host).join(volume)
}

fn read_manifest(dir: &Path) -> Manifest {
    let path = dir.join("manifest.json");
    fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or(Manifest {
            events_through_id: 0,
            history_days: Vec::new(),
        })
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, bytes).map_err(|e| e.to_string())?;
    fs::rename(&tmp, path).map_err(|e| e.to_string())?;
    Ok(())
}

fn read_events(path: &Path) -> BTreeMap<i64, EventLine> {
    let mut map = BTreeMap::new();
    let Ok(text) = fs::read_to_string(path) else {
        return map;
    };
    for line in text.lines() {
        if let Ok(row) = serde_json::from_str::<EventLine>(line) {
            map.insert(row.id, row);
        }
    }
    map
}

fn read_history_file(path: &Path) -> BTreeMap<String, i64> {
    let mut map = BTreeMap::new();
    let Ok(text) = fs::read_to_string(path) else {
        return map;
    };
    for line in text.lines() {
        if let Ok(row) = serde_json::from_str::<HistLine>(line) {
            map.insert(row.path, row.total_bytes);
        }
    }
    map
}

/// Merge rows into the archive. Shipping the same rows again does not duplicate them.
pub fn ship(
    archive: &Path,
    host: &str,
    volume: &str,
    events: &[EventRow],
    history: &[HistRow],
) -> Result<Manifest, String> {
    let dir = vol_dir(archive, host, volume);
    fs::create_dir_all(dir.join("history")).map_err(|e| e.to_string())?;
    let mut existing = read_events(&dir.join("events.jsonl"));
    for event in events {
        existing.insert(
            event.id,
            EventLine {
                id: event.id,
                t: event.t,
                path: event.path.clone(),
                delta_bytes: event.delta_bytes,
            },
        );
    }
    let mut body = String::new();
    let mut through = 0i64;
    for row in existing.values() {
        body.push_str(&serde_json::to_string(row).map_err(|e| e.to_string())?);
        body.push('\n');
        through = through.max(row.id);
    }
    write_atomic(&dir.join("events.jsonl"), body.as_bytes())?;

    let mut days: BTreeMap<String, BTreeMap<String, i64>> = BTreeMap::new();
    for row in history {
        days.entry(row.day.clone())
            .or_default()
            .insert(row.path.clone(), row.total_bytes);
    }
    let mut day_names: Vec<String> = Vec::new();
    for (day, incoming) in &days {
        let path = dir.join("history").join(format!("{day}.jsonl"));
        let mut merged = read_history_file(&path);
        for (p, bytes) in incoming {
            merged.insert(p.clone(), *bytes);
        }
        let mut body = String::new();
        for (p, bytes) in &merged {
            let line = HistLine {
                day: day.clone(),
                path: p.clone(),
                total_bytes: *bytes,
            };
            body.push_str(&serde_json::to_string(&line).map_err(|e| e.to_string())?);
            body.push('\n');
        }
        write_atomic(&path, body.as_bytes())?;
        day_names.push(day.clone());
    }
    let mut manifest = read_manifest(&dir);
    manifest.events_through_id = manifest.events_through_id.max(through);
    for day in day_names {
        if !manifest.history_days.iter().any(|d| d == &day) {
            manifest.history_days.push(day);
        }
    }
    manifest.history_days.sort();
    let text = serde_json::to_string_pretty(&manifest).map_err(|e| e.to_string())?;
    write_atomic(&dir.join("manifest.json"), text.as_bytes())?;
    Ok(manifest)
}

#[allow(dead_code)]
pub fn read_manifest_file(archive: &Path, host: &str, volume: &str) -> Manifest {
    read_manifest(&vol_dir(archive, host, volume))
}

pub fn history_for(archive: &Path, host_filter: Option<&str>, path: &str) -> Vec<HistRow> {
    let Ok(hosts) = fs::read_dir(archive) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for host_ent in hosts.flatten() {
        if !host_ent.path().is_dir() {
            continue;
        }
        let host_name = host_ent.file_name().to_string_lossy().into_owned();
        if let Some(want) = host_filter {
            if want != "all" && want != host_name {
                continue;
            }
        }
        let Ok(vols) = fs::read_dir(host_ent.path()) else {
            continue;
        };
        for vol in vols.flatten() {
            let hist = vol.path().join("history");
            let Ok(days) = fs::read_dir(&hist) else {
                continue;
            };
            for day_ent in days.flatten() {
                let text = fs::read_to_string(day_ent.path()).unwrap_or_default();
                for line in text.lines() {
                    if let Ok(row) = serde_json::from_str::<HistLine>(line) {
                        if row.path == path {
                            out.push(HistRow {
                                day: row.day,
                                path: row.path,
                                total_bytes: row.total_bytes,
                            });
                        }
                    }
                }
            }
        }
    }
    out.sort_by(|a, b| a.day.cmp(&b.day));
    out
}

/// Buffer rows while the archive volume is away. Re-appending the same id is a no-op.
pub fn spool_merge(
    spool: &Path,
    host: &str,
    volume: &str,
    events: &[EventRow],
    history: &[HistRow],
) -> Result<(), String> {
    let dir = vol_dir(spool, host, volume);
    fs::create_dir_all(dir.join("history")).map_err(|e| e.to_string())?;
    let _ = ship(spool, host, volume, events, history)?;
    Ok(())
}

pub fn spool_take(
    spool: &Path,
    host: &str,
    volume: &str,
) -> Result<(Vec<EventRow>, Vec<HistRow>), String> {
    let dir = vol_dir(spool, host, volume);
    let events = read_events(&dir.join("events.jsonl"))
        .into_values()
        .map(|row| EventRow {
            id: row.id,
            t: row.t,
            path: row.path,
            delta_bytes: row.delta_bytes,
        })
        .collect();
    let mut history = Vec::new();
    if let Ok(days) = fs::read_dir(dir.join("history")) {
        for day_ent in days.flatten() {
            let text = fs::read_to_string(day_ent.path()).unwrap_or_default();
            for line in text.lines() {
                if let Ok(row) = serde_json::from_str::<HistLine>(line) {
                    history.push(HistRow {
                        day: row.day,
                        path: row.path,
                        total_bytes: row.total_bytes,
                    });
                }
            }
        }
    }
    Ok((events, history))
}

pub fn spool_clear(spool: &Path, host: &str, volume: &str) -> Result<(), String> {
    let dir = vol_dir(spool, host, volume);
    if dir.exists() {
        fs::remove_dir_all(&dir).map_err(|e| e.to_string())?;
    }
    Ok(())
}
