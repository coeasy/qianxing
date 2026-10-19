//! 双腿各自的可复核回测产物。

use super::{
    barframe_dataset_identity, barframe_input_provenance, multi_leg_quality_fingerprint,
    persist_backtest_artifacts, persist_backtest_run_manifest, read_runtime_config,
    rejection_facts, BacktestAccountBase, BacktestArtifacts,
    BACKTEST_ACCOUNT_BASE_FUNDING_RULE_SOURCE, BAR_MATCHING_KERNEL,
};
use crate::build_identity;
use qx_core::Money;
use qx_datastruct::BarFrame;
use qx_guanxing::Bar;
use qx_xingban::RunManifestIdentity;
use std::path::{Path, PathBuf};

pub(crate) struct MultiLegArtifactInput<'a> {
    pub(crate) root: &'a Path,
    pub(crate) strategy_id: &'a str,
    pub(crate) runtime_config_path: Option<&'a Path>,
    pub(crate) primary_path: &'a Path,
    pub(crate) reference_path: &'a Path,
    pub(crate) primary_frame: &'a BarFrame,
    pub(crate) reference_frame: &'a BarFrame,
    pub(crate) primary_bars: &'a [Bar],
    pub(crate) reference_bars: &'a [Bar],
    pub(crate) primary_spec_version: &'static str,
    pub(crate) reference_spec_version: &'static str,
    pub(crate) primary_report: &'a qx_xingban::BacktestReport,
    pub(crate) reference_report: &'a qx_xingban::BacktestReport,
    pub(crate) primary_cash: Money,
    pub(crate) reference_cash: Money,
    pub(crate) risk_rule_set_version: &'a str,
    pub(crate) risk_rule_source: &'static str,
    pub(crate) cost_source: &'a str,
    pub(crate) fill_model: (&'static str, &'static str),
    pub(crate) dataset_quality: [Option<&'a serde_json::Value>; 2],
}

pub(crate) fn persist_multi_leg_run_artifacts(
    input: MultiLegArtifactInput<'_>,
) -> Result<[PathBuf; 2], String> {
    let MultiLegArtifactInput {
        root,
        strategy_id,
        runtime_config_path,
        primary_path,
        reference_path,
        primary_frame,
        reference_frame,
        primary_bars,
        reference_bars,
        primary_spec_version,
        reference_spec_version,
        primary_report,
        reference_report,
        primary_cash,
        reference_cash,
        risk_rule_set_version,
        risk_rule_source,
        cost_source,
        fill_model,
        dataset_quality,
    } = input;
    let base_config_hash = if let Some(path) = runtime_config_path {
        read_runtime_config(path)?.fingerprint()?
    } else {
        let primary_fingerprint = qx_data::fingerprint_bar_frame(primary_frame)?;
        let reference_fingerprint = qx_data::fingerprint_bar_frame(reference_frame)?;
        qx_strategy::sha256_hex(
            format!(
                "{strategy_id}|{primary_fingerprint}|{reference_fingerprint}|{}|{}",
                primary_cash.raw(),
                reference_cash.raw()
            )
            .as_bytes(),
        )
    };
    let config_hash = multi_leg_quality_fingerprint(dataset_quality).map_or_else(
        || base_config_hash.clone(),
        |quality_fingerprint| {
            qx_strategy::sha256_hex(
                format!("{base_config_hash}|dataset-quality:{quality_fingerprint}").as_bytes(),
            )
        },
    );
    let legs = [
        (
            "primary",
            primary_path,
            primary_frame,
            primary_bars,
            primary_spec_version,
            primary_report,
            primary_cash,
        ),
        (
            "reference",
            reference_path,
            reference_frame,
            reference_bars,
            reference_spec_version,
            reference_report,
            reference_cash,
        ),
    ];
    let mut summaries = Vec::with_capacity(2);
    for (index, (label, frame_path, frame, bars, spec_version, report, cash)) in
        legs.into_iter().enumerate()
    {
        let (_, dataset_manifest) = barframe_dataset_identity(frame_path, frame)?;
        let input = barframe_input_provenance(frame_path, &dataset_manifest);
        let run_id = format!("multi-leg:{strategy_id}:{label}:{}", frame.instrument);
        let data_fingerprint = format!("{}:{}", input.kind, input.fingerprint);
        let manifest = report.run_manifest_with_data_fingerprint(
            RunManifestIdentity {
                run_id: &run_id,
                code_commit: build_identity::BUILD_REVISION,
                config_hash: &config_hash,
                strategy_version: &format!("{strategy_id}-v1"),
                instrument_spec_version: spec_version,
                runtime_version: "multi-leg-backtest-v1",
            },
            &data_fingerprint,
        )?;
        let manifest_path = persist_backtest_run_manifest(root, &manifest)?;
        let rejections = rejection_facts(&report.event_log);
        let account_base = BacktestAccountBase {
            cash,
            source: BACKTEST_ACCOUNT_BASE_FUNDING_RULE_SOURCE,
        };
        let (summary_path, _, _) = persist_backtest_artifacts(
            &manifest_path,
            &BacktestArtifacts {
                runtime_config_path,
                market_spec_path: None,
                strategy_id: &format!("{strategy_id}-{label}"),
                instrument: &frame.instrument,
                sample_unit: "bar",
                samples: bars.len(),
                sample_ts: qx_app::artifact_store::BacktestSampleTimestamps::Bars(bars),
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
                risk_rule_set_version,
                risk_rule_source,
                cost_source,
                fill_model: Some(fill_model),
                fill_model_details: Some(&report.fill_model_descriptor),
                execution_model_details: None,
                account_base,
                matching_kernel: BAR_MATCHING_KERNEL,
                rejections: &rejections,
                input,
                signal: None,
                dataset_quality: dataset_quality[index],
            },
        )?;
        summaries.push(summary_path);
    }
    summaries
        .try_into()
        .map_err(|_| "多腿回测未生成恰好两份腿级摘要".to_string())
}

