//! Execute declared cost scenarios and evaluate their training/OOS robustness.

use super::*;

pub(in crate::cases) fn cost_sensitivity_fold_delta(
    fold: usize,
    train_range: [usize; 2],
    test_range: [usize; 2],
    series_offset: usize,
    baseline: &[i128],
    stressed: &[i128],
) -> ExecutionCostSensitivityFoldDelta {
    let relative = |range: [usize; 2]| {
        debug_assert!(range[0] >= series_offset && range[1] >= series_offset);
        [range[0] - series_offset, range[1] - series_offset]
    };
    let return_delta = |range: [usize; 2]| {
        let range = relative(range);
        i64::from(window_return_bps(stressed, range[0], range[1]))
            - i64::from(window_return_bps(baseline, range[0], range[1]))
    };
    let drawdown_delta = |range: [usize; 2]| {
        let range = relative(range);
        i64::from(window_max_drawdown_bps(stressed, range[0], range[1]))
            - i64::from(window_max_drawdown_bps(baseline, range[0], range[1]))
    };
    ExecutionCostSensitivityFoldDelta {
        fold,
        train_range: [train_range[0] as u64, train_range[1] as u64],
        test_range: [test_range[0] as u64, test_range[1] as u64],
        training_return_delta_bps: return_delta(train_range),
        training_drawdown_delta_bps: drawdown_delta(train_range),
        out_of_sample_return_delta_bps: return_delta(test_range),
        out_of_sample_drawdown_delta_bps: drawdown_delta(test_range),
    }
}

pub(in crate::cases) fn cost_robustness_passed(
    threshold_bps: Option<u32>,
    fold_deltas: &[ExecutionCostSensitivityFoldDelta],
    stress_completed: bool,
) -> Option<bool> {
    threshold_bps.map(|threshold| {
        stress_completed
            && !fold_deltas.is_empty()
            && fold_deltas
                .iter()
                .all(|fold| fold.training_return_delta_bps >= -i64::from(threshold))
    })
}

pub(super) struct CostSensitivityRequest<'a, 'factory> {
    pub(super) spec: &'a RunExperimentSpec,
    pub(super) context: &'a RunContext,
    pub(super) cancellation: Option<&'a CancellationToken>,
    pub(super) identity: Option<&'a StrategyArtifactIdentity>,
    pub(super) factory: Option<&'a mut StrategyFactory<'factory>>,
    pub(super) candidates: &'a [ExperimentCandidateResult],
    pub(super) search_trace: &'a [ExperimentSearchTrial],
    pub(super) combinations: &'a [BTreeMap<String, i128>],
    pub(super) root: &'a Path,
    pub(super) spec_fingerprint: &'a str,
    pub(super) input_data_fingerprint: &'a str,
    pub(super) evaluation_bar_count: Option<usize>,
}

