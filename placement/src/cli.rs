//! `plan` prints. `apply` moves, on this Mac only. `status` writes the JSON the rest of the fleet reads.

use crate::apply::{ApplyOpts, apply};
use crate::eval::evaluate;
use crate::init::{fleet_policy, host_id_from_diskmap};
use crate::model::{load_policy, render_policy, ActionKind};
use crate::status::{format_plan, format_status, status_file};
use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn run(args: Vec<String>) -> i32 {
    let mut args = args;
    if args.len() <= 1 {
        eprintln!("{}", usage());
        return 2;
    }
    let cmd = args[1].clone();
    let rest = args.split_off(2);
    match cmd.as_str() {
        "plan" => cmd_plan(&rest),
        "apply" => cmd_apply(&rest),
        "status" => cmd_status(&rest),
        "init" => cmd_init(&rest),
        "help" | "-h" | "--help" => {
            println!("{}", usage());
            0
        }
        other => {
            eprintln!("unknown command {other}\n{}", usage());
            2
        }
    }
}

fn usage() -> &'static str {
    "placement plan [--config FILE] [--diskmap DIR] [--replicas FILE] [--json]\n\
     placement apply [--config FILE] [--diskmap DIR] [--host ID] [--home DIR] [--trash DIR] [--ledger FILE] [--volumes DIR]\n\
     placement status [--config FILE] [--diskmap DIR] [--out FILE] [--json]\n\
     placement init --from-drive-registry [--drives FILE] [--datasets FILE] [--out FILE]"
}

struct Flags {
    config: Option<PathBuf>,
    diskmap: Option<PathBuf>,
    replicas: Option<PathBuf>,
    host: Option<String>,
    home: Option<PathBuf>,
    trash: Option<PathBuf>,
    ledger: Option<PathBuf>,
    volumes: Option<PathBuf>,
    out: Option<PathBuf>,
    drives: Option<PathBuf>,
    datasets: Option<PathBuf>,
    json: bool,
    from_external: bool,
}

fn flags(args: &[String]) -> Result<Flags, String> {
    let mut out = Flags {
        config: None,
        diskmap: None,
        replicas: None,
        host: None,
        home: None,
        trash: None,
        ledger: None,
        volumes: None,
        out: None,
        drives: None,
        datasets: None,
        json: false,
        from_external: false,
    };
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        let val = || -> Result<String, String> {
            args.get(i + 1)
                .cloned()
                .ok_or_else(|| format!("missing value for {a}"))
        };
        match a.as_str() {
            "--json" => out.json = true,
            "--from-drive-registry" => out.from_external = true,
            "--config" => {
                out.config = Some(PathBuf::from(val()?));
                i += 1;
            }
            "--diskmap" => {
                out.diskmap = Some(PathBuf::from(val()?));
                i += 1;
            }
            "--replicas" => {
                out.replicas = Some(PathBuf::from(val()?));
                i += 1;
            }
            "--host" => {
                out.host = Some(val()?);
                i += 1;
            }
            "--home" => {
                out.home = Some(PathBuf::from(val()?));
                i += 1;
            }
            "--trash" => {
                out.trash = Some(PathBuf::from(val()?));
                i += 1;
            }
            "--ledger" => {
                out.ledger = Some(PathBuf::from(val()?));
                i += 1;
            }
            "--volumes" => {
                out.volumes = Some(PathBuf::from(val()?));
                i += 1;
            }
            "--out" => {
                out.out = Some(PathBuf::from(val()?));
                i += 1;
            }
            "--drives" => {
                out.drives = Some(PathBuf::from(val()?));
                i += 1;
            }
            "--datasets" => {
                out.datasets = Some(PathBuf::from(val()?));
                i += 1;
            }
            other => return Err(format!("unknown flag {other}")),
        }
        i += 1;
    }
    Ok(out)
}

fn cmd_plan(args: &[String]) -> i32 {
    let flags = match flags(args) {
        Ok(f) => f,
        Err(err) => {
            eprintln!("placement: {err}");
            return 2;
        }
    };
    let policy = match read_policy(flags.config.as_deref()) {
        Ok(p) => p,
        Err(err) => {
            eprintln!("placement: {err}");
            return 1;
        }
    };
    let obs = observation(&flags);
    let plan = evaluate(&policy, &obs);
    if flags.json {
        match serde_json::to_string_pretty(&plan_json(&plan)) {
            Ok(text) => println!("{text}"),
            Err(err) => {
                eprintln!("placement: {err}");
                return 1;
            }
        }
    } else {
        print!("{}", format_plan(&plan.actions));
    }
    0
}

