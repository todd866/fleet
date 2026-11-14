//! Shared notebook: INDEX.md plus one file per fact.
//! Import reads Claude's `projects/<id>/memory/*.md` and skips secret-shaped files.

use std::fs;
use std::path::{Path, PathBuf};

const SKIP_NAME: &[&str] = &["secret", "credential", "password", "token"];
const SECRET_BODY: &[&str] = &["api_key", "BEGIN PRIVATE", "AKIA", "sk-"];

pub fn import_claude(from: &Path, notebook: &Path) -> Result<String, String> {
    let facts = notebook.join("facts");
    fs::create_dir_all(&facts).map_err(|e| e.to_string())?;
    let index_path = notebook.join("INDEX.md");
    if !index_path.exists() {
        fs::write(&index_path, "# Memory index\n\n").map_err(|e| e.to_string())?;
    }
    let mut index = fs::read_to_string(&index_path).unwrap_or_else(|_| "# Memory index\n\n".into());
    let mut imported = 0usize;
    let mut unchanged = 0usize;
    let mut skipped = 0usize;
    for file in claude_fact_files(from) {
        let stem = file.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
        if stem.is_empty() || stem == "MEMORY" {
            continue;
        }
        let lower = stem.to_ascii_lowercase();
        if SKIP_NAME.iter().any(|w| lower.contains(w)) {
            skipped += 1;
            continue;
        }
        let raw = fs::read_to_string(&file).map_err(|e| e.to_string())?;
        if SECRET_BODY.iter().any(|w| raw.contains(w)) {
            skipped += 1;
            continue;
        }
        let title = title_of(&raw, &stem);
        let summary = summary_of(&raw);
        let rel = file.strip_prefix(from).unwrap_or(&file).display().to_string();
        let body = format!(
            "---\nid: {stem}\ntitle: {title}\nsource: claude:{rel}\n---\n\n{}\n",
            raw.trim_end()
        );
        let dest = facts.join(format!("{stem}.md"));
        if dest.exists() && fs::read_to_string(&dest).unwrap_or_default() == body {
            unchanged += 1;
        } else if dest.exists() {
            // Keep the first import. A later Claude edit is a manual merge, not a silent overwrite.
            unchanged += 1;
        } else {
            fs::write(&dest, &body).map_err(|e| e.to_string())?;
            imported += 1;
        }
        let link = format!("facts/{stem}.md");
        if !index.contains(&link) {
            if !index.ends_with('\n') {
                index.push('\n');
            }
            index.push_str(&format!("- [{title}]({link}) — {summary}\n"));
        }
    }
    fs::write(&index_path, index).map_err(|e| e.to_string())?;
    Ok(format!("agentkit memory: imported {imported}, unchanged {unchanged}, skipped {skipped}\n"))
}

fn claude_fact_files(from: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    collect_memory_dir(&from.join("memory"), &mut out);
    if let Ok(rd) = fs::read_dir(from) {
        for ent in rd.flatten() {
            collect_memory_dir(&ent.path().join("memory"), &mut out);
        }
    }
    out.sort();
    out
}

fn collect_memory_dir(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for ent in rd.flatten() {
        let path = ent.path();
        if path.extension().and_then(|e| e.to_str()) == Some("md") {
            out.push(path);
        }
    }
}

fn title_of(raw: &str, stem: &str) -> String {
    if let Some(v) = front_value(raw, "name") {
        return v;
    }
    for line in raw.lines() {
        if let Some(rest) = line.strip_prefix("# ") {
            return rest.trim().to_string();
        }
    }
    stem.replace('-', " ")
}

fn summary_of(raw: &str) -> String {
    if let Some(v) = front_value(raw, "description") {
        return clip(&v);
    }
    for line in strip_front(raw).lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        return clip(t);
    }
    String::new()
}

fn front_value(raw: &str, key: &str) -> Option<String> {
    let text = raw.strip_prefix("---\n")?;
    let end = text.find("\n---")?;
    for line in text[..end].lines() {
        if let Some(rest) = line.strip_prefix(&format!("{key}:")) {
            let v = rest.trim().trim_matches('"').to_string();
            if !v.is_empty() {
                return Some(v);
            }
        }
    }
    None
}

fn strip_front(raw: &str) -> &str {
    if let Some(text) = raw.strip_prefix("---\n") {
        if let Some(end) = text.find("\n---\n") {
            return &text[end + 5..];
        }
    }
    raw
}

fn clip(s: &str) -> String {
    let mut t = s.trim().to_string();
    if t.len() > 120 {
        t.truncate(117);
        t.push_str("...");
    }
    t
}
