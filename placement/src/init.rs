//! Starting policy for `placement init`. The evaluator does not know any of these names; init
//! writes them into site config (`~/.config/fleet/placement.toml`), which is not part of the repo.
//!
//! Nothing here names a real machine or disk. Host ids and volumes are read from the drive
//! registry file init is given; where the registry says nothing, the neutral names of
//! `docs/examples/placement.toml` are used (`primary`, `hub`, `shared`; `media`, `vault-a`,
//! `vault-b`) and the owner edits the result.
//!
//! What the preset decides:
//! - Travelling primary: floor 150 GB, free-space band 200–400 GB, keeps the working set.
//! - Home hub: internal floor 40 GB. Bulk masters live on its volumes.
//! - Shared laptop: floor 30 GB, no bulk unless a pin or an open in the last 30 days.
//! - An encrypted volume accepts personal data; an unencrypted one accepts re-obtainable data
//!   only, so a volume the registry marks unencrypted never becomes a personal replica.
//!   The first of each kind, in registry order, is the master.
//! - Games: Saves sync everywhere. Other children are installs, kept on the primary until use
//!   is known, and on the shared laptop only after an open. Archives: master on the hub volume.
//! - A dataset class of `work` or `personal` becomes personal-unique. Unknown classes do too,
//!   so a typo cannot make unique files eligible for eviction.
//! - Replica volume names are targets. They are not verified copies.
//!
//! Optional keys in the drive registry:
//! ```json
//! { "placement": { "primary": "laptop", "hub": "server", "shared": "family", "offsite": "backup-service" },
//!   "drives": [ { "volumes": { "name": { "encrypted": true, "host": "server", "subpath": "Media",
//!                                        "accepts": ["re-obtainable"] } } } ] }
//! ```

use crate::model::{
    Bulk, Class, Dataset, Host, Part, Policy, Role, SyncFolder, SyncMethod, Volume,
};
use serde_json::Value;

/// The ids init writes. The defaults are the example names; a registry `placement` object
/// replaces any of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitNames {
    pub primary: String,
    pub hub: String,
    pub shared: String,
    /// Label of the off-site copy recorded for personal data.
    pub offsite: String,
}

impl Default for InitNames {
    fn default() -> Self {
        Self {
            primary: "primary".into(),
            hub: "hub".into(),
            shared: "shared".into(),
            offsite: "offsite-backup".into(),
        }
    }
}

impl InitNames {
    fn from_registry(drives: &Value) -> Self {
        let mut names = Self::default();
        let Some(site) = drives.get("placement").and_then(|v| v.as_object()) else {
            return names;
        };
        let pick = |key: &str, into: &mut String| {
            if let Some(value) = site.get(key).and_then(|v| v.as_str()).map(str::trim)
                && !value.is_empty()
            {
                *into = value.to_string();
            }
        };
        pick("primary", &mut names.primary);
        pick("hub", &mut names.hub);
        pick("shared", &mut names.shared);
        pick("offsite", &mut names.offsite);
        names
    }
}

pub fn fleet_policy(drives_json: &str, datasets_json: &str) -> Result<Policy, String> {
    let drives: Value = serde_json::from_str(drives_json).map_err(|e| e.to_string())?;
    let datasets: Value = serde_json::from_str(datasets_json).map_err(|e| e.to_string())?;
    let names = InitNames::from_registry(&drives);

    let mut volumes = registry_volumes(&drives, &names);
    if volumes.is_empty() {
        volumes = example_volumes(&names);
    }
    let replica_volumes: Vec<String> = volumes
        .iter()
        .filter(|v| v.encrypted && v.accepts.contains(&Class::PersonalUnique))
        .map(|v| v.name.clone())
        .collect();
    let personal_master = replica_volumes.first().cloned().unwrap_or_default();
    let bulk_master = volumes
        .iter()
        .find(|v| v.accepts.contains(&Class::Reobtainable))
        .map(|v| v.name.clone())
        .unwrap_or_default();

    let mut policy_datasets = Vec::new();
    for ds in datasets
        .get("datasets")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
    {
        let name = ds.get("name").and_then(|v| v.as_str()).unwrap_or("").trim();
        if name.is_empty() {
            continue;
        }
        let class = map_class(ds.get("class").and_then(|v| v.as_str()).unwrap_or(""));
        let paths: Vec<String> = ds
            .get("paths")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|p| p.as_str().map(|s| s.to_string()))
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        let icloud = paths.iter().any(|p| {
            let p = p.trim_start_matches("~/");
            p == "Desktop"
                || p == "Documents"
                || p.starts_with("Desktop/")
                || p.starts_with("Documents/")
        });
        let (master_volume, replicas, offsite, keep) = match class {
            Class::PersonalUnique => (
                personal_master.clone(),
                replica_volumes.clone(),
                names.offsite.clone(),
                vec![names.primary.clone()],
            ),
            Class::Reobtainable => (
                bulk_master.clone(),
                Vec::new(),
                String::new(),
                vec![names.primary.clone()],
            ),
            Class::SyncedSmall => (String::new(), Vec::new(), String::new(), vec!["*".into()]),
            Class::Scratch => (String::new(), Vec::new(), String::new(), Vec::new()),
        };
        policy_datasets.push(Dataset {
            name: name.into(),
            class,
            paths,
            sync: if class == Class::SyncedSmall {
                SyncMethod::Syncthing
            } else {
                SyncMethod::None
            },
            master_host: if master_volume.is_empty() {
                String::new()
            } else {
                volume_host(&volumes, &master_volume, &names)
            },
            master_volume,
            keep,
            pins: Vec::new(),
            played_within_days: None,
            icloud,
            offsite,
            replica_volumes: replicas,
        });
    }

    Ok(Policy {
        hosts: vec![
            Host {
                id: names.primary.clone(),
                role: Role::TravellingPrimary,
                floor_gb: 150.0,
                band_low_gb: Some(200.0),
                band_high_gb: Some(400.0),
                bulk: Bulk::WorkingSet,
            },
            Host {
                id: names.hub.clone(),
                role: Role::HomeHub,
                floor_gb: 40.0,
                band_low_gb: None,
                band_high_gb: None,
                bulk: Bulk::MastersOnExternal,
            },
            Host {
                id: names.shared.clone(),
                role: Role::SharedLaptop,
                floor_gb: 30.0,
                band_low_gb: None,
                band_high_gb: None,
                bulk: Bulk::NoBulk,
            },
        ],
        sync_folders: vec![games_folder(&names, &volumes, &bulk_master)],
        volumes,
        datasets: policy_datasets,
        pins: Vec::new(),
    })
}

