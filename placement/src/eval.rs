//! Where a data set should live, given the policy and what diskmap has actually seen.

use crate::model::*;
use std::collections::BTreeSet;

#[derive(Debug, Clone)]
pub struct Unit {
    pub label: String,
    pub path: String,
    pub class: Class,
    pub keep: Vec<String>,
    pub pins: Vec<String>,
    pub master_host: String,
    pub master_volume: String,
    pub played_within_days: Option<u32>,
    pub sync: SyncMethod,
    pub icloud: bool,
    pub from_sync: bool,
    pub archive: bool,
}

enum Desire {
    Keep(String),
    Drop(String),
    /// The local copy should go, and it must not: the guard failed.
    Blocked(String),
}

pub fn evaluate(policy: &Policy, obs: &Observation) -> Plan {
    let units = expand_units(policy, obs);
    let budgets: Vec<HostBudget> = policy
        .hosts
        .iter()
        .map(|host| {
            let free = free_bytes(obs, &host.id);
            HostBudget {
                host: host.id.clone(),
                role: host.role,
                free_bytes: free,
                floor_gb: host.floor_gb,
                band_low_gb: host.band_low_gb,
                band_high_gb: host.band_high_gb,
                state: budget_state(host, free),
            }
        })
        .collect();

    let mut actions = Vec::new();
    let mut violations = Vec::new();
    let mut seen_viol = BTreeSet::new();

    if !obs.diskmap_present {
        push_viol(
            &mut violations,
            &mut seen_viol,
            Violation {
                host: String::new(),
                dataset: "diskmap".into(),
                code: "reading".into(),
                detail: "no summary".into(),
            },
        );
    }

    for unit in &units {
        let copies = verified_encrypted_copies(unit, obs);
        if unit.class == Class::PersonalUnique {
            if copies < 2 {
                push_viol(
                    &mut violations,
                    &mut seen_viol,
                    Violation {
                        host: unit.master_host.clone(),
                        dataset: unit.label.clone(),
                        code: "personal-copies".into(),
                        detail: format!("{copies} encrypted copy"),
                    },
                );
            }
            if !verified_offsite(unit, obs) {
                push_viol(
                    &mut violations,
                    &mut seen_viol,
                    Violation {
                        host: unit.master_host.clone(),
                        dataset: unit.label.clone(),
                        code: "offsite".into(),
                        detail: "no offsite".into(),
                    },
                );
            }
        }
        let anywhere = obs
            .present
            .iter()
            .any(|p| path_covers(&unit.path, &p.path) && p.bytes > 0);
        if unit.class == Class::Reobtainable
            && !unit.master_volume.is_empty()
            && anywhere
            && !master_has_bytes(unit, obs)
        {
            push_viol(
                &mut violations,
                &mut seen_viol,
                Violation {
                    host: unit.master_host.clone(),
                    dataset: unit.label.clone(),
                    code: "master".into(),
                    detail: "master empty".into(),
                },
            );
        }

        for host in &policy.hosts {
            let state = budgets
                .iter()
                .find(|b| b.host == host.id)
                .map(|b| b.state)
                .unwrap_or(BudgetState::Unknown);
            let desire = decide(unit, host, obs, state);
            let bytes = internal_bytes(obs, &host.id, &unit.path);
            let present = bytes.is_some();
            let bytes = bytes.unwrap_or(0);
            let base = |kind, reason: String, dest: String| Action {
                host: host.id.clone(),
                kind,
                path: unit.path.clone(),
                bytes,
                dest,
                reason,
                class: unit.class,
                label: unit.label.clone(),
                icloud: unit.icloud || icloud_path(&unit.path),
            };
            match desire {
                Desire::Keep(reason) => {
                    if present {
                        actions.push(base(ActionKind::Keep, reason, String::new()));
                    } else if unit.class == Class::SyncedSmall || unit.sync == SyncMethod::Syncthing
                    {
                        actions.push(base(ActionKind::Sync, reason, String::new()));
                    }
                }
                Desire::Blocked(reason) => {
                    if present {
                        actions.push(base(ActionKind::Refuse, reason, String::new()));
                    }
                }
                Desire::Drop(reason) => {
                    if present {
                        if unit.class != Class::Scratch {
                            push_viol(
                                &mut violations,
                                &mut seen_viol,
                                Violation {
                                    host: host.id.clone(),
                                    dataset: unit.label.clone(),
                                    code: "misplaced".into(),
                                    detail: if unit.archive {
                                        "archive".into()
                                    } else {
                                        "not placed".into()
                                    },
                                },
                            );
                        }
                        if unit.class == Class::Scratch {
                            actions.push(base(ActionKind::Clear, reason, String::new()));
                        } else if unit.master_volume.is_empty() {
                            actions.push(base(
                                ActionKind::Refuse,
                                "no master".into(),
                                String::new(),
                            ));
                        } else {
                            actions.push(base(
                                ActionKind::Move,
                                reason,
                                unit.master_volume.clone(),
                            ));
                        }
                    } else if unit.from_sync {
                        actions.push(base(ActionKind::Ignore, reason, String::new()));
                    }
                }
            }
        }
    }

    sort_actions(&mut actions);
    Plan {
        actions,
        budgets,
        violations,
    }
}