/// Persist a composition-level summary through qx-app's artifact store.
pub(crate) struct MultiCompositionArtifactInput<'a> {
    pub(crate) root: &'a Path,
    pub(crate) runtime_config_path: Option<&'a Path>,
    pub(crate) strategy_id: &'a str,
    pub(crate) primary_path: &'a Path,
    pub(crate) reference_path: &'a Path,
    pub(crate) primary_frame: &'a BarFrame,
    pub(crate) reference_frame: &'a BarFrame,
    pub(crate) primary_report: &'a qx_xingban::BacktestReport,
    pub(crate) reference_report: &'a qx_xingban::BacktestReport,
    pub(crate) leg_summary_paths: [&'a Path; 2],
    pub(crate) dataset_quality: [Option<&'a serde_json::Value>; 2],
    pub(crate) summary: serde_json::Value,
}

pub(crate) fn persist_multi_composition_artifacts(
    input: MultiCompositionArtifactInput<'_>,
) -> Result<PathBuf, String> {
    let MultiCompositionArtifactInput {
        root,
        runtime_config_path,
        strategy_id,
        primary_path,
        reference_path,
        primary_frame,
        reference_frame,
        primary_report,
        reference_report,
        leg_summary_paths,
        dataset_quality,
        summary,
    } = input;
    let (_, primary_dataset) = barframe_dataset_identity(primary_path, primary_frame)?;
    let (_, reference_dataset) = barframe_dataset_identity(reference_path, reference_frame)?;
    let primary_input = barframe_input_provenance(primary_path, &primary_dataset);
    let reference_input = barframe_input_provenance(reference_path, &reference_dataset);
    let config_fingerprint = if let Some(path) = runtime_config_path {
        Some(read_runtime_config(path)?.fingerprint()?)
    } else {
        None
    };
    let runtime_config_path = runtime_config_path.map(|path| path.to_string_lossy().into_owned());
    qx_app::artifact_store::persist_multi_leg_composition_artifacts(
        qx_app::artifact_store::MultiLegCompositionArtifactInput {
            root,
            config_fingerprint: config_fingerprint.as_deref(),
            runtime_config_path: runtime_config_path.as_deref(),
            code_commit: build_identity::BUILD_REVISION,
            strategy_id,
            primary_input: &primary_input,
            reference_input: &reference_input,
            primary_frame,
            reference_frame,
            primary_report,
            reference_report,
            leg_summary_paths,
            dataset_quality,
            summary,
        },
    )
}
