//! Independent read-back verification for depth-backtest artifacts.

use super::artifacts::{csv_rows, last_equity_raw, read_text, unverified};
use super::{attach, guard::guard_panics};
use crate::{AppError, AppErrorCategory, DepthBacktestOutcome, RunContext, VerificationResult};
use qx_core::RunManifest;
use serde_json::Value;

/// Re-read the four artifacts of an L1/L2 run and check their identities and counts.
pub fn verify_depth_run(
    outcome: &DepthBacktestOutcome,
    context: &RunContext,
) -> Result<VerificationResult, AppError> {
    let correlation_id = context.correlation_id().to_string();
    if outcome.run_id.trim().is_empty() {
        return Err(AppError::new(
            AppErrorCategory::InvalidInput,
            "verify_depth_run 需要一份带 run_id 的运行结果",
        )
        .with_correlation_id(&correlation_id));
    }
    guard_panics(&correlation_id, || Ok(verify_inner(outcome)))
        .map_err(|error| attach(error, &correlation_id))
}

fn verify_inner(outcome: &DepthBacktestOutcome) -> VerificationResult {
    let manifest_body = match read_text(&outcome.artifacts.run_manifest) {
        Ok(body) => body,
        Err(error) => return unverified(&outcome.run_id, format!("run manifest 不可读: {error}")),
    };
    let manifest = match RunManifest::from_json(&manifest_body) {
        Ok(value) => value,
        Err(error) => return unverified(&outcome.run_id, format!("run manifest 无效: {error}")),
    };
    let summary_body = match read_text(&outcome.artifacts.summary) {
        Ok(body) => body,
        Err(error) => return unverified(&outcome.run_id, format!("summary 不可读: {error}")),
    };
    let summary: Value = match serde_json::from_str(&summary_body) {
        Ok(value) => value,
        Err(error) => return unverified(&outcome.run_id, format!("summary 无效: {error}")),
    };
    let equity = match read_text(&outcome.artifacts.equity) {
        Ok(body) => body,
        Err(error) => return unverified(&outcome.run_id, format!("equity 不可读: {error}")),
    };
    let fills = match read_text(&outcome.artifacts.fills) {
        Ok(body) => body,
        Err(error) => return unverified(&outcome.run_id, format!("fills 不可读: {error}")),
    };

    let mut checks = Vec::new();
    let mut mismatches = Vec::new();
    check(
        &mut checks,
        &mut mismatches,
        "manifest.run_id",
        manifest.run_id == outcome.run_id,
    );
    check(
        &mut checks,
        &mut mismatches,
        "manifest.result_hash",
        manifest.result_hash == outcome.result_hash,
    );
    check(
        &mut checks,
        &mut mismatches,
        "manifest.data_fingerprint",
        manifest.data_fingerprint == outcome.data_fingerprint,
    );
    check(
        &mut checks,
        &mut mismatches,
        "manifest.determinism_mode",
        manifest.determinism_mode,
    );
    check_value(
        &summary,
        "run_id",
        &outcome.run_id,
        &mut checks,
        &mut mismatches,
    );
    check_value(
        &summary,
        "result_hash",
        &outcome.result_hash,
        &mut checks,
        &mut mismatches,
    );
    check_value(
        &summary,
        "data_fingerprint",
        &outcome.data_fingerprint,
        &mut checks,
        &mut mismatches,
    );
    check_value(
        &summary,
        "instrument",
        &outcome.instrument,
        &mut checks,
        &mut mismatches,
    );
    check_value(
        &summary,
        "tier",
        &outcome.tier,
        &mut checks,
        &mut mismatches,
    );
    check_number(
        &summary,
        "fills",
        outcome.fills,
        &mut checks,
        &mut mismatches,
    );
    check_number(
        &summary,
        "equity_points",
        outcome.equity_points,
        &mut checks,
        &mut mismatches,
    );
    check_signed(
        &summary,
        "return_bps",
        outcome.return_bps as i64,
        &mut checks,
        &mut mismatches,
    );
    check_number(
        &summary,
        "max_drawdown_bps",
        outcome.max_drawdown_bps as u64,
        &mut checks,
        &mut mismatches,
    );
    let equity_rows = csv_rows(&equity).len() as u64;
    check(
        &mut checks,
        &mut mismatches,
        "equity.csv 行数",
        equity_rows == outcome.equity_points,
    );
    if let (Some(last), Ok(expected)) = (
        last_equity_raw(&equity),
        summary["final_equity_raw"].to_string().parse::<i128>(),
    ) {
        check(
            &mut checks,
            &mut mismatches,
            "equity.csv 末值",
            last == expected,
        );
    } else {
        mismatches.push("equity.csv 末值或 summary.final_equity_raw 无法解析".into());
    }
    let fill_rows = csv_rows(&fills).len() as u64;
    check(
        &mut checks,
        &mut mismatches,
        "fills.csv 行数",
        fill_rows == outcome.fills,
    );
    let ledger_entries = summary["ledger_entries"].as_u64();
    let replay_entries = summary["replay_ledger_entries"].as_u64();
    check(
        &mut checks,
        &mut mismatches,
        "重放账簿条数",
        ledger_entries.is_some() && ledger_entries == replay_entries,
    );

    VerificationResult {
        run_id: outcome.run_id.clone(),
        result_hash: outcome.result_hash.clone(),
        data_fingerprint: outcome.data_fingerprint.clone(),
        verified: mismatches.is_empty(),
        checks,
        mismatches,
    }
}

fn check(checks: &mut Vec<String>, mismatches: &mut Vec<String>, label: &str, passed: bool) {
    if passed {
        checks.push(format!("{label} 一致"));
    } else {
        mismatches.push(format!("{label} 不一致"));
    }
}

fn check_value(
    summary: &Value,
    key: &str,
    expected: &str,
    checks: &mut Vec<String>,
    mismatches: &mut Vec<String>,
) {
    check(
        checks,
        mismatches,
        &format!("summary.{key}"),
        summary[key].as_str() == Some(expected),
    );
}

fn check_number(
    summary: &Value,
    key: &str,
    expected: u64,
    checks: &mut Vec<String>,
    mismatches: &mut Vec<String>,
) {
    check(
        checks,
        mismatches,
        &format!("summary.{key}"),
        summary[key].as_u64() == Some(expected),
    );
}

fn check_signed(
    summary: &Value,
    key: &str,
    expected: i64,
    checks: &mut Vec<String>,
    mismatches: &mut Vec<String>,
) {
    check(
        checks,
        mismatches,
        &format!("summary.{key}"),
        summary[key].as_i64() == Some(expected),
    );
}