fn push_viol(out: &mut Vec<Violation>, seen: &mut BTreeSet<String>, viol: Violation) {
    let key = format!(
        "{}|{}|{}|{}",
        viol.code, viol.dataset, viol.host, viol.detail
    );
    if seen.insert(key) {
        out.push(viol);
    }
}

pub fn keeps_local(policy: &Policy, obs: &Observation, host_id: &str, unit: &Unit) -> bool {
    let Some(host) = policy.hosts.iter().find(|h| h.id == host_id) else {
        return false;
    };
    let state = budget_state(host, free_bytes(obs, host_id));
    matches!(decide(unit, host, obs, state), Desire::Keep(_))
}

pub fn expand_units(policy: &Policy, obs: &Observation) -> Vec<Unit> {
    let mut out = Vec::new();
    for ds in &policy.datasets {
        let paths = if ds.paths.is_empty() {
            vec![ds.name.clone()]
        } else {
            ds.paths.clone()
        };
        for path in paths {
            out.push(Unit {
                label: ds.name.clone(),
                path: norm_path(&path),
                class: ds.class,
                keep: ds.keep.clone(),
                pins: ds.pins.clone(),
                master_host: ds.master_host.clone(),
                master_volume: ds.master_volume.clone(),
                played_within_days: ds.played_within_days,
                sync: ds.sync,
                icloud: ds.icloud || icloud_path(&path),
                from_sync: ds.sync == SyncMethod::Syncthing,
                archive: false,
            });
        }
    }
    for folder in &policy.sync_folders {
        for part in &folder.parts {
            if part.path == "*" {
                continue;
            }
            out.push(unit_from_part(
                folder,
                part,
                &format!("{}/{}", folder.root, part.path),
            ));
        }
    }
    for folder in &policy.sync_folders {
        let explicit: Vec<&str> = folder
            .parts
            .iter()
            .filter(|p| p.path != "*")
            .map(|p| p.path.as_str())
            .collect();
        let Some(star) = folder.parts.iter().find(|p| p.path == "*") else {
            continue;
        };
        for name in sync_children(folder, obs, &explicit) {
            out.push(unit_from_part(
                folder,
                star,
                &format!("{}/{}", folder.root, name),
            ));
        }
    }
    for pin in &policy.pins {
        for unit in &mut out {
            if path_covers(&pin.path, &unit.path) || path_covers(&unit.path, &pin.path) {
                for host in &pin.hosts {
                    if !unit.pins.iter().any(|p| p == host) {
                        unit.pins.push(host.clone());
                    }
                }
            }
        }
    }
    out
}

fn unit_from_part(folder: &SyncFolder, part: &Part, path: &str) -> Unit {
    let window = if part.class == Class::Reobtainable && !part.archive {
        Some(folder.played_within_days)
    } else {
        None
    };
    Unit {
        label: format!(
            "{}/{}",
            folder.name,
            path.rsplit('/').next().unwrap_or(&part.path)
        ),
        path: norm_path(path),
        class: part.class,
        keep: part.keep.clone(),
        pins: part.pins.clone(),
        master_host: part.master_host.clone(),
        master_volume: part.master_volume.clone(),
        played_within_days: window,
        sync: folder.method,
        icloud: false,
        from_sync: true,
        archive: part.archive,
    }
}

fn sync_children(folder: &SyncFolder, obs: &Observation, explicit: &[&str]) -> BTreeSet<String> {
    let mut children = BTreeSet::new();
    for row in &obs.present {
        if let Some(name) = child_name(&folder.root, &row.path) {
            if !explicit.contains(&name.as_str()) {
                children.insert(name);
            }
        }
    }
    for row in &obs.access {
        if let Some(name) = child_name(&folder.root, &row.path) {
            if !explicit.contains(&name.as_str()) {
                children.insert(name);
            }
        }
    }
    children
}

