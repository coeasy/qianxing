//! Shared persistence operations for versioned application artifacts.

use crate::AppError;
use qx_core::RunManifest;
use qx_xingban::BacktestReport;
use std::path::{Path, PathBuf};

mod backtest_summary;
mod evidence_bundle;
mod multi_leg_attribution;
mod multi_leg_composition;
mod portable_build_attestation;
mod portable_run_evidence;
mod run_evidence_assembly;
mod run_evidence_files;
mod run_evidence_readout;
mod run_manifest;
mod run_record;

pub use backtest_summary::{
    persist_backtest_artifacts, write_backtest_artifact, BacktestAccountBase, BacktestArtifacts,
    BacktestInputProvenance, BacktestSampleTimestamps, BacktestSignalParams,
    BACKTEST_SUMMARY_SCHEMA_VERSION,
};
pub use evidence_bundle::serialize_verified_evidence_bundle;
pub use multi_leg_attribution::persist_multi_leg_attribution_artifact;
pub use multi_leg_composition::{
    persist_multi_leg_composition_artifacts, MultiLegCompositionArtifactInput,
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
pub use run_evidence_assembly::{
    assemble_run_evidence_payload, persist_run_evidence_bundle, prepare_run_evidence_assembly,
    RunEvidenceAssembly, RunEvidencePreparation,
};
pub use run_evidence_files::{
    canonical_run_evidence_path, load_run_evidence_identity_files,
    run_evidence_artifact_references, run_evidence_dataset_component,
    run_evidence_quality_manifest_reference, run_evidence_report_analysis, RunEvidenceRunFiles,
};
pub use run_evidence_readout::{
    run_evidence_assumption_rows, run_evidence_fidelity_classification, run_evidence_summary_str,
    run_evidence_summary_u64, run_evidence_unverified_rows,
};
pub use run_manifest::verify_declared_run_manifest;
pub use run_record::{persist_verified_run_record, verify_declared_run_record};

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
