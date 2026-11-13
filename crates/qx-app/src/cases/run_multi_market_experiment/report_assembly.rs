//! Final multi-market experiment result assembly and publication.

use super::*;
use qx_spec::CanonicalRunSpec;
use std::path::Path;

pub(super) struct MultiMarketExperimentAssembly<'a> {
    pub(super) spec: &'a RunMultiMarketExperimentSpec,
    pub(super) total: usize,
    pub(super) spec_fingerprint: String,
    pub(super) candidates: Vec<MultiMarketExperimentCandidate>,
    pub(super) ranking: Vec<MultiMarketExperimentRankedCandidate>,
    pub(super) out_of_sample: Option<crate::ExperimentOutOfSampleReport>,
    pub(super) fold_candidate_failures: Vec<MultiMarketExperimentFoldCandidateFailure>,
    pub(super) fold_searches: Vec<MultiMarketExperimentFoldSearchReport>,
    pub(super) artifact_path: String,
    pub(super) canonical_run_spec_json: String,
    pub(super) canonical_run_spec_fingerprint: String,
    pub(super) input_data_fingerprint: String,
    pub(super) canonical_run_spec: &'a CanonicalRunSpec,
    pub(super) root: &'a Path,
    pub(super) spec_path: &'a str,
    pub(super) canonical_input: &'a str,
    pub(super) cancellation: Option<&'a CancellationToken>,
}

pub(super) fn assemble_multi_market_experiment_result(
    input: MultiMarketExperimentAssembly<'_>,
) -> Result<Option<RunMultiMarketExperimentResult>, AppError> {
    let MultiMarketExperimentAssembly {
        spec,
        total,
        spec_fingerprint,
        candidates,
        ranking,
        out_of_sample,
        fold_candidate_failures,
        fold_searches,
        artifact_path,
        canonical_run_spec_json,
        canonical_run_spec_fingerprint,
        input_data_fingerprint,
        canonical_run_spec,
        root,
        spec_path,
        canonical_input,
        cancellation,
    } = input;
    let succeeded_candidates = ranking.len();
    let search = spec
        .search
        .as_ref()
        .map(|search| MultiMarketExperimentSearchReport {
            algorithm: search.algorithm,
            objective: if search.algorithm == ExperimentSearchAlgorithm::ParetoTpe {
                ExperimentSearchObjective::ParetoReturnDrawdown
            } else {
                ExperimentSearchObjective::Return
            },
            seed: search.seed,
            budget: search.budget,
            max_training_drawdown_bps: search.max_training_drawdown_bps,
            fold_local_search: search.fold_local_search,
            fold_searches,
        });
    let result = RunMultiMarketExperimentResult {
        schema_version: RUN_MULTI_MARKET_EXPERIMENT_SCHEMA_VERSION,
        experiment_id: spec.experiment_id.clone(),
        target_strategy_id: spec.target_strategy_id.clone(),
        spec_fingerprint,
        total_candidates: total,
        completed_candidates: candidates.len(),
        succeeded_candidates,
        failed_candidates: total - succeeded_candidates,
        candidates,
        ranking,
        out_of_sample,
        fold_candidate_failures,
        search,
        artifact_path: artifact_path.clone(),
        canonical_run_spec_json: Some(canonical_run_spec_json),
        canonical_run_spec_fingerprint: Some(canonical_run_spec_fingerprint),
        input_data_fingerprint: Some(input_data_fingerprint),
        result_hash: None,
        run_manifest: None,
        run_record: None,
        artifact_locations: None,
    };
    if cancellation.is_some_and(CancellationToken::is_cancelled) {
        return Ok(None);
    }
    std::fs::create_dir_all(root)
        .map_err(|error| AppError::from_io("创建多市场实验目录", &error))?;
    artifacts::write_input_spec(spec_path, canonical_input)?;
    let persisted =
        artifacts::persist_result(result, canonical_run_spec, root, &artifact_path, spec_path)?;
    artifacts::verify_persisted_result(&persisted)?;
    Ok(Some(persisted))
}
