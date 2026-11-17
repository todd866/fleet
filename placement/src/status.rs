//! Instrument text and the JSON document fleet-health and mac-registry read.

use crate::eval::fmt_bytes;
use crate::model::{Action, BudgetState, GB, HostBudget, PeerSeen, Plan, Violation};
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct StatusHost {
    pub id: String,
    pub role: String,
    pub free_bytes: Option<u64>,
    pub free_gb: Option<f64>,
    pub floor_gb: f64,
    pub band_low_gb: Option<f64>,
    pub band_high_gb: Option<f64>,
    pub state: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct StatusFile {
    pub generated: i64,
    pub ok: bool,
    pub hosts: Vec<StatusHost>,
    pub violations: Vec<Violation>,
    /// Other Macs' rows came from these caches; their ages say how current the gaps are.
    pub peers: Vec<PeerSeen>,
}

pub fn status_file(plan: &Plan, generated: i64) -> StatusFile {
    let hosts = plan
        .budgets
        .iter()
        .map(|b| StatusHost {
            id: b.host.clone(),
            role: b.role.as_str().into(),
            free_bytes: b.free_bytes,
            free_gb: b
                .free_bytes
                .map(|n| (n as f64 / GB as f64 * 10.0).round() / 10.0),
            floor_gb: b.floor_gb,
            band_low_gb: b.band_low_gb,
            band_high_gb: b.band_high_gb,
            state: b.state.as_str().into(),
        })
        .collect();
    let ok = plan.violations.is_empty()
        && plan.budgets.iter().all(|b| {
            !matches!(
                b.state,
                BudgetState::BelowFloor | BudgetState::BelowBand | BudgetState::Unknown
            )
        });
    StatusFile {
        generated,
        ok,
        hosts,
        violations: plan.violations.clone(),
        peers: Vec::new(),
    }
}

pub fn format_status(plan: &Plan) -> String {
    let mut rows: Vec<[String; 6]> = Vec::new();
    rows.push(row("", "host", "free", "floor", "band", "state"));
    for budget in &plan.budgets {
        rows.push(host_row(budget));
    }
    let mut text = render(&rows, &[false, false, true, true, true, false]);
    let mut grouped: Vec<(String, String, String)> = Vec::new();
    for viol in &plan.violations {
        let class = plan
            .actions
            .iter()
            .find(|a| a.label == viol.dataset || a.path == viol.dataset)
            .map(|a| a.class.as_str())
            .unwrap_or("—");
        if let Some(row) = grouped
            .iter_mut()
            .find(|(name, _, _)| name == &viol.dataset)
        {
            if !row.2.split(", ").any(|part| part == viol.detail) {
                row.2.push_str(", ");
                row.2.push_str(&viol.detail);
            }
        } else {
            grouped.push((viol.dataset.clone(), class.to_string(), viol.detail.clone()));
        }
    }
    if !grouped.is_empty() {
        let mut data: Vec<[String; 4]> = Vec::new();
        data.push(["", "data", "class", "issue"].map(str::to_string));
        for (name, class, detail) in grouped {
            data.push(["!".into(), name, class, detail]);
        }
        text.push('\n');
        text.push_str(&render(&data, &[false, false, false, false]));
    }
    text
}

/// One line: which peer caches fed the view and how old each is (measured on the peer).
pub fn format_peers(peers: &[PeerSeen], now: i64) -> String {
    if peers.is_empty() {
        return String::new();
    }
    let parts: Vec<String> = peers
        .iter()
        .map(|p| format!("{} {} rows {} old", p.host, p.rows, fmt_age(now, p.measured)))
        .collect();
    format!("\n   peers  {}\n", parts.join(" · "))
}

fn fmt_age(now: i64, then: i64) -> String {
    if then <= 0 {
        return "—".into();
    }
    let s = (now - then).max(0);
    if s < 90 * 60 {
        format!("{}m", s / 60)
    } else if s < 48 * 3600 {
        format!("{}h", s / 3600)
    } else {
        format!("{}d", s / 86_400)
    }
}

fn host_row(budget: &HostBudget) -> [String; 6] {
    let mark = match budget.state {
        BudgetState::BelowFloor => "!",
        BudgetState::BelowBand => "~",
        BudgetState::Unknown => "?",
        BudgetState::InBand | BudgetState::AboveBand => "*",
    };
    row(
        mark,
        &budget.host,
        &budget
            .free_bytes
            .map(fmt_bytes)
            .unwrap_or_else(|| "—".into()),
        &fmt_bytes(crate::model::gb_to_bytes(budget.floor_gb)),
        &band(budget),
        budget.state.label(),
    )
}

fn band(budget: &HostBudget) -> String {
    match (budget.band_low_gb, budget.band_high_gb) {
        (Some(low), Some(high)) => format!("{}–{} GB", gb_num(low), gb_num(high)),
        (Some(low), None) => format!("{} GB–", gb_num(low)),
        _ => "—".into(),
    }
}

fn gb_num(gb: f64) -> String {
    if gb >= 10.0 {
        format!("{gb:.0}")
    } else {
        format!("{gb:.1}")
    }
}

fn row(mark: &str, host: &str, free: &str, floor: &str, band: &str, state: &str) -> [String; 6] {
    [
        mark.into(),
        host.into(),
        free.into(),
        floor.into(),
        band.into(),
        state.into(),
    ]
}

pub fn format_plan(actions: &[Action]) -> String {
    let mut rows: Vec<[String; 6]> = Vec::new();
    rows.push(["", "host", "action", "bytes", "path", "why"].map(str::to_string));
    for action in actions {
        rows.push([
            " ".into(),
            action.host.clone(),
            action.kind.as_str().into(),
            fmt_bytes(action.bytes),
            action.path.clone(),
            action.reason.clone(),
        ]);
    }
    render(&rows, &[false, false, false, true, false, false])
}

fn render<const N: usize>(rows: &[[String; N]], right: &[bool]) -> String {
    let widths: Vec<usize> = (0..N)
        .map(|i| rows.iter().map(|r| r[i].chars().count()).max().unwrap_or(0))
        .collect();
    let mut text = String::new();
    for row in rows {
        let mut line = String::new();
        for i in 0..N {
            if i > 0 {
                line.push_str("  ");
            }
            let cell = &row[i];
            let pad = widths[i].saturating_sub(cell.chars().count());
            if right.get(i).copied().unwrap_or(false) {
                line.push_str(&" ".repeat(pad));
                line.push_str(cell);
            } else {
                line.push_str(cell);
                line.push_str(&" ".repeat(pad));
            }
        }
        while line.ends_with(' ') {
            line.pop();
        }
        text.push_str(&line);
        text.push('\n');
    }
    text
}
