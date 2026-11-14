//! Syncthing `.stignore` for one host. Includes come first: the first match wins.
//! Never `(?d)`. Ignoring a file must not make Syncthing delete it.

use crate::eval::{expand_units, keeps_local};
use crate::model::{Observation, Policy, norm_path};
use std::collections::BTreeSet;

pub fn stignore(policy: &Policy, obs: &Observation, host_id: &str, folder: &str) -> String {
    let Some(sync) = policy
        .sync_folders
        .iter()
        .find(|f| f.name == folder || f.root == folder)
    else {
        return String::new();
    };
    let root = norm_path(&sync.root);
    let mut include = BTreeSet::new();
    for unit in expand_units(policy, obs) {
        if !unit.from_sync {
            continue;
        }
        let path = norm_path(&unit.path);
        let Some(rel) = path.strip_prefix(&root).and_then(|r| r.strip_prefix('/')) else {
            continue;
        };
        if rel.is_empty() || rel.contains('/') {
            continue;
        }
        if keeps_local(policy, obs, host_id, &unit) {
            include.insert(rel.to_string());
        }
    }
    let mut out = format!("// placement {} {host_id}\n", sync.name);
    for rel in include {
        out.push_str(&format!("!/{rel}\n!/{rel}/**\n"));
    }
    out.push_str("*\n");
    out
}
