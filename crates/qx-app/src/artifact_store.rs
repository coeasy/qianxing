//! Shared persistence operations for versioned application artifacts.

use crate::AppError;
use qx_core::RunManifest;
use qx_xingban::BacktestReport;
use std::path::{Path, PathBuf};

mod backtest_input_verification;
mod backtest_summary;
mod evidence_bundle;
mod multi_builtin_artifacts;
mod multi_leg_attribution;
mod multi_leg_composition;
mod multi_leg_legs;
mod multi_leg_verification;
mod portable_build_attestation;
mod portable_run_evidence;
mod reader;
mod run_evidence_assembly;
mod run_evidence_files;
mod run_evidence_readout;
mod run_manifest;
mod run_record;
pub mod sink;
mod strategy_backtest_artifacts;

pub use backtest_input_verification::{
    recompute_declared_backtest_input, verified_barframe_event_contexts, verified_barframe_preview,
    verified_depthframe_preview, VerifiedBarFrameEventContext, VerifiedBarFramePoint,
    VerifiedBarFramePreview, VerifiedDepthFramePoint, VerifiedDepthFramePreview,
    VerifiedDepthLevel,
};
pub use backtest_summary::{
    account_base_source_label, persist_backtest_artifacts, persist_backtest_artifacts_with_summary,
    serialize_backtest_fills_csv, serialize_multi_leg_backtest_fills_csv, write_backtest_artifact,
    BacktestAccountBase, BacktestArtifacts, BacktestInputProvenance, BacktestSampleTimestamps,
    BacktestSignalParams, PersistedBacktestArtifacts, PortfolioFillAttribution,
    BACKTEST_ACCOUNT_BASE_BOTH_DECLARED_SOURCE, BACKTEST_ACCOUNT_BASE_CONFIG_SOURCE,
    BACKTEST_ACCOUNT_BASE_DEFAULT_SOURCE, BACKTEST_ACCOUNT_BASE_FUNDING_RULE_SOURCE,
    BACKTEST_ACCOUNT_BASE_REGISTRY_SOURCE, BACKTEST_SUMMARY_SCHEMA_VERSION,
};
pub use evidence_bundle::serialize_verified_evidence_bundle;
pub use multi_builtin_artifacts::{
    publish_multi_builtin_backtest_artifacts, MultiBuiltinArtifactPublicationInput,
    MultiBuiltinArtifactSet, MultiBuiltinPublishedArtifacts,
};
pub use multi_leg_attribution::persist_multi_leg_attribution_artifact;
pub use multi_leg_composition::{
    persist_multi_leg_composition_artifacts, MultiLegCompositionArtifactInput,
};
pub use multi_leg_legs::{persist_multi_leg_run_artifacts, MultiLegRunArtifactInput};
pub use multi_leg_verification::{
    recompute_multi_barframe_input, recompute_multi_barframe_input_from_evidence,
    verify_barframe_dataset_quality,
};
pub use portable_build_attestation::{
    prepare_portable_strategy_build_attestation, validate_portable_strategy_build_attestation,
    verify_portable_build_attestation_artifact_refs, StrategyBuildAttestation,
    StrategyBuildMaterial,
};
pub use portable_run_evidence::{
    attach_portable_run_evidence_supplements, commit_portable_run_evidence_export,
    copy_portable_run_evidence_file, prepare_portable_run_evidence_dependencies,
    prepare_portable_run_evidence_export, prepare_portable_run_evidence_file,
    resolve_portable_run_dependency, run_evidence_safe_component, run_evidence_source_path,
    verify_portable_run_dependency_digest, PortableRunEvidenceDependencyPlan,
    PortableRunEvidenceExportPlan, PortableRunEvidenceGeneratedFile,
    PortableRunEvidencePreparedFile, PortableRunEvidenceSource, PortableRunEvidenceSupplements,
};
pub(crate) use reader::{
    artifact_too_large, read_text_bounded_by_id, scan_csv_artifact, scan_csv_artifact_by_id,
    MAX_ARTIFACT_ROWS,
};
pub use reader::{
    read_bounded_line, read_text_bounded, ArtifactId, ArtifactLocatorV1, ArtifactReader,
    ArtifactStream, FileArtifactReader,
};
pub use run_evidence_assembly::{
    assemble_run_evidence_payload, persist_run_evidence_bundle, prepare_run_evidence_assembly,
    RunEvidenceAssembly, RunEvidencePreparation,
};
pub use run_evidence_files::{
    canonical_run_evidence_path, load_run_evidence_identity_files,
    run_evidence_artifact_references, run_evidence_dataset_component,
    run_evidence_depth_strategy_diagnostics_array, run_evidence_quality_manifest_reference,
    run_evidence_report_analysis, run_evidence_strategy_diagnostics,
    run_evidence_strategy_diagnostics_array, RunEvidenceRunFiles,
};
pub use run_evidence_readout::{
    run_evidence_assumption_rows, run_evidence_fidelity_classification, run_evidence_summary_str,
    run_evidence_summary_u64, run_evidence_unverified_rows,
};
pub use run_manifest::verify_declared_run_manifest;
pub use run_manifest::{
    verify_canonical_run_spec_for_manifest, verify_declared_run_manifest_with_reader,
};
pub(crate) use run_record::{declared_run_record_artifact_locators, RunRecordArtifactLocators};
pub use run_record::{persist_verified_run_record, verify_declared_run_record};
pub use run_record::{
    verify_declared_run_record_with_reader, MULTI_ACCOUNT_JOINT_BACKTEST_ENVELOPE_KIND,
    MULTI_ACCOUNT_JOINT_BACKTEST_ENVELOPE_SCHEMA_VERSION,
};
pub use strategy_backtest_artifacts::{
    persist_strategy_backtest_artifacts, StrategyBacktestArtifactsInput,
    StrategyBacktestArtifactsResult,
};

