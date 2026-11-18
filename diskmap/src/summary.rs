//! Fleet summary written for Glancebar and fleet-health, and the text instruments.

use crate::cloud::CloudFolder;
use crate::logic::{self, Alert, DriveRollup, Trend};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TrendBytes {
    pub h1: Option<i64>,
    pub h24: Option<i64>,
    pub d7: Option<i64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Grower {
    pub path: String,
    pub delta_bytes: i64,
}

/// Walk progress for one volume. Absent on cloud rows and on summaries
/// written before the walker recorded a map.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct MapStatus {
    pub dirs: u64,
    pub percent: u8,
    pub done: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Location {
    pub kind: String,
    pub id: String,
    pub host: String,
    pub name: String,
    pub volume_uuid: Option<String>,
    pub physical_drive: Option<String>,
    pub mount: Option<String>,
    pub mounted: bool,
    pub encrypted: bool,
    pub total_bytes: u64,
    pub free_bytes: u64,
    pub local_bytes: u64,
    pub dataless_bytes: u64,
    pub trend_bytes: TrendBytes,
    pub top_grower: Option<Grower>,
    pub last_seen: i64,
    pub detail: String,
    pub folders: Vec<CloudFolder>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub map: Option<MapStatus>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DriveOut {
    pub name: String,
    pub mounted: bool,
    pub total_bytes: u64,
    pub free_bytes: u64,
    pub last_seen: i64,
    pub volume_uuids: Vec<String>,
    pub encrypted_mounted: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Summary {
    pub generated: i64,
    pub host_id: String,
    pub local_host_name: String,
    pub locations: Vec<Location>,
    pub drives: Vec<DriveOut>,
    pub alerts: Vec<Alert>,
    /// Full Disk Access and what goes unmeasured without it. Absent on
    /// summaries from builds before 18 Nov 2026.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access: Option<AccessSummary>,
}

/// A tree the walker does not open while diskmap lacks Full Disk Access; its
/// size is the last one measured (0 when never measured).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Unmeasured {
    pub path: String,
    pub bytes: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AccessSummary {
    pub full_disk: bool,
    pub checked_at: i64,
    pub unmeasured: Vec<Unmeasured>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WhereHit {
    pub host: String,
    pub location: String,
    pub path: String,
    pub home_path: Option<String>,
    pub bytes: u64,
    pub as_of: i64,
    pub kind: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WhereFile {
    pub generated: i64,
    pub min_bytes: u64,
    pub matches: Vec<WhereHit>,
}

pub fn trend_bytes(trend: &Trend) -> TrendBytes {
    fn gb_to_bytes(v: Option<f64>) -> Option<i64> {
        v.map(|g| (g * 1e9) as i64)
    }
    TrendBytes {
        h1: gb_to_bytes(trend.h1),
        h24: gb_to_bytes(trend.h24),
        d7: gb_to_bytes(trend.d7),
    }
}

pub fn drive_out(row: &DriveRollup) -> DriveOut {
    DriveOut {
        name: row.name.clone(),
        mounted: row.mounted,
        total_bytes: row.total_bytes,
        free_bytes: row.free_bytes,
        last_seen: row.last_seen,
        volume_uuids: row.volume_uuids.clone(),
        encrypted_mounted: row.encrypted_mounted,
    }
}

pub fn location_from_drive(
    host: &str,
    drive: &DriveRollup,
    trend: TrendBytes,
    grower: Option<Grower>,
    map: Option<MapStatus>,
) -> Location {
    let kind = "volume";
    Location {
        kind: kind.into(),
        id: format!("{host}:{}", drive.name),
        host: host.into(),
        name: drive.name.clone(),
        volume_uuid: drive.volume_uuids.first().cloned(),
        physical_drive: Some(drive.name.clone()),
        mount: None,
        mounted: drive.mounted,
        encrypted: drive.encrypted_mounted,
        total_bytes: drive.total_bytes,
        free_bytes: drive.free_bytes,
        local_bytes: drive.total_bytes.saturating_sub(drive.free_bytes),
        dataless_bytes: 0,
        trend_bytes: trend,
        top_grower: grower,
        last_seen: drive.last_seen,
        detail: String::new(),
        folders: Vec::new(),
        map,
    }
}

#[derive(Clone, Debug)]
pub struct FleetLine {
    pub mark: String,
    pub host: String,
    pub where_: String,
    pub total: String,
    pub free: String,
    pub h1: String,
    pub h24: String,
    pub d7: String,
    pub grower: String,
    pub seen: String,
    pub map: String,
}

/// One instrument row per Mac disk, physical drive, iCloud and IDrive.
/// The same drive mounted on one Mac wins over an unplugged copy elsewhere.
pub fn fleet_lines(summaries: &[Summary], now: i64) -> Vec<FleetLine> {
    let mut chosen: Vec<&Location> = Vec::new();
    for summary in summaries {
        for loc in &summary.locations {
            let key = loc_key(loc);
            if let Some(idx) = chosen.iter().position(|have| loc_key(have) == key) {
                if prefer(loc, chosen[idx]) {
                    chosen[idx] = loc;
                }
            } else {
                chosen.push(loc);
            }
        }
    }
    chosen.sort_by(|a, b| {
        a.host
            .cmp(&b.host)
            .then(kind_rank(&a.kind).cmp(&kind_rank(&b.kind)))
            .then(a.name.cmp(&b.name))
    });
    chosen.into_iter().map(|loc| line_for(loc, summaries, now)).collect()
}

fn loc_key(loc: &Location) -> String {
    if loc.kind == "volume" {
        if let Some(drive) = &loc.physical_drive {
            if drive != "internal" {
                return format!("drive:{drive}");
            }
        }
        return format!("vol:{}:{}", loc.host, loc.name);
    }
    format!("{}:{}:{}", loc.kind, loc.host, loc.name)
}

fn kind_rank(kind: &str) -> u8 {
    match kind {
        "volume" => 0,
        "icloud" => 1,
        "idrive" => 2,
        _ => 9,
    }
}

fn prefer(new: &Location, old: &Location) -> bool {
    if new.mounted != old.mounted {
        return new.mounted;
    }
    new.last_seen > old.last_seen
}

fn line_for(loc: &Location, summaries: &[Summary], now: i64) -> FleetLine {
    let alert = summaries.iter().flat_map(|s| s.alerts.iter()).any(|a| {
        a.host == loc.host && (a.location == loc.name || a.location == loc.kind)
    });
    let mark = if alert {
        "!".to_string()
    } else if loc.kind == "volume" && !loc.mounted {
        "o".to_string()
    } else {
        "*".to_string()
    };
    let (total, free) = match loc.kind.as_str() {
        "icloud" => (
            format!("{} local", logic::fmt_gb(loc.local_bytes as i64)),
            format!("{} cloud", logic::fmt_gb(loc.dataless_bytes as i64)),
        ),
        "idrive" => (
            if loc.detail.is_empty() {
                "—".into()
            } else {
                loc.detail.clone()
            },
            if loc.folders.is_empty() {
                "—".into()
            } else {
                format!("{} paths", loc.folders.len())
            },
        ),
        _ if !loc.mounted && loc.last_seen == 0 => ("—".into(), "—".into()),
        _ => (
            logic::fmt_gb(loc.total_bytes as i64),
            format!("{} free", logic::fmt_gb(loc.free_bytes as i64)),
        ),
    };
    let grower = if let Some(g) = &loc.top_grower {
        let name = logic::basename(&g.path);
        format!("{name} {}", logic::fmt_gb_signed(g.delta_bytes))
    } else if loc.kind == "idrive" {
        loc.detail.clone()
    } else {
        "—".into()
    };
    FleetLine {
        mark,
        host: loc.host.clone(),
        where_: loc.name.clone(),
        total,
        free,
        h1: opt_gb(loc.trend_bytes.h1),
        h24: opt_gb(loc.trend_bytes.h24),
        d7: opt_gb(loc.trend_bytes.d7),
        grower,
        seen: logic::fmt_age(now, loc.last_seen),
        map: format_map_cell(loc),
    }
}

pub fn format_map_cell(loc: &Location) -> String {
    match &loc.map {
        Some(map) if map.done => "done".into(),
        Some(map) => format!("{}%", map.percent),
        None => "—".into(),
    }
}

pub fn format_bytes_cell(bytes: i64, partial: bool) -> String {
    let text = logic::fmt_gb(bytes);
    if partial {
        format!("≥ {text} (walking)")
    } else {
        text
    }
}

fn opt_gb(v: Option<i64>) -> String {
    match v {
        Some(n) => logic::fmt_gb_signed(n),
        None => "—".into(),
    }
}

pub fn format_fleet(lines: &[FleetLine]) -> String {
    let header = FleetLine {
        mark: " ".into(),
        host: "host".into(),
        where_: "where".into(),
        total: "total".into(),
        free: "free".into(),
        h1: "1h".into(),
        h24: "24h".into(),
        d7: "7d".into(),
        grower: "grower".into(),
        seen: "seen".into(),
        map: "map".into(),
    };
    let all = std::iter::once(&header).chain(lines.iter());
    let rows: Vec<[&str; 11]> = all
        .map(|l| {
            [
                l.mark.as_str(),
                l.host.as_str(),
                l.where_.as_str(),
                l.total.as_str(),
                l.free.as_str(),
                l.h1.as_str(),
                l.h24.as_str(),
                l.d7.as_str(),
                l.grower.as_str(),
                l.seen.as_str(),
                l.map.as_str(),
            ]
        })
        .collect();
    let widths: Vec<usize> = (0..11)
        .map(|i| rows.iter().map(|r| r[i].chars().count()).max().unwrap_or(0))
        .collect();
    let right = [false, false, false, true, true, true, true, true, false, true, true];
    let mut text = String::new();
    for row in &rows {
        let mut line = String::new();
        for i in 0..11 {
            if i > 0 {
                line.push_str("  ");
            }
            let cell = row[i];
            let w = widths[i];
            let pad = w.saturating_sub(cell.chars().count());
            if right[i] {
                line.push_str(&" ".repeat(pad));
                line.push_str(cell);
            } else {
                line.push_str(cell);
                line.push_str(&" ".repeat(pad));
            }
        }
        text.push_str(&line);
        text.push('\n');
    }
    text
}

pub fn to_json(summary: &Summary) -> String {
    serde_json::to_string_pretty(summary).unwrap_or_else(|_| "{}".into()) + "\n"
}

#[derive(Clone, Debug)]
pub struct TopMap {
    pub volume: String,
    pub dirs: u64,
    pub percent: u8,
    pub done: bool,
}

#[derive(Clone, Debug)]
pub struct TopEntry {
    pub volume: String,
    pub path: String,
    pub bytes: i64,
    pub partial: bool,
}

/// Map progress, then the largest children. Partial totals are a lower bound.
pub fn format_top(maps: &[TopMap], entries: &[TopEntry]) -> String {
    let mut entries = entries.to_vec();
    entries.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.path.cmp(&b.path)));
    entries.truncate(20);
    let mut rows: Vec<(String, String, String)> = Vec::new();
    for map in maps {
        let value = if map.done {
            "done".into()
        } else {
            format!("{}%", map.percent)
        };
        rows.push((map.volume.clone(), format!("map {} dirs", map.dirs), value));
    }
    for entry in &entries {
        let path = if entry.path.is_empty() { "/".into() } else { entry.path.clone() };
        rows.push((
            entry.volume.clone(),
            path,
            format_bytes_cell(entry.bytes, entry.partial),
        ));
    }
    if rows.is_empty() {
        return String::new();
    }
    let widths = [
        rows.iter().map(|r| r.0.chars().count()).max().unwrap_or(0),
        rows.iter().map(|r| r.1.chars().count()).max().unwrap_or(0),
        rows.iter().map(|r| r.2.chars().count()).max().unwrap_or(0),
    ];
    let mut text = String::new();
    for (volume, label, value) in &rows {
        let vpad = widths[0].saturating_sub(volume.chars().count());
        let lpad = widths[1].saturating_sub(label.chars().count());
        let spad = widths[2].saturating_sub(value.chars().count());
        text.push_str(volume);
        text.push_str(&" ".repeat(vpad));
        text.push_str("  ");
        text.push_str(label);
        text.push_str(&" ".repeat(lpad));
        text.push_str("  ");
        text.push_str(&" ".repeat(spad));
        text.push_str(value);
        text.push('\n');
    }
    text
}
