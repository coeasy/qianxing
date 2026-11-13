//! Independent adaptive search on each walk-forward training window.

use super::artifacts::{equity_series, read_text, write_text};
use super::experiment_grid::apply_parameters;
use super::experiment_search::{
    continuous_neighborhood_report_with_radius_and_budget, neighborhood_parameter_sets,
    neighborhood_report_with_radius_and_budget, ContinuousSearchState, DrawdownPruningWindow,
    ExperimentFoldSearchReport, ExperimentSearchAlgorithm, ExperimentSearchObjective,
    ExperimentSearchTrial, SearchBacktestAttempt, SearchState,
};
use super::experiment_split::{
    experiment_folds, summarize_folds, window_max_drawdown_bps, window_return_bps,
    window_robustness_score_bps, ExperimentFoldReport, ExperimentOutOfSampleReport,
};
use super::run_backtest::{
    run_backtest_inner, run_backtest_inner_for_experiment,
    run_backtest_inner_with_experiment_strategy,
};
use super::run_experiment::{
    cost_robustness_passed, cost_sensitivity_fold_delta, ExecutionCostSensitivityNeighborResult,
    ExecutionCostSensitivityNeighborhoodResult, ExecutionCostSensitivityResult,
    ExperimentCandidateResult, RunExperimentSpec, StrategyFactory,
};
use crate::run_handle::CancellationToken;
use crate::{AppError, AppErrorCategory, RunContext, StrategyArtifactIdentity};
use qx_datastruct::BarFrame;
use std::collections::BTreeMap;
use std::path::Path;

mod checkpoint;
pub(super) use checkpoint::{
    artifact_fingerprint, load_cost_scenario_checkpoint, write_cost_scenario_checkpoint,
};
use checkpoint::{load_fold_candidate_checkpoint, write_fold_candidate_checkpoint};

pub(super) const MAX_FOLD_LOCAL_RUNS: usize = 4_096;

type FoldLocalSearchResult = Option<(
    Option<ExperimentOutOfSampleReport>,
    Vec<ExperimentFoldSearchReport>,
)>;
type BestFoldCandidate = (
    usize,
    i32,
    i32,
    Option<i32>,
    u32,
    Vec<i128>,
    String,
    BTreeMap<String, i128>,
);

pub(super) fn fold_local_execution_count(
    fold_count: usize,
    search_budget: usize,
    cost_scenario_count: usize,
    neighborhood_cost_scenario_count: usize,
) -> Option<usize> {
    let per_search = search_execution_count(
        search_budget,
        cost_scenario_count,
        neighborhood_cost_scenario_count,
    )?;
    fold_count.checked_add(1)?.checked_mul(per_search)
}

pub(super) fn search_execution_count(
    search_budget: usize,
    cost_scenario_count: usize,
    neighborhood_cost_scenario_count: usize,
) -> Option<usize> {
    let neighborhood_extra =
        neighborhood_cost_scenario_count.checked_mul(search_budget.saturating_sub(1))?;
    search_budget
        .checked_add(cost_scenario_count)?
        .checked_add(neighborhood_extra)
}

