//! Cached copies of each peer's documents, so readers on this Mac (placement) can see data
//! that lives on another Mac's drives without asking it. `diskmap peers` refreshes them over
//! ssh; a peer that does not answer keeps its last cache, and the file's ages say how old it is.

use crate::summary::{Summary, WhereHit};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// `peers/<peer>.where.json`: the peer's where rows, plus when they were measured there
/// (`generated`, the newest row) and when this Mac fetched them (`fetched`).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PeerWhere {
    pub generated: i64,
    pub fetched: i64,
    pub min_bytes: u64,
    pub matches: Vec<WhereHit>,
}

pub fn where_path(state: &Path, peer: &str) -> PathBuf {
    state.join("peers").join(format!("{peer}.where.json"))
}

pub fn summary_path(state: &Path, peer: &str) -> PathBuf {
    state.join("peers").join(format!("{peer}.json"))
}

/// Store the output of `diskmap where '*' --json --remote-ask` from `peer`. Refuses output
/// that does not parse or holds no rows, so a broken answer never replaces a good cache.
pub fn store_where(state: &Path, peer: &str, stdout: &[u8], now: i64) -> Result<PeerWhere, String> {
    let rows: Vec<WhereHit> = serde_json::from_slice(stdout).map_err(|e| format!("where: {e}"))?;
    if rows.is_empty() {
        return Err("where: no rows".into());
    }
    let doc = PeerWhere {
        generated: rows.iter().map(|r| r.as_of).max().unwrap_or(0),
        fetched: now,
        min_bytes: 0,
        matches: rows,
    };
    let text = serde_json::to_string(&doc).map_err(|e| e.to_string())? + "\n";
    write_atomic(&where_path(state, peer), text.as_bytes())?;
    Ok(doc)
}

/// Store the output of `diskmap json` from `peer`. Same refusal rule as `store_where`.
pub fn store_summary(state: &Path, peer: &str, stdout: &[u8]) -> Result<Summary, String> {
    let summary: Summary = serde_json::from_slice(stdout).map_err(|e| format!("summary: {e}"))?;
    write_atomic(&summary_path(state, peer), stdout)?;
    Ok(summary)
}

pub fn read_where(state: &Path, peer: &str) -> Option<PeerWhere> {
    let text = fs::read_to_string(where_path(state, peer)).ok()?;
    serde_json::from_str(&text).ok()
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path.parent().ok_or("no parent")?;
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let tmp = parent.join(format!(
        ".{}.tmp",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("peer")
    ));
    fs::write(&tmp, bytes).map_err(|e| e.to_string())?;
    fs::rename(&tmp, path).map_err(|e| e.to_string())
}