fn cmd_status(args: &[String]) -> i32 {
    let flags = match flags(args) {
        Ok(f) => f,
        Err(err) => {
            eprintln!("placement: {err}");
            return 2;
        }
    };
    let policy = match read_policy(flags.config.as_deref()) {
        Ok(p) => p,
        Err(err) => {
            eprintln!("placement: {err}");
            return 1;
        }
    };
    let now = unix_now();
    let obs = observation(&flags);
    let plan = evaluate(&policy, &obs);
    let mut doc = status_file(&plan, now);
    doc.peers = obs.peers.clone();
    let out = flags.out.unwrap_or_else(default_status_path);
    if let Some(parent) = out.parent() {
        if !parent.as_os_str().is_empty() {
            if let Err(err) = fs::create_dir_all(parent) {
                eprintln!("placement: {err}");
                return 1;
            }
        }
    }
    let json = match serde_json::to_string_pretty(&doc) {
        Ok(text) => text + "\n",
        Err(err) => {
            eprintln!("placement: {err}");
            return 1;
        }
    };
    if let Err(err) = fs::write(&out, &json) {
        eprintln!("placement: {err}");
        return 1;
    }
    if flags.json {
        print!("{json}");
    } else {
        print!("{}", format_status(&plan));
        print!("{}", crate::status::format_peers(&obs.peers, now));
    }
    0
}

fn cmd_apply(args: &[String]) -> i32 {
    let flags = match flags(args) {
        Ok(f) => f,
        Err(err) => {
            eprintln!("placement: {err}");
            return 2;
        }
    };
    let policy = match read_policy(flags.config.as_deref()) {
        Ok(p) => p,
        Err(err) => {
            eprintln!("placement: {err}");
            return 1;
        }
    };
    let obs = observation(&flags);
    let host = match resolve_host(&policy, flags.host.as_deref(), flags.diskmap.as_deref()) {
        Ok(h) => h,
        Err(err) => {
            eprintln!("placement: {err}");
            return 2;
        }
    };
    let plan = evaluate(&policy, &obs);
    for action in plan.actions.iter().filter(|action| action.host == host) {
        if action.kind != ActionKind::Move {
            continue;
        }
        if let Some(line) = cowork_refuses(&host, &action.dest) {
            eprintln!("{line}");
            return 75;
        }
    }
    let home = flags.home.clone().unwrap_or_else(home_dir);
    let mut opts = ApplyOpts::new(home, host);
    opts.trash = flags.trash.unwrap_or_else(|| home_dir().join(".Trash"));
    opts.ledger = flags.ledger.unwrap_or_else(default_ledger);
    opts.volume_root = flags.volumes.unwrap_or_else(|| PathBuf::from("/Volumes"));
    opts.metered = metered_link();
    opts.focus_active = focus_active();
    opts.now = unix_now();
    opts.hostname = local_hostname();
    opts.replica_log = Some(flags.replicas.clone().unwrap_or_else(default_replicas));
    match apply(&policy, &obs, &plan, &opts) {
        Ok(report) => {
            print!(
                "{}",
                format_plan(
                    &plan
                        .actions
                        .iter()
                        .filter(|a| a.host == opts.host_id)
                        .cloned()
                        .collect::<Vec<_>>()
                )
            );
            for err in &report.errors {
                eprintln!("placement: {err}");
            }
            if report.deferred {
                eprintln!("placement: deferred ({})", report.defer_reason);
                return 75;
            }
            if !report.errors.is_empty() {
                return 1;
            }
            0
        }
        Err(err) => {
            eprintln!("placement: {err}");
            1
        }
    }
}

