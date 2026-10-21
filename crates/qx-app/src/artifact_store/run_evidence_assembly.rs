//! Assemble the application-owned run-generation payload from verified run facts.

use qx_core::RunManifest;
use qx_spec::{
    ArtifactRef, CapabilityLevel, EvidenceBundle, RunEvidenceBuild, RunEvidenceDataset,
    RunEvidenceDatasetComponent, RunEvidenceIdentity, RunEvidencePayload, RunEvidenceRecompute,
    RunEvidenceReportAnalysis, RunEvidenceRun, RunEvidenceVerification, RunRecord,
};
use std::path::{Path, PathBuf};

/// Adapter-resolved facts for constructing the canonical application-owned assembly input.
pub struct RunEvidencePreparation<'a> {
    pub summary_path: &'a Path,
    pub summary: &'a serde_json::Value,
    pub report_analysis: Option<&'a serde_json::Value>,
    pub components: Vec<qx_spec::RunEvidenceDatasetComponent>,
    pub build_profile: &'a str,
    pub target_triple: &'a str,
    pub operator: &'a str,
}

/// Facts collected and verified by the outer adapter before application assembly.
pub struct RunEvidenceAssembly {
    pub summary: serde_json::Value,
    pub manifest: RunManifest,
    pub record: RunRecord,
    pub dataset_path: String,
    pub dataset_kind: String,
    pub quality_manifest_path: Option<String>,
    pub quality_manifest_sha256: Option<String>,
    pub components: Vec<RunEvidenceDatasetComponent>,
    pub report_analysis: Option<RunEvidenceReportAnalysis>,
    pub artifacts: Vec<ArtifactRef>,
    pub recompute_inputs: Vec<String>,
    pub build_profile: String,
    pub target_triple: String,
    pub operator: String,
}

/// Collect the run EvidenceBundle assembly facts from an already-reverified summary.
///
/// The caller invokes this only after the report path has verified the summary, referenced
/// artifacts, and dataset input. Provider-specific multi-input frame decoding remains in the CLI;
/// evidence identity, report-analysis projection, recompute inputs, and dataset-path semantics live
/// here with the bundle contract.
pub fn prepare_run_evidence_assembly(
    input: RunEvidencePreparation<'_>,
) -> Result<(bool, RunEvidenceAssembly), String> {
    let RunEvidencePreparation {
        summary_path,
        summary,
        report_analysis,
        components,
        build_profile,
        target_triple,
        operator,
    } = input;
    let has_report_analysis = report_analysis.is_some();
    let run_files = crate::artifact_store::load_run_evidence_identity_files(summary)?;
    let artifacts =
        crate::artifact_store::run_evidence_artifact_references(summary, &run_files.record)?;
    let dataset_kind =
        crate::artifact_store::run_evidence_summary_str(summary, &["input", "kind"])?;
    let (quality_manifest_path, quality_manifest_sha256) =
        crate::artifact_store::run_evidence_quality_manifest_reference(summary)?;
    let report_analysis = crate::artifact_store::run_evidence_report_analysis(report_analysis)?;
    let mut recompute_inputs = vec![
        run_files.manifest_path.clone(),
        crate::artifact_store::canonical_run_evidence_path(
            &summary_path.to_string_lossy(),
            "回测摘要",
        )?,
        crate::artifact_store::run_evidence_summary_str(summary, &["input", "path"])?,
    ];
    recompute_inputs.extend(artifacts.iter().map(|artifact| artifact.path.clone()));
    if dataset_kind == "multi-barframe" {
        recompute_inputs.extend(components.iter().map(|component| component.path.clone()));
    }
    if let Some(calendar_path) = report_analysis
        .as_ref()
        .and_then(|analysis| analysis.calendar_input_path.as_deref())
    {
        recompute_inputs.push(calendar_path.to_string());
    }
    let input_path = crate::artifact_store::run_evidence_summary_str(summary, &["input", "path"])?;
    let dataset_path = if dataset_kind == "multi-barframe" {
        input_path
    } else {
        crate::artifact_store::canonical_run_evidence_path(&input_path, "数据输入")?
    };
    Ok((
        has_report_analysis,
        RunEvidenceAssembly {
            summary: summary.clone(),
            manifest: run_files.manifest,
            record: run_files.record,
            dataset_path,
            dataset_kind,
            quality_manifest_path,
            quality_manifest_sha256,
            components,
            report_analysis,
            artifacts,
            recompute_inputs,
            build_profile: build_profile.to_string(),
            target_triple: target_triple.to_string(),
            operator: operator.to_string(),
        },
    ))
}

/// Persist an already-verified run EvidenceBundle with its stable report-derived filename.
///
/// The CLI gathers filesystem and provider-specific facts; the application layer owns the final
/// run bundle, schema round-trip, filename policy, and idempotent artifact commit.
pub fn persist_run_evidence_bundle(
    summary_path: &Path,
    has_report_analysis: bool,
    facts: RunEvidenceAssembly,
) -> Result<PathBuf, String> {
    let bundle = EvidenceBundle::run(assemble_run_evidence_payload(facts)?);
    let payload =
        crate::artifact_store::serialize_verified_evidence_bundle(&bundle, |written, decoded| {
            format!(
                "运行证据包规范化读回与写入对象不一致: report_analysis input={:?} decoded={:?}",
                match &written.generation {
                    qx_spec::EvidenceGeneration::Run(run) => run.report_analysis.clone(),
                    _ => None,
                },
                match &decoded.generation {
                    qx_spec::EvidenceGeneration::Run(run) => run.report_analysis.clone(),
                    _ => None,
                }
            )
        })?;
    let path = summary_path.with_file_name(
        summary_path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_suffix(".summary.json"))
            .map(|stem| {
                if has_report_analysis {
                    format!("{stem}.report-analysis.evidence.json")
                } else {
                    format!("{stem}.evidence.json")
                }
            })
            .ok_or_else(|| format!("回测摘要文件名无效: {}", summary_path.display()))?,
    );
    super::write_backtest_artifact(&path, &payload, "EvidenceBundle")?;
    Ok(path)
}

