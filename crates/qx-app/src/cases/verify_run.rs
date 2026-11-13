//! `VerifyRun` 用例（T2-2；方案 §15.8 G1 的「artifact/report → verify」那一跳）。
//!
//! 它把四份产物**重新读回来**，逐条核对它们互相说的是不是同一件事。这是"可复现"升级成
//! "可核验"的那一步：产物在盘不等于产物自洽，而读者（报告、对比、台账）只念产物。
//!
//! ## 复核口径
//!
//! 1. `result_hash` 三处一致：`run.json` == `summary.json` == 调用方手上的 `BacktestOutcome`。
//! 2. `data_fingerprint` 三处一致（同上）。
//! 3. `run_id` 三处一致。
//! 4. `equity.csv` 行数 == `summary.equity_points`，且**末行** == `summary.final_equity_raw`。
//! 5. `fills.csv` 行数 == `summary.fills`。
//! 6. 调用方 outcome 与 summary 的标的、成交数、权益点数、收益和回撤逐项一致。
//! 7. manifest 与 summary 的策略版本、时钟区间和随机种子一致，且 manifest 声明确定性运行。
//! 8. `summary.replay_ledger_entries` == `summary.ledger_entries`（重放自检的读回面）。
//!
//! ## 为什么产物缺失不是 `Err`
//!
//! 「这一轮产物不完整」是一个**可核验的结论**，不是一次故障。把它做成 `Err` 会让
//! "复核不通过"与"复核跑不起来"在调用方那里长得一样——而这两件事的下一步动作完全不同
//! （前者去查那一轮到底怎么了，后者去查存储）。所以产物读不到也返回
//! `Ok(VerificationResult { verified: false, .. })`，理由进 `mismatches`。
//!
//! 唯一的 `Err` 留给"这份调用方手上的 outcome 自己就不合法"——没有 run_id 的复核请求
//! 没有意义。

use crate::artifact_store::FileArtifactReader;
use crate::artifact_store::{
    declared_run_record_artifact_locators, read_text_bounded, read_text_bounded_by_id,
    scan_csv_artifact, scan_csv_artifact_by_id, ArtifactReader,
};
use crate::cases::artifacts::{
    summary_from_json, unverified, unverified_artifact_read, verify_declared_artifact_identity,
};
use crate::cases::attach;
use crate::cases::guard::guard_panics;
use crate::context::RunContext;
use crate::error::{AppError, AppErrorCategory};
use crate::spec::{BacktestOutcome, VerificationResult};
use qx_core::RunManifest;

const MAX_VERIFY_ARTIFACT_BYTES: u64 = 128 * 1024 * 1024;

/// 复核一次运行的产物。
///
/// 权限：**不需要**能力档——纯读产物，没有副作用，也不碰任何账户或场所。所以这里**不调**
/// `context.require`；取 [`RunContext`] 只为了拿调用方的 correlation id，让"复核不通过"
/// 这条结论也能和这次调用、这份日志对齐（三个用例的边界因此是同一个形状）。
/// 幂等：纯读。
/// 取消：同步纯读，没有取消点。
pub fn verify_run(
    outcome: &BacktestOutcome,
    context: &RunContext,
) -> Result<VerificationResult, AppError> {
    verify_run_with_reader(outcome, context, &FileArtifactReader)
}

/// Verify a Bar run using a caller-provided artifact storage adapter.
pub(crate) fn verify_run_with_reader(
    outcome: &BacktestOutcome,
    context: &RunContext,
    reader: &dyn ArtifactReader,
) -> Result<VerificationResult, AppError> {
    let correlation_id = context.correlation_id().to_string();
    if outcome.run_id.trim().is_empty() {
        return Err(AppError::new(
            AppErrorCategory::InvalidInput,
            "verify_run 需要一份带 run_id 的运行结果——没有身份的复核请求没有对象",
        )
        .with_correlation_id(&correlation_id));
    }
    let outcome = guard_panics(&correlation_id, || Ok(verify_run_inner(outcome, reader)));
    outcome.map_err(|error| attach(error, &correlation_id))
}