fn cmd_init(args: &[String]) -> i32 {
    let flags = match flags(args) {
        Ok(f) => f,
        Err(err) => {
            eprintln!("placement: {err}");
            return 2;
        }
    };
    if !flags.from_external {
        eprintln!(
            "placement: init requires --from-drive-registry\n{}",
            usage()
        );
        return 2;
    }
    let drives_path = flags.drives.clone().unwrap_or_else(default_drives);
    let datasets_path = flags.datasets.clone().unwrap_or_else(|| {
        drives_path
            .parent()
            .unwrap_or(std::path::Path::new("."))
            .join("datasets.json")
    });
    let drives = match fs::read_to_string(&drives_path) {
        Ok(t) => t,
        Err(err) => {
            eprintln!("placement: {}: {err}", drives_path.display());
            return 1;
        }
    };
    let datasets = match fs::read_to_string(&datasets_path) {
        Ok(t) => t,
        Err(err) => {
            eprintln!("placement: {}: {err}", datasets_path.display());
            return 1;
        }
    };
    let policy = match fleet_policy(&drives, &datasets) {
        Ok(p) => p,
        Err(err) => {
            eprintln!("placement: {err}");
            return 1;
        }
    };
    let text = match render_policy(&policy) {
        Ok(t) => t,
        Err(err) => {
            eprintln!("placement: {err}");
            return 1;
        }
    };
    let out = flags.out.unwrap_or_else(default_config_path);
    if let Some(parent) = out.parent() {
        if !parent.as_os_str().is_empty() {
            if let Err(err) = fs::create_dir_all(parent) {
                eprintln!("placement: {err}");
                return 1;
            }
        }
    }
    if let Err(err) = fs::write(&out, text) {
        eprintln!("placement: {err}");
        return 1;
    }
    println!("{}", out.display());
    0
}

fn observation(flags: &Flags) -> crate::model::Observation {
    let dir = flags.diskmap.clone().unwrap_or_else(default_diskmap);
    let replicas = flags.replicas.clone().unwrap_or_else(default_replicas);
    let mut obs = crate::observe::load_observation_with_peers(&dir, Some(&replicas), unix_now(), &peer_trust());
    if obs.now == 0 {
        obs.now = unix_now();
    }
    obs
}

/// This Mac's id from the diskmap host map (or PLACEMENT_HOST), and the peers it may read.
/// No id, no peers: fail closed rather than guess which rows are this Mac's.
fn peer_trust() -> crate::observe::PeerTrust {
    let text = fs::read_to_string(diskmap_config_path()).unwrap_or_default();
    let self_id = env::var("PLACEMENT_HOST")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| crate::init::host_id_from_diskmap(&text, &local_hostname()))
        .unwrap_or_default();
    if self_id.is_empty() {
        return crate::observe::PeerTrust::default();
    }
    let peers = crate::init::peers_from_diskmap(&text, &self_id);
    crate::observe::PeerTrust { self_id, peers }
}

fn read_policy(path: Option<&std::path::Path>) -> Result<crate::model::Policy, String> {
    let path = path.map(PathBuf::from).unwrap_or_else(default_config_path);
    let text = fs::read_to_string(&path).map_err(|_| {
        format!(
            "no policy at {} — run `placement init --from-drive-registry`",
            path.display()
        )
    })?;
    load_policy(&text)
}

pub fn resolve_host(
    policy: &crate::model::Policy,
    explicit: Option<&str>,
    diskmap_dir: Option<&std::path::Path>,
) -> Result<String, String> {
    if let Some(id) = explicit {
        return Ok(id.to_string());
    }
    if let Ok(id) = env::var("PLACEMENT_HOST") {
        if !id.is_empty() {
            return Ok(id);
        }
    }
    let diskmap = diskmap_dir
        .map(PathBuf::from)
        .unwrap_or_else(default_diskmap);
    let config = diskmap_config_path();
    if let Ok(text) = fs::read_to_string(&config) {
        let hostname = local_hostname();
        if let Some(id) = host_id_from_diskmap(&text, &hostname) {
            if policy.hosts.iter().any(|h| h.id == id) {
                return Ok(id);
            }
        }
    }
    let _ = diskmap;
    if policy.hosts.len() == 1 {
        return Ok(policy.hosts[0].id.clone());
    }
    Err("pass --host; this Mac is not named in the diskmap config".into())
}

fn plan_json(plan: &crate::model::Plan) -> serde_json::Value {
    serde_json::json!({
        "actions": plan.actions.iter().map(|a| serde_json::json!({
            "host": a.host,
            "action": a.kind.as_str(),
            "path": a.path,
            "bytes": a.bytes,
            "dest": a.dest,
            "reason": a.reason,
            "class": a.class.as_str(),
        })).collect::<Vec<_>>(),
        "violations": plan.violations,
    })
}