pub(super) fn execute_cost_sensitivity<'a, 'factory>(
    request: CostSensitivityRequest<'a, 'factory>,
) -> Result<Option<Vec<ExecutionCostSensitivityResult>>, AppError> {
    let CostSensitivityRequest {
        spec,
        context,
        cancellation,
        identity,
        mut factory,
        candidates,
        search_trace,
        combinations,
        root,
        spec_fingerprint,
        input_data_fingerprint,
        evaluation_bar_count,
    } = request;
    let mut cost_sensitivity = Vec::with_capacity(spec.cost_sensitivity.len());
    if !spec.cost_sensitivity.is_empty() {
        let selected = search_trace
            .iter()
            .filter(|trial| {
                trial.selection_score_bps().is_some()
                    && spec
                        .search
                        .as_ref()
                        .and_then(|search| search.minimum_training_robustness_score_bps)
                        .is_none_or(|minimum| {
                            trial
                                .training_robustness_score_bps
                                .is_some_and(|score| score >= minimum)
                        })
                    && spec
                        .search
                        .as_ref()
                        .and_then(|search| search.max_training_drawdown_bps)
                        .is_none_or(|limit| {
                            trial
                                .training_max_drawdown_bps
                                .is_some_and(|dd| dd <= limit)
                        })
            })
            .max_by(|left, right| {
                left.selection_score_bps()
                    .cmp(&right.selection_score_bps())
                    .then_with(|| {
                        right
                            .training_max_drawdown_bps
                            .cmp(&left.training_max_drawdown_bps)
                    })
                    .then_with(|| right.ordinal.cmp(&left.ordinal))
            })
            .ok_or_else(|| {
                AppError::new(
                    AppErrorCategory::InvalidInput,
                    "没有满足训练稳健收益与最大训练回撤门槛的成功候选，无法执行成本敏感性",
                )
            })?;
        let baseline_outcome = candidates
            .iter()
            .find(|candidate| candidate.ordinal == selected.ordinal)
            .and_then(|candidate| candidate.outcome.as_ref())
            .ok_or_else(|| {
                AppError::new(
                    AppErrorCategory::InternalInvariant,
                    "成本敏感性选中候选缺少已完成的基线回测",
                )
            })?;
        let baseline_payload = read_text(&baseline_outcome.artifacts.equity)?;
        let baseline_values = equity_series(&baseline_payload).ok_or_else(|| {
            AppError::new(
                AppErrorCategory::StorageFailure,
                "成本敏感性基线权益曲线无法解析",
            )
        })?;
        let split = spec.split_plan.as_ref().ok_or_else(|| {
            AppError::new(
                AppErrorCategory::InternalInvariant,
                "成本敏感性验证后缺少切分计划",
            )
        })?;
        let total_bars = evaluation_bar_count.ok_or_else(|| {
            AppError::new(
                AppErrorCategory::InternalInvariant,
                "成本敏感性缺少已验证的 Bar 计数",
            )
        })?;
        if baseline_values.len() != total_bars || baseline_values.iter().any(|value| *value <= 0) {
            return Err(AppError::new(
                AppErrorCategory::InternalInvariant,
                "成本敏感性基线权益点数与 Bar 数不一致或包含非正权益",
            ));
        }
        let sensitivity_folds = experiment_folds(split, total_bars)?;
        for (scenario_index, scenario) in spec.cost_sensitivity.iter().enumerate() {
            if cancellation.is_some_and(CancellationToken::is_cancelled) {
                return Ok(None);
            }
            let mut candidate = spec.base.clone();
            candidate.strategy.kind = spec.base.strategy.kind;
            apply_parameters(&mut candidate.strategy, &selected.parameters)?;
            candidate.run_id = format!("{}-cost-{:02}", spec.experiment_id, scenario_index + 1);
            candidate.output_dir = root
                .join("cost-sensitivity")
                .join(format!("scenario-{:02}", scenario_index + 1))
                .to_string_lossy()
                .into_owned();
            candidate.fee_schedule = None;
            candidate.execution_cost_rules = scenario.execution_cost_rules.clone();
            candidate.ashare_rules = scenario.ashare_rules.clone();
            candidate.fill_model = Some(scenario.fill_model);
            let checkpoint_path = root.join("cost-sensitivity").join(format!(
                "scenario-{:02}.checkpoint.json",
                scenario_index + 1
            ));
            let cached_outcome = load_cost_scenario_checkpoint(
                &checkpoint_path,
                spec_fingerprint,
                input_data_fingerprint,
                0,
                selected.ordinal,
                scenario_index + 1,
                &scenario.name,
                &selected.parameters,
                &candidate,
                context,
            )?;
            let reused_checkpoint = cached_outcome.is_some();
            let outcome = if let Some(outcome) = cached_outcome {
                Ok(Some(outcome))
            } else {
                match (identity, factory.as_deref_mut()) {
                    (Some(identity), Some(factory)) => factory(&candidate)
                        .map_err(|error| {
                            AppError::new(
                                AppErrorCategory::InvalidInput,
                                format!("成本情景策略 factory 失败: {error}"),
                            )
                        })
                        .and_then(|strategy| {
                            run_backtest_inner_with_experiment_strategy(
                                &candidate,
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
                    (None, None) => run_backtest_inner(&candidate, context, cancellation),
                    _ => Err(AppError::new(
                        AppErrorCategory::InternalInvariant,
                        "成本情景自定义策略身份与 factory 状态不一致",
                    )),
                }
            };
            let outcome = match outcome {
                Ok(Some(outcome)) => {
                    if !reused_checkpoint {
                        write_cost_scenario_checkpoint(
                            &checkpoint_path,
                            spec_fingerprint,
                            input_data_fingerprint,
                            0,
                            selected.ordinal,
                            scenario_index + 1,
                            &scenario.name,
                            &selected.parameters,
                            &outcome,
                        )?;
                    }
                    (Some(outcome), None)
                }
                Ok(None) => return Ok(None),
                Err(error) => (None, Some(error.to_json())),
            };
            let baseline = candidates
                .iter()
                .find(|candidate| candidate.ordinal == selected.ordinal)
                .and_then(|candidate| candidate.outcome.as_ref());
            let return_delta_bps = baseline
                .zip(outcome.0.as_ref())
                .map(|(baseline, stressed)| {
                    i64::from(stressed.return_bps) - i64::from(baseline.return_bps)
                });
            let drawdown_delta_bps =
                baseline
                    .zip(outcome.0.as_ref())
                    .map(|(baseline, stressed)| {
                        i64::from(stressed.max_drawdown_bps) - i64::from(baseline.max_drawdown_bps)
                    });
            let fold_deltas = if let Some(stressed) = outcome.0.as_ref() {
                let stressed_payload = read_text(&stressed.artifacts.equity)?;
                let stressed_values = equity_series(&stressed_payload).ok_or_else(|| {
                    AppError::new(
                        AppErrorCategory::StorageFailure,
                        "成本敏感性权益曲线无法解析",
                    )
                })?;
                if stressed_values.len() != total_bars
                    || stressed_values.iter().any(|value| *value <= 0)
                {
                    return Err(AppError::new(
                        AppErrorCategory::InternalInvariant,
                        "成本敏感性权益点数与 Bar 数不一致或包含非正权益",
                    ));
                }
                sensitivity_folds
                    .iter()
                    .enumerate()
                    .map(|(index, ranges)| {
                        cost_sensitivity_fold_delta(
                            index + 1,
                            ranges.train,
                            ranges.test,
                            0,
                            &baseline_values,
                            &stressed_values,
                        )
                    })
                    .collect()
            } else {
                Vec::new()
            };
            let selected_cost_robustness_passed = cost_robustness_passed(
                scenario.max_training_return_degradation_bps,
                &fold_deltas,
                outcome.0.is_some(),
            );
            let neighborhood_cost_robustness = if let Some(threshold_bps) =
                scenario.max_neighborhood_training_return_degradation_bps
            {
                let search = spec.search.as_ref().ok_or_else(|| {
                    AppError::new(
                        AppErrorCategory::InternalInvariant,
                        "邻域成本稳健性配置缺少搜索计划",
                    )
                })?;
                let radius = search.neighborhood_radius.unwrap_or(1);
                let neighbor_parameters = neighborhood_parameter_sets(
                    &spec.parameter_space,
                    combinations,
                    &search.continuous_parameter_space,
                    &selected.parameters,
                    radius,
                );
                let mut neighbor_results = Vec::new();
                for parameters in &neighbor_parameters {
                    let Some(trial) = search_trace
                        .iter()
                        .find(|trial| trial.parameters == *parameters)
                    else {
                        continue;
                    };
                    let neighbor_candidate = candidates
                        .iter()
                        .find(|candidate| candidate.ordinal == trial.ordinal);
                    let Some(neighbor_baseline) =
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
                    let neighbor_baseline_values =
                        equity_series(&read_text(&neighbor_baseline.artifacts.equity)?)
                            .ok_or_else(|| {
                                AppError::new(
                                    AppErrorCategory::StorageFailure,
                                    "成本敏感性邻域基线权益曲线无法解析",
                                )
                            })?;
                    if neighbor_baseline_values.len() != total_bars
                        || neighbor_baseline_values.iter().any(|value| *value <= 0)
                    {
                        return Err(AppError::new(
                            AppErrorCategory::InternalInvariant,
                            "成本敏感性邻域基线权益点数与 Bar 数不一致或包含非正权益",
                        ));
                    }
                    let mut neighbor = spec.base.clone();
                    neighbor.strategy.kind = spec.base.strategy.kind;
                    apply_parameters(&mut neighbor.strategy, parameters)?;
                    neighbor.run_id = format!(
                        "{}-cost-{:02}-neighbor-{:04}",
                        spec.experiment_id,
                        scenario_index + 1,
                        trial.ordinal
                    );
                    neighbor.output_dir = root
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
                    let checkpoint_path = root
                        .join("cost-sensitivity")
                        .join(format!("scenario-{:02}", scenario_index + 1))
                        .join("neighbors")
                        .join(format!("candidate-{:04}.checkpoint.json", trial.ordinal));
                    let cached = load_cost_scenario_checkpoint(
                        &checkpoint_path,
                        spec_fingerprint,
                        input_data_fingerprint,
                        0,
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
                                        format!("成本敏感性邻域策略 factory 失败: {error}"),
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
                                                "未启用早停的邻域成本情景被意外剪枝",
                                            )),
                                        }
                                    })
                                }),
                            (None, None) => run_backtest_inner(&neighbor, context, cancellation),
                            _ => Err(AppError::new(
                                AppErrorCategory::InternalInvariant,
                                "成本情景自定义策略身份与 factory 状态不一致",
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
                                    0,
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
                    let neighbor_deltas = if let Some(stressed) = stressed.0.as_ref() {
                        let stressed_values = equity_series(&read_text(
                            &stressed.artifacts.equity,
                        )?)
                        .ok_or_else(|| {
                            AppError::new(
                                AppErrorCategory::StorageFailure,
                                "成本敏感性邻域压力权益曲线无法解析",
                            )
                        })?;
                        if stressed_values.len() != total_bars
                            || stressed_values.iter().any(|value| *value <= 0)
                        {
                            return Err(AppError::new(
                                AppErrorCategory::InternalInvariant,
                                "成本敏感性邻域压力权益点数与 Bar 数不一致或包含非正权益",
                            ));
                        }
                        sensitivity_folds
                            .iter()
                            .enumerate()
                            .map(|(index, ranges)| {
                                cost_sensitivity_fold_delta(
                                    index + 1,
                                    ranges.train,
                                    ranges.test,
                                    0,
                                    &neighbor_baseline_values,
                                    &stressed_values,
                                )
                            })
                            .collect::<Vec<_>>()
                    } else {
                        Vec::new()
                    };
                    let passed = cost_robustness_passed(
                        Some(threshold_bps),
                        &neighbor_deltas,
                        stressed.0.is_some(),
                    ) == Some(true);
                    neighbor_results.push(ExecutionCostSensitivityNeighborResult {
                        ordinal: trial.ordinal,
                        parameters: trial.parameters.clone(),
                        fold_deltas: neighbor_deltas,
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
            cost_sensitivity.push(ExecutionCostSensitivityResult {
                selected_ordinal: selected.ordinal,
                name: scenario.name.clone(),
                execution_cost_rules: scenario.execution_cost_rules.clone(),
                ashare_rules: scenario.ashare_rules.clone(),
                fill_model: scenario.fill_model,
                return_delta_bps,
                drawdown_delta_bps,
                max_training_return_degradation_bps: scenario.max_training_return_degradation_bps,
                cost_robustness_passed: selected_cost_robustness_passed,
                max_neighborhood_training_return_degradation_bps: scenario
                    .max_neighborhood_training_return_degradation_bps,
                neighborhood_cost_robustness,
                fold_deltas,
                outcome: outcome.0,
                error: outcome.1,
            });
        }
    }
    Ok(Some(cost_sensitivity))
}
