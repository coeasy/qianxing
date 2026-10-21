//! Persist the complete artifact set for the independent-ledger multi-builtin workflow.

use super::{
    persist_multi_leg_attribution_artifact, persist_multi_leg_composition_artifacts,
    persist_multi_leg_run_artifacts, persist_run_evidence_bundle, prepare_run_evidence_assembly,
    recompute_multi_barframe_input, run_evidence_dataset_component, BacktestInputProvenance,
    BacktestSampleTimestamps, MultiLegCompositionArtifactInput, MultiLegRunArtifactInput,
    RunEvidencePreparation,
};
use crate::MultiBuiltinAttribution;
use crate::MultiBuiltinBacktestRun;
use qx_core::Money;
use qx_datastruct::BarFrame;
use qx_guanxing::Bar;
use qx_xingban::BacktestReport;
use std::path::{Path, PathBuf};

struct MultiBuiltinArtifactSetInput<'a> {
    root: &'a Path,
    code_commit: &'a str,
    strategy_id: &'a str,
    strategy_kind: &'a str,
    quantity: i64,
    funding_bps: i64,
    runtime_config_path: Option<&'a Path>,
    config_fingerprint: Option<&'a str>,
    primary_path: &'a Path,
    reference_path: &'a Path,
    primary_frame: &'a BarFrame,
    reference_frame: &'a BarFrame,
    primary_spec_path: Option<&'a Path>,
    reference_spec_path: Option<&'a Path>,
    spec_versions: [&'static str; 2],
    reports: [&'a BacktestReport; 2],
    initial_cash: [Money; 2],
    risk_rule_set_version: &'a str,
    risk_source: &'static str,
    cost_source: &'a str,
    fill_model: (&'static str, &'static str),
    matching_kernel: &'a str,
    account_base_source: &'a str,
    dataset_quality: [Option<&'a serde_json::Value>; 2],
    pub attribution: &'a MultiBuiltinAttribution,
}

pub struct MultiBuiltinArtifactSet {
    pub leg_summaries: [PathBuf; 2],
    pub attribution: PathBuf,
    pub composition_summary: PathBuf,
    pub composition_summary_json: serde_json::Value,
}

/// Facade-resolved identities needed to publish the complete artifact set for an app-owned run.
pub struct MultiBuiltinArtifactPublicationInput<'a> {
    pub root: &'a Path,
    pub code_commit: &'a str,
    pub strategy_id: &'a str,
    pub strategy_kind: &'a str,
    pub quantity: i64,
    pub funding_bps: i64,
    pub runtime_config_path: Option<&'a Path>,
    pub config_fingerprint: Option<&'a str>,
    pub primary_path: &'a Path,
    pub reference_path: &'a Path,
    pub primary_frame: &'a BarFrame,
    pub reference_frame: &'a BarFrame,
    pub primary_spec_path: Option<&'a Path>,
    pub reference_spec_path: Option<&'a Path>,
    pub spec_versions: [&'static str; 2],
    pub initial_cash: [Money; 2],
    pub risk_rule_set_version: &'a str,
    pub risk_source: &'static str,
    pub cost_source: &'a str,
    pub fill_model: (&'static str, &'static str),
    pub matching_kernel: &'a str,
    pub account_base_source: &'a str,
    pub dataset_quality: [Option<&'a serde_json::Value>; 2],
    pub build_profile: &'a str,
    pub target_triple: &'a str,
    pub operator: &'a str,
}

pub struct MultiBuiltinPublishedArtifacts {
    pub artifacts: MultiBuiltinArtifactSet,
    pub evidence_bundle: PathBuf,
}

/// Publish and verify all multi-leg artifacts from one completed application outcome.
pub fn publish_multi_builtin_backtest_artifacts(
    run: &MultiBuiltinBacktestRun,
    input: MultiBuiltinArtifactPublicationInput<'_>,
) -> Result<MultiBuiltinPublishedArtifacts, String> {
    let MultiBuiltinArtifactPublicationInput {
        build_profile,
        target_triple,
        operator,
        ..
    } = &input;
    let artifacts = persist_multi_builtin_artifact_set(MultiBuiltinArtifactSetInput {
        root: input.root,
        code_commit: input.code_commit,
        strategy_id: input.strategy_id,
        strategy_kind: input.strategy_kind,
        quantity: input.quantity,
        funding_bps: input.funding_bps,
        runtime_config_path: input.runtime_config_path,
        config_fingerprint: input.config_fingerprint,
        primary_path: input.primary_path,
        reference_path: input.reference_path,
        primary_frame: input.primary_frame,
        reference_frame: input.reference_frame,
        primary_spec_path: input.primary_spec_path,
        reference_spec_path: input.reference_spec_path,
        spec_versions: input.spec_versions,
        reports: [&run.primary.report, &run.reference.report],
        initial_cash: input.initial_cash,
        risk_rule_set_version: input.risk_rule_set_version,
        risk_source: input.risk_source,
        cost_source: input.cost_source,
        fill_model: input.fill_model,
        matching_kernel: input.matching_kernel,
        account_base_source: input.account_base_source,
        dataset_quality: input.dataset_quality,
        attribution: &run.attribution,
    })?;
    recompute_multi_barframe_input(&artifacts.composition_summary_json)?;
    let components = [
        (
            "primary",
            input.primary_path,
            input.primary_frame.ts.len() as u64,
        ),
        (
            "reference",
            input.reference_path,
            input.reference_frame.ts.len() as u64,
        ),
    ]
    .into_iter()
    .map(|(role, path, rows)| {
        let path = path
            .canonicalize()
            .map_err(|error| format!("解析 {role} BarFrame 路径失败: {error}"))?;
        run_evidence_dataset_component(
            &artifacts.composition_summary_json,
            role,
            rows,
            path.to_string_lossy().into_owned(),
        )
    })
    .collect::<Result<Vec<_>, String>>()?;
    let (has_report_analysis, assembly) = prepare_run_evidence_assembly(RunEvidencePreparation {
        summary_path: &artifacts.composition_summary,
        summary: &artifacts.composition_summary_json,
        report_analysis: None,
        components,
        build_profile,
        target_triple,
        operator,
    })?;
    let evidence_bundle = persist_run_evidence_bundle(
        &artifacts.composition_summary,
        has_report_analysis,
        assembly,
    )?;
    Ok(MultiBuiltinPublishedArtifacts {
        artifacts,
        evidence_bundle,
    })
}

/// Persist both leg facts, attribution, and the combined summary from one application outcome.
fn persist_multi_builtin_artifact_set(
    input: MultiBuiltinArtifactSetInput<'_>,
) -> Result<MultiBuiltinArtifactSet, String> {
    let inputs = [
        input_provenance(input.primary_path, input.primary_frame)?,
        input_provenance(input.reference_path, input.reference_frame)?,
    ];
    let fallback_fingerprint = if input.config_fingerprint.is_none() {
        Some(fallback_config_fingerprint(
            input.strategy_id,
            input.primary_frame,
            input.reference_frame,
            input.initial_cash,
        )?)
    } else {
        None
    };
    let config_fingerprint = input
        .config_fingerprint
        .or(fallback_fingerprint.as_deref())
        .ok_or("多腿回测缺少配置指纹")?;
    let bars: [Vec<Bar>; 2] = [input.primary_frame.into(), input.reference_frame.into()];
    let leg_summaries = persist_multi_leg_run_artifacts(MultiLegRunArtifactInput {
        root: input.root,
        code_commit: input.code_commit,
        strategy_id: input.strategy_id,
        runtime_config_path: input.runtime_config_path,
        config_fingerprint,
        inputs: [&inputs[0], &inputs[1]],
        frames: [input.primary_frame, input.reference_frame],
        sample_timestamps: [
            BacktestSampleTimestamps::Bars(&bars[0]),
            BacktestSampleTimestamps::Bars(&bars[1]),
        ],
        spec_versions: input.spec_versions,
        reports: input.reports,
        initial_cash: input.initial_cash,
        risk_rule_set_version: input.risk_rule_set_version,
        risk_rule_source: input.risk_source,
        cost_source: input.cost_source,
        fill_model: input.fill_model,
        dataset_quality: input.dataset_quality,
    })?;

    let summary =
        crate::multi_builtin_composition_summary_json(crate::MultiBuiltinCompositionSummary {
            strategy_id: input.strategy_id,
            strategy_kind: input.strategy_kind,
            quantity: input.quantity,
            primary_instrument: &input.primary_frame.instrument.to_string(),
            reference_instrument: &input.reference_frame.instrument.to_string(),
            funding_bps: input.funding_bps,
            risk_rule_set_version: input.risk_rule_set_version,
            risk_source: input.risk_source,
            matching_kernel: input.matching_kernel,
            cost_source: input.cost_source,
            fill_model_name: input.fill_model.0,
            fill_model_source: input.fill_model.1,
            account_base_source: input.account_base_source,
            primary_initial_cash_raw: input.initial_cash[0].raw(),
            reference_initial_cash_raw: input.initial_cash[1].raw(),
            primary_final_equity_raw: input.reports[0].final_equity(),
            reference_final_equity_raw: input.reports[1].final_equity(),
            primary_spec_path: input.primary_spec_path,
            reference_spec_path: input.reference_spec_path,
            attribution: input.attribution,
        });
    let attribution_path = persist_multi_leg_attribution_artifact(
        input.root,
        input.strategy_kind,
        input.reports[0].result_hash(),
        input.reports[1].result_hash(),
        &summary,
    )?;
    let runtime_config_text = input
        .runtime_config_path
        .map(|path| path.to_string_lossy().into_owned());
    let (composition_summary, composition_summary_json) =
        persist_multi_leg_composition_artifacts(MultiLegCompositionArtifactInput {
            root: input.root,
            config_fingerprint: input.config_fingerprint,
            runtime_config_path: runtime_config_text.as_deref(),
            code_commit: input.code_commit,
            strategy_id: input.strategy_id,
            primary_input: &inputs[0],
            reference_input: &inputs[1],
            primary_frame: input.primary_frame,
            reference_frame: input.reference_frame,
            primary_report: input.reports[0],
            reference_report: input.reports[1],
            leg_summary_paths: [&leg_summaries[0], &leg_summaries[1]],
            dataset_quality: input.dataset_quality,
            summary,
        })?;
    Ok(MultiBuiltinArtifactSet {
        leg_summaries,
        attribution: attribution_path,
        composition_summary,
        composition_summary_json,
    })
}

fn fallback_config_fingerprint(
    strategy_id: &str,
    primary: &BarFrame,
    reference: &BarFrame,
    cash: [Money; 2],
) -> Result<String, String> {
    let primary_fingerprint = qx_data::fingerprint_bar_frame(primary)?;
    let reference_fingerprint = qx_data::fingerprint_bar_frame(reference)?;
    Ok(qx_strategy::sha256_hex(
        format!(
            "{strategy_id}|{primary_fingerprint}|{reference_fingerprint}|{}|{}",
            cash[0].raw(),
            cash[1].raw()
        )
        .as_bytes(),
    ))
}

fn input_provenance(path: &Path, frame: &BarFrame) -> Result<BacktestInputProvenance, String> {
    let dataset = super::multi_leg_verification::barframe_dataset_identity(path, frame)?;
    Ok(BacktestInputProvenance {
        kind: "barframe",
        path: path.to_string_lossy().into_owned(),
        dataset_id: dataset.dataset_id,
        dataset_version: dataset.version,
        fingerprint: dataset.fingerprint,
    })
}
