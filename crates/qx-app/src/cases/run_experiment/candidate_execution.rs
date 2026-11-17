//! Candidate proposal, resumable execution, and adaptive search feedback.

use super::recovery::{load_search_candidate_checkpoint, write_search_candidate_checkpoint};
use super::*;

pub(super) struct CandidateExecutionRequest<'a, 'factory> {
    pub(super) spec: &'a RunExperimentSpec,
    pub(super) total: usize,
    pub(super) root: &'a Path,
    pub(super) context: &'a RunContext,
    pub(super) cancellation: Option<&'a CancellationToken>,
    pub(super) identity: Option<&'a StrategyArtifactIdentity>,
    pub(super) factory: Option<&'a mut StrategyFactory<'factory>>,
    pub(super) spec_fingerprint: &'a str,
    pub(super) input_data_fingerprint: &'a str,
    pub(super) evaluation_bars: Option<&'a [qx_guanxing::Bar]>,
    pub(super) pruning_window: Option<DrawdownPruningWindow>,
}

pub(super) struct CandidateExecutionResult {
    pub(super) combinations: Vec<BTreeMap<String, i128>>,
    pub(super) candidates: Vec<ExperimentCandidateResult>,
    pub(super) search_trace: Vec<ExperimentSearchTrial>,
}

