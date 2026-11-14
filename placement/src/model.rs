//! Policy document. Host ids, paths and volume names come from config, not from the evaluator.

use serde::{Deserialize, Serialize};

/// Decimal gigabyte, matching diskmap and fleet-health.
pub const GB: u64 = 1_000_000_000;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    #[serde(default, rename = "host")]
    pub hosts: Vec<Host>,
    #[serde(default, rename = "volume")]
    pub volumes: Vec<Volume>,
    #[serde(default, rename = "dataset")]
    pub datasets: Vec<Dataset>,
    #[serde(default, rename = "sync_folder")]
    pub sync_folders: Vec<SyncFolder>,
    #[serde(default, rename = "pin")]
    pub pins: Vec<Pin>,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            hosts: Vec::new(),
            volumes: Vec::new(),
            datasets: Vec::new(),
            sync_folders: Vec::new(),
            pins: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Host {
    pub id: String,
    pub role: Role,
    pub floor_gb: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub band_low_gb: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub band_high_gb: Option<f64>,
    pub bulk: Bulk,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Role {
    TravellingPrimary,
    HomeHub,
    SharedLaptop,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::TravellingPrimary => "travelling-primary",
            Role::HomeHub => "home-hub",
            Role::SharedLaptop => "shared-laptop",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Bulk {
    /// Keep the working set: what this host uses.
    WorkingSet,
    /// Bulk masters live on named external volumes, not on the internal disk.
    MastersOnExternal,
    /// No bulk unless a pin or a recent open says so.
    NoBulk,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Volume {
    pub name: String,
    pub host: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub encrypted: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub accepts: Vec<Class>,
    /// Directory under the volume root. Empty means the volume root.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub subpath: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Class {
    #[serde(rename = "personal-unique")]
    PersonalUnique,
    #[serde(rename = "re-obtainable")]
    Reobtainable,
    Scratch,
    #[serde(rename = "synced-small")]
    SyncedSmall,
}

impl Class {
    pub fn as_str(self) -> &'static str {
        match self {
            Class::PersonalUnique => "personal-unique",
            Class::Reobtainable => "re-obtainable",
            Class::Scratch => "scratch",
            Class::SyncedSmall => "synced-small",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum SyncMethod {
    #[default]
    None,
    Syncthing,
    Copy,
}

fn is_none_sync(sync: &SyncMethod) -> bool {
    *sync == SyncMethod::None
}

fn is_false(v: &bool) -> bool {
    !*v
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Dataset {
    pub name: String,
    pub class: Class,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
    #[serde(default, skip_serializing_if = "is_none_sync")]
    pub sync: SyncMethod,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub master_host: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub master_volume: String,
    /// Host ids that keep a local copy. `*` is every host in the policy.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keep: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pins: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub played_within_days: Option<u32>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub icloud: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub offsite: String,
    /// Volumes that should hold encrypted copies. A name here is not a verified copy.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub replica_volumes: Vec<String>,
}

impl Default for Dataset {
    fn default() -> Self {
        Self {
            name: String::new(),
            class: Class::Scratch,
            paths: Vec::new(),
            sync: SyncMethod::None,
            master_host: String::new(),
            master_volume: String::new(),
            keep: Vec::new(),
            pins: Vec::new(),
            played_within_days: None,
            icloud: false,
            offsite: String::new(),
            replica_volumes: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyncFolder {
    pub name: String,
    pub root: String,
    #[serde(default, skip_serializing_if = "is_none_sync")]
    pub method: SyncMethod,
    /// Opens newer than this many days count as "played here".
    #[serde(default = "default_days")]
    pub played_within_days: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parts: Vec<Part>,
}

fn default_days() -> u32 {
    30
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Part {
    /// Path relative to the sync folder. `*` is every other child directory.
    pub path: String,
    pub class: Class,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keep: Vec<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub master_host: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub master_volume: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pins: Vec<String>,
    /// Master only: not a working install on a laptop.
    #[serde(default, skip_serializing_if = "is_false")]
    pub archive: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pin {
    pub path: String,
    pub hosts: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetState {
    BelowFloor,
    BelowBand,
    InBand,
    AboveBand,
    Unknown,
}

impl BudgetState {
    pub fn as_str(self) -> &'static str {
        match self {
            BudgetState::BelowFloor => "below-floor",
            BudgetState::BelowBand => "below-band",
            BudgetState::InBand => "in-band",
            BudgetState::AboveBand => "above-band",
            BudgetState::Unknown => "unknown",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            BudgetState::BelowFloor => "below floor",
            BudgetState::BelowBand => "below band",
            BudgetState::InBand => "in band",
            BudgetState::AboveBand => "above band",
            BudgetState::Unknown => "no reading",
        }
    }
}

pub fn gb_to_bytes(gb: f64) -> u64 {
    (gb * GB as f64).round() as u64
}

pub fn budget_state(host: &Host, free_bytes: Option<u64>) -> BudgetState {
    let Some(free) = free_bytes else {
        return BudgetState::Unknown;
    };
    if free < gb_to_bytes(host.floor_gb) {
        return BudgetState::BelowFloor;
    }
    if let Some(low) = host.band_low_gb {
        if free < gb_to_bytes(low) {
            return BudgetState::BelowBand;
        }
    }
    if let Some(high) = host.band_high_gb {
        if free > gb_to_bytes(high) {
            return BudgetState::AboveBand;
        }
    }
    BudgetState::InBand
}

pub fn load_policy(text: &str) -> Result<Policy, String> {
    toml::from_str(text).map_err(|err| err.to_string())
}

pub fn render_policy(policy: &Policy) -> Result<String, String> {
    let body = toml::to_string_pretty(policy).map_err(|err| err.to_string())?;
    Ok(format!(
        "# Generated by `placement init`. Site config; do not commit.\n{body}"
    ))
}

pub fn norm_path(path: &str) -> String {
    let p = path.trim().trim_start_matches("~/");
    let p = p.trim_start_matches('/');
    p.trim_end_matches('/').to_string()
}

pub fn path_covers(unit: &str, candidate: &str) -> bool {
    let unit = norm_path(unit);
    let candidate = norm_path(candidate);
    candidate == unit || candidate.starts_with(&format!("{unit}/"))
}

/// Desktop and Documents are iCloud-managed on this fleet's laptops. Reading them on a
/// metered link downloads placeholders. Callers can also set `icloud` on the data set.
pub fn icloud_path(path: &str) -> bool {
    let p = norm_path(path);
    p == "Desktop" || p == "Documents" || p.starts_with("Desktop/") || p.starts_with("Documents/")
}

#[derive(Debug, Clone, Default)]
pub struct Observation {
    pub now: i64,
    pub diskmap_present: bool,
    pub frees: Vec<Free>,
    pub present: Vec<Present>,
    pub access: Vec<Access>,
    pub replicas: Vec<Replica>,
}

#[derive(Debug, Clone)]
pub struct Free {
    pub host: String,
    pub free_bytes: u64,
}

#[derive(Debug, Clone)]
pub struct Present {
    pub host: String,
    /// `internal` or a volume name.
    pub location: String,
    pub path: String,
    pub bytes: u64,
}

#[derive(Debug, Clone)]
pub struct Access {
    pub host: String,
    pub path: String,
    pub last_access: i64,
}

/// A copy that has been checked. `verified` without bytes is not a copy: an empty
/// directory layout has been mistaken for one before.
#[derive(Debug, Clone)]
pub struct Replica {
    pub path: String,
    pub volume: String,
    pub encrypted: bool,
    pub verified: bool,
    pub offsite: bool,
    pub bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionKind {
    Keep,
    Move,
    Sync,
    Ignore,
    Clear,
    Refuse,
}

impl ActionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ActionKind::Keep => "keep",
            ActionKind::Move => "move",
            ActionKind::Sync => "sync",
            ActionKind::Ignore => "ignore",
            ActionKind::Clear => "clear",
            ActionKind::Refuse => "refuse",
        }
    }

    fn rank(self) -> u8 {
        match self {
            ActionKind::Clear => 0,
            ActionKind::Move => 1,
            ActionKind::Refuse => 2,
            ActionKind::Sync => 3,
            ActionKind::Keep => 4,
            ActionKind::Ignore => 5,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Action {
    pub host: String,
    pub kind: ActionKind,
    pub path: String,
    pub bytes: u64,
    pub dest: String,
    pub reason: String,
    pub class: Class,
    pub label: String,
    pub icloud: bool,
}

#[derive(Debug, Clone)]
pub struct HostBudget {
    pub host: String,
    pub role: Role,
    pub free_bytes: Option<u64>,
    pub floor_gb: f64,
    pub band_low_gb: Option<f64>,
    pub band_high_gb: Option<f64>,
    pub state: BudgetState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Violation {
    pub host: String,
    pub dataset: String,
    pub code: String,
    pub detail: String,
}

#[derive(Debug, Clone)]
pub struct Plan {
    pub actions: Vec<Action>,
    pub budgets: Vec<HostBudget>,
    pub violations: Vec<Violation>,
}

pub(crate) fn sort_actions(actions: &mut [Action]) {
    actions.sort_by(|a, b| {
        a.kind
            .rank()
            .cmp(&b.kind.rank())
            .then(a.host.cmp(&b.host))
            .then(a.path.cmp(&b.path))
    });
}