fn volume_host(volumes: &[Volume], name: &str, names: &InitNames) -> String {
    volumes
        .iter()
        .find(|v| v.name == name)
        .map(|v| v.host.clone())
        .unwrap_or_else(|| names.hub.clone())
}

/// Every volume the registry lists, in its order. Encrypted volumes take personal data,
/// the others re-obtainable data, unless the entry says otherwise with `accepts`.
fn registry_volumes(drives: &Value, names: &InitNames) -> Vec<Volume> {
    let mut out: Vec<Volume> = Vec::new();
    for drive in drives.get("drives").and_then(|v| v.as_array()).into_iter().flatten() {
        let Some(volumes) = drive.get("volumes").and_then(|v| v.as_object()) else {
            continue;
        };
        for (name, entry) in volumes {
            let name = name.trim();
            if name.is_empty() || out.iter().any(|v| v.name == name) {
                continue;
            }
            let encrypted = entry.get("encrypted").and_then(|v| v.as_bool()).unwrap_or(false);
            let listed: Vec<Class> = entry
                .get("accepts")
                .and_then(|v| v.as_array())
                .into_iter()
                .flatten()
                .filter_map(|c| c.as_str())
                .map(map_class)
                // personal data never goes to a volume the registry does not call encrypted
                .filter(|c| encrypted || *c != Class::PersonalUnique)
                .collect();
            let accepts = if !listed.is_empty() {
                listed
            } else if encrypted {
                vec![Class::PersonalUnique]
            } else {
                vec![Class::Reobtainable]
            };
            let text = |key: &str| entry.get(key).and_then(|v| v.as_str()).map(str::trim).unwrap_or("");
            out.push(Volume {
                name: name.to_string(),
                host: if text("host").is_empty() { names.hub.clone() } else { text("host").to_string() },
                encrypted,
                accepts,
                subpath: text("subpath").to_string(),
            });
        }
    }
    out
}

/// The volumes of `docs/examples/placement.toml`, for a registry that lists none.
fn example_volumes(names: &InitNames) -> Vec<Volume> {
    let volume = |name: &str, encrypted: bool, class: Class| Volume {
        name: name.into(),
        host: names.hub.clone(),
        encrypted,
        accepts: vec![class],
        subpath: String::new(),
    };
    vec![
        volume("media", false, Class::Reobtainable),
        volume("vault-a", true, Class::PersonalUnique),
        volume("vault-b", true, Class::PersonalUnique),
    ]
}

fn games_folder(names: &InitNames, volumes: &[Volume], bulk_master: &str) -> SyncFolder {
    let master_host = if bulk_master.is_empty() {
        String::new()
    } else {
        volume_host(volumes, bulk_master, names)
    };
    SyncFolder {
        name: "games".into(),
        root: "Games".into(),
        method: SyncMethod::Syncthing,
        played_within_days: 30,
        parts: vec![
            Part {
                path: "Saves".into(),
                class: Class::SyncedSmall,
                keep: vec!["*".into()],
                master_host: String::new(),
                master_volume: String::new(),
                pins: Vec::new(),
                archive: false,
            },
            Part {
                path: "Archives".into(),
                class: Class::Reobtainable,
                keep: Vec::new(),
                master_host: master_host.clone(),
                master_volume: bulk_master.to_string(),
                pins: Vec::new(),
                archive: true,
            },
            Part {
                path: "*".into(),
                class: Class::Reobtainable,
                keep: vec![names.primary.clone()],
                master_host,
                master_volume: bulk_master.to_string(),
                pins: Vec::new(),
                archive: false,
            },
        ],
    }
}

fn map_class(class: &str) -> Class {
    match class.trim() {
        "re-obtainable" | "reobtainable" => Class::Reobtainable,
        "scratch" => Class::Scratch,
        "synced-small" => Class::SyncedSmall,
        // personal, work, empty, and anything unrecognised stay personal-unique.
        _ => Class::PersonalUnique,
    }
}

/// `~/.config/diskmap/config.json` maps a local host name to the id used in the policy.
pub fn host_id_from_diskmap(config_text: &str, local_host_name: &str) -> Option<String> {
    let value: Value = serde_json::from_str(config_text).ok()?;
    let id = value
        .get("hosts")?
        .get(local_host_name)?
        .get("id")?
        .as_str()?;
    if id.is_empty() {
        None
    } else {
        Some(id.to_string())
    }
}