/// Map verified run facts into the stable qx-spec run-generation shape.
///
/// Hashing, path resolution, summary file IO, and multi-input decoding remain adapter concerns;
/// this function owns the meaning and mapping of the EvidenceBundle payload.
pub fn assemble_run_evidence_payload(
    facts: RunEvidenceAssembly,
) -> Result<RunEvidencePayload, String> {
    let summary = &facts.summary;
    let manifest = &facts.manifest;
    let record = &facts.record;
    let quality_report = summary
        .pointer("/dataset_quality/quality_report")
        .cloned()
        .map(without_quality_row_count)
        .map(serde_json::from_value)
        .transpose()
        .map_err(|error| format!("回测摘要的数据质量报告无效: {error}"))?;

    Ok(RunEvidencePayload {
        run: RunEvidenceRun {
            run_id: manifest.run_id.clone(),
            code_commit: manifest.code_commit.clone(),
            runtime_version: manifest.runtime_version.clone(),
            config_hash: manifest.config_hash.clone(),
            data_fingerprint: manifest.data_fingerprint.clone(),
            input_components: manifest.input_components.clone(),
            clock_start: manifest.clock_start,
            clock_end: manifest.clock_end,
            global_seed: manifest.global_seed,
            determinism_mode: manifest.determinism_mode,
            result_hash: manifest.result_hash.clone(),
            strategy_version: manifest.strategy_version.clone(),
            instrument_spec_version: manifest.instrument_spec_version.clone(),
            model_fingerprint: manifest.model_fingerprint.clone(),
            input_event_hash: manifest.input_event_hash.clone(),
            output_event_hash: manifest.output_event_hash.clone(),
        },
        verification: RunEvidenceVerification {
            // Local backtest evidence reaches L1; L3/L4 requires a real venue window.
            capability_level: CapabilityLevel::L1,
            replay_verdict: record.replay_verdict,
            artifact_digests_verified: true,
            verified_artifact_count: facts.artifacts.len() as u64,
        },
        dataset: RunEvidenceDataset {
            kind: facts.dataset_kind,
            dataset_id: crate::artifact_store::run_evidence_summary_str(
                summary,
                &["input", "dataset_id"],
            )?,
            version: crate::artifact_store::run_evidence_summary_str(
                summary,
                &["input", "dataset_version"],
            )?,
            content_fingerprint: crate::artifact_store::run_evidence_summary_str(
                summary,
                &["input", "fingerprint"],
            )?,
            // The composed identity comes from RunManifest and is pinned to the run block.
            composed_fingerprint: manifest.data_fingerprint.clone(),
            row_count: crate::artifact_store::run_evidence_summary_u64(summary, &["bars"])?,
            path: facts.dataset_path,
            manifest_fingerprint: summary
                .pointer("/dataset_quality/manifest_fingerprint")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            quality_manifest_path: facts.quality_manifest_path,
            quality_manifest_sha256: facts.quality_manifest_sha256,
            quality_report,
            components: facts.components,
        },
        report_analysis: facts.report_analysis,
        identity: RunEvidenceIdentity {
            strategy_id: crate::artifact_store::run_evidence_summary_str(
                summary,
                &["strategy_id"],
            )?,
            strategy_version: manifest.strategy_version.clone(),
            instrument: crate::artifact_store::run_evidence_summary_str(summary, &["instrument"])?,
            config_digest: manifest.config_hash.clone(),
        },
        assumptions: crate::artifact_store::run_evidence_assumption_rows(summary)?,
        fidelity: Some(crate::artifact_store::run_evidence_fidelity_classification(
            summary,
        )),
        build: RunEvidenceBuild {
            runtime_version: manifest.runtime_version.clone(),
            code_commit: manifest.code_commit.clone(),
            profile: facts.build_profile,
            target_triple: facts.target_triple,
        },
        unverified: crate::artifact_store::run_evidence_unverified_rows(summary)?,
        recompute: RunEvidenceRecompute {
            steps: vec![
                format!(
                    "按 run.data_fingerprint={} 取回同一份数据集输入（摘要 input.path 那一份）",
                    manifest.data_fingerprint
                ),
                format!(
                    "用 config_hash={} 对应的运行时配置重跑同一档回测入口",
                    manifest.config_hash
                ),
                format!(
                    "比对重跑产出的 result_hash 与 recompute.expected_result_hash={}，两者必须逐字相等",
                    manifest.result_hash
                ),
            ],
            inputs: facts.recompute_inputs,
            expected_result_hash: manifest.result_hash.clone(),
        },
        operator: facts.operator,
        artifacts: facts.artifacts,
    })
}

fn without_quality_row_count(mut report: serde_json::Value) -> serde_json::Value {
    if let Some(fields) = report.as_object_mut() {
        fields.remove("row_count");
    }
    report
}
