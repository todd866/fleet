//! Canonical agent operating layer. Provider files are generated; guards decide
//! allow / ask / deny from a command, not from which CLI invoked them.

pub mod context;
pub mod doctor;
pub mod generate;
pub mod guard;
pub mod inventory;
pub mod memory;
pub mod site;

use std::env;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process;

/// Die on a closed pipe (`inventory | head`) instead of panicking on EPIPE.
pub fn quiet_sigpipe() {
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
}

pub fn run(args: &[String]) -> i32 {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.get(1).copied() {
        None | Some("-h") | Some("--help") => {
            print_usage();
            if args.get(1).is_some() { 0 } else { 2 }
        }
        Some("inventory") => cmd_inventory(&args[2..]),
        Some("sync") => cmd_sync(&args[2..]),
        Some("doctor") => cmd_doctor(&args[2..]),
        Some("context") => {
            print!("{}", context::render_default());
            0
        }
        Some("hook") => cmd_hook(&args[2..]),
        Some("memory") => cmd_memory(&args[2..]),
        Some(other) => {
            eprintln!("agentkit: unknown command {other}");
            print_usage();
            2
        }
    }
}

fn print_usage() {
    println!(
        "\
agentkit inventory [--out FILE]     print the mechanism inventory
agentkit sync [--apply] [--config DIR]
                                    dry-run (default); --apply writes provider files
agentkit doctor [--provider NAME] [--config DIR]
                                    exit non-zero when a provider has drifted
agentkit context                    time, capacity, unread mail, fleet alerts
agentkit hook <claude|codex|cursor> <guard>
                                    provider adapter: tool-call JSON on stdin
agentkit memory import --from DIR   import Claude memory files into the notebook
agentkit memory path                print the notebook directory"
    );
}

fn flag<'a>(args: &'a [&str], name: &str) -> Option<&'a str> {
    args.windows(2).find(|w| w[0] == name).map(|w| w[1])
}

fn cmd_inventory(args: &[&str]) -> i32 {
    let text = inventory::markdown();
    if let Some(path) = flag(args, "--out") {
        if let Err(e) = std::fs::write(path, &text) {
            eprintln!("agentkit inventory: {e}");
            return 1;
        }
    } else {
        print!("{text}");
    }
    0
}

fn cmd_sync(args: &[&str]) -> i32 {
    let apply = args.contains(&"--apply");
    let paths = paths_from(args);
    match generate::sync(&paths, apply) {
        Ok(report) => {
            print!("{report}");
            0
        }
        Err(e) => {
            eprintln!("agentkit sync: {e}");
            2
        }
    }
}

fn cmd_doctor(args: &[&str]) -> i32 {
    let provider = flag(args, "--provider");
    let paths = paths_from(args);
    match doctor::run(&paths, provider) {
        Ok((code, text)) => {
            print!("{text}");
            code
        }
        Err(e) => {
            eprintln!("agentkit doctor: {e}");
            2
        }
    }
}

fn cmd_hook(args: &[&str]) -> i32 {
    let (Some(provider), Some(guard_name)) = (args.first().copied(), args.get(1).copied()) else {
        eprintln!("usage: agentkit hook <claude|codex|cursor> <guard>");
        return 2;
    };
    let mut raw = String::new();
    if io::stdin().read_to_string(&mut raw).is_err() {
        return 0;
    }
    let cfg = site::guard_config_from_env();
    let decision = match guard::decide_json(guard_name, &raw, &cfg) {
        Ok(d) => d,
        Err(_) => return 0,
    };
    let rendered = match provider {
        "claude" | "codex" => guard::format_claude(&decision),
        "cursor" => guard::format_cursor(&decision),
        other => {
            eprintln!("agentkit hook: unknown provider {other}");
            return 2;
        }
    };
    let mut out = io::stdout().lock();
    let _ = out.write_all(rendered.as_bytes());
    0
}

fn cmd_memory(args: &[&str]) -> i32 {
    match args.first().copied() {
        Some("path") => {
            println!("{}", paths_from(args).notebook().display());
            0
        }
        Some("import") => {
            let Some(from) = flag(args, "--from") else {
                eprintln!("usage: agentkit memory import --from DIR");
                return 2;
            };
            match memory::import_claude(PathBuf::from(from).as_path(), &paths_from(args).notebook())
            {
                Ok(report) => {
                    print!("{report}");
                    0
                }
                Err(e) => {
                    eprintln!("agentkit memory import: {e}");
                    1
                }
            }
        }
        _ => {
            eprintln!("usage: agentkit memory import --from DIR | agentkit memory path");
            2
        }
    }
}

pub struct Paths {
    pub home: PathBuf,
    pub config: PathBuf,
    pub state: PathBuf,
}

impl Paths {
    pub fn notebook(&self) -> PathBuf {
        self.config.join("memory")
    }
    pub fn backup_root(&self) -> PathBuf {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        self.state.join("backups").join(ts.to_string())
    }
}

fn paths_from(args: &[&str]) -> Paths {
    let home = env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let config = flag(args, "--config")
        .map(PathBuf::from)
        .or_else(|| env::var_os("AGENTKIT_CONFIG").map(PathBuf::from))
        .unwrap_or_else(|| home.join(".config/agentkit"));
    let state = env::var_os("AGENTKIT_STATE")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".local/state/agentkit"));
    Paths { home, config, state }
}

pub fn guard_main(name: &str) -> i32 {
    let mut raw = String::new();
    if io::stdin().read_to_string(&mut raw).is_err() {
        return 0;
    }
    let cfg = site::guard_config_from_env();
    let decision = match guard::decide_json(name, &raw, &cfg) {
        Ok(d) => d,
        Err(_) => return 0,
    };
    println!("{}", guard::format_neutral(&decision));
    0
}

pub fn context_main() {
    print!("{}", context::render_default());
    let _ = process::id();
}