/// Verify a multi-account Bar batch's parent manifest against its child manifests and aggregate
/// result. This local read-side helper uses the filesystem artifact adapter, like `verify_run`.
pub fn verify_multi_account_backtest_result(
    result: &crate::spec::MultiAccountBacktestResult,
) -> Result<(), AppError> {
    crate::cases::verify_multi_account_backtest_result(result)
}

/// Verify a multi-account Bar batch using an authorized artifact reader.
pub fn verify_multi_account_backtest_result_with_reader(
    result: &crate::spec::MultiAccountBacktestResult,
    reader: &dyn ArtifactReader,
) -> Result<(), AppError> {
    crate::cases::verify_multi_account_backtest_result_with_reader(result, reader)
}

/// Verify a multi-account Depth batch's parent manifest against its child manifests and summary.
pub fn verify_multi_account_depth_backtest_result(
    result: &crate::spec::MultiAccountDepthBacktestResult,
) -> Result<(), AppError> {
    verify_multi_account_depth_backtest_result_with_reader(result, &FileArtifactReader)
}

/// Verify a multi-account Depth batch using an authorized artifact reader.
pub fn verify_multi_account_depth_backtest_result_with_reader(
    result: &crate::spec::MultiAccountDepthBacktestResult,
    reader: &dyn ArtifactReader,
) -> Result<(), AppError> {
    crate::cases::verify_multi_account_depth_backtest_result(result, reader)
}

/// Persist the canonical identity and complete artifact set for one strategy run.
///
/// The manifest is written first, preserving the existing durable publication order. Facades
/// should use this operation so a run cannot accidentally publish artifacts against a separately
/// assembled manifest.
pub fn persist_strategy_backtest_result(
    root: &Path,
    report: &BacktestReport,
    manifest_input: crate::StrategyBacktestRunManifestInput<'_>,
    artifacts_input: StrategyBacktestArtifactsInput<'_>,
) -> Result<PersistedStrategyBacktestResult, String> {
    let (_run_id, manifest, manifest_path) =
        persist_strategy_backtest_run_manifest(root, report, manifest_input)?;
    let artifacts = persist_strategy_backtest_artifacts(&manifest_path, report, artifacts_input)?;
    Ok(PersistedStrategyBacktestResult {
        manifest,
        manifest_path,
        artifacts,
    })
}

/// Durable outputs published for a complete strategy backtest.
pub struct PersistedStrategyBacktestResult {
    pub manifest: RunManifest,
    pub manifest_path: PathBuf,
    pub artifacts: StrategyBacktestArtifactsResult,
}

/// Persist the canonical qx-core RunManifest under `<root>/runs`.
///
/// Identical manifests are idempotent; a conflicting payload at the same content-addressed
/// path is rejected. Backtest facades use this shared operation instead of writing manifests.
pub fn persist_run_manifest(root: &Path, manifest: &RunManifest) -> Result<PathBuf, AppError> {
    crate::cases::artifacts::persist_run_manifest(root, manifest)
}

/// Derive and persist a strategy run's canonical identity and manifest together.
///
/// Keeping the two operations in one application entrypoint prevents a facade from persisting
/// a manifest whose run ID was derived from a different set of completed-run facts.
pub fn persist_strategy_backtest_run_manifest(
    root: &Path,
    report: &BacktestReport,
    input: crate::StrategyBacktestRunManifestInput<'_>,
) -> Result<(String, RunManifest, PathBuf), String> {
    let (run_id, manifest) = crate::build_strategy_backtest_run_manifest(report, input)?;
    let path =
        persist_run_manifest(root, &manifest).map_err(|error| error.message().to_string())?;
    Ok((run_id, manifest, path))
}
