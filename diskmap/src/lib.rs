//! Disk contents, history and free-space alerts for the fleet.
//! Read-only against user files. State lives in `~/.local/state/diskmap`.

mod archive;
mod cli;
mod cloud;
mod darwin;
mod engine;
mod logic;
mod peers;
mod sqlite;
mod store;
mod summary;
mod tick;

pub use archive::{ship, spool_clear, spool_merge, spool_take, Manifest};
pub use darwin::{cpu_time, read_dir_entries, read_priority, set_skip, skipped, thermal_blocks_walk, worker_priority, Entry};
pub use engine::{
    apply_opaque, apply_plan, apply_scans, enqueue, growth_against_snapshot, growth_since, hold_count, map_progress,
    map_volume, migrate_churn, opaque_len, opaque_measure, opaque_next, OpaqueWalk, prune_missing, queue_len, remember_volume, scanned_dirs,
    walk_one_batch, walk_slice, MapProgress, MapStats, OpaqueTotal, PlanStats, WALK_BATCH,
};
pub use logic::{
    account_entries, archive_allowed, coalesce, collapse_grew, confirm_success, default_config,
    floor_breach, gate_alerts, glob_match, grew_status_line, group_drives, load_config, map_percent,
    parse_backup_summary, parse_drives, parse_local_mdy, parse_log_records, parse_since, pause_walk,
    drain_pace, needs_worker, DrainPace, baseline_pace, budget_sleep, churn_root, hold_due, plan_scans, protected_root, HoldKind,
    ScanPlan, BASELINE_BUDGET_PCT, BURST_MIN, BURST_MIN_DEPTH, CHURN_EVERY_SECS, CHURN_TREES, DRAIN_BUDGET_PCT,
    MAX_HOLD_SECS, QUIET_SECS,
    photos_kind, plist_path_strings, resolve_host, samples_from_jsonl, split_bytes, sustained_drop,
    trend, where_match, Alert,
    DirAccount, DiskConfig, DriveDef, DriveRollup, FileAttr, Grew, Growth, HostProfile, LogRecord,
    PhotoKind, RawEvent, ResolvedHost, ScanReq, Trend, VolumeDef, VolumeObs, EVENT_MIN_BYTES,
    HISTORY_MIN_BYTES, ITEM_IS_FILE, MUST_SCAN_SUBDIRS, SF_DATALESS,
};
pub use sqlite::Db;
pub use store::{
    dir, events_after_id, events_since, history_all, history_for_path, insert_event, open as open_db,
    open_readonly, propagate, prune_confirmed, snapshot_history, upsert_history, upsert_own, DirRow,
    EventRow, HistRow,
};
pub use summary::{
    fleet_lines, format_bytes_cell, format_fleet, format_top, FleetLine, Grower, Location, MapStatus,
    Summary, TopEntry, TopMap, TrendBytes,
};
pub use peers::{read_where, store_summary, store_where, summary_path, where_path, PeerWhere};
pub use summary::{WhereFile, WhereHit};
pub use tick::{grew_report, GrewHit, GrewReport, Roots};

pub fn run(args: Vec<String>) -> i32 {
    cli::run(args)
}
