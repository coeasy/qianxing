//! Durable account/venue reconciliation evidence shared by API, workers, and the supervisor.

use serde::{Deserialize, Serialize};
use std::path::Path;

pub const LIVE_RECONCILE_MAX_AGE_MS: u64 = 60_000;

/// Versioned report persisted by venue reconciliation workers and consumed by live admission.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ReconcileReportSnapshot {
    pub schema_version: u32,
    pub worker_id: String,
    pub account_id: String,
    pub venue_id: String,
    pub observed_ts: u64,
    #[serde(default)]
    pub order_issues: Vec<serde_json::Value>,
    pub balances_count: usize,
    #[serde(default)]
    pub balance_discrepancies: Vec<serde_json::Value>,
    #[serde(default)]
    pub position_snapshots_count: Option<usize>,
    #[serde(default)]
    pub funding_rate_snapshots_count: Option<usize>,
    #[serde(default)]
    pub cashflow_count: Option<usize>,
}

impl ReconcileReportSnapshot {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != 1
            || self.worker_id.trim().is_empty()
            || self.account_id.trim().is_empty()
            || self.venue_id.trim().is_empty()
        {
            return Err("对账报告身份或 schema_version 非法".into());
        }
        Ok(())
    }
}

pub fn load_reconcile_reports(root: &Path) -> Result<Vec<ReconcileReportSnapshot>, String> {
    let mut reports = Vec::new();
    let report_root = root.join("reconcile");
    if report_root.exists() {
        for entry in std::fs::read_dir(&report_root)
            .map_err(|error| format!("读取 API 对账报告目录失败: {error}"))?
        {
            let path = entry
                .map_err(|error| format!("读取 API 对账报告目录项失败: {error}"))?
                .path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let report: ReconcileReportSnapshot = serde_json::from_slice(
                &std::fs::read(&path)
                    .map_err(|error| format!("读取对账报告失败 {}: {error}", path.display()))?,
            )
            .map_err(|error| format!("对账报告 JSON 无效 {}: {error}", path.display()))?;
            report
                .validate()
                .map_err(|error| format!("对账报告校验失败 {}: {error}", path.display()))?;
            reports.push(report);
        }
        reports.sort_by(|left, right| left.worker_id.cmp(&right.worker_id));
    }
    Ok(reports)
}

pub fn require_fresh_clean_reconciliation(
    reports: &[ReconcileReportSnapshot],
    account_id: &str,
    venue_id: &str,
    now: u64,
    max_age_ms: u64,
) -> Result<(), String> {
    let matching: Vec<_> = reports
        .iter()
        .filter(|report| {
            report.account_id == account_id && report.venue_id.eq_ignore_ascii_case(venue_id)
        })
        .collect();
    if matching.is_empty() {
        return Err(format!(
            "LIVE_READINESS_RECONCILE_MISSING account={account_id} venue={venue_id}"
        ));
    }
    let mut freshest_age_ms = u64::MAX;
    let mut fresh_reports = 0usize;
    for report in matching {
        report.validate().map_err(|error| {
            format!(
                "LIVE_READINESS_RECONCILE_INVALID worker={}: {error}",
                report.worker_id
            )
        })?;
        let age = now.checked_sub(report.observed_ts).ok_or_else(|| {
            format!(
                "LIVE_READINESS_RECONCILE_FROM_FUTURE worker={} observed_ts={} now={now}",
                report.worker_id, report.observed_ts
            )
        })?;
        freshest_age_ms = freshest_age_ms.min(age);
        if age > max_age_ms {
            continue;
        }
        fresh_reports += 1;
        if !report.order_issues.is_empty() || !report.balance_discrepancies.is_empty() {
            return Err(format!(
                "LIVE_READINESS_RECONCILE_DISCREPANCY worker={} order_issues={} balance_discrepancies={}",
                report.worker_id,
                report.order_issues.len(),
                report.balance_discrepancies.len()
            ));
        }
    }
    if fresh_reports == 0 {
        return Err(format!(
            "LIVE_READINESS_RECONCILE_STALE freshest_age_ms={freshest_age_ms} max_age_ms={max_age_ms}"
        ));
    }
    Ok(())
}
