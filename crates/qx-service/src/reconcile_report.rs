//! Durable order and balance reconciliation report projection shared by Venue hosts.

#![allow(clippy::items_after_test_module)] // Keep admission tests beside the rules they exercise.

use qx_adapter::AdapterReconcileIssue;
use qx_api::ReconcileReportSnapshot;
use qx_runtime::RuntimeBalanceDiscrepancy;
use qx_storage::JsonStateStore;
use std::path::Path;

/// Live strategy intents require a recent, clean reconciliation for the exact account and
/// venue. This is an admission check; the public report remains observational.
pub const LIVE_RECONCILE_MAX_AGE_MS: u64 = qx_runtime::LIVE_RECONCILE_MAX_AGE_MS;

pub fn require_fresh_clean_reconciliation(
    reports: &[ReconcileReportSnapshot],
    account_id: &str,
    venue_id: &str,
    now: u64,
) -> Result<(), String> {
    qx_runtime::require_fresh_clean_reconciliation(
        reports,
        account_id,
        venue_id,
        now,
        LIVE_RECONCILE_MAX_AGE_MS,
    )
}

/// Require a clean account/venue reconciliation observed strictly after a failed command.
/// All still-fresh reports are checked by `require_fresh_clean_reconciliation`; this additional
/// boundary prevents a clean report from before the command failure from resolving it.
pub fn require_fresh_clean_reconciliation_after(
    reports: &[ReconcileReportSnapshot],
    account_id: &str,
    venue_id: &str,
    now: u64,
    after_ts: u64,
) -> Result<(), String> {
    require_fresh_clean_reconciliation(reports, account_id, venue_id, now)?;
    let has_post_failure_report = reports.iter().any(|report| {
        report.account_id == account_id
            && report.venue_id.eq_ignore_ascii_case(venue_id)
            && report.observed_ts > after_ts
            && now
                .checked_sub(report.observed_ts)
                .is_some_and(|age| age <= LIVE_RECONCILE_MAX_AGE_MS)
    });
    if !has_post_failure_report {
        return Err(format!(
            "LIVE_READINESS_RECONCILE_NOT_AFTER_FAILURE account={account_id} venue={venue_id} failure_ts={after_ts}"
        ));
    }
    Ok(())
}

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

    fn clean_report_at(observed_ts: u64) -> ReconcileReportSnapshot {
        ReconcileReportSnapshot {
            schema_version: 1,
            worker_id: "reconciler-a".into(),
            account_id: "main".into(),
            venue_id: "paper".into(),
            observed_ts,
            order_issues: Vec::new(),
            balances_count: 0,
            balance_discrepancies: Vec::new(),
            position_snapshots_count: None,
            funding_rate_snapshots_count: None,
            cashflow_count: None,
        }
    }

    #[test]
    fn post_failure_reconciliation_must_be_fresh_clean_and_later_than_failure() {
        assert!(require_fresh_clean_reconciliation_after(
            &[clean_report_at(1_000)],
            "main",
            "paper",
            2_000,
            999,
        )
        .is_ok());
        let error = require_fresh_clean_reconciliation_after(
            &[clean_report_at(1_000)],
            "main",
            "paper",
            2_000,
            1_000,
        )
        .unwrap_err();
        assert!(error.contains("NOT_AFTER_FAILURE"));
        assert!(require_fresh_clean_reconciliation_after(
            &[clean_report_at(1_000)],
            "main",
            "paper",
            62_000,
            999,
        )
        .unwrap_err()
        .contains("STALE"));
    }

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

    fn clean_report() -> ReconcileReportSnapshot {
        ReconcileReportSnapshot {
            schema_version: 1,
            worker_id: "reconcile-main".into(),
            account_id: "main".into(),
            venue_id: "binance".into(),
            observed_ts: 10_000,
            order_issues: vec![],
            balances_count: 1,
            balance_discrepancies: vec![],
            position_snapshots_count: Some(0),
            funding_rate_snapshots_count: None,
            cashflow_count: None,
        }
    }

    #[test]
    fn live_reconciliation_admission_fails_closed_on_missing_stale_or_discrepant_evidence() {
        let clean = clean_report();
        assert!(require_fresh_clean_reconciliation(
            std::slice::from_ref(&clean),
            "main",
            "BINANCE",
            10_500
        )
        .is_ok());
        assert!(
            require_fresh_clean_reconciliation(&[], "main", "binance", 10_500)
                .unwrap_err()
                .contains("RECONCILE_MISSING")
        );
        assert!(require_fresh_clean_reconciliation(
            std::slice::from_ref(&clean),
            "main",
            "binance",
            10_000 + LIVE_RECONCILE_MAX_AGE_MS + 1
        )
        .unwrap_err()
        .contains("RECONCILE_STALE"));
        assert!(require_fresh_clean_reconciliation(
            std::slice::from_ref(&clean),
            "other",
            "binance",
            10_500
        )
        .unwrap_err()
        .contains("RECONCILE_MISSING"));
        let mut discrepant = clean.clone();
        discrepant
            .order_issues
            .push(serde_json::json!({"kind": "unknown_order"}));
        assert!(
            require_fresh_clean_reconciliation(&[discrepant], "main", "binance", 10_500)
                .unwrap_err()
                .contains("RECONCILE_DISCREPANCY")
        );
        let mut concurrent_discrepant = clean.clone();
        concurrent_discrepant.worker_id = "reconcile-secondary".into();
        concurrent_discrepant
            .balance_discrepancies
            .push(serde_json::json!({"asset": "USDT"}));
        assert!(require_fresh_clean_reconciliation(
            &[clean.clone(), concurrent_discrepant],
            "main",
            "binance",
            10_500
        )
        .unwrap_err()
        .contains("RECONCILE_DISCREPANCY"));
        assert!(
            require_fresh_clean_reconciliation(&[clean], "main", "binance", 9_999)
                .unwrap_err()
                .contains("RECONCILE_FROM_FUTURE")
        );
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
