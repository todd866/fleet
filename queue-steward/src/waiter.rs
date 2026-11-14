use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Waiter {
    pub pid: i32,
    pub label: String,
    pub project: String,
    pub origin: String,
    pub cmd: String,
    pub expected_s: Option<u32>,
    pub since: i64,
    pub host: String,
    /// "waiting" (wants a slot), "transferring" (moving data ahead of processing, not yet asking
    /// for a slot) or "processing" (holds a slot). A slot is full only while processing
    /// (owner, 5 Nov 2026).
    #[serde(default = "default_state")]
    pub state: String,
}

fn default_state() -> String {
    "waiting".to_string()
}

impl Waiter {
    pub fn queue_path(dir: &Path, pid: i32) -> PathBuf {
        dir.join("queue").join(format!("{pid}.json"))
    }
}
