use crate::focus::active_project;
use crate::score::{ScoreBreakdown, reason_for, score_all};
use crate::waiter::Waiter;
use std::path::Path;

pub fn tie_break(scored: &mut [(Waiter, ScoreBreakdown)]) {
    scored.sort_by(|(wa, sa), (wb, sb)| {
        sb.total()
            .partial_cmp(&sa.total())
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| wa.since.cmp(&wb.since))
    });
}

pub fn ordered_waiters(state_dir: &Path, now: i64) -> Vec<(Waiter, ScoreBreakdown)> {
    let waiters = crate::registry::load_waiters(state_dir);
    let mut scored = score_all(&waiters, now, state_dir);
    tie_break(&mut scored);
    scored
}

/// Only a waiting entry can be picked: a transferring one is not asking for a slot yet and a
/// processing one already holds it.
pub fn pick_winner(state_dir: &Path, now: i64) -> Option<(Waiter, ScoreBreakdown)> {
    ordered_waiters(state_dir, now)
        .into_iter()
        .find(|(w, _)| w.state == "waiting")
}

pub fn pick_reason(w: &Waiter, bd: &ScoreBreakdown, now: i64) -> String {
    let active = active_project(now);
    reason_for(bd, w, active.as_deref())
}
