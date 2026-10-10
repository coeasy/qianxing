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

use crate::cases::artifacts::{
    csv_rows, last_equity_raw, read_text, summary_from_json, unverified,
};
use crate::cases::attach;
use crate::cases::guard::guard_panics;
use crate::context::RunContext;
use crate::error::{AppError, AppErrorCategory};
use crate::spec::{BacktestOutcome, VerificationResult};
use qx_core::RunManifest;

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
    let correlation_id = context.correlation_id().to_string();
    if outcome.run_id.trim().is_empty() {
        return Err(AppError::new(
            AppErrorCategory::InvalidInput,
            "verify_run 需要一份带 run_id 的运行结果——没有身份的复核请求没有对象",
        )
        .with_correlation_id(&correlation_id));
    }
    let outcome = guard_panics(&correlation_id, || Ok(verify_run_inner(outcome)));
    outcome.map_err(|error| attach(error, &correlation_id))
}

fn verify_run_inner(outcome: &BacktestOutcome) -> VerificationResult {
    let manifest_body = match read_text(&outcome.artifacts.run_manifest) {
        Ok(body) => body,
        Err(error) => {
            return unverified(
                &outcome.run_id,
                format!("产物缺失或不完整（run manifest）: {error}"),
            )
        }
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
            )
        }
    };
    let summary_body = match read_text(&outcome.artifacts.summary) {
        Ok(body) => body,
        Err(error) => {
            return unverified(
                &outcome.run_id,
                format!("产物缺失或不完整（summary）: {error}"),
            )
        }
    };
    let summary = match summary_from_json(&summary_body) {
        Ok(summary) => summary,
        Err(error) => return unverified(&outcome.run_id, error.message().to_string()),
    };
    let equity_body = match read_text(&outcome.artifacts.equity) {
        Ok(body) => body,
        Err(error) => {
            return unverified(
                &outcome.run_id,
                format!("产物缺失或不完整（equity）: {error}"),
            )
        }
    };
    let fills_body = match read_text(&outcome.artifacts.fills) {
        Ok(body) => body,
        Err(error) => {
            return unverified(
                &outcome.run_id,
                format!("产物缺失或不完整（fills）: {error}"),
            )
        }
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

    let equity_rows = csv_rows(&equity_body).len() as u64;
    if equity_rows == summary.equity_points && equity_rows == outcome.equity_points {
        checks.push(format!("equity.csv 行数 == equity_points == {equity_rows}"));
    } else {
        mismatches.push(format!(
            "equity.csv 行数 {equity_rows} != summary.equity_points {} / outcome.equity_points {}",
            summary.equity_points, outcome.equity_points
        ));
    }
    match last_equity_raw(&equity_body) {
        Some(last) if last == summary.final_equity_raw => {
            checks.push(format!("equity.csv 末行 == final_equity_raw == {last}"));
        }
        Some(last) => mismatches.push(format!(
            "equity.csv 末行 {last} != summary.final_equity_raw {}",
            summary.final_equity_raw
        )),
        None => mismatches.push("equity.csv 读不出末行权益值".to_string()),
    }

    let fill_rows = csv_rows(&fills_body).len() as u64;
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
