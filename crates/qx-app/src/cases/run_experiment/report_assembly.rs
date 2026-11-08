//! Training report projection, OOS/fold assembly, and persistence handoff.

use super::*;
use qx_guanxing::Bar;
use std::path::PathBuf;

pub(super) struct ExperimentResultAssemblyInput<'a, 'factory> {
    pub(super) spec: &'a RunExperimentSpec,
    pub(super) context: &'a RunContext,
    pub(super) cancellation: Option<&'a CancellationToken>,
    pub(super) identity: Option<&'a StrategyArtifactIdentity>,
    pub(super) factory: Option<&'a mut StrategyFactory<'factory>>,
    pub(super) evaluation_bars: Option<Vec<Bar>>,
    pub(super) evaluation_frame: Option<BarFrame>,
    pub(super) root: PathBuf,
    pub(super) spec_fingerprint: String,
    pub(super) input_data_fingerprint: String,
    pub(super) combinations: Vec<BTreeMap<String, i128>>,
    pub(super) total: usize,
    pub(super) candidates: Vec<ExperimentCandidateResult>,
    pub(super) search_trace: Vec<ExperimentSearchTrial>,
    pub(super) cost_sensitivity: Vec<ExecutionCostSensitivityResult>,
    pub(super) canonical_run_spec: CanonicalRunSpec,
    pub(super) canonical_run_spec_json: String,
    pub(super) canonical_run_spec_fingerprint: String,
    pub(super) persisted_input: String,
    pub(super) report_index_path: String,
    pub(super) spec_artifact_path: String,
}

