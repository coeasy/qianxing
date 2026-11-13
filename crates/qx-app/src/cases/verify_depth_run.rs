//! Independent read-back verification for depth-backtest artifacts.

use super::artifacts::{unverified, unverified_artifact_read, verify_declared_artifact_identity};
use super::{attach, guard::guard_panics};
use crate::artifact_store::FileArtifactReader;
use crate::artifact_store::{
    declared_run_record_artifact_locators, read_text_bounded, read_text_bounded_by_id,
    scan_csv_artifact, scan_csv_artifact_by_id, ArtifactReader,
};
use crate::{AppError, AppErrorCategory, DepthBacktestOutcome, RunContext, VerificationResult};
use qx_core::RunManifest;
use serde_json::Value;

const MAX_VERIFY_ARTIFACT_BYTES: u64 = 128 * 1024 * 1024;

/// Re-read the four artifacts of an L1/L2 run and check their identities and counts.
pub fn verify_depth_run(
    outcome: &DepthBacktestOutcome,
    context: &RunContext,
) -> Result<VerificationResult, AppError> {
    verify_depth_run_with_reader(outcome, context, &FileArtifactReader)
}

/// Verify a depth run using a caller-provided artifact storage adapter.
pub(crate) fn verify_depth_run_with_reader(
    outcome: &DepthBacktestOutcome,
    context: &RunContext,
    reader: &dyn ArtifactReader,
) -> Result<VerificationResult, AppError> {
    let correlation_id = context.correlation_id().to_string();
    if outcome.run_id.trim().is_empty() {
        return Err(AppError::new(
            AppErrorCategory::InvalidInput,
            "verify_depth_run 需要一份带 run_id 的运行结果",
        )
        .with_correlation_id(&correlation_id));
    }
    guard_panics(&correlation_id, || Ok(verify_inner(outcome, reader)))
        .map_err(|error| attach(error, &correlation_id))
}

fn verify_inner(outcome: &DepthBacktestOutcome, reader: &dyn ArtifactReader) -> VerificationResult {
    let summary_body = match read_text_bounded(
        reader,
        &outcome.artifacts.summary,
        MAX_VERIFY_ARTIFACT_BYTES,
        "depth summary",
    ) {
        Ok(body) => body,
        Err(error) => return unverified(&outcome.run_id, format!("summary 不可读: {error}")),
    };
    let summary: Value = match serde_json::from_str(&summary_body) {
        Ok(value) => value,
        Err(error) => return unverified(&outcome.run_id, format!("summary 无效: {error}")),
    };
    let declared_artifacts = match declared_run_record_artifact_locators(&summary, reader) {
        Ok(locators) => locators,
        Err(error) => {
            return unverified(
                &outcome.run_id,
                format!("RunRecord SHA-256 复核失败（工件身份不可用）: {error}"),
            );
        }
    };
    let manifest_body_result = match &declared_artifacts {
        Some(locators) => read_text_bounded_by_id(
            reader,
            &locators.run_manifest.id,
            Some(&locators.run_manifest.legacy_reference),
            MAX_VERIFY_ARTIFACT_BYTES,
            "depth run manifest",
        ),
        None => read_text_bounded(
            reader,
            &outcome.artifacts.run_manifest,
            MAX_VERIFY_ARTIFACT_BYTES,
            "depth run manifest",
        ),
    };
    let manifest_body = match manifest_body_result {
        Ok(body) => body,
        Err(error) => return unverified_artifact_read(&outcome.run_id, "run manifest", &error),
    };
    let manifest = match RunManifest::from_json(&manifest_body) {
        Ok(value) => value,
        Err(error) => return unverified(&outcome.run_id, format!("run manifest 无效: {error}")),
    };
    let equity_result = match &declared_artifacts {
        Some(locators) => {
            scan_csv_artifact_by_id(reader, &locators.equity, MAX_VERIFY_ARTIFACT_BYTES, true)
        }
        None => scan_csv_artifact(
            reader,
            &outcome.artifacts.equity,
            MAX_VERIFY_ARTIFACT_BYTES,
            true,
        ),
    };
    let equity = match equity_result {
        Ok(check) => check,
        Err(error) => return unverified_artifact_read(&outcome.run_id, "equity", &error),
    };
    let fills_result = match &declared_artifacts {
        Some(locators) => {
            scan_csv_artifact_by_id(reader, &locators.fills, MAX_VERIFY_ARTIFACT_BYTES, false)
        }
        None => scan_csv_artifact(
            reader,
            &outcome.artifacts.fills,
            MAX_VERIFY_ARTIFACT_BYTES,
            false,
        ),
    };
    let fills = match fills_result {
        Ok(check) => check,
        Err(error) => return unverified_artifact_read(&outcome.run_id, "fills", &error),
    };

    let mut checks = Vec::new();
    let mut mismatches = Vec::new();
    let expected_manifest_run_id = outcome
        .manifest_run_id
        .as_deref()
        .unwrap_or(outcome.run_id.as_str());
    check(
        &mut checks,
        &mut mismatches,
        "manifest.run_id",
        manifest.run_id == expected_manifest_run_id,
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
    if outcome.manifest_run_id.is_some() {
        check_value(
            &summary,
            "manifest_run_id",
            expected_manifest_run_id,
            &mut checks,
            &mut mismatches,
        );
    }
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
    if let Some(account_id) = outcome.account_id.as_deref() {
        check_value(
            &summary,
            "account_id",
            account_id,
            &mut checks,
            &mut mismatches,
        );
    }
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
    let equity_rows = equity.rows;
    check(
        &mut checks,
        &mut mismatches,
        "equity.csv 行数",
        equity_rows == outcome.equity_points,
    );
    if let (Some(last), Ok(expected)) = (
        equity.final_equity_raw,
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
    let fill_rows = fills.rows;
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
    verify_declared_artifact_identity(&summary, &mut checks, &mut mismatches, reader);

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
