//! Read diskmap's summary and where documents. Never measure the disk itself.

use crate::model::{Access, Free, Observation, Present, Replica};
use serde::Deserialize;
use std::fs;
use std::path::Path;

pub fn load_observation(diskmap_dir: &Path, replicas: Option<&Path>, now: i64) -> Observation {
    let mut obs = Observation {
        now,
        diskmap_present: false,
        ..Observation::default()
    };
    let summary = diskmap_dir.join("summary.json");
    if let Ok(text) = fs::read_to_string(&summary) {
        if let Ok(file) = serde_json::from_str::<SummaryFile>(&text) {
            obs.diskmap_present = true;
            for loc in file.locations {
                let kind = loc.kind.as_deref().unwrap_or("volume");
                if kind != "volume" {
                    continue;
                }
                let name = loc.name.as_deref().unwrap_or("");
                let drive = loc.physical_drive.as_deref().unwrap_or("");
                if name != "internal" && drive != "internal" {
                    continue;
                }
                if let (Some(host), Some(free)) = (loc.host, loc.free_bytes) {
                    obs.frees.push(Free {
                        host,
                        free_bytes: free,
                    });
                }
            }
        }
    }
    let where_file = diskmap_dir.join("where.json");
    if let Ok(text) = fs::read_to_string(&where_file) {
        if let Ok(file) = serde_json::from_str::<WhereFile>(&text) {
            for row in file.matches {
                let path = row
                    .home_path
                    .clone()
                    .filter(|p| !p.is_empty())
                    .or(row.path.clone())
                    .unwrap_or_default();
                if path.is_empty() {
                    continue;
                }
                obs.present.push(Present {
                    host: row.host.clone(),
                    location: row.location.unwrap_or_else(|| "internal".into()),
                    path: path.clone(),
                    bytes: row.bytes.unwrap_or(0),
                });
                if let Some(when) = row.last_access.or(row.newest_mtime) {
                    obs.access.push(Access {
                        host: row.host,
                        path,
                        last_access: when,
                    });
                }
            }
        }
    }
    let access = diskmap_dir.join("access.json");
    if let Ok(text) = fs::read_to_string(&access) {
        if let Ok(file) = serde_json::from_str::<AccessFile>(&text) {
            for row in file.rows {
                obs.access.push(Access {
                    host: row.host,
                    path: row.path,
                    last_access: row.last_access,
                });
            }
        }
    }
    if let Some(path) = replicas {
        if let Ok(text) = fs::read_to_string(path) {
            if let Ok(file) = serde_json::from_str::<ReplicaFile>(&text) {
                obs.replicas
                    .extend(file.copies.into_iter().map(|c| Replica {
                        path: c.path,
                        volume: c.volume,
                        encrypted: c.encrypted,
                        verified: c.verified,
                        offsite: c.offsite,
                        bytes: c.bytes,
                    }));
            }
        }
    }
    obs
}

#[derive(Deserialize)]
struct SummaryFile {
    #[serde(default)]
    locations: Vec<SummaryLoc>,
}

#[derive(Deserialize)]
struct SummaryLoc {
    kind: Option<String>,
    host: Option<String>,
    name: Option<String>,
    physical_drive: Option<String>,
    free_bytes: Option<u64>,
}

#[derive(Deserialize)]
struct WhereFile {
    #[serde(default)]
    matches: Vec<WhereMatch>,
}

#[derive(Deserialize)]
struct WhereMatch {
    host: String,
    location: Option<String>,
    path: Option<String>,
    home_path: Option<String>,
    bytes: Option<u64>,
    last_access: Option<i64>,
    newest_mtime: Option<i64>,
}

#[derive(Deserialize)]
struct AccessFile {
    #[serde(default)]
    rows: Vec<AccessRow>,
}

#[derive(Deserialize)]
struct AccessRow {
    host: String,
    path: String,
    last_access: i64,
}

#[derive(Deserialize)]
struct ReplicaFile {
    #[serde(default)]
    copies: Vec<ReplicaRow>,
}

#[derive(Deserialize)]
struct ReplicaRow {
    path: String,
    #[serde(default)]
    volume: String,
    #[serde(default)]
    encrypted: bool,
    #[serde(default)]
    verified: bool,
    #[serde(default)]
    offsite: bool,
    #[serde(default)]
    bytes: u64,
}
