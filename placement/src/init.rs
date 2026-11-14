//! Fleet preset. The evaluator does not know these names; `placement init` writes them
//! into site config (`~/.config/fleet/placement.toml`), which is not part of the repo.
//!
//! Decisions encoded here, for this household's three Macs:
//! - m5 travelling primary: floor 150 GB, free-space band 200–400 GB, keeps the working set.
//! - m2 home hub: internal floor 40 GB. Re-obtainable masters go to OTV Mirror (the drive that
//!   stays on the hub). Stash/Media is the alternate, not the master: that disk does not
//!   enumerate on the hub's port. Personal masters are Archive_Encrypted2 and Vault-T,
//!   and only while those volumes are encrypted.
//! - m1 shared laptop: floor 30 GB, no bulk unless a pin or an open in the last 30 days.
//! - Games: Saves sync everywhere. Other children are installs, kept on m5 until use is known,
//!   and on m1 only after an open. ROMs are an archive: master on the hub external only.
//! - A dataset class of `work` or `personal` becomes personal-unique. Unknown classes do too,
//!   so a typo cannot make unique files eligible for eviction.
//! - Replica volume names are targets. They are not verified copies.

use crate::model::{
    Bulk, Class, Dataset, Host, Part, Policy, Role, SyncFolder, SyncMethod, Volume,
};
use serde_json::Value;

pub fn fleet_policy(drives_json: &str, datasets_json: &str) -> Result<Policy, String> {
    let drives: Value = serde_json::from_str(drives_json).map_err(|e| e.to_string())?;
    let datasets: Value = serde_json::from_str(datasets_json).map_err(|e| e.to_string())?;

    let personal_volumes = ["Archive_Encrypted2", "Vault-T"];
    let mut replica_volumes = Vec::new();
    for name in personal_volumes {
        if volume_encrypted(&drives, name, true) {
            replica_volumes.push(name.to_string());
        }
    }
    let master = replica_volumes
        .first()
        .cloned()
        .unwrap_or_else(|| "Archive_Encrypted2".into());

    let volumes = vec![
        Volume {
            name: "Archive_Encrypted2".into(),
            host: "m2".into(),
            encrypted: volume_encrypted(&drives, "Archive_Encrypted2", true),
            accepts: vec![Class::PersonalUnique],
            subpath: String::new(),
        },
        Volume {
            name: "Vault-T".into(),
            host: "m2".into(),
            encrypted: volume_encrypted(&drives, "Vault-T", true),
            accepts: vec![Class::PersonalUnique],
            subpath: String::new(),
        },
        Volume {
            name: "OTV Mirror".into(),
            host: "m2".into(),
            encrypted: volume_encrypted(&drives, "OTV Mirror", false),
            accepts: vec![Class::Reobtainable],
            subpath: String::new(),
        },
        Volume {
            name: "Stash".into(),
            host: "m2".into(),
            encrypted: volume_encrypted(&drives, "Stash", false),
            accepts: vec![Class::Reobtainable],
            subpath: "Media".into(),
        },
    ];

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
                master.clone(),
                replica_volumes.clone(),
                "idrive".into(),
                vec!["m5".into()],
            ),
            Class::Reobtainable => (
                "OTV Mirror".into(),
                Vec::new(),
                String::new(),
                vec!["m5".into()],
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
                "m2".into()
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
                id: "m5".into(),
                role: Role::TravellingPrimary,
                floor_gb: 150.0,
                band_low_gb: Some(200.0),
                band_high_gb: Some(400.0),
                bulk: Bulk::WorkingSet,
            },
            Host {
                id: "m2".into(),
                role: Role::HomeHub,
                floor_gb: 40.0,
                band_low_gb: None,
                band_high_gb: None,
                bulk: Bulk::MastersOnExternal,
            },
            Host {
                id: "m1".into(),
                role: Role::SharedLaptop,
                floor_gb: 30.0,
                band_low_gb: None,
                band_high_gb: None,
                bulk: Bulk::NoBulk,
            },
        ],
        volumes,
        datasets: policy_datasets,
        sync_folders: vec![games_folder()],
        pins: Vec::new(),
    })
}

fn games_folder() -> SyncFolder {
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
                path: "ROMs".into(),
                class: Class::Reobtainable,
                keep: Vec::new(),
                master_host: "m2".into(),
                master_volume: "OTV Mirror".into(),
                pins: Vec::new(),
                archive: true,
            },
            Part {
                path: "*".into(),
                class: Class::Reobtainable,
                keep: vec!["m5".into()],
                master_host: "m2".into(),
                master_volume: "OTV Mirror".into(),
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

fn volume_encrypted(drives: &Value, name: &str, default: bool) -> bool {
    let Some(list) = drives.get("drives").and_then(|v| v.as_array()) else {
        return default;
    };
    for drive in list {
        let Some(volumes) = drive.get("volumes").and_then(|v| v.as_object()) else {
            continue;
        };
        if let Some(vol) = volumes.get(name) {
            return vol
                .get("encrypted")
                .and_then(|v| v.as_bool())
                .unwrap_or(default);
        }
    }
    default
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
