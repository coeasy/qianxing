//! Application-owned projection and ranking rules for same-input backtest reports.

use super::{summary_block, summary_number, summary_text};
use qx_spec::RunEvidenceFidelity;
use serde::Serialize;
use serde_json::Value;
use std::path::Path;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComparisonInputIdentity {
    pub instrument: String,
    pub input_kind: String,
    pub dataset_id: String,
    pub dataset_version: String,
    pub fingerprint: String,
}

/// Require an exact shared input identity before comparing multiple completed runs.
pub fn validate_comparison_input_identity(
    expected: Option<&ComparisonInputIdentity>,
    instrument: &str,
    input: &crate::artifact_store::BacktestInputProvenance,
) -> Result<ComparisonInputIdentity, String> {
    let current = ComparisonInputIdentity {
        instrument: instrument.to_owned(),
        input_kind: input.kind.to_owned(),
        dataset_id: input.dataset_id.clone(),
        dataset_version: input.dataset_version.clone(),
        fingerprint: input.fingerprint.clone(),
    };
    if expected.is_some_and(|expected| expected != &current) {
        return Err("横向比较要求标的、输入种类、数据集身份、版本和重算指纹全部相同".into());
    }
    Ok(current)
}

#[derive(Clone, Debug, Serialize)]
pub struct ComparisonEntry {
    pub rank: usize,
    pub benchmark_rank: Option<usize>,
    pub run_id: String,
    pub summary_path: String,
    pub strategy_id: Option<String>,
    pub result_hash: Option<String>,
    pub fidelity: RunEvidenceFidelity,
    pub signal: Option<Value>,
    pub assumptions: Option<Value>,
    pub model_descriptors: Option<Value>,
    pub risk_rule_set_version: Option<String>,
    pub risk_source: Option<String>,
    pub execution_cost_source: Option<String>,
    pub return_bps: i128,
    pub max_drawdown_bps: i128,
    pub fees_raw: Option<i128>,
    pub turnover_raw: Option<i128>,
    pub final_equity_raw: Option<i128>,
    pub fills: Option<i128>,
    pub sharpe: Option<f64>,
    pub sortino: Option<f64>,
    pub calmar: Option<f64>,
    pub win_rate: Option<f64>,
    pub profit_factor: Option<f64>,
    pub benchmark: Option<ComparisonBenchmark>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ComparisonBenchmark {
    pub instrument: String,
    pub fingerprint: String,
    pub price_basis: String,
    pub alignment: String,
    pub period_basis: String,
    pub benchmark_return_bps: Option<f64>,
    pub active_return_bps: Option<f64>,
    pub mean_active_return_bps: Option<f64>,
    pub tracking_error_bps: Option<f64>,
    pub information_ratio: Option<f64>,
}

/// Project an already identity-verified run summary into the shared comparison report model.
pub fn comparison_entry_from_summary(
    summary_path: &Path,
    summary: &Value,
    run_id: String,
) -> Result<ComparisonEntry, String> {
    let required = |pointer: &str, label: &str| {
        summary_number(summary, pointer).ok_or_else(|| format!("对比摘要缺少有效 {label}"))
    };
    let max_drawdown_bps = required("/metrics/max_drawdown_bps", "max_drawdown_bps")?;
    if !(0..=10_000).contains(&max_drawdown_bps) {
        return Err(format!("摘要 {run_id} 的最大回撤超出 0..=10000 bps"));
    }
    Ok(ComparisonEntry {
        rank: 0,
        benchmark_rank: None,
        run_id,
        summary_path: summary_path.to_string_lossy().into_owned(),
        strategy_id: summary_text(summary, "/strategy_id"),
        result_hash: summary_text(summary, "/result_hash"),
        fidelity: crate::artifact_store::run_evidence_fidelity_classification(summary),
        signal: summary_block(summary, "signal").cloned(),
        assumptions: summary_block(summary, "assumptions").cloned(),
        model_descriptors: summary_block(summary, "model_descriptors").cloned(),
        risk_rule_set_version: summary_text(summary, "/risk_rules/rule_set_version"),
        risk_source: summary_text(summary, "/risk_rules/source"),
        execution_cost_source: summary_text(summary, "/execution_costs/source"),
        return_bps: required("/metrics/return_bps", "return_bps")?,
        max_drawdown_bps,
        fees_raw: summary_number(summary, "/metrics/fees_raw"),
        turnover_raw: summary_number(summary, "/metrics/turnover_raw"),
        final_equity_raw: summary_number(summary, "/metrics/final_equity_raw"),
        fills: summary_number(summary, "/fills"),
        sharpe: summary.pointer("/metrics/sharpe").and_then(Value::as_f64),
        sortino: summary.pointer("/metrics/sortino").and_then(Value::as_f64),
        calmar: summary.pointer("/metrics/calmar").and_then(Value::as_f64),
        win_rate: summary.pointer("/metrics/win_rate").and_then(Value::as_f64),
        profit_factor: summary
            .pointer("/metrics/profit_factor")
            .and_then(Value::as_f64),
        benchmark: None,
    })
}

/// Rank runs by return descending, drawdown ascending, then stable run identity.
pub fn rank_comparison_by_return(entries: &mut [ComparisonEntry]) {
    entries.sort_by(|left, right| {
        right
            .return_bps
            .cmp(&left.return_bps)
            .then_with(|| left.max_drawdown_bps.cmp(&right.max_drawdown_bps))
            .then_with(|| left.run_id.cmp(&right.run_id))
    });
    for (index, entry) in entries.iter_mut().enumerate() {
        entry.rank = index + 1;
    }
}

/// Rank only runs with a computed benchmark active return, preserving missing entries as absent.
pub fn rank_comparison_by_active_return(entries: &mut [ComparisonEntry]) {
    let mut ordered = entries
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            entry
                .benchmark
                .as_ref()
                .and_then(|metrics| metrics.active_return_bps)
                .map(|active_return| (index, active_return))
        })
        .collect::<Vec<_>>();
    ordered.sort_by(|(left_index, left_active), (right_index, right_active)| {
        right_active
            .total_cmp(left_active)
            .then_with(|| {
                entries[*left_index]
                    .max_drawdown_bps
                    .cmp(&entries[*right_index].max_drawdown_bps)
            })
            .then_with(|| {
                entries[*left_index]
                    .run_id
                    .cmp(&entries[*right_index].run_id)
            })
    });
    for (rank, (index, _)) in ordered.into_iter().enumerate() {
        entries[index].benchmark_rank = Some(rank + 1);
    }
}
