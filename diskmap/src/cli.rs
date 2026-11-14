//! Command line. `json` is local; `fleet` is what asks peers.

use crate::archive;
use crate::logic;
use crate::store;
use crate::engine;
use crate::summary::{self, Summary, TopEntry, TopMap};
use crate::tick::{self, Roots};
use std::fs;
use std::path::Path;
use std::process::Command;

pub fn run(args: Vec<String>) -> i32 {
    let mut args = args;
    if args.len() <= 1 {
        eprintln!("{}", usage());
        return 2;
    }
    let cmd = args[1].clone();
    let rest = args.split_off(2);
    match cmd.as_str() {
        "fleet" => cmd_fleet(&rest),
        "grew" => cmd_grew(&rest),
        "top" => cmd_top(&rest),
        "history" => cmd_history(&rest),
        "where" => cmd_where(&rest),
        "json" => cmd_json(),
        "tick" => cmd_tick(),
        "walk" => cmd_walk(),
        "prune" => match crate::tick::prune_now(&Roots::from_env()) {
            Ok(rows) => {
                for (name, removed) in rows {
                    println!("{name}: {removed} deleted directories dropped from the map");
                }
                0
            }
            Err(err) => {
                eprintln!("diskmap: {err}");
                1
            }
        },
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
    "diskmap fleet | grew [--host H|all] [--since 24h] | top [path] | history <path> [--host H] | where <name|glob> [--json] | json | tick"
}

fn cmd_tick() -> i32 {
    let roots = Roots::from_env();
    match tick::tick(&roots) {
        Ok(_) => 0,
        Err(err) => {
            eprintln!("diskmap: {err}");
            1
        }
    }
}

fn cmd_walk() -> i32 {
    let roots = Roots::from_env();
    let lock = match tick::hold_walk_lock(&roots.state.join("walk.lock")) {
        Ok(Some(file)) => file,
        Ok(None) => return 0,
        Err(err) => {
            eprintln!("diskmap: {err}");
            return 1;
        }
    };
    crate::darwin::be_gentle();
    unsafe {
        libc::setpriority(libc::PRIO_PROCESS, 0, 10);
    }
    let result = tick::walk_worker(&roots);
    drop(lock);
    match result {
        Ok(()) => 0,
        Err(err) => {
            eprintln!("diskmap: {err}");
            1
        }
    }
}

fn cmd_json() -> i32 {
    let roots = Roots::from_env();
    match tick::rebuild_summary(&roots) {
        Ok(summary) => {
            print!("{}", summary::to_json(&summary));
            0
        }
        Err(err) => {
            eprintln!("diskmap: {err}");
            1
        }
    }
}

fn cmd_fleet(args: &[String]) -> i32 {
    let roots = Roots::from_env();
    let local = match load_or_build(&roots) {
        Ok(s) => s,
        Err(err) => {
            eprintln!("diskmap: {err}");
            return 1;
        }
    };
    let mut summaries = vec![local];
    let (cfg, host) = tick::load(&roots);
    if !args.iter().any(|a| a == "--local") && std::env::var("DISKMAP_NO_SSH").ok().as_deref() != Some("1") {
        for peer in &cfg.peers {
            if peer == &host.id || peer == &host.local_host_name {
                continue;
            }
            match fetch_peer(&roots, peer) {
                Ok(summary) => summaries.push(summary),
                Err(err) => eprintln!("diskmap: {peer}: {err}"),
            }
        }
    }
    let lines = summary::fleet_lines(&summaries, tick::unix_now());
    print!("{}", summary::format_fleet(&lines));
    0
}

fn load_or_build(roots: &Roots) -> Result<Summary, String> {
    let path = roots.state.join("summary.json");
    if let Ok(text) = fs::read_to_string(&path) {
        if let Ok(summary) = serde_json::from_str::<Summary>(&text) {
            let age = tick::unix_now() - summary.generated;
            if age >= 0 && age < 90 {
                return Ok(summary);
            }
        }
    }
    tick::rebuild_summary(roots)
}

fn fetch_peer(roots: &Roots, peer: &str) -> Result<Summary, String> {
    let cache = roots.state.join("peers").join(format!("{peer}.json"));
    let out = Command::new("ssh")
        .args([
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=5",
            peer,
            "diskmap json",
        ])
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        if let Ok(summary) = serde_json::from_slice::<Summary>(&out.stdout) {
            if let Some(parent) = cache.parent() {
                let _ = fs::create_dir_all(parent);
            }
            let _ = fs::write(&cache, &out.stdout);
            return Ok(summary);
        }
    }
    if let Ok(text) = fs::read_to_string(&cache) {
        if let Ok(summary) = serde_json::from_str(&text) {
            return Ok(summary);
        }
    }
    Err("unreachable".into())
}

fn cmd_grew(args: &[String]) -> i32 {
    let since = flag_value(args, "--since").unwrap_or("24h");
    let secs = match logic::parse_since(since) {
        Ok(s) => s,
        Err(err) => {
            eprintln!("diskmap: {err}");
            return 2;
        }
    };
    let host = flag_value(args, "--host").unwrap_or("all");
    let local = !args.iter().any(|a| a == "--remote-ask");
    let roots = Roots::from_env();
    let (_, me) = tick::load(&roots);
    if host != "all" && host != me.id && host != me.local_host_name && local {
        return ssh_print(&host, &format!("diskmap grew --since {since} --host {host} --remote-ask"));
    }
    let now = tick::unix_now();
    let report = tick::grew_report(&roots, now - secs, since);
    for hit in &report.hits {
        println!(
            "{:<4}  {:<16}  {:<24}  {:>10}  since {}",
            me.id,
            hit.volume,
            hit.path,
            logic::fmt_gb_signed(hit.delta),
            hit.since_label
        );
    }
    if let Some(banner) = &report.banner {
        println!("{banner}");
    }
    0
}