// These are independent fold/search inputs; grouping them would hide the data provenance.
#[allow(clippy::too_many_arguments)]
pub(super) fn evaluate_fold_local_search(
    spec: &RunExperimentSpec,
    frame: &BarFrame,
    grid: &[BTreeMap<String, i128>],
    root: &Path,
    spec_fingerprint: &str,
    input_data_fingerprint: &str,
    context: &RunContext,
    cancellation: Option<&CancellationToken>,
    identity: Option<&StrategyArtifactIdentity>,
    mut factory: Option<&mut StrategyFactory<'_>>,
) -> Result<FoldLocalSearchResult, AppError> {
    let plan = spec.search.as_ref().ok_or_else(|| {
        AppError::new(AppErrorCategory::InternalInvariant, "折内搜索缺少搜索计划")
    })?;
    let split = spec.split_plan.as_ref().ok_or_else(|| {
        AppError::new(AppErrorCategory::InternalInvariant, "折内搜索缺少切分计划")
    })?;
    let folds = experiment_folds(split, frame.len())?;
    let neighborhood_cost_scenario_count = spec
        .cost_sensitivity
        .iter()
        .filter(|scenario| {
            scenario
                .max_neighborhood_training_return_degradation_bps
                .is_some()
        })
        .count();
    if fold_local_execution_count(
        folds.len(),
        plan.budget,
        spec.cost_sensitivity.len(),
        neighborhood_cost_scenario_count,
    )
    .is_none_or(|runs| runs > MAX_FOLD_LOCAL_RUNS)
    {
        return Err(AppError::new(
            AppErrorCategory::InvalidInput,
            format!("折内外搜索候选与成本情景合计最多允许 {MAX_FOLD_LOCAL_RUNS} 次回测"),
        ));
    }
    let mut reports = Vec::with_capacity(folds.len());
    let mut searches = Vec::with_capacity(folds.len());
    let mut has_unselected_fold = false;
    for (fold_index, ranges) in folds.into_iter().enumerate() {
        if cancellation.is_some_and(CancellationToken::is_cancelled) {
            return Ok(None);
        }
        let fold_root = root
            .join("fold-local")
            .join(format!("fold-{:04}", fold_index + 1));
        std::fs::create_dir_all(&fold_root)
            .map_err(|error| AppError::from_io("创建折内实验目录", &error))?;
        let first = ranges.train[0];
        let last = ranges.test[1];
        let fold_frame = BarFrame {
            instrument: frame.instrument.clone(),
            source: frame.source.clone(),
            ts: frame.ts[first..last].to_vec(),
            open_raw: frame.open_raw[first..last].to_vec(),
            high_raw: frame.high_raw[first..last].to_vec(),
            low_raw: frame.low_raw[first..last].to_vec(),
            close_raw: frame.close_raw[first..last].to_vec(),
            volume_raw: frame.volume_raw[first..last].to_vec(),
            exact_ohlcv: frame
                .exact_ohlcv
                .as_ref()
                .map(|values| values[first..last].to_vec()),
        };
        let fold_input = fold_root.join("bars.json");
        write_text(&fold_input.to_string_lossy(), &fold_frame.to_json())?;
        let fold_data_fingerprint = format!(
            "barframe:{}",
            qx_data::fingerprint_bar_frame(&fold_frame)
                .map_err(|error| { AppError::new(AppErrorCategory::InvalidInput, error) })?
        );
        let seed = plan
            .seed
            .wrapping_add((fold_index as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15));
        let mut state = plan
            .continuous_parameter_space
            .is_empty()
            .then(|| SearchState::new(grid.len(), seed));
        let mut continuous_state = if plan.continuous_parameter_space.is_empty() {
            None
        } else {
            Some(ContinuousSearchState::new(
                &plan.continuous_parameter_space,
                seed,
            )?)
        };
        let mut observed_scores = Vec::<(usize, i32)>::new();
        let mut observed_drawdowns = Vec::<(usize, u32)>::new();
        let mut trials: Vec<ExperimentSearchTrial> = Vec::with_capacity(plan.budget);
        let mut candidates = Vec::with_capacity(plan.budget);
        let mut best: Option<BestFoldCandidate> = None;
        let pruning_window = plan
            .early_pruning
            .as_ref()
            .zip(plan.max_training_drawdown_bps)
            .map(|(early, max_drawdown_bps)| {
                Ok(DrawdownPruningWindow {
                    min_bars: usize::try_from(early.min_training_bars).map_err(|_| {
                        AppError::new(
                            AppErrorCategory::InvalidInput,
                            "早停最小训练 Bar 数超过平台上限",
                        )
                    })?,
                    training_bars: ranges.train[1] - first,
                    max_drawdown_bps,
                })
            })
            .transpose()?;
        for trial_index in 0..plan.budget {
            if cancellation.is_some_and(CancellationToken::is_cancelled) {
                return Ok(None);
            }
            let eligible_scores = observed_scores
                .iter()
                .copied()
                .filter(|(candidate_index, _)| {
                    plan.max_training_drawdown_bps.is_none_or(|limit| {
                        observed_drawdowns
                            .iter()
                            .any(|(index, drawdown)| index == candidate_index && *drawdown <= limit)
                    })
                })
                .collect::<Vec<_>>();
            let eligible_drawdowns = observed_drawdowns
                .iter()
                .copied()
                .filter(|(_, drawdown)| {
                    plan.max_training_drawdown_bps
                        .is_none_or(|limit| *drawdown <= limit)
                })
                .collect::<Vec<_>>();
            let objective = if plan.algorithm == ExperimentSearchAlgorithm::ParetoTpe {
                ExperimentSearchObjective::ParetoReturnDrawdown
            } else {
                ExperimentSearchObjective::Return
            };
            let ordinal = trial_index + 1;
            let (candidate_index, proposal, parameters) = if plan
                .continuous_parameter_space
                .is_empty()
            {
                let (candidate_index, proposal) = state
                    .as_mut()
                    .and_then(|state| {
                        state.next_with_objective(
                            plan.algorithm,
                            objective,
                            4,
                            &spec.parameter_space,
                            grid,
                            &eligible_scores,
                            &eligible_drawdowns,
                        )
                    })
                    .ok_or_else(|| {
                        AppError::new(AppErrorCategory::InternalInvariant, "折内搜索候选提前耗尽")
                    })?;
                (
                    candidate_index,
                    proposal.to_string(),
                    grid[candidate_index].clone(),
                )
            } else {
                let eligible_trials = trials
                    .iter()
                    .filter(|trial| {
                        plan.max_training_drawdown_bps.is_none_or(|limit| {
                            trial
                                .training_max_drawdown_bps
                                .is_some_and(|drawdown| drawdown <= limit)
                        })
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                let (parameters, proposal) = continuous_state
                    .as_mut()
                    .and_then(|state| {
                        state.next(
                            plan.algorithm,
                            objective,
                            4,
                            &plan.continuous_parameter_space,
                            &eligible_trials,
                        )
                    })
                    .ok_or_else(|| {
                        AppError::new(
                            AppErrorCategory::InternalInvariant,
                            "折内连续搜索未能提出不同候选",
                        )
                    })?;
                (trial_index, proposal.to_string(), parameters)
            };
            let mut run_spec = spec.base.clone();
            run_spec.bars_path = fold_input.to_string_lossy().into_owned();
            run_spec.run_id = format!(
                "{}-f{:04}-t{:04}",
                spec.experiment_id,
                fold_index + 1,
                ordinal
            );
            run_spec.output_dir = fold_root
                .join(format!("trial-{ordinal:04}"))
                .to_string_lossy()
                .into_owned();
            let checkpoint_path = fold_root.join(format!("trial-{ordinal:04}.checkpoint.json"));
            let cached = load_fold_candidate_checkpoint(
                &checkpoint_path,
                spec_fingerprint,
                input_data_fingerprint,
                fold_index + 1,
                ordinal,
                &proposal,
                &parameters,
                &run_spec,
                &fold_data_fingerprint,
                context,
            )?;
            let attempt = if let Some(checkpoint) = &cached {
                Ok(SearchBacktestAttempt::Completed(Some(
                    checkpoint.outcome.clone(),
                )))
            } else {
                match apply_parameters(&mut run_spec.strategy, &parameters) {
                    Ok(()) => {
                        let run = match (identity, factory.as_deref_mut()) {
                            (Some(identity), Some(factory)) => factory(&run_spec)
                                .map_err(|error| {
                                    AppError::new(
                                        AppErrorCategory::InvalidInput,
                                        format!(
                                            "第 {} 折候选 {ordinal} 策略 factory 失败: {error}",
                                            fold_index + 1
                                        ),
                                    )
                                })
                                .and_then(|strategy| {
                                    run_backtest_inner_with_experiment_strategy(
                                        &run_spec,
                                        context,
                                        cancellation,
                                        identity.clone(),
                                        strategy,
                                        pruning_window,
                                    )
                                }),
                            (None, None) => run_backtest_inner_for_experiment(
                                &run_spec,
                                context,
                                cancellation,
                                Vec::new(),
                                pruning_window,
                            ),
                            _ => Err(AppError::new(
                                AppErrorCategory::InternalInvariant,
                                "折内自定义策略身份与 factory 状态不一致",
                            )),
                        };
                        run
                    }
                    Err(error) => Err(error),
                }
            };
            let mut score = None;
            let mut drawdown = None;
            let mut robustness_score = None;
            let candidate = match attempt {
                Ok(SearchBacktestAttempt::Completed(Some(outcome))) => {
                    let payload = read_text(&outcome.artifacts.equity)?;
                    let values = equity_series(&payload).ok_or_else(|| {
                        AppError::new(AppErrorCategory::StorageFailure, "折内权益曲线无法解析")
                    })?;
                    if values.len() != fold_frame.len() || values.iter().any(|value| *value <= 0) {
                        return Err(AppError::new(
                            AppErrorCategory::InternalInvariant,
                            "折内权益曲线与切片 Bar 数不一致或包含非正权益",
                        ));
                    }
                    let training_end = ranges.train[1] - first;
                    let training_score = window_return_bps(&values, 0, training_end);
                    let training_drawdown = window_max_drawdown_bps(&values, 0, training_end);
                    let training_robustness_score =
                        plan.robustness_subwindows
                            .map(|count| {
                                window_robustness_score_bps(&values, 0, training_end, count)
                                    .ok_or_else(|| {
                                        AppError::new(
                                            AppErrorCategory::InternalInvariant,
                                            "稳健性子窗无法覆盖折内训练范围",
                                        )
                                    })
                            })
                            .transpose()?;
                    if let Some(checkpoint) = &cached {
                        if checkpoint.training_score_bps != training_score
                            || checkpoint.training_max_drawdown_bps != training_drawdown
                            || checkpoint.training_robustness_score_bps != training_robustness_score
                        {
                            return Err(AppError::new(
                                AppErrorCategory::StorageFailure,
                                format!(
                                    "第 {} 折候选 {ordinal} 检查点训练指标与权益产物不一致",
                                    fold_index + 1
                                ),
                            ));
                        }
                    } else {
                        write_fold_candidate_checkpoint(
                            &checkpoint_path,
                            spec_fingerprint,
                            input_data_fingerprint,
                            fold_index + 1,
                            ordinal,
                            &proposal,
                            &parameters,
                            &outcome,
                            training_score,
                            training_drawdown,
                            training_robustness_score,
                        )?;
                    }
                    score = Some(training_score);
                    drawdown = Some(training_drawdown);
                    robustness_score = training_robustness_score;
                    observed_scores.push((
                        candidate_index,
                        training_robustness_score.unwrap_or(training_score),
                    ));
                    observed_drawdowns.push((candidate_index, training_drawdown));
                    if plan
                        .max_training_drawdown_bps
                        .is_none_or(|limit| training_drawdown <= limit)
                        && plan
                            .minimum_training_robustness_score_bps
                            .is_none_or(|minimum| {
                                training_robustness_score.is_some_and(|score| score >= minimum)
                            })
                        && best.as_ref().is_none_or(
                            |(best_ordinal, best_selection_score, _, _, _, _, _, _)| {
                                training_robustness_score.unwrap_or(training_score)
                                    > *best_selection_score
                                    || (training_robustness_score.unwrap_or(training_score)
                                        == *best_selection_score
                                        && ordinal < *best_ordinal)
                            },
                        )
                    {
                        best = Some((
                            ordinal,
                            training_robustness_score.unwrap_or(training_score),
                            training_score,
                            training_robustness_score,
                            training_drawdown,
                            values,
                            outcome.artifacts.run_manifest.clone(),
                            parameters.clone(),
                        ));
                    }
                    ExperimentCandidateResult {
                        ordinal,
                        strategy_kind: None,
                        parameters: parameters.clone(),
                        outcome: Some(outcome),
                        error: None,
                        pruning: None,
                    }
                }
                Ok(SearchBacktestAttempt::Completed(None)) => return Ok(None),
                Ok(SearchBacktestAttempt::Pruned(pruning)) => ExperimentCandidateResult {
                    ordinal,
                    strategy_kind: None,
                    parameters: parameters.clone(),
                    outcome: None,
                    error: None,
                    pruning: Some(pruning),
                },
                Err(error) => ExperimentCandidateResult {
                    ordinal,
                    strategy_kind: None,
                    parameters: parameters.clone(),
                    outcome: None,
                    error: Some(error.to_json()),
                    pruning: None,
                },
            };
            trials.push(ExperimentSearchTrial {
                ordinal,
                proposal: proposal.to_owned(),
                parameters,
                training_score_bps: score,
                training_max_drawdown_bps: drawdown,
                training_robustness_score_bps: robustness_score,
            });
            candidates.push(candidate);
        }
        let Some((
            selected_ordinal,
            _selection_score,
            in_sample,
            selected_robustness_score,
            selected_drawdown,
            values,
            selected_manifest,
            selected_parameters,
        )) = best
        else {
            has_unselected_fold = true;
            let neighborhood = if plan.continuous_parameter_space.is_empty() {
                neighborhood_report_with_radius_and_budget(
                    &spec.parameter_space,
                    grid,
                    &trials,
                    plan.neighborhood_radius.unwrap_or(1),
                    plan.max_training_drawdown_bps,
                )
            } else {
                continuous_neighborhood_report_with_radius_and_budget(
                    &plan.continuous_parameter_space,
                    &trials,
                    plan.neighborhood_radius.unwrap_or(1),
                    plan.max_training_drawdown_bps,
                )
            };
            searches.push(ExperimentFoldSearchReport {
                fold: fold_index + 1,
                seed,
                train_range: [ranges.train[0] as u64, ranges.train[1] as u64],
                test_range: [ranges.test[0] as u64, ranges.test[1] as u64],
                trials,
                candidates,
                selected_ordinal: None,
                selected_run_manifest: None,
                neighborhood,
                cost_sensitivity: Vec::new(),
            });
            continue;
        };
        let baseline_return =
            window_return_bps(&values, ranges.test[0] - first, ranges.test[1] - first);
        let baseline_drawdown =
            window_max_drawdown_bps(&values, ranges.test[0] - first, ranges.test[1] - first);
        let mut cost_sensitivity = Vec::with_capacity(spec.cost_sensitivity.len());
        for (scenario_index, scenario) in spec.cost_sensitivity.iter().enumerate() {
            if cancellation.is_some_and(CancellationToken::is_cancelled) {
                return Ok(None);
            }
            let neighborhood_cost_robustness = if let Some(threshold_bps) =
                scenario.max_neighborhood_training_return_degradation_bps
            {
                let radius = plan.neighborhood_radius.unwrap_or(1);
                let neighbor_parameters = neighborhood_parameter_sets(
                    &spec.parameter_space,
                    grid,
                    &plan.continuous_parameter_space,
                    &selected_parameters,
                    radius,
                );
                let mut neighbor_results = Vec::new();
                for parameters in &neighbor_parameters {
                    let Some(trial) = trials.iter().find(|trial| trial.parameters == *parameters)
                    else {
                        continue;
                    };
                    let neighbor_candidate = candidates
                        .iter()
                        .find(|candidate| candidate.ordinal == trial.ordinal);
                    let Some(baseline) =
                        neighbor_candidate.and_then(|candidate| candidate.outcome.as_ref())
                    else {
                        neighbor_results.push(ExecutionCostSensitivityNeighborResult {
                            ordinal: trial.ordinal,
                            parameters: trial.parameters.clone(),
                            fold_deltas: Vec::new(),
                            cost_robustness_passed: false,
                            outcome: None,
                            error: neighbor_candidate.and_then(|candidate| candidate.error.clone()),
                        });
                        continue;
                    };
                    let baseline_values = equity_series(&read_text(&baseline.artifacts.equity)?)
                        .ok_or_else(|| {
                            AppError::new(
                                AppErrorCategory::StorageFailure,
                                format!("第 {} 折邻域基线权益曲线无法解析", fold_index + 1),
                            )
                        })?;
                    if baseline_values.len() != fold_frame.len()
                        || baseline_values.iter().any(|value| *value <= 0)
                    {
                        return Err(AppError::new(
                            AppErrorCategory::InternalInvariant,
                            format!(
                                "第 {} 折邻域基线权益点数与切片 Bar 数不一致或包含非正权益",
                                fold_index + 1
                            ),
                        ));
                    }
                    let mut neighbor = spec.base.clone();
                    neighbor.bars_path = fold_input.to_string_lossy().into_owned();
                    neighbor.strategy.kind = spec.base.strategy.kind;
                    apply_parameters(&mut neighbor.strategy, parameters)?;
                    neighbor.run_id = format!(
                        "{}-f{:04}-cost-{:02}-neighbor-{:04}",
                        spec.experiment_id,
                        fold_index + 1,
                        scenario_index + 1,
                        trial.ordinal
                    );
                    neighbor.output_dir = fold_root
                        .join("cost-sensitivity")
                        .join(format!("scenario-{:02}", scenario_index + 1))
                        .join("neighbors")
                        .join(format!("candidate-{:04}", trial.ordinal))
                        .to_string_lossy()
                        .into_owned();
                    neighbor.fee_schedule = None;
                    neighbor.execution_cost_rules = scenario.execution_cost_rules.clone();
                    neighbor.ashare_rules = scenario.ashare_rules.clone();
                    neighbor.fill_model = Some(scenario.fill_model);
                    let checkpoint_path = fold_root
                        .join("cost-sensitivity")
                        .join(format!("scenario-{:02}", scenario_index + 1))
                        .join("neighbors")
                        .join(format!("candidate-{:04}.checkpoint.json", trial.ordinal));
                    let cached = load_cost_scenario_checkpoint(
                        &checkpoint_path,
                        spec_fingerprint,
                        input_data_fingerprint,
                        fold_index + 1,
                        trial.ordinal,
                        scenario_index + 1,
                        &scenario.name,
                        parameters,
                        &neighbor,
                        context,
                    )?;
                    let reused = cached.is_some();
                    let run = if let Some(cached) = cached {
                        Ok(Some(cached))
                    } else {
                        match (identity, factory.as_deref_mut()) {
                            (Some(identity), Some(factory)) => factory(&neighbor)
                                .map_err(|error| {
                                    AppError::new(
                                        AppErrorCategory::InvalidInput,
                                        format!(
                                            "第 {} 折邻域成本策略 factory 失败: {error}",
                                            fold_index + 1
                                        ),
                                    )
                                })
                                .and_then(|strategy| {
                                    run_backtest_inner_with_experiment_strategy(
                                        &neighbor,
                                        context,
                                        cancellation,
                                        identity.clone(),
                                        strategy,
                                        None,
                                    )
                                    .and_then(|attempt| {
                                        match attempt {
                                            SearchBacktestAttempt::Completed(outcome) => {
                                                Ok(outcome)
                                            }
                                            SearchBacktestAttempt::Pruned(_) => Err(AppError::new(
                                                AppErrorCategory::InternalInvariant,
                                                "未启用早停的折内邻域成本情景被意外剪枝",
                                            )),
                                        }
                                    })
                                }),
                            (None, None) => run_backtest_inner(&neighbor, context, cancellation),
                            _ => Err(AppError::new(
                                AppErrorCategory::InternalInvariant,
                                "折内成本情景自定义策略身份与 factory 状态不一致",
                            )),
                        }
                    };
                    let stressed = match run {
                        Ok(Some(stressed)) => {
                            if !reused {
                                write_cost_scenario_checkpoint(
                                    &checkpoint_path,
                                    spec_fingerprint,
                                    input_data_fingerprint,
                                    fold_index + 1,
                                    trial.ordinal,
                                    scenario_index + 1,
                                    &scenario.name,
                                    parameters,
                                    &stressed,
                                )?;
                            }
                            (Some(stressed), None)
                        }
                        Ok(None) => return Ok(None),
                        Err(error) => (None, Some(error.to_json())),
                    };
                    let fold_deltas = if let Some(stressed) = stressed.0.as_ref() {
                        let stressed_values = equity_series(&read_text(
                            &stressed.artifacts.equity,
                        )?)
                        .ok_or_else(|| {
                            AppError::new(
                                AppErrorCategory::StorageFailure,
                                format!("第 {} 折邻域压力权益曲线无法解析", fold_index + 1),
                            )
                        })?;
                        if stressed_values.len() != fold_frame.len()
                            || stressed_values.iter().any(|value| *value <= 0)
                        {
                            return Err(AppError::new(
                                AppErrorCategory::InternalInvariant,
                                format!(
                                    "第 {} 折邻域压力权益点数与切片 Bar 数不一致或包含非正权益",
                                    fold_index + 1
                                ),
                            ));
                        }
                        vec![cost_sensitivity_fold_delta(
                            fold_index + 1,
                            ranges.train,
                            ranges.test,
                            first,
                            &baseline_values,
                            &stressed_values,
                        )]
                    } else {
                        Vec::new()
                    };
                    let passed = cost_robustness_passed(
                        Some(threshold_bps),
                        &fold_deltas,
                        stressed.0.is_some(),
                    ) == Some(true);
                    neighbor_results.push(ExecutionCostSensitivityNeighborResult {
                        ordinal: trial.ordinal,
                        parameters: trial.parameters.clone(),
                        fold_deltas,
                        cost_robustness_passed: passed,
                        outcome: stressed.0,
                        error: stressed.1,
                    });
                }
                let evaluated_neighbors = neighbor_results.len();
                let successful_neighbors = neighbor_results
                    .iter()
                    .filter(|candidate| candidate.outcome.is_some())
                    .count();
                let passed_neighbors = neighbor_results
                    .iter()
                    .filter(|candidate| candidate.cost_robustness_passed)
                    .count();
                let complete = evaluated_neighbors == neighbor_parameters.len();
                let passed = !neighbor_parameters.is_empty()
                    && complete
                    && successful_neighbors == neighbor_parameters.len()
                    && passed_neighbors == neighbor_parameters.len();
                Some(ExecutionCostSensitivityNeighborhoodResult {
                    radius,
                    possible_neighbors: neighbor_parameters.len(),
                    evaluated_neighbors,
                    successful_neighbors,
                    passed_neighbors,
                    complete,
                    passed,
                    candidates: neighbor_results,
                })
            } else {
                None
            };
            let mut stressed = spec.base.clone();
            stressed.bars_path = fold_input.to_string_lossy().into_owned();
            stressed.strategy.kind = spec.base.strategy.kind;
            apply_parameters(&mut stressed.strategy, &selected_parameters)?;
            stressed.run_id = format!(
                "{}-f{:04}-cost-{:02}",
                spec.experiment_id,
                fold_index + 1,
                scenario_index + 1
            );
            stressed.output_dir = fold_root
                .join("cost-sensitivity")
                .join(format!("scenario-{:02}", scenario_index + 1))
                .to_string_lossy()
                .into_owned();
            stressed.fee_schedule = None;
            stressed.execution_cost_rules = scenario.execution_cost_rules.clone();
            stressed.ashare_rules = scenario.ashare_rules.clone();
            stressed.fill_model = Some(scenario.fill_model);
            let checkpoint_path = fold_root.join("cost-sensitivity").join(format!(
                "scenario-{:02}.checkpoint.json",
                scenario_index + 1
            ));
            let cached_outcome = load_cost_scenario_checkpoint(
                &checkpoint_path,
                spec_fingerprint,
                input_data_fingerprint,
                fold_index + 1,
                selected_ordinal,
                scenario_index + 1,
                &scenario.name,
                &selected_parameters,
                &stressed,
                context,
            )?;
            let reused_checkpoint = cached_outcome.is_some();
            let run = if let Some(outcome) = cached_outcome {
                Ok(Some(outcome))
            } else {
                match (identity, factory.as_deref_mut()) {
                    (Some(identity), Some(factory)) => factory(&stressed)
                        .map_err(|error| {
                            AppError::new(
                                AppErrorCategory::InvalidInput,
                                format!(
                                    "第 {} 折成本情景策略 factory 失败: {error}",
                                    fold_index + 1
                                ),
                            )
                        })
                        .and_then(|strategy| {
                            run_backtest_inner_with_experiment_strategy(
                                &stressed,
                                context,
                                cancellation,
                                identity.clone(),
                                strategy,
                                None,
                            )
                            .and_then(|attempt| match attempt {
                                SearchBacktestAttempt::Completed(outcome) => Ok(outcome),
                                SearchBacktestAttempt::Pruned(_) => Err(AppError::new(
                                    AppErrorCategory::InternalInvariant,
                                    "未启用早停的成本情景被意外剪枝",
                                )),
                            })
                        }),
                    (None, None) => run_backtest_inner(&stressed, context, cancellation),
                    _ => Err(AppError::new(
                        AppErrorCategory::InternalInvariant,
                        "折内成本情景策略身份与 factory 状态不一致",
                    )),
                }
            };
            let outcome = match run {
                Ok(Some(outcome)) => Some(outcome),
                Ok(None) => return Ok(None),
                Err(error) => {
                    cost_sensitivity.push(ExecutionCostSensitivityResult {
                        selected_ordinal,
                        name: scenario.name.clone(),
                        execution_cost_rules: scenario.execution_cost_rules.clone(),
                        ashare_rules: scenario.ashare_rules.clone(),
                        fill_model: scenario.fill_model,
                        return_delta_bps: None,
                        drawdown_delta_bps: None,
                        max_training_return_degradation_bps: scenario
                            .max_training_return_degradation_bps,
                        cost_robustness_passed: scenario
                            .max_training_return_degradation_bps
                            .map(|_| false),
                        max_neighborhood_training_return_degradation_bps: scenario
                            .max_neighborhood_training_return_degradation_bps,
                        neighborhood_cost_robustness: neighborhood_cost_robustness.clone(),
                        fold_deltas: Vec::new(),
                        outcome: None,
                        error: Some(error.to_json()),
                    });
                    continue;
                }
            };
            let outcome = outcome.ok_or_else(|| {
                AppError::new(
                    AppErrorCategory::InternalInvariant,
                    "成功的成本情景没有回测结果",
                )
            })?;
            let payload = read_text(&outcome.artifacts.equity)?;
            let stressed_values = equity_series(&payload).ok_or_else(|| {
                AppError::new(
                    AppErrorCategory::StorageFailure,
                    format!("第 {} 折成本情景权益曲线无法解析", fold_index + 1),
                )
            })?;
            if stressed_values.len() != fold_frame.len()
                || stressed_values.iter().any(|value| *value <= 0)
            {
                return Err(AppError::new(
                    AppErrorCategory::InternalInvariant,
                    format!(
                        "第 {} 折成本情景权益曲线与切片 Bar 数不一致或包含非正权益",
                        fold_index + 1
                    ),
                ));
            }
            let stressed_return = window_return_bps(
                &stressed_values,
                ranges.test[0] - first,
                ranges.test[1] - first,
            );
            let stressed_drawdown = window_max_drawdown_bps(
                &stressed_values,
                ranges.test[0] - first,
                ranges.test[1] - first,
            );
            if !reused_checkpoint {
                write_cost_scenario_checkpoint(
                    &checkpoint_path,
                    spec_fingerprint,
                    input_data_fingerprint,
                    fold_index + 1,
                    selected_ordinal,
                    scenario_index + 1,
                    &scenario.name,
                    &selected_parameters,
                    &outcome,
                )?;
            }
            let fold_deltas = vec![cost_sensitivity_fold_delta(
                fold_index + 1,
                ranges.train,
                ranges.test,
                first,
                &values,
                &stressed_values,
            )];
            let cost_robustness_passed = cost_robustness_passed(
                scenario.max_training_return_degradation_bps,
                &fold_deltas,
                true,
            );
            cost_sensitivity.push(ExecutionCostSensitivityResult {
                selected_ordinal,
                name: scenario.name.clone(),
                execution_cost_rules: scenario.execution_cost_rules.clone(),
                ashare_rules: scenario.ashare_rules.clone(),
                fill_model: scenario.fill_model,
                return_delta_bps: Some(i64::from(stressed_return) - i64::from(baseline_return)),
                drawdown_delta_bps: Some(
                    i64::from(stressed_drawdown) - i64::from(baseline_drawdown),
                ),
                max_training_return_degradation_bps: scenario.max_training_return_degradation_bps,
                cost_robustness_passed,
                max_neighborhood_training_return_degradation_bps: scenario
                    .max_neighborhood_training_return_degradation_bps,
                neighborhood_cost_robustness,
                fold_deltas,
                outcome: Some(outcome),
                error: None,
            });
        }
        reports.push(ExperimentFoldReport {
            fold: fold_index + 1,
            train_range: [ranges.train[0] as u64, ranges.train[1] as u64],
            test_range: [ranges.test[0] as u64, ranges.test[1] as u64],
            purge_range: (ranges.purge[0] < ranges.purge[1])
                .then_some([ranges.purge[0] as u64, ranges.purge[1] as u64]),
            embargo_range: (ranges.embargo[0] < ranges.embargo[1])
                .then_some([ranges.embargo[0] as u64, ranges.embargo[1] as u64]),
            selected_ordinal,
            selected_strategy_kind: None,
            selected_parameters,
            in_sample_return_bps: in_sample,
            selected_training_robustness_score_bps: selected_robustness_score,
            selected_training_max_drawdown_bps: Some(selected_drawdown),
            max_training_drawdown_bps: plan.max_training_drawdown_bps,
            out_of_sample_return_bps: window_return_bps(
                &values,
                ranges.test[0] - first,
                ranges.test[1] - first,
            ),
        });
        let neighborhood = if plan.continuous_parameter_space.is_empty() {
            neighborhood_report_with_radius_and_budget(
                &spec.parameter_space,
                grid,
                &trials,
                plan.neighborhood_radius.unwrap_or(1),
                plan.max_training_drawdown_bps,
            )
        } else {
            continuous_neighborhood_report_with_radius_and_budget(
                &plan.continuous_parameter_space,
                &trials,
                plan.neighborhood_radius.unwrap_or(1),
                plan.max_training_drawdown_bps,
            )
        };
        searches.push(ExperimentFoldSearchReport {
            fold: fold_index + 1,
            seed,
            train_range: [ranges.train[0] as u64, ranges.train[1] as u64],
            test_range: [ranges.test[0] as u64, ranges.test[1] as u64],
            trials,
            candidates,
            selected_ordinal: Some(selected_ordinal),
            selected_run_manifest: Some(selected_manifest),
            neighborhood,
            cost_sensitivity,
        });
    }
    let out_of_sample = (!has_unselected_fold).then(|| summarize_folds(reports));
    Ok(Some((out_of_sample, searches)))
}
