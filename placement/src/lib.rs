//! Decide where every data set lives, and keep it there.
//!
//! The rules are generic. Host names, floors and paths come from
//! `~/.config/fleet/placement.toml`. Sizes come from diskmap. This crate does not
//! scan a disk with `du`, does not open a network connection, and does not take a
//! heavy-slot: a copy is an I/O job.

mod apply;
mod cli;
mod eval;
mod init;
mod model;
mod observe;
mod status;
mod stignore;

pub use apply::{ApplyOpts, ApplyReport, TAKES_HEAVY_SLOT, apply, copy_tree, verify_trees, ymd};
pub use eval::{evaluate, expand_units, free_bytes, verified_encrypted_copies};
pub use init::{fleet_policy, host_id_from_diskmap, peers_from_diskmap};
pub use model::*;
pub use observe::{PeerTrust, load_observation, load_observation_with_peers};
pub use status::{format_plan, format_status, status_file};
pub use stignore::stignore;

pub fn run(args: Vec<String>) -> i32 {
    cli::run(args)
}
