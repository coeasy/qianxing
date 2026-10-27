//! Durable order and balance reconciliation report projection shared by Venue hosts.

use qx_adapter::AdapterReconcileIssue;
use qx_api::ReconcileReportSnapshot;
use qx_runtime::RuntimeBalanceDiscrepancy;
use qx_storage::JsonStateStore;
use std::path::Path;

pub struct ReconcileReportInput<'a> {
    pub pipeline_root: &'a Path,
    pub worker_id: &'a str,
    pub account_id: &'a str,
    pub venue_id: &'a str,
    pub observed_ts: u64,
    pub issues: &'a [AdapterReconcileIssue],
    pub additional_order_issues: &'a [serde_json::Value],
    pub balances_count: usize,
    pub balance_discrepancies: &'a [RuntimeBalanceDiscrepancy],
    /// None means this round did not query the surface; Some(0) means it queried and was empty.
    pub position_snapshots_count: Option<usize>,
    pub funding_rate_snapshots_count: Option<usize>,
    pub cashflow_count: Option<usize>,
}

pub fn reconcile_issue_json(issue: &AdapterReconcileIssue) -> serde_json::Value {
    let mut value = serde_json::json!({
        "kind": issue.reason_code(),
        "action": issue.action().reason_code(),
        "client_order_id": issue.client_order_id(),
    });
    let Some(object) = value.as_object_mut() else {
        return value;
    };
    match issue {
        AdapterReconcileIssue::MissingLocally { .. }
        | AdapterReconcileIssue::MissingAtVenue { .. } => {}
        AdapterReconcileIssue::StatusMismatch { local, venue, .. } => {
            object.insert("local".into(), serde_json::json!(local));
            object.insert("venue".into(), serde_json::json!(venue));
        }
        AdapterReconcileIssue::FilledMismatch { local, venue, .. } => {
            object.insert("local_raw".into(), serde_json::json!(local.raw()));
            object.insert("venue_raw".into(), serde_json::json!(venue.raw()));
        }
        AdapterReconcileIssue::FilledExactMismatch { local, venue, .. } => {
            object.insert("local_exact".into(), serde_json::json!(local.to_string()));
            object.insert("venue_exact".into(), serde_json::json!(venue.to_string()));
        }
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use qx_core::ExactDecimal;

    #[test]
    fn exact_fill_reconciliation_report_keeps_decimal_values() {
        let value = reconcile_issue_json(&AdapterReconcileIssue::FilledExactMismatch {
            client_order_id: 42,
            local: ExactDecimal::parse("0.0000000001").unwrap(),
            venue: ExactDecimal::parse("0.0000000002").unwrap(),
        });
        assert_eq!(value["kind"], "filled_mismatch");
        assert_eq!(value["action"], "resync");
        assert_eq!(value["local_exact"], "0.0000000001");
        assert_eq!(value["venue_exact"], "0.0000000002");
        assert!(value.get("local_raw").is_none());
    }
}

pub fn persist_reconcile_report(input: ReconcileReportInput<'_>) -> Result<(), String> {
    let report = ReconcileReportSnapshot {
        schema_version: 1,
        worker_id: input.worker_id.into(),
        account_id: input.account_id.into(),
        venue_id: input.venue_id.into(),
        observed_ts: input.observed_ts,
        order_issues: input
            .issues
            .iter()
            .map(reconcile_issue_json)
            .chain(input.additional_order_issues.iter().cloned())
            .collect(),
        balances_count: input.balances_count,
        balance_discrepancies: input
            .balance_discrepancies
            .iter()
            .map(serde_json::to_value)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("序列化余额差异失败: {error}"))?,
        position_snapshots_count: input.position_snapshots_count,
        funding_rate_snapshots_count: input.funding_rate_snapshots_count,
        cashflow_count: input.cashflow_count,
    };
    report.validate()?;
    JsonStateStore::new(input.pipeline_root)
        .save_json_at(format!("reconcile/{}.json", input.worker_id), &report)
        .map(|_| ())
        .map_err(|error| format!("保存对账报告失败: {error:?}"))
}
