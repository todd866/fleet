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
agentkit sync [--apply]             dry-run (default); --apply writes provider files
agentkit doctor [--provider NAME] [--conformance] [--json]
                                    exit non-zero when a provider has drifted
  --home DIR [--config DIR] [--state DIR] [--ledger PATH|off]
                                    work in another tree (tests, trying a site out); never this
                                    account's home, and no environment variable places agentkit
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
    let Some(paths) = paths_or_complain(args) else { return 2 };
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
    let Some(paths) = paths_or_complain(args) else { return 2 };
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
    let Some(judged) = judge(guard_name, args) else { return 0 };
    let rendered = match provider {
        "claude" | "codex" => guard::format_claude(&judged.decision),
        "cursor" => guard::format_cursor(&judged.decision),
        _ => guard::format_agy(&judged.decision),
    };
    {
        let mut out = io::stdout().lock();
        let _ = out.write_all(rendered.as_bytes());
        let _ = out.flush();
    }
    // After the answer is out: the ledger can only add time, never change the decision.
    ledger::record(provider, guard_name, &judged.value, &judged.decision, judged.ledger.as_deref());
    0
}

/// A hook's answer, the call it was about (for the ledger) and where the ledger is.
struct Judged {
    decision: guard::Decision,
    value: serde_json::Value,
    ledger: Option<PathBuf>,
}

/// A refusal because the hook could not do its job, not because of what the command is.
fn cannot_judge(rule: &str, what: &str) -> guard::Decision {
    guard::Decision::deny(
        rule,
        format!("{what}, so the call is refused (fail closed). Rephrasing will not help; report this to the owner."),
    )
}

/// Read the tool call from stdin and decide it. Everything the hook needs and cannot get is a
/// deny that is recorded: its own paths, its input (unreadable, not UTF-8, not a JSON object),
/// the site config (present but unreadable or invalid). The providers read "no answer" as "no
/// objection", so until 28 Nov 2026 a tool call the JSON parser rejected (a lone surrogate
/// escape, 129 levels of nesting) was allowed and left no ledger line. `None` only for a single
/// guard the site has switched off.
fn judge(guard_name: &str, args: &[&str]) -> Option<Judged> {
    let paths = match paths_from(args) {
        Ok(p) => p,
        Err(e) => {
            return Some(Judged {
                decision: cannot_judge("guard-setup", &format!("agentkit cannot place its config and ledger ({e})")),
                value: serde_json::Value::Null,
                ledger: None,
            });
        }
    };
    let ledger = paths.ledger.clone();
    let mut bytes = Vec::new();
    let read = io::stdin().read_to_end(&mut bytes);
    let unread = |why: &str, text: String| Judged {
        decision: cannot_judge("guard-input", &format!("the guard could not read this tool call ({why})")),
        // What arrived, as the command, so the ledger shows it (redacted and cut like any other).
        value: serde_json::json!({ "command": text }),
        ledger: ledger.clone(),
    };
    if let Err(e) = read {
        return Some(unread(&format!("stdin: {e}"), String::from_utf8_lossy(&bytes).into_owned()));
    }
    let raw = match String::from_utf8(bytes) {
        Ok(raw) => raw,
        Err(e) => return Some(unread("it is not UTF-8", String::from_utf8_lossy(e.as_bytes()).into_owned())),
    };
    let value: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(e) => return Some(unread(&format!("it is not JSON: {e}"), raw)),
    };
    if !value.is_object() {
        return Some(unread("it is not a JSON object", raw));
    }
    let (site, cfg) = match site::load_for_hook(&paths) {
        Ok(loaded) => loaded,
        Err(e) => {
            return Some(Judged {
                decision: cannot_judge("guard-setup", &format!("agentkit's site config cannot be used ({e})")),
                value,
                ledger,
            });
        }
    };
    if guard_name != "all" && !site::guard_enabled(guard_name, &site.guards.disabled) {
        let _ = writeln!(io::stderr(), "agentkit: {guard_name} disabled (site)");
        return None;
    }
    let enabled: Vec<&str> = guard::ALL_GUARD_NAMES
        .iter()
        .copied()
        .filter(|n| site::guard_enabled(n, &site.guards.disabled))
        .collect();
    let decision = guard_fail_closed(|| {
        if guard_name == "all" {
            guard::decide_all(&value, &cfg, &enabled)
        } else {
            guard::decide_value(guard_name, &value, &cfg)
        }
    });
    Some(Judged { decision, value, ledger })
}