fn cmd_top(args: &[String]) -> i32 {
    let path = args.iter().find(|a| !a.starts_with('-')).map(|s| logic::norm_rel(s)).unwrap_or_default();
    let roots = Roots::from_env();
    let Ok(rd) = fs::read_dir(&roots.state) else {
        return 0;
    };
    let mut maps = Vec::new();
    let mut entries = Vec::new();
    for ent in rd.flatten() {
        let db_path = ent.path();
        if db_path.extension().and_then(|e| e.to_str()) != Some("db") {
            continue;
        }
        let Ok(db) = store::open(&db_path) else { continue };
        let name = store::meta_get(&db, "name").ok().flatten().unwrap_or_else(|| "volume".into());
        if let Ok(progress) = engine::map_progress(&db) {
            maps.push(TopMap {
                volume: name.clone(),
                dirs: progress.dirs,
                percent: progress.percent,
                done: progress.done,
            });
        }
        let Ok(kids) = store::top_children(&db, &path, 40) else { continue };
        for kid in kids {
            entries.push(TopEntry {
                volume: name.clone(),
                path: kid.path,
                bytes: kid.total_bytes,
                partial: kid.partial || kid.scanned_at == 0,
            });
        }
    }
    print!("{}", summary::format_top(&maps, &entries));
    0
}

fn cmd_history(args: &[String]) -> i32 {
    let path = args.iter().find(|a| !a.starts_with('-')).cloned();
    let Some(path) = path else {
        eprintln!("diskmap history <path>");
        return 2;
    };
    let path = logic::norm_rel(&path);
    let host = flag_value(args, "--host");
    let roots = Roots::from_env();
    let (_, me) = tick::load(&roots);
    if let Some(host) = host {
        if host != "all" && host != me.id && host != me.local_host_name && !args.iter().any(|a| a == "--remote-ask") {
            return ssh_print(&host, &format!("diskmap history {path} --host {host} --remote-ask"));
        }
    }
    let mut rows = Vec::new();
    if let Ok(rd) = fs::read_dir(&roots.state) {
        for ent in rd.flatten() {
            let db_path = ent.path();
            if db_path.extension().and_then(|e| e.to_str()) != Some("db") {
                continue;
            }
            if let Ok(db) = store::open(&db_path) {
                if let Ok(local) = store::history_for_path(&db, &path) {
                    rows.extend(local);
                }
            }
        }
    }
    let (cfg, _) = tick::load(&roots);
    let archive_path = format!("/Volumes/{}/{}", cfg.archive_volume, cfg.archive_rel);
    rows.extend(archive::history_for(Path::new(&archive_path), host, &path));
    rows.sort_by(|a, b| a.day.cmp(&b.day));
    rows.dedup_by(|a, b| a.day == b.day && a.path == b.path);
    for row in rows {
        println!("{:<12}  {:>10}  {}", row.day, logic::fmt_gb(row.total_bytes), row.path);
    }
    0
}

fn cmd_where(args: &[String]) -> i32 {
    let json = args.iter().any(|a| a == "--json");
    let pattern = args.iter().find(|a| !a.starts_with('-')).cloned();
    let Some(pattern) = pattern else {
        eprintln!("diskmap where <name|glob>");
        return 2;
    };
    let roots = Roots::from_env();
    let (_, me) = tick::load(&roots);
    let mut hits = Vec::new();
    if let Ok(text) = fs::read_to_string(roots.state.join("where.json")) {
        if let Ok(file) = serde_json::from_str::<summary::WhereFile>(&text) {
            hits.extend(file.matches);
        }
    }
    if !args.iter().any(|a| a == "--remote-ask") && std::env::var("DISKMAP_NO_SSH").ok().as_deref() != Some("1") {
        let (cfg, _) = tick::load(&roots);
        for peer in cfg.peers {
            if peer == me.id || peer == me.local_host_name {
                continue;
            }
            if let Ok(out) = Command::new("ssh")
                .args([
                    "-o",
                    "BatchMode=yes",
                    "-o",
                    "ConnectTimeout=5",
                    &peer,
                    &format!("diskmap where {pattern} --json --remote-ask"),
                ])
                .output()
            {
                if let Ok(more) = serde_json::from_slice::<Vec<summary::WhereHit>>(&out.stdout) {
                    hits.extend(more);
                }
            }
        }
    }
    hits.retain(|hit| {
        logic::where_match(&pattern, &hit.path)
            || hit.home_path.as_deref().is_some_and(|p| logic::where_match(&pattern, p))
    });
    if json {
        println!("{}", serde_json::to_string(&hits).unwrap_or_else(|_| "[]".into()));
        return 0;
    }
    let now = tick::unix_now();
    for hit in hits {
        println!(
            "{:<4}  {:<16}  {:<40}  {:>10}  {}",
            hit.host,
            hit.location,
            hit.home_path.as_deref().filter(|p| !p.is_empty()).unwrap_or(&hit.path),
            logic::fmt_gb(hit.bytes as i64),
            logic::fmt_age(now, hit.as_of)
        );
    }
    0
}

fn flag_value<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).map(|s| s.as_str())
}

fn ssh_print(host: &str, command: &str) -> i32 {
    match Command::new("ssh")
        .args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=5", host, command])
        .status()
    {
        Ok(status) => status.code().unwrap_or(1),
        Err(err) => {
            eprintln!("diskmap: {host}: {err}");
            1
        }
    }
}
