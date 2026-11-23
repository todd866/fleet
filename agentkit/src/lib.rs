//! Canonical agent operating layer. Provider files are generated; guards decide
//! allow / ask / deny from a command, not from which CLI invoked them.

pub mod conformance;
pub mod context;
pub mod posture;
pub mod doctor;
pub mod generate;
pub mod guard;
pub mod inventory;
pub mod ledger;
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
        Some("posture") => cmd_posture(&args[2..]),
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
agentkit doctor [--provider NAME] [--config DIR] [--conformance] [--json]
                                    exit non-zero when a provider has drifted
agentkit doctor --accept-guards     owner only: accept the site's [guards] (disabled guards warn, not fail)
agentkit context                    time, capacity, unread mail, fleet alerts
agentkit hook <claude|codex|cursor|agy> <guard|all>
                                    provider adapter: tool-call JSON on stdin; `all` runs every enabled guard
agentkit memory import --from DIR   import Claude memory files into the notebook
agentkit memory path                print the notebook directory
agentkit posture write [--out PATH] refresh ~/.local/state/ai-budget/posture.json from ai-capacity"
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
    let conformance_only = args.contains(&"--conformance");
    let json = args.contains(&"--json");
    let paths = paths_from(args);
    if args.contains(&"--accept-guards") {
        // The owner's step only; the guard-config guard refuses it from an agent.
        // Interactive only: a hook or an agent's shell has no terminal on both ends, and the
        // owner types the word himself (21 Nov 2026: the record was forgeable by any caller).
        let confirm = |change: &str| -> bool {
            // SAFETY: isatty only reads the descriptor's state.
            let tty = unsafe { libc::isatty(0) == 1 && libc::isatty(1) == 1 };
            if !tty {
                eprintln!("agentkit doctor --accept-guards: needs a terminal (the owner runs it himself)");
                return false;
            }
            print!("{change}Type accept to record this [guards] as the owner's: ");
            let _ = io::stdout().flush();
            let mut line = String::new();
            io::stdin().read_line(&mut line).is_ok() && line.trim() == "accept"
        };
        return match doctor::accept_guards(&paths, confirm) {
            Ok(text) => {
                print!("{text}");
                0
            }
            Err(e) => {
                eprintln!("agentkit doctor --accept-guards: {e}");
                2
            }
        };
    }
    match doctor::run(&paths, provider, conformance_only, json) {
        Ok((code, text)) => {
            if json {
                print!("{text}");
            } else {
                print!("{text}");
            }
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
        eprintln!("usage: agentkit hook <claude|codex|cursor|agy> <guard|all>");
        return 2;
    };
    if !matches!(provider, "claude" | "codex" | "cursor" | "agy") {
        eprintln!("agentkit hook: unknown provider {provider}");
        return 2;
    }
    if guard_name != "all" && !guard::ALL_GUARD_NAMES.contains(&guard_name) {
        eprintln!("agentkit hook: unknown guard {guard_name}");
        return 2;
    }
    let mut raw = String::new();
    if io::stdin().read_to_string(&mut raw).is_err() {
        return 0;
    }
    let paths = paths_from(args);
    let site = match site::Site::load(&paths.config) {
        Ok(s) => s,
        Err(_) => site::Site {
            schema: 1,
            manage_model: false,
            providers: vec![],
            projects: vec![],
            retired_cursor_rules: vec![],
            guards: site::GuardFile::default(),
            models: site::Models::default(),
        },
    };
    if guard_name != "all" && !site::guard_enabled(guard_name, &site.guards.disabled) {
        let _ = writeln!(io::stderr(), "agentkit: {guard_name} disabled (site)");
        return 0;
    }
    let cfg = site::guard_config_from_env();
    let enabled: Vec<&str> = guard::ALL_GUARD_NAMES
        .iter()
        .copied()
        .filter(|n| site::guard_enabled(n, &site.guards.disabled))
        .collect();
    let decided = guard_fail_closed(|| {
        if guard_name == "all" {
            let value: serde_json::Value = if raw.trim().is_empty() {
                serde_json::Value::Null
            } else {
                match serde_json::from_str(&raw) {
                    Ok(v) => v,
                    Err(_) => return None,
                }
            };
            Some(guard::decide_all(&value, &cfg, &enabled))
        } else {
            guard::decide_json(guard_name, &raw, &cfg).ok()
        }
    });
    let Some(decision) = decided else { return 0 };
    let rendered = match provider {
        "claude" | "codex" => guard::format_claude(&decision),
        "cursor" => guard::format_cursor(&decision),
        "agy" => guard::format_agy(&decision),
        other => {
            eprintln!("agentkit hook: unknown provider {other}");
            return 2;
        }
    };
    {
        let mut out = io::stdout().lock();
        let _ = out.write_all(rendered.as_bytes());
        let _ = out.flush();
    }
    // After the answer is out: the ledger can only add time, never change the decision.
    let value: serde_json::Value = serde_json::from_str(&raw).unwrap_or(serde_json::Value::Null);
    ledger::record(provider, guard_name, &value, &decision, &paths.state);
    0
}

fn cmd_posture(args: &[&str]) -> i32 {
    match args.first().copied() {
        Some("write") => {
            let paths = paths_from(args);
            let out = flag(args, "--out")
                .map(PathBuf::from)
                .unwrap_or_else(|| posture::default_posture_path(&paths.home));
            let cap = env::var_os("AGENTKIT_AI_CAPACITY")
                .map(PathBuf::from)
                .unwrap_or_else(|| paths.home.join(".local/bin/ai-capacity"));
            match posture::refresh_from_ai_capacity(&paths.home, &cap, &out) {
                Ok(doc) => {
                    println!(
                        "agentkit posture: wrote {} (asOf {}, route build {})",
                        out.display(),
                        doc.as_of,
                        doc.route.build
                    );
                    0
                }
                Err(e) => {
                    eprintln!("agentkit posture write: {e}");
                    1
                }
            }
        }
        _ => {
            eprintln!("usage: agentkit posture write [--out PATH]");
            2
        }
    }
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
    let state = ledger::default_state_dir(&home);
    Paths { home, config, state }
}

/// Run a guard decision; a panic anywhere inside it is a deny, never a silent allow (a hook
/// that crashes is treated by the providers as no answer, which lets the call through).
pub fn guard_fail_closed<F: FnOnce() -> Option<guard::Decision>>(f: F) -> Option<guard::Decision> {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        #[cfg(debug_assertions)]
        if env::var("AGENTKIT_TEST_PANIC").ok().as_deref() == Some("1") {
            panic!("test panic");
        }
        f()
    }));
    std::panic::set_hook(previous);
    match result {
        Ok(d) => d,
        Err(_) => Some(guard::Decision::deny(
            "guard-panic",
            "the guard crashed on this command, so it is refused (fail closed). Rephrasing will not help; report the command to the owner so the guard can be fixed.",
        )),
    }
}

pub fn guard_main(name: &str) -> i32 {
    let mut raw = String::new();
    if io::stdin().read_to_string(&mut raw).is_err() {
        return 0;
    }
    let cfg = site::guard_config_from_env();
    let Some(decision) = guard_fail_closed(|| guard::decide_json(name, &raw, &cfg).ok()) else { return 0 };
    println!("{}", guard::format_neutral(&decision));
    let _ = io::stdout().flush();
    let value: serde_json::Value = serde_json::from_str(&raw).unwrap_or(serde_json::Value::Null);
    ledger::record("bin", name, &value, &decision, &paths_from(&[]).state);
    0
}

pub fn context_main() {
    print!("{}", context::render_default());
    let _ = process::id();
}