fn verify_run_inner(outcome: &BacktestOutcome, reader: &dyn ArtifactReader) -> VerificationResult {
    let summary_body = match read_text_bounded(
        reader,
        &outcome.artifacts.summary,
        MAX_VERIFY_ARTIFACT_BYTES,
        "研究摘要",
    ) {
        Ok(body) => body,
        Err(error) => {
            return unverified(
                &outcome.run_id,
                format!("产物缺失或不完整（summary）: {error}"),
            );
        }
    };
    let summary_value: serde_json::Value = match serde_json::from_str(&summary_body) {
        Ok(value) => value,
        Err(error) => return unverified(&outcome.run_id, format!("研究摘要 JSON 无效: {error}")),
    };
    let declared_artifacts = match declared_run_record_artifact_locators(&summary_value, reader) {
        Ok(locators) => locators,
        Err(error) => {
            return unverified(
                &outcome.run_id,
                format!("RunRecord SHA-256 复核失败（工件身份不可用）: {error}"),
            );
        }
    };
    let summary = match summary_from_json(&summary_body) {
        Ok(summary) => summary,
        Err(error) => return unverified(&outcome.run_id, error.message().to_string()),
    };
    let manifest_body_result = match &declared_artifacts {
        Some(locators) => read_text_bounded_by_id(
            reader,
            &locators.run_manifest.id,
            Some(&locators.run_manifest.legacy_reference),
            MAX_VERIFY_ARTIFACT_BYTES,
            "run manifest",
        ),
        None => read_text_bounded(
            reader,
            &outcome.artifacts.run_manifest,
            MAX_VERIFY_ARTIFACT_BYTES,
            "run manifest",
        ),
    };
    let manifest_body = match manifest_body_result {
        Ok(body) => body,
        Err(error) => return unverified_artifact_read(&outcome.run_id, "run manifest", &error),
    };
    let manifest = match RunManifest::from_json(&manifest_body) {
        Ok(manifest) => manifest,
        Err(error) => {
            return unverified(
                &outcome.run_id,
                format!(
                    "run manifest 无法解析 {}: {error}",
                    outcome.artifacts.run_manifest
                ),
            );
        }
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
    compare(
        &mut checks,
        &mut mismatches,
        "run_id（manifest ↔ outcome）",
        &manifest.run_id,
        &outcome.run_id,
    );
    compare(
        &mut checks,
        &mut mismatches,
        "run_id（summary ↔ outcome）",
        &summary.run_id,
        &outcome.run_id,
    );
    compare(
        &mut checks,
        &mut mismatches,
        "result_hash（manifest ↔ outcome）",
        &manifest.result_hash,
        &outcome.result_hash,
    );
    compare(
        &mut checks,
        &mut mismatches,
        "result_hash（summary ↔ outcome）",
        &summary.result_hash,
        &outcome.result_hash,
    );
    compare(
        &mut checks,
        &mut mismatches,
        "data_fingerprint（manifest ↔ outcome）",
        &manifest.data_fingerprint,
        &outcome.data_fingerprint,
    );
    compare(
        &mut checks,
        &mut mismatches,
        "data_fingerprint（summary ↔ outcome）",
        &summary.data_fingerprint,
        &outcome.data_fingerprint,
    );
    compare(
        &mut checks,
        &mut mismatches,
        "config_hash（manifest ↔ summary）",
        &manifest.config_hash,
        &summary.config_hash,
    );
    compare(
        &mut checks,
        &mut mismatches,
        "strategy_version（manifest ↔ summary）",
        &manifest.strategy_version,
        &summary.strategy_version,
    );
    compare(
        &mut checks,
        &mut mismatches,
        "clock_start（manifest ↔ summary）",
        &manifest.clock_start.to_string(),
        &summary.clock_start.to_string(),
    );
    compare(
        &mut checks,
        &mut mismatches,
        "clock_end（manifest ↔ summary）",
        &manifest.clock_end.to_string(),
        &summary.clock_end.to_string(),
    );
    compare(
        &mut checks,
        &mut mismatches,
        "seed（manifest ↔ summary）",
        &manifest.global_seed.to_string(),
        &summary.seed.to_string(),
    );
    if manifest.determinism_mode {
        checks.push("manifest.determinism_mode == true".to_string());
    } else {
        mismatches.push("manifest.determinism_mode 为 false".to_string());
    }
    compare(
        &mut checks,
        &mut mismatches,
        "instrument（summary ↔ outcome）",
        &summary.instrument,
        &outcome.instrument,
    );
    if let Some(account_id) = &outcome.account_id {
        compare(
            &mut checks,
            &mut mismatches,
            "account_id（summary ↔ outcome）",
            &summary.account_id,
            account_id,
        );
    }
    compare(
        &mut checks,
        &mut mismatches,
        "equity_points（summary ↔ outcome）",
        &summary.equity_points.to_string(),
        &outcome.equity_points.to_string(),
    );
    compare(
        &mut checks,
        &mut mismatches,
        "return_bps（summary ↔ outcome）",
        &summary.return_bps.to_string(),
        &outcome.return_bps.to_string(),
    );
    compare(
        &mut checks,
        &mut mismatches,
        "max_drawdown_bps（summary ↔ outcome）",
        &summary.max_drawdown_bps.to_string(),
        &outcome.max_drawdown_bps.to_string(),
    );

    let equity_rows = equity.rows;
    if equity_rows == summary.equity_points && equity_rows == outcome.equity_points {
        checks.push(format!("equity.csv 行数 == equity_points == {equity_rows}"));
    } else {
        mismatches.push(format!(
            "equity.csv 行数 {equity_rows} != summary.equity_points {} / outcome.equity_points {}",
            summary.equity_points, outcome.equity_points
        ));
    }
    match equity.final_equity_raw {
        Some(last) if last == summary.final_equity_raw => {
            checks.push(format!("equity.csv 末行 == final_equity_raw == {last}"));
        }
        Some(last) => mismatches.push(format!(
            "equity.csv 末行 {last} != summary.final_equity_raw {}",
            summary.final_equity_raw
        )),
        None => mismatches.push("equity.csv 读不出末行权益值".to_string()),
    }
    let exact_final = summary_value
        .pointer("/metrics_exact/equity/final")
        .and_then(serde_json::Value::as_str);
    match (equity.final_equity_exact.as_deref(), exact_final) {
        (Some(actual), Some(expected)) => {
            let actual_decimal = qx_core::ExactDecimal::parse(actual);
            let expected_decimal = qx_core::ExactDecimal::parse(expected);
            if actual_decimal.is_ok_and(|value| value.to_string() == actual)
                && expected_decimal.is_ok_and(|value| value.to_string() == expected)
                && actual == expected
            {
                checks.push("equity.csv 末行 exact == metrics_exact.equity.final".into());
            } else {
                mismatches.push(
                    "equity.csv 末行 exact 与 metrics_exact.equity.final 不一致或格式非法".into(),
                );
            }
        }
        (Some(_), None) => mismatches.push(
            "权益 CSV 声明 equity_exact，但摘要缺少 metrics_exact.equity.final".into(),
        ),
        (None, Some(expected)) => match qx_core::ExactDecimal::parse(expected) {
            Ok(value)
                if value
                    .checked_mul_at_scale(
                        qx_core::ExactDecimal::ONE,
                        9,
                        qx_core::DecimalRounding::TowardZero,
                    )
                    .and_then(|projection| projection.to_raw(9).ok())
                    == Some(summary.final_equity_raw) =>
            {
                checks.push("精确期末权益可由 legacy raw 曲线无损表示".into());
            }
            _ => mismatches.push(
                "metrics_exact.equity.final 无法由 legacy raw 曲线无损表示，缺少 equity_exact CSV 列".into(),
            ),
        },
        (None, None) => {}
    }

    let fill_rows = fills.rows;
    if summary.fills == outcome.fills && fill_rows == summary.fills {
        checks.push(format!("fills.csv 行数 == fills == {fill_rows}"));
    } else {
        mismatches.push(format!(
            "fills.csv 行数 {fill_rows}、summary.fills {}、outcome.fills {} 不一致",
            summary.fills, outcome.fills
        ));
    }

    if summary.replay_ledger_entries == summary.ledger_entries {
        checks.push(format!(
            "重放账簿条数 == 运行账簿条数 == {}",
            summary.ledger_entries
        ));
    } else {
        mismatches.push(format!(
            "重放账簿条数 {} != 运行账簿条数 {}——这一轮的产物不该被当成可信事实",
            summary.replay_ledger_entries, summary.ledger_entries
        ));
    }

    verify_declared_artifact_identity(&summary_value, &mut checks, &mut mismatches, reader);

    VerificationResult {
        run_id: outcome.run_id.clone(),
        result_hash: summary.result_hash.clone(),
        data_fingerprint: summary.data_fingerprint.clone(),
        verified: mismatches.is_empty(),
        checks,
        mismatches,
    }
}

/// 一处口径的比对：一致进 `checks`，不一致进 `mismatches`。**两边都记**——
/// 只记不一致的话，读者没法从产物回答"到底核过哪几条"。
fn compare(
    checks: &mut Vec<String>,
    mismatches: &mut Vec<String>,
    label: &str,
    left: &str,
    right: &str,
) {
    if left == right {
        checks.push(format!("{label} 一致: {left}"));
    } else {
        mismatches.push(format!("{label} 不一致: {left} != {right}"));
    }
}
