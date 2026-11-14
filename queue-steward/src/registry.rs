use crate::waiter::Waiter;
use std::fs;
use std::path::Path;

pub fn pid_alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    unsafe { libc::kill(pid, 0) == 0 }
}

pub fn load_waiters(state_dir: &Path) -> Vec<Waiter> {
    let queue_dir = state_dir.join("queue");
    let Ok(entries) = fs::read_dir(&queue_dir) else {
        return Vec::new();
    };
    let mut waiters = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        if name == "overrides" {
            continue;
        }
        let Ok(raw) = fs::read_to_string(&path) else {
            continue;
        };
        let Ok(mut w) = serde_json::from_str::<Waiter>(&raw) else {
            continue;
        };
        if !pid_alive(w.pid) {
            // A dead `processing` entry is a job that was lost (its heavy-slot was killed, or
            // the Mac restarted). heavy-slot's `reap_lost` writes that to jobs.jsonl from this
            // file and then removes it; removing it here first would lose the record. It is not
            // a live entry either way, so it is neither counted nor picked.
            if w.state != "processing" {
                let _ = fs::remove_file(&path);
            }
            continue;
        }
        w.pid = name.parse().unwrap_or(w.pid);
        waiters.push(w);
    }
    waiters
}

pub fn write_waiter(state_dir: &Path, waiter: &Waiter) -> std::io::Result<()> {
    let queue_dir = state_dir.join("queue");
    fs::create_dir_all(&queue_dir)?;
    let path = Waiter::queue_path(state_dir, waiter.pid);
    let line = serde_json::to_string(waiter).expect("waiter encodes");
    fs::write(path, format!("{line}\n"))
}

pub fn remove_waiter(state_dir: &Path, pid: i32) {
    let path = Waiter::queue_path(state_dir, pid);
    let _ = fs::remove_file(path);
}
