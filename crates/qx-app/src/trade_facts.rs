//! Stable shared execution facts used by the application and its facades.

use std::collections::BTreeMap;

/// Stable id for a spread group; the live and research facades share this identity rule.
pub fn spread_group_id(strategy_id: &str, run_id: u64, signal_id: u64) -> String {
    format!("spread-{strategy_id}-{run_id}-{signal_id}")
}

/// Collapse rejected events into one deterministic, severity-neutral summary.
pub fn rejection_facts(event_log: &qx_core::EventLog) -> Vec<(String, usize)> {
    let mut grouped = BTreeMap::<String, usize>::new();
    for reason in event_log
        .events()
        .iter()
        .filter_map(|event| match &event.kind {
            qx_core::EventKind::Rejected { reason, .. } => Some(reason.clone()),
            _ => None,
        })
    {
        *grouped.entry(reason).or_default() += 1;
    }
    let mut pairs = grouped.into_iter().collect::<Vec<_>>();
    pairs.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    pairs
}

pub fn rejection_facts_line(rejections: &[(String, usize)]) -> String {
    let line = rejections
        .iter()
        .map(|(reason, count)| format!("{count}x{}", reason.replace(' ', "_")))
        .collect::<Vec<_>>()
        .join(";");
    if line.is_empty() {
        "none".to_string()
    } else {
        line
    }
}

pub fn rejection_count(rejections: &[(String, usize)]) -> usize {
    rejections.iter().map(|(_, count)| count).sum()
}
