//! Persist the two independent runs produced by a multi-leg backtest.

use super::{
    persist_backtest_artifacts, persist_run_manifest, BacktestAccountBase, BacktestArtifacts,
    BacktestInputProvenance, BacktestSampleTimestamps, BACKTEST_ACCOUNT_BASE_FUNDING_RULE_SOURCE,
    BACKTEST_SUMMARY_SCHEMA_VERSION,
};
use crate::multi_leg_quality_fingerprint;
use qx_core::Money;
use qx_datastruct::BarFrame;
use qx_xingban::{BacktestReport, RunManifestIdentity};
use std::path::{Path, PathBuf};

pub struct MultiLegRunArtifactInput<'a> {
    pub root: &'a Path,
    pub code_commit: &'a str,
    pub strategy_id: &'a str,
    pub runtime_config_path: Option<&'a Path>,
    pub config_fingerprint: &'a str,
    pub inputs: [&'a BacktestInputProvenance; 2],
    pub frames: [&'a BarFrame; 2],
    pub sample_timestamps: [BacktestSampleTimestamps<'a>; 2],
    pub spec_versions: [&'static str; 2],
    pub reports: [&'a BacktestReport; 2],
    pub initial_cash: [Money; 2],
    pub risk_rule_set_version: &'a str,
    pub risk_rule_source: &'static str,
    pub cost_source: &'a str,
    pub fill_model: (&'static str, &'static str),
    pub dataset_quality: [Option<&'a serde_json::Value>; 2],
}

/// Persist both leg manifests and summaries from the same verified application result.
pub fn persist_multi_leg_run_artifacts(
    input: MultiLegRunArtifactInput<'_>,
) -> Result<[PathBuf; 2], String> {
    let base_config_hash = input.config_fingerprint;
    let config_hash = multi_leg_quality_fingerprint(input.dataset_quality).map_or_else(
        || base_config_hash.to_owned(),
        |quality| {
            qx_strategy::sha256_hex(
                format!("{base_config_hash}|dataset-quality:{quality}").as_bytes(),
            )
        },
    );
    let mut paths = Vec::with_capacity(2);
    for index in 0..2 {
        let label = if index == 0 { "primary" } else { "reference" };
        let frame = input.frames[index];
        let report = input.reports[index];
        let leg_strategy_id = format!("{}-{label}", input.strategy_id);
        let run_id = format!(
            "multi-leg:{}:{label}:{}",
            input.strategy_id, frame.instrument
        );
        let data_fingerprint = format!(
            "{}:{}",
            input.inputs[index].kind, input.inputs[index].fingerprint
        );
        let manifest = report.run_manifest_with_data_fingerprint(
            RunManifestIdentity {
                run_id: &run_id,
                code_commit: input.code_commit,
                config_hash: &config_hash,
                strategy_version: &format!("{}-v1", input.strategy_id),
                instrument_spec_version: input.spec_versions[index],
                runtime_version: "multi-leg-backtest-v1",
            },
            &data_fingerprint,
        )?;
        let manifest_path = persist_run_manifest(input.root, &manifest)
            .map_err(|error| error.message().to_string())?;
        let rejections = crate::trade_facts::rejection_facts(&report.event_log);
        let account_base = BacktestAccountBase {
            cash: input.initial_cash[index],
            source: BACKTEST_ACCOUNT_BASE_FUNDING_RULE_SOURCE,
        };
        let (summary_path, _, _) = persist_backtest_artifacts(
            &manifest_path,
            &BacktestArtifacts {
                runtime_config_path: input.runtime_config_path,
                market_spec_path: None,
                strategy_id: &leg_strategy_id,
                instrument: &frame.instrument,
                sample_unit: "bar",
                samples: frame.ts.len(),
                sample_ts: input.sample_timestamps[index],
                equity: &report.equity,
                positions: &report.positions,
                fills: &report.fills,
                execution_attempts: &report.execution_attempts,
                clock_start: report.clock_start,
                clock_end: report.clock_end,
                input_data_hash: report.input_data_hash,
                result_hash: report.result_hash(),
                event_log: &report.event_log,
                ledger: &report.ledger,
                issuer_capital: &report.issuer_capital_snapshots,
                issuer_capital_at_end: report.issuer_capital_at(report.clock_end),
                return_bps: report.return_bps,
                max_drawdown_bps: report.max_drawdown_bps,
                fees_raw: report.fees_raw,
                turnover_raw: report.turnover_raw,
                final_equity_raw: report.final_equity(),
                assumptions: &report.assumptions,
                model_descriptors: &report.model_descriptors,
                risk_rule_set_version: input.risk_rule_set_version,
                risk_rule_source: input.risk_rule_source,
                cost_source: input.cost_source,
                fill_model: Some(input.fill_model),
                fill_model_details: Some(&report.fill_model_descriptor),
                execution_model_details: None,
                account_base,
                matching_kernel: crate::BAR_MATCHING_KERNEL,
                rejections: &rejections,
                input: input.inputs[index].clone(),
                signal: None,
                dataset_quality: input.dataset_quality[index],
                account_id: None,
                portfolio: None,
                summary_extra: None,
            },
        )?;
        paths.push(summary_path);
    }
    paths
        .try_into()
        .map_err(|_| format!("多腿回测未生成恰好两份 v{BACKTEST_SUMMARY_SCHEMA_VERSION} 腿级摘要"))
}