pub(super) fn assemble_and_persist<'a, 'factory>(
    input: ExperimentResultAssemblyInput<'a, 'factory>,
) -> Result<Option<RunExperimentResult>, AppError> {
    let ExperimentResultAssemblyInput {
        spec,
        context,
        cancellation,
        identity,
        factory,
        evaluation_bars,
        evaluation_frame,
        root,
        spec_fingerprint,
        input_data_fingerprint,
        combinations,
        total,
        candidates,
        search_trace,
        cost_sensitivity,
        canonical_run_spec,
        canonical_run_spec_json,
        canonical_run_spec_fingerprint,
        persisted_input,
        report_index_path,
        spec_artifact_path,
    } = input;
    let successful: Vec<_> = candidates
        .iter()
        .filter_map(|candidate| candidate.outcome.as_ref())
        .collect();
    let comparison = if successful.len() >= 2 {
        let runs = successful
            .iter()
            .map(|outcome| ComparedRun {
                run_id: outcome.run_id.clone(),
                instrument: outcome.instrument.clone(),
                data_fingerprint: outcome.data_fingerprint.clone(),
                result_hash: outcome.result_hash.clone(),
                return_bps: outcome.return_bps,
                max_drawdown_bps: outcome.max_drawdown_bps,
            })
            .collect();
        Some(compare_runs(
            &CompareRunsSpec {
                schema_version: COMPARE_RUNS_SCHEMA_VERSION,
                runs,
            },
            context,
        )?)
    } else {
        None
    };
    // 切分与基准都要 Bar 序列本身（基准用收盘价、切分用样本根数），所以只在**真被请求**时
    // 才多读一次数据文件——没写这两格的 spec 走的还是原来那条路，一次多余 IO 都没有。
    let (out_of_sample, baselines, fold_searches) = if let Some(bars) = evaluation_bars.as_deref() {
        let baselines = evaluate_baselines(bars, &spec.baselines);
        let out_of_sample = match &spec.split_plan {
            Some(plan) => {
                if spec
                    .search
                    .as_ref()
                    .is_some_and(|search| search.fold_local_search)
                {
                    let frame = evaluation_frame.as_ref().ok_or_else(|| {
                        AppError::new(AppErrorCategory::InternalInvariant, "折内搜索缺少 BarFrame")
                    })?;
                    let result = evaluate_fold_local_search(
                        spec,
                        frame,
                        &combinations,
                        &root,
                        &spec_fingerprint,
                        &input_data_fingerprint,
                        context,
                        cancellation,
                        identity,
                        factory,
                    )?;
                    let Some((report, searches)) = result else {
                        return Ok(None);
                    };
                    (report, searches)
                } else {
                    let drawdown_limit = spec
                        .search
                        .as_ref()
                        .and_then(|search| search.max_training_drawdown_bps);
                    let has_candidate_for_oos = candidates.iter().any(|candidate| {
                        candidate.outcome.is_some()
                            && spec
                                .search
                                .as_ref()
                                .and_then(|search| search.minimum_training_robustness_score_bps)
                                .is_none_or(|minimum| {
                                    search_trace
                                        .iter()
                                        .find(|trial| trial.ordinal == candidate.ordinal)
                                        .and_then(|trial| trial.training_robustness_score_bps)
                                        .is_some_and(|score| score >= minimum)
                                })
                            && drawdown_limit.is_none_or(|limit| {
                                search_trace
                                    .iter()
                                    .find(|trial| trial.ordinal == candidate.ordinal)
                                    .and_then(|trial| trial.training_max_drawdown_bps)
                                    .is_some_and(|drawdown| drawdown <= limit)
                            })
                    });
                    if !has_candidate_for_oos {
                        (None, Vec::new())
                    } else {
                        let report = evaluate_out_of_sample(
                            plan,
                            bars.len(),
                            &candidates,
                            drawdown_limit,
                            spec.search
                                .as_ref()
                                .and_then(|search| search.robustness_subwindows),
                            spec.search
                                .as_ref()
                                .and_then(|search| search.minimum_training_robustness_score_bps),
                            || cancellation.is_some_and(CancellationToken::is_cancelled),
                        )?;
                        let Some(report) = report else {
                            return Ok(None);
                        };
                        (Some(report), Vec::new())
                    }
                }
            }
            None => (None, Vec::new()),
        };
        (out_of_sample.0, baselines, out_of_sample.1)
    } else {
        (None, Vec::new(), Vec::new())
    };
    let cost_robustness_scenario_names = spec
        .cost_sensitivity
        .iter()
        .filter(|scenario| scenario.max_training_return_degradation_bps.is_some())
        .map(|scenario| scenario.name.as_str())
        .collect::<Vec<_>>();
    let cost_robustness_passed = (!cost_robustness_scenario_names.is_empty()).then(|| {
        let scenario_passes = |results: &[ExecutionCostSensitivityResult]| {
            cost_robustness_scenario_names.iter().all(|name| {
                results
                    .iter()
                    .find(|result| result.name.as_str() == *name)
                    .and_then(|result| result.cost_robustness_passed)
                    == Some(true)
            })
        };
        if spec
            .search
            .as_ref()
            .is_some_and(|search| search.fold_local_search)
        {
            !fold_searches.is_empty()
                && fold_searches
                    .iter()
                    .all(|fold| scenario_passes(&fold.cost_sensitivity))
        } else {
            scenario_passes(&cost_sensitivity)
        }
    });
    let neighborhood_cost_scenario_names = spec
        .cost_sensitivity
        .iter()
        .filter(|scenario| {
            scenario
                .max_neighborhood_training_return_degradation_bps
                .is_some()
        })
        .map(|scenario| scenario.name.as_str())
        .collect::<Vec<_>>();
    let neighborhood_cost_robustness_passed =
        (!neighborhood_cost_scenario_names.is_empty()).then(|| {
            let scenarios_pass = |results: &[ExecutionCostSensitivityResult]| {
                neighborhood_cost_scenario_names.iter().all(|name| {
                    results
                        .iter()
                        .find(|result| result.name.as_str() == *name)
                        .and_then(|result| result.neighborhood_cost_robustness.as_ref())
                        .is_some_and(|result| result.passed)
                })
            };
            if spec
                .search
                .as_ref()
                .is_some_and(|search| search.fold_local_search)
            {
                !fold_searches.is_empty()
                    && fold_searches
                        .iter()
                        .all(|fold| scenarios_pass(&fold.cost_sensitivity))
            } else {
                scenarios_pass(&cost_sensitivity)
            }
        });
    let search_report = spec.search.as_ref().map(|plan| ExperimentSearchReport {
        algorithm: plan.algorithm,
        objective: if plan.algorithm == ExperimentSearchAlgorithm::ParetoTpe {
            ExperimentSearchObjective::ParetoReturnDrawdown
        } else {
            ExperimentSearchObjective::Return
        },
        seed: plan.seed,
        budget: plan.budget,
        max_training_drawdown_bps: plan.max_training_drawdown_bps,
        early_pruning: plan.early_pruning.clone(),
        robustness_subwindows: plan.robustness_subwindows,
        minimum_training_robustness_score_bps: plan.minimum_training_robustness_score_bps,
        cost_robustness_passed,
        neighborhood_cost_robustness_passed,
        neighborhood: if plan.continuous_parameter_space.is_empty() {
            neighborhood_report_with_radius_and_budget(
                &spec.parameter_space,
                &combinations,
                &search_trace,
                plan.neighborhood_radius.unwrap_or(1),
                plan.max_training_drawdown_bps,
            )
        } else {
            continuous_neighborhood_report_with_radius_and_budget(
                &plan.continuous_parameter_space,
                &search_trace,
                plan.neighborhood_radius.unwrap_or(1),
                plan.max_training_drawdown_bps,
            )
        },
        trials: search_trace,
        fold_searches,
    });
    let result = RunExperimentResult {
        schema_version: RUN_EXPERIMENT_SCHEMA_VERSION,
        experiment_id: spec.experiment_id.clone(),
        spec_fingerprint,
        canonical_run_spec_json: Some(canonical_run_spec_json),
        canonical_run_spec_fingerprint: Some(canonical_run_spec_fingerprint.clone()),
        input_data_fingerprint: Some(input_data_fingerprint),
        result_hash: None,
        run_manifest: None,
        run_record: None,
        artifact_locations: None,
        total_candidates: total,
        completed_candidates: candidates.len(),
        succeeded_candidates: successful.len(),
        failed_candidates: candidates
            .iter()
            .filter(|candidate| candidate.outcome.is_none() && candidate.pruning.is_none())
            .count(),
        pruned_candidates: candidates
            .iter()
            .filter(|candidate| candidate.pruning.is_some())
            .count(),
        candidates,
        comparison,
        out_of_sample,
        baselines,
        search: search_report,
        cost_sensitivity,
        artifact_path: report_index_path.clone(),
    };
    if cancellation.is_some_and(CancellationToken::is_cancelled) {
        return Ok(None);
    }
    std::fs::create_dir_all(&root)
        .map_err(|error| AppError::from_io("创建实验产物目录", &error))?;
    write_spec_artifact(&spec_artifact_path, &persisted_input, "参数实验")?;
    persistence::verify_experiment_candidate_outcomes(&result, context)?;
    let persisted = persistence::persist_experiment_result(
        result,
        &canonical_run_spec,
        &root,
        &report_index_path,
        &spec_artifact_path,
    )?;
    persistence::verify_persisted_experiment_result(&persisted)?;
    Ok(Some(persisted))
}