fn child_name(root: &str, path: &str) -> Option<String> {
    let root = norm_path(root);
    let path = norm_path(path);
    let rest = path.strip_prefix(&root)?.strip_prefix('/')?;
    let name = rest.split('/').next().unwrap_or("");
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

fn decide(unit: &Unit, host: &Host, obs: &Observation, state: BudgetState) -> Desire {
    match desire(unit, host, obs, state) {
        Desire::Drop(reason) if unit.class == Class::PersonalUnique => {
            let copies = verified_encrypted_copies(unit, obs);
            if copies < 2 {
                Desire::Blocked(format!("{copies} encrypted copy"))
            } else {
                Desire::Drop(reason)
            }
        }
        other => other,
    }
}

fn desire(unit: &Unit, host: &Host, obs: &Observation, state: BudgetState) -> Desire {
    if unit.class == Class::SyncedSmall {
        return Desire::Keep("synced-small".into());
    }
    if unit.class == Class::Scratch {
        if pressure(state) {
            return Desire::Drop("scratch, below band".into());
        }
        return Desire::Keep("scratch, local".into());
    }
    if unit.pins.iter().any(|p| p == &host.id) {
        return Desire::Keep("pinned".into());
    }
    if unit.archive {
        return Desire::Drop(format!("archive on {}", vol_name(unit)));
    }
    if host.bulk == Bulk::MastersOnExternal && !unit.master_volume.is_empty() {
        return Desire::Drop(format!("master on {}", unit.master_volume));
    }

    let window = unit.played_within_days.unwrap_or(30);
    match access_age_days(unit, host, obs) {
        Some(age) if age <= window as i64 => Desire::Keep(format!("opened {age}d ago")),
        Some(_) => {
            let why = format!("not opened in {window}d");
            if no_bulk(host) {
                Desire::Drop(format!("shared, {why}"))
            } else if pressure(state) {
                Desire::Drop(format!("{why}, below band"))
            } else if listed(&unit.keep, host) {
                Desire::Keep("in band".into())
            } else {
                Desire::Drop(why)
            }
        }
        None => {
            if no_bulk(host) {
                Desire::Drop("shared, not opened here".into())
            } else if listed(&unit.keep, host) && state == BudgetState::BelowFloor {
                Desire::Drop("below floor, no use recorded".into())
            } else if listed(&unit.keep, host) {
                Desire::Keep("kept until use is known".into())
            } else {
                Desire::Drop("not placed".into())
            }
        }
    }
}

fn no_bulk(host: &Host) -> bool {
    host.bulk == Bulk::NoBulk || host.role == Role::SharedLaptop
}

fn pressure(state: BudgetState) -> bool {
    matches!(state, BudgetState::BelowBand | BudgetState::BelowFloor)
}

fn listed(keep: &[String], host: &Host) -> bool {
    keep.iter().any(|k| k == "*" || k == &host.id)
}

fn vol_name(unit: &Unit) -> &str {
    if unit.master_volume.is_empty() {
        "external"
    } else {
        &unit.master_volume
    }
}

fn access_age_days(unit: &Unit, host: &Host, obs: &Observation) -> Option<i64> {
    let last = obs
        .access
        .iter()
        .filter(|a| a.host == host.id && path_covers(&unit.path, &a.path))
        .map(|a| a.last_access)
        .max()?;
    Some(obs.now.saturating_sub(last) / 86_400)
}

pub fn verified_encrypted_copies(unit: &Unit, obs: &Observation) -> u32 {
    obs.replicas
        .iter()
        .filter(|r| {
            replica_matches(unit, r) && r.verified && r.encrypted && !r.offsite && r.bytes > 0
        })
        .count() as u32
}

fn verified_offsite(unit: &Unit, obs: &Observation) -> bool {
    obs.replicas
        .iter()
        .any(|r| replica_matches(unit, r) && r.verified && r.offsite && r.bytes > 0)
}

fn replica_matches(unit: &Unit, replica: &Replica) -> bool {
    path_covers(&unit.path, &replica.path)
        || path_covers(&replica.path, &unit.path)
        || replica.path == unit.label
}

fn master_has_bytes(unit: &Unit, obs: &Observation) -> bool {
    let on_volume = obs.present.iter().any(|p| {
        p.location == unit.master_volume && path_covers(&unit.path, &p.path) && p.bytes > 0
    });
    let verified = obs.replicas.iter().any(|r| {
        r.volume == unit.master_volume && replica_matches(unit, r) && r.verified && r.bytes > 0
    });
    on_volume || verified
}

pub fn free_bytes(obs: &Observation, host: &str) -> Option<u64> {
    obs.frees
        .iter()
        .find(|f| f.host == host)
        .map(|f| f.free_bytes)
}

pub fn internal_bytes(obs: &Observation, host: &str, path: &str) -> Option<u64> {
    let rows: Vec<&Present> = obs
        .present
        .iter()
        .filter(|p| p.host == host && is_internal(&p.location) && path_covers(path, &p.path))
        .collect();
    if rows.is_empty() {
        return None;
    }
    if let Some(exact) = rows.iter().find(|p| norm_path(&p.path) == norm_path(path)) {
        return Some(exact.bytes);
    }
    Some(rows.iter().map(|p| p.bytes).sum())
}

fn is_internal(location: &str) -> bool {
    location.is_empty() || location == "internal"
}

pub fn fmt_bytes(bytes: u64) -> String {
    if bytes >= GB {
        let v = bytes as f64 / GB as f64;
        if v >= 10.0 {
            format!("{v:.0} GB")
        } else {
            format!("{v:.1} GB")
        }
    } else if bytes >= 1_000_000 {
        format!("{} MB", bytes / 1_000_000)
    } else {
        format!("{bytes} B")
    }
}