pub(super) fn execute_candidates<'a, 'factory>(
    request: CandidateExecutionRequest<'a, 'factory>,
) -> Result<Option<CandidateExecutionResult>, AppError> {
    let CandidateExecutionRequest {
        spec,
        total,
        root,
        context,
        cancellation,
        identity,
        mut factory,
        spec_fingerprint,
        input_data_fingerprint,
        evaluation_bars,
        pruning_window,
    } = request;
    let mut kinds = if spec.strategy_kinds.is_empty() {
        vec![spec.base.strategy.kind]
    } else {
        spec.strategy_kinds.clone()
    };
    kinds.sort_by_key(|kind| kind.name());
    let combinations = if spec.parameter_space.is_empty() {
        vec![BTreeMap::new()]
    } else {
        expand_grid(&spec.parameter_space)
    };
    let candidate_pairs = kinds
        .into_iter()
        .flat_map(|kind| {
            combinations
                .iter()
                .cloned()
                .map(move |parameters| (kind, parameters))
        })
        .collect::<Vec<_>>();
    if spec.search.is_none() {
        debug_assert_eq!(candidate_pairs.len(), total);
    }

    let mut candidates = Vec::with_capacity(total);
    let mut search_trace: Vec<ExperimentSearchTrial> =
        Vec::with_capacity(spec.search.as_ref().map_or(0, |plan| plan.budget));
    let mut observed_training_scores = Vec::new();
    let mut observed_training_drawdowns = Vec::new();
    let mut search_state = spec.search.as_ref().and_then(|plan| {
        plan.continuous_parameter_space
            .is_empty()
            .then(|| SearchState::new(combinations.len(), plan.seed))
    });
    let mut continuous_search_state = match spec.search.as_ref() {
        Some(plan) if !plan.continuous_parameter_space.is_empty() => Some(
            ContinuousSearchState::new(&plan.continuous_parameter_space, plan.seed)?,
        ),
        _ => None,
    };
    for index in 0..total {
        if cancellation.is_some_and(CancellationToken::is_cancelled) {
            return Ok(None);
        }
        let ordinal = index + 1;
        let (kind, parameters, proposal, candidate_index) = if let Some(plan) = &spec.search {
            if !plan.continuous_parameter_space.is_empty() {
                let eligible_trials = search_trace
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
                let (parameters, proposal) = continuous_search_state
                    .as_mut()
                    .and_then(|state| {
                        state.next(
                            plan.algorithm,
                            if plan.algorithm == ExperimentSearchAlgorithm::ParetoTpe {
                                ExperimentSearchObjective::ParetoReturnDrawdown
                            } else {
                                ExperimentSearchObjective::Return
                            },
                            4,
                            &plan.continuous_parameter_space,
                            &eligible_trials,
                        )
                    })
                    .ok_or_else(|| {
                        AppError::new(
                            AppErrorCategory::InternalInvariant,
                            "连续搜索未能提出不同候选；值域计数与 budget 不一致",
                        )
                    })?;
                (
                    spec.base.strategy.kind,
                    parameters,
                    Some(proposal.to_string()),
                    Some(ordinal - 1),
                )
            } else {
                let (candidate_index, proposal) = search_state
                    .as_mut()
                    .and_then(|state| {
                        state.next_with_objective(
                            plan.algorithm,
                            if plan.algorithm == ExperimentSearchAlgorithm::ParetoTpe {
                                ExperimentSearchObjective::ParetoReturnDrawdown
                            } else {
                                ExperimentSearchObjective::Return
                            },
                            4,
                            &spec.parameter_space,
                            &combinations,
                            &if let Some(limit) = plan.max_training_drawdown_bps {
                                observed_training_scores
                                    .iter()
                                    .copied()
                                    .filter(|(index, _)| {
                                        observed_training_drawdowns.iter().any(
                                            |(drawdown_index, dd)| {
                                                drawdown_index == index && *dd <= limit
                                            },
                                        )
                                    })
                                    .collect::<Vec<_>>()
                            } else {
                                observed_training_scores.clone()
                            },
                            &if let Some(limit) = plan.max_training_drawdown_bps {
                                observed_training_drawdowns
                                    .iter()
                                    .copied()
                                    .filter(|(_, dd)| *dd <= limit)
                                    .collect::<Vec<_>>()
                            } else {
                                observed_training_drawdowns.clone()
                            },
                        )
                    })
                    .ok_or_else(|| {
                        AppError::new(
                            AppErrorCategory::InternalInvariant,
                            "搜索候选提前耗尽，budget 与有限候选空间不一致",
                        )
                    })?;
                (
                    spec.base.strategy.kind,
                    combinations[candidate_index].clone(),
                    Some(proposal.to_string()),
                    Some(candidate_index),
                )
            }
        } else {
            let (kind, parameters) = candidate_pairs.get(index).cloned().ok_or_else(|| {
                AppError::new(
                    AppErrorCategory::InternalInvariant,
                    "实验候选数量与组合数量不一致",
                )
            })?;
            (kind, parameters, None, None)
        };
        let mut candidate = spec.base.clone();
        candidate.strategy.kind = kind;
        candidate.run_id = format!("{}-{ordinal:04}", spec.experiment_id);
        candidate.output_dir = root
            .join(format!("run-{ordinal:04}"))
            .to_string_lossy()
            .into();
        let checkpoint_path = proposal
            .as_ref()
            .map(|_| root.join(format!("search-trial-{ordinal:04}.checkpoint.json")));
        let mut cached_checkpoint = None;
        let attempt = match apply_parameters(&mut candidate.strategy, &parameters) {
            Ok(()) => {
                if let (Some(proposal), Some(candidate_index), Some(checkpoint_path)) = (
                    proposal.as_deref(),
                    candidate_index,
                    checkpoint_path.as_deref(),
                ) {
                    cached_checkpoint = load_search_candidate_checkpoint(
                        checkpoint_path,
                        spec_fingerprint,
                        input_data_fingerprint,
                        ordinal,
                        candidate_index,
                        proposal,
                        kind.name(),
                        &parameters,
                        &candidate,
                        context,
                    )?;
                }
                if let Some(checkpoint) = &cached_checkpoint {
                    Ok(SearchBacktestAttempt::Completed(Some(
                        checkpoint.outcome.clone(),
                    )))
                } else {
                    let run = match (identity, factory.as_deref_mut()) {
                        (Some(identity), Some(factory)) => factory(&candidate)
                            .map_err(|error| {
                                AppError::new(
                                    AppErrorCategory::InvalidInput,
                                    format!("候选 {ordinal} 自定义策略 factory 失败: {error}"),
                                )
                            })
                            .and_then(|strategy| {
                                run_backtest_inner_with_experiment_strategy(
                                    &candidate,
                                    context,
                                    cancellation,
                                    identity.clone(),
                                    strategy,
                                    pruning_window,
                                )
                            }),
                        (None, None) => run_backtest_inner_for_experiment(
                            &candidate,
                            context,
                            cancellation,
                            Vec::new(),
                            pruning_window,
                        ),
                        _ => Err(AppError::new(
                            AppErrorCategory::InternalInvariant,
                            "自定义策略实验身份与 factory 状态不一致",
                        )),
                    };
                    run
                }
            }
            Err(error) => Err(error),
        };
        let (candidate_result, training_metrics) = match attempt {
            Ok(SearchBacktestAttempt::Completed(Some(outcome))) => {
                let training_metrics = if spec.search.is_some() {
                    let bars = evaluation_bars.ok_or_else(|| {
                        AppError::new(
                            AppErrorCategory::InternalInvariant,
                            "adaptive search lost its validated BarFrame",
                        )
                    })?;
                    let payload = read_text(&outcome.artifacts.equity)?;
                    let values = equity_series(&payload).ok_or_else(|| {
                        AppError::new(
                            AppErrorCategory::StorageFailure,
                            format!("搜索候选权益曲线产物无法解析: {}", outcome.artifacts.equity),
                        )
                    })?;
                    let split = spec.split_plan.as_ref().ok_or_else(|| {
                        AppError::new(
                            AppErrorCategory::InternalInvariant,
                            "adaptive search lost its validated split plan",
                        )
                    })?;
                    let (score, drawdown) =
                        training_metrics_for_equity(split, bars.len(), &values)?;
                    let robustness_score = spec
                        .search
                        .as_ref()
                        .and_then(|plan| plan.robustness_subwindows)
                        .map(|count| {
                            let ranges = experiment_folds(split, bars.len())
                                .and_then(|folds| {
                                    folds.first().copied().ok_or_else(|| {
                                        AppError::new(
                                            AppErrorCategory::InternalInvariant,
                                            "adaptive search has no training fold",
                                        )
                                    })
                                })?;
                            window_robustness_score_bps(
                                &values,
                                ranges.train[0],
                                ranges.train[1],
                                count,
                            )
                            .ok_or_else(|| {
                                AppError::new(
                                    AppErrorCategory::InternalInvariant,
                                    "robustness subwindows do not fit the validated training window",
                                )
                            })
                        })
                        .transpose()?;
                    Some((score, drawdown, robustness_score))
                } else {
                    None
                };
                if let Some((
                    training_score_bps,
                    training_max_drawdown_bps,
                    training_robustness_score_bps,
                )) = training_metrics
                {
                    if let Some(checkpoint) = &cached_checkpoint {
                        if checkpoint.training_score_bps != training_score_bps
                            || checkpoint.training_max_drawdown_bps != training_max_drawdown_bps
                            || checkpoint.training_robustness_score_bps
                                != training_robustness_score_bps
                        {
                            return Err(AppError::new(
                                AppErrorCategory::StorageFailure,
                                format!("候选 {ordinal} 检查点训练指标与权益产物不一致"),
                            ));
                        }
                    } else if let (Some(proposal), Some(candidate_index), Some(checkpoint_path)) = (
                        proposal.as_ref(),
                        candidate_index,
                        checkpoint_path.as_deref(),
                    ) {
                        write_search_candidate_checkpoint(
                            checkpoint_path,
                            spec_fingerprint,
                            input_data_fingerprint,
                            ordinal,
                            candidate_index,
                            proposal,
                            kind.name(),
                            &parameters,
                            &outcome,
                            training_score_bps,
                            training_max_drawdown_bps,
                            training_robustness_score_bps,
                        )?;
                    }
                }
                (
                    ExperimentCandidateResult {
                        ordinal,
                        strategy_kind: (!spec.strategy_kinds.is_empty())
                            .then_some(kind.name().to_string()),
                        parameters: parameters.clone(),
                        outcome: Some(outcome),
                        error: None,
                        pruning: None,
                    },
                    training_metrics,
                )
            }
            Ok(SearchBacktestAttempt::Completed(None)) => return Ok(None),
            Ok(SearchBacktestAttempt::Pruned(pruning)) => (
                ExperimentCandidateResult {
                    ordinal,
                    strategy_kind: (!spec.strategy_kinds.is_empty())
                        .then_some(kind.name().to_string()),
                    parameters: parameters.clone(),
                    outcome: None,
                    error: None,
                    pruning: Some(pruning),
                },
                None,
            ),
            Err(error) => (
                ExperimentCandidateResult {
                    ordinal,
                    strategy_kind: (!spec.strategy_kinds.is_empty())
                        .then_some(kind.name().to_string()),
                    parameters: parameters.clone(),
                    outcome: None,
                    error: Some(error.to_json()),
                    pruning: None,
                },
                None,
            ),
        };
        let training_score_bps = training_metrics.map(|metrics| metrics.0);
        let training_max_drawdown_bps = training_metrics.map(|metrics| metrics.1);
        let training_robustness_score_bps = training_metrics.and_then(|metrics| metrics.2);
        if let (Some(proposal), Some(candidate_index)) = (proposal, candidate_index) {
            search_trace.push(ExperimentSearchTrial {
                ordinal,
                proposal,
                parameters: parameters.clone(),
                training_score_bps,
                training_max_drawdown_bps,
                training_robustness_score_bps,
            });
            if let Some(score) = training_robustness_score_bps.or(training_score_bps) {
                observed_training_scores.push((candidate_index, score));
            }
            if let Some(drawdown) = training_max_drawdown_bps {
                observed_training_drawdowns.push((candidate_index, drawdown));
            }
        }
        candidates.push(candidate_result);
    }
    Ok(Some(CandidateExecutionResult {
        combinations,
        candidates,
        search_trace,
    }))
}
