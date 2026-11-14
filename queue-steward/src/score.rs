use crate::focus::{active_project, focus_guard_active};
use crate::overrides::{
    OverrideEntry, hour_bucket, local_hour_weekday, read_overrides, weekday_class,
};
use crate::waiter::Waiter;
use std::path::Path;

#[derive(Debug, Clone, PartialEq)]
pub struct ScoreBreakdown {
    pub origin: f64,
    pub focus_project: f64,
    pub focus_keyboard: f64,
    pub short: f64,
    pub age: f64,
    pub override_bonus: f64,
    pub learned: f64,
}

impl ScoreBreakdown {
    pub fn total(&self) -> f64 {
        self.origin
            + self.focus_project
            + self.focus_keyboard
            + self.short
            + self.age
            + self.override_bonus
            + self.learned
    }
}

pub fn origin_points(origin: &str) -> f64 {
    match origin {
        "interactive" => 3.0,
        "agent" => 2.0,
        "scheduled" => 0.0,
        _ => 0.0,
    }
}

pub fn short_points(expected_s: Option<u32>, waiter_count: usize) -> f64 {
    if waiter_count < 2 {
        return 0.0;
    }
    match expected_s {
        Some(s) if s < 120 => 1.0,
        Some(s) if s > 1800 => -1.0,
        _ => 0.0,
    }
}

pub fn age_points(since: i64, now: i64) -> f64 {
    let waited = (now - since).max(0) as f64;
    let blocks = (waited / 300.0).floor();
    (blocks * 0.5).min(3.0)
}

pub fn override_points(waiter: &Waiter, overrides: &[OverrideEntry], now: i64) -> f64 {
    for o in overrides {
        if o.until <= now {
            continue;
        }
        if o.label == waiter.label || o.project == waiter.project {
            return 10.0;
        }
    }
    0.0
}

pub fn learned_points(waiter: &Waiter, overrides: &[OverrideEntry], now: i64) -> f64 {
    let cutoff = now - 30 * 24 * 3600;
    let (hour, wday) = local_hour_weekday(now);
    let bucket = hour_bucket(hour);
    let weekend = weekday_class(wday);
    let mut count = 0u32;
    for o in overrides {
        if o.at < cutoff {
            continue;
        }
        if o.project != waiter.project {
            continue;
        }
        if weekday_class(o.weekday) != weekend {
            continue;
        }
        if hour_bucket(o.hour) != bucket {
            continue;
        }
        count += 1;
    }
    (count as f64).min(3.0)
}

pub fn score_waiter(
    waiter: &Waiter,
    now: i64,
    state_dir: &Path,
    waiter_count: usize,
    active: Option<&str>,
    at_keyboard: bool,
    overrides: &[OverrideEntry],
) -> ScoreBreakdown {
    let origin = origin_points(&waiter.origin);
    let mut focus_project = 0.0;
    let mut focus_keyboard = 0.0;
    if active.is_some_and(|a| a == waiter.project) {
        focus_project = 3.0;
        if at_keyboard {
            focus_keyboard = 1.0;
        }
    }
    let short = short_points(waiter.expected_s, waiter_count);
    let age = age_points(waiter.since, now);
    let override_bonus = override_points(waiter, overrides, now);
    let learned = learned_points(waiter, overrides, now);
    let _ = (state_dir,);
    ScoreBreakdown {
        origin,
        focus_project,
        focus_keyboard,
        short,
        age,
        override_bonus,
        learned,
    }
}

pub fn score_all(waiters: &[Waiter], now: i64, state_dir: &Path) -> Vec<(Waiter, ScoreBreakdown)> {
    let active = active_project(now);
    let at_keyboard = focus_guard_active();
    let overrides = read_overrides(state_dir);
    let n = waiters.len();
    waiters
        .iter()
        .cloned()
        .map(|w| {
            let bd = score_waiter(
                &w,
                now,
                state_dir,
                n,
                active.as_deref(),
                at_keyboard,
                &overrides,
            );
            (w, bd)
        })
        .collect()
}

pub fn reason_for(bd: &ScoreBreakdown, waiter: &Waiter, active: Option<&str>) -> String {
    let mut parts: Vec<&str> = Vec::new();
    if bd.override_bonus > 0.0 {
        parts.push("override");
    }
    if bd.focus_project > 0.0 && active.is_some() {
        parts.push("active project");
    }
    if bd.focus_keyboard > 0.0 {
        parts.push("at keyboard");
    }
    match waiter.origin.as_str() {
        "interactive" => parts.push("interactive"),
        "agent" => parts.push("agent"),
        "scheduled" => parts.push("scheduled"),
        _ => {}
    }
    if bd.short > 0.0 {
        parts.push("short job");
    } else if bd.short < 0.0 {
        parts.push("long job");
    }
    if bd.age >= 1.0 {
        parts.push("waiting");
    }
    if bd.learned > 0.0 {
        parts.push("learned");
    }
    if parts.is_empty() {
        "default order".to_string()
    } else {
        parts.join(", ")
    }
}

pub fn format_score(s: f64) -> String {
    if (s - s.round()).abs() < 0.001 {
        format!("{}", s as i64)
    } else {
        format!("{s:.1}")
    }
}

pub fn waiting_minutes(since: i64, now: i64) -> u64 {
    ((now - since).max(0) as u64) / 60
}