fn cmd_posture(args: &[&str]) -> i32 {
    match args.first().copied() {
        Some("write") => {
            let Some(paths) = paths_or_complain(args) else { return 2 };
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
    let Some(paths) = paths_or_complain(args) else { return 2 };
    match args.first().copied() {
        Some("path") => {
            println!("{}", paths.notebook().display());
            0
        }
        Some("import") => {
            let Some(from) = flag(args, "--from") else {
                eprintln!("usage: agentkit memory import --from DIR");
                return 2;
            };
            match memory::import_claude(PathBuf::from(from).as_path(), &paths.notebook())
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
    /// The decision ledger; `None` only when a test passed `--ledger off`.
    pub ledger: Option<PathBuf>,
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

/// Where agentkit reads and writes. The home is the account's, from the user database; the
/// config, state folder and ledger sit under it. No environment variable moves any of them
/// (until 28 Nov 2026 `HOME`, `AGENTKIT_CONFIG`, `AGENTKIT_STATE` and `AGENTKIT_DECISIONS`
/// did): a hook's environment is inherited from the agent it guards, so an agent could start a
/// nested agent, or set a Claude settings `env` block, and run with a config of its own that
/// switched every guard off and a ledger that recorded nothing.
///
/// The flags `--home`, `--config`, `--state` and `--ledger` place agentkit in another tree,
/// for the test suite and for trying a site out. The generated hook commands never carry
/// them. `--config`, `--state` and `--ledger` are refused unless `--home` names a folder that
/// is not the account's home: this account's provider files are only ever written from, and
/// its hooks only ever judged by, this account's own config (`agentkit sync --apply --config
/// DIR` used to rewrite the real hook files from any folder, one with every guard disabled
/// included).
fn paths_from(args: &[&str]) -> Result<Paths, String> {
    let account = site::account_home();
    let home = match flag(args, "--home") {
        Some(dir) => PathBuf::from(dir),
        None => account.clone().ok_or("this account has no home folder in the user database")?,
    };
    let same = |a: &std::path::Path, b: &std::path::Path| {
        a == b || matches!((std::fs::canonicalize(a), std::fs::canonicalize(b)), (Ok(x), Ok(y)) if x == y)
    };
    let own_home = account.as_deref().is_some_and(|acct| same(&home, acct));
    if own_home {
        let moved: Vec<&str> = ["--config", "--state", "--ledger"].into_iter().filter(|f| args.contains(f)).collect();
        // Naming the default again is harmless and stays allowed.
        let default_config = flag(args, "--config").is_some_and(|c| same(std::path::Path::new(c), &home.join(".config/agentkit")));
        if !moved.is_empty() && !(moved == ["--config"] && default_config) {
            return Err(format!(
                "{} would point agentkit at another config, state folder or ledger while it works on this account's own home; that is only allowed together with --home DIR for a folder that is not this home",
                moved.join(", ")
            ));
        }
    }
    let config = flag(args, "--config")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".config/agentkit"));
    let state = flag(args, "--state")
        .map(PathBuf::from)
        .unwrap_or_else(|| ledger::default_state_dir(&home));
    let ledger = match flag(args, "--ledger") {
        Some("off") => None,
        Some(path) => Some(PathBuf::from(path)),
        None => Some(ledger::ledger_path(&state)),
    };
    Ok(Paths { home, config, state, ledger })
}

fn paths_or_complain(args: &[&str]) -> Option<Paths> {
    match paths_from(args) {
        Ok(paths) => Some(paths),
        Err(e) => {
            eprintln!("agentkit: {e}");
            None
        }
    }
}

/// Run a guard decision; a panic anywhere inside it is a deny, never a silent allow (a hook
/// that crashes is treated by the providers as no answer, which lets the call through).
pub fn guard_fail_closed<F: FnOnce() -> guard::Decision>(f: F) -> guard::Decision {
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
        Err(_) => guard::Decision::deny(
            "guard-panic",
            "the guard crashed on this command, so it is refused (fail closed). Rephrasing will not help; report the command to the owner so the guard can be fixed.",
        ),
    }
}

/// The standalone `guard-*` programs: one guard, the neutral answer format. Input they cannot
/// read is a deny here too, and a guard the site switched off answers allow.
pub fn guard_main(name: &str) -> i32 {
    let args: Vec<String> = env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let judged = judge(name, &args).unwrap_or_else(|| Judged {
        decision: guard::Decision::allow(),
        value: serde_json::Value::Null,
        ledger: None,
    });
    println!("{}", guard::format_neutral(&judged.decision));
    let _ = io::stdout().flush();
    ledger::record("bin", name, &judged.value, &judged.decision, judged.ledger.as_deref());
    0
}

pub fn context_main() {
    print!("{}", context::render_default());
    let _ = process::id();
}
