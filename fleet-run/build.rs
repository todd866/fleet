//! Compiles the site's allowlist into the binary, so changing it means a rebuild and a re-sign.
//! Source: $FLEET_RUN_ALLOWLIST, else ~/.config/fleet/fleet-run.allow; missing = empty list.
//! Line format (tab-separated; `#` comments):
//!   LABEL <TAB> HASHED_FILE <TAB> SHA256 <TAB> ARGV0 [<TAB> ARGV1 ...]
//! Paths may start with `~/` (the account's home from the password database, not $HOME).
use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=FLEET_RUN_ALLOWLIST");
    let path = env::var_os("FLEET_RUN_ALLOWLIST").map(PathBuf::from).unwrap_or_else(|| {
        PathBuf::from(env::var_os("HOME").unwrap_or_default()).join(".config/fleet/fleet-run.allow")
    });
    println!("cargo:rerun-if-changed={}", path.display());
    let text = fs::read_to_string(&path).unwrap_or_default();
    let out = PathBuf::from(env::var("OUT_DIR").unwrap()).join("allow.rs");
    // Keep the raw text; the binary parses it with the same code the tests exercise.
    fs::write(out, format!("pub const ALLOWLIST: &str = {:?};\n", text)).unwrap();
}