/// `cowork check` for a volume this apply would write. Missing cowork does not block;
/// a holder does. Placement's own `.writer` check still runs inside apply.
fn cowork_refuses(host: &str, volume: &str) -> Option<String> {
    if env::var("PLACEMENT_SKIP_COWORK").ok().as_deref() == Some("1") {
        return None;
    }
    if volume.is_empty() {
        return None;
    }
    let scope = format!("drive:{host}:/Volumes/{volume}");
    let bin = env::var("COWORK_BIN").unwrap_or_else(|_| "cowork".to_string());
    let output = Command::new(bin).arg("check").arg(scope).output().ok()?;
    if output.status.code() != Some(1) {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let line = text
        .lines()
        .find(|line| line.starts_with("held by"))
        .unwrap_or("")
        .trim()
        .to_string();
    if line.is_empty() { None } else { Some(line) }
}

fn focus_active() -> bool {
    if env::var("PLACEMENT_FORCE").ok().as_deref() == Some("1") {
        return false;
    }
    match env::var("PLACEMENT_FOCUS") {
        Ok(v) => return v == "active",
        Err(_) => {}
    }
    match std::process::Command::new("focus-guard")
        .arg("active")
        .status()
    {
        Ok(status) => status.success(),
        Err(_) => false,
    }
}

fn metered_link() -> bool {
    if let Ok(path) = env::var("PLACEMENT_METERED") {
        return PathBuf::from(path).exists();
    }
    default_state_root().join("METERED_LINK").exists()
}

fn local_hostname() -> String {
    env::var("PLACEMENT_HOSTNAME").unwrap_or_else(|_| {
        std::process::Command::new("scutil")
            .arg("--get")
            .arg("LocalHostName")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_default()
    })
}

fn unix_now() -> i64 {
    if let Ok(v) = env::var("PLACEMENT_NOW") {
        if let Ok(n) = v.parse() {
            return n;
        }
    }
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn home_dir() -> PathBuf {
    env::var_os("PLACEMENT_HOME")
        .or_else(|| env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn default_state_root() -> PathBuf {
    home_dir().join(".local/state")
}

fn default_config_path() -> PathBuf {
    env::var_os("PLACEMENT_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".config/fleet/placement.toml"))
}

fn default_diskmap() -> PathBuf {
    env::var_os("DISKMAP_STATE")
        .map(PathBuf::from)
        .unwrap_or_else(|| default_state_root().join("diskmap"))
}

fn default_status_path() -> PathBuf {
    env::var_os("PLACEMENT_STATE")
        .map(PathBuf::from)
        .unwrap_or_else(|| default_state_root().join("placement"))
        .join("status.json")
}

fn default_replicas() -> PathBuf {
    default_status_path()
        .parent()
        .unwrap_or(std::path::Path::new("."))
        .join("replicas.json")
}

fn default_ledger() -> PathBuf {
    env::var_os("PLACEMENT_LEDGER")
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".config/fleet/placement-ledger.md"))
}

fn default_drives() -> PathBuf {
    env::var_os("PLACEMENT_DRIVES")
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".config/fleet/drives.json"))
}

fn diskmap_config_path() -> PathBuf {
    env::var_os("DISKMAP_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".config/diskmap/config.json"))
}

#[cfg(test)]
mod tests {
    use super::cowork_refuses;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    struct ClearEnv(&'static str);
    impl Drop for ClearEnv {
        fn drop(&mut self) {
            // Single-threaded test (`--test-threads=1`). The var is process-global.
            unsafe { std::env::remove_var(self.0) }
        }
    }

    #[test]
    fn a_held_volume_is_the_cowork_line_and_a_missing_binary_is_not() {
        let dir = std::env::temp_dir().join(format!("cowork-place-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("cowork");
        fs::write(
            &bin,
            "#!/bin/sh\nif [ \"$1\" = check ] && [ \"$2\" = \"drive:primary:/Volumes/media\" ]; then\n  echo 'held by claude · glancebar-62 (claude, primary) — ask: cowork ask music-offload \"…\"'\n  exit 1\nfi\nexit 0\n",
        )
        .unwrap();
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
        let _guard = ClearEnv("COWORK_BIN");
        unsafe { std::env::set_var("COWORK_BIN", &bin) }
        let line = cowork_refuses("primary", "media").expect("holder");
        assert!(line.starts_with("held by claude"));
        assert!(line.contains("cowork ask music-offload"));
        assert!(cowork_refuses("primary", "other").is_none());
        unsafe { std::env::set_var("COWORK_BIN", dir.join("missing")) }
        assert!(cowork_refuses("primary", "media").is_none());
        let _ = fs::remove_dir_all(&dir);
    }
}
