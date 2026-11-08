//! Walk-forward training selection and out-of-sample evaluation.

use super::preparation::window_for_range;
use super::*;
use crate::ExperimentOutOfSampleReport;

pub(super) struct FoldExecutionRequest<'a, F> {
    pub(super) spec: &'a RunMultiMarketExperimentSpec,
    pub(super) root: &'a std::path::Path,
    pub(super) timeline: Option<&'a [u64]>,
    pub(super) folds: &'a [FoldRanges],
    pub(super) total: usize,
    pub(super) candidate_plans: &'a [CandidatePlan],
    pub(super) candidates: &'a [MultiMarketExperimentCandidate],
    pub(super) target_kind: BuiltinStrategyKind,
    pub(super) callback_parameter_search: bool,
    pub(super) active_parameter_space: &'a [ExperimentParameterSpace],
    pub(super) combinations: &'a [BTreeMap<String, i128>],
    pub(super) initial_fold_searches: Vec<MultiMarketExperimentFoldSearchReport>,
    pub(super) context: &'a RunContext,
    pub(super) cancellation: Option<&'a CancellationToken>,
    pub(super) make_injected: &'a mut F,
}

pub(super) struct FoldExecutionResult {
    pub(super) ranking: Vec<MultiMarketExperimentRankedCandidate>,
    pub(super) out_of_sample: Option<ExperimentOutOfSampleReport>,
    pub(super) fold_candidate_failures: Vec<MultiMarketExperimentFoldCandidateFailure>,
    pub(super) fold_searches: Vec<MultiMarketExperimentFoldSearchReport>,
}

pub(super) fn execute_folds<F>(
    request: FoldExecutionRequest<'_, F>,
) -> Result<Option<FoldExecutionResult>, AppError>
where
    F: FnMut(
        &MultiMarketBacktestSpec,
        &BTreeMap<String, i128>,
    ) -> Result<BTreeMap<String, InjectedMarketStrategy>, AppError>,
{
    let FoldExecutionRequest {
        spec,
        root,
        timeline,
        folds,
        total,
        candidate_plans,
        candidates,
        target_kind,
        callback_parameter_search,
        active_parameter_space,
        combinations,
        mut initial_fold_searches,
        context,
        cancellation,
        make_injected,
    } = request;
    let ranking = rank_candidates(candidates);
    let mut out_of_sample = None;
    let mut fold_candidate_failures = candidates
        .iter()
        .filter_map(|candidate| {
            candidate
                .error
                .as_ref()
                .map(|error| MultiMarketExperimentFoldCandidateFailure {
                    fold: 1,
                    ordinal: candidate.ordinal,
                    error: error.clone(),
                })
        })
        .collect::<Vec<_>>();
    if let Some(timeline) = timeline {
        if ranking.is_empty() {
            let first_failure = candidates
                .iter()
                .find_map(|candidate| candidate.error.as_deref())
                .unwrap_or("无候选错误详情");
            return Err(AppError::new(
                AppErrorCategory::FidelityInsufficient,
                format!(
                    "多市场实验首个训练窗没有成功候选，无法生成样本外验证；首个候选失败原因：{first_failure}"
                ),
            ));
        }
        let mut fold_reports = Vec::with_capacity(folds.len());
        for (fold_index, fold) in folds.iter().enumerate() {
            if cancellation.is_some_and(CancellationToken::is_cancelled) {
                return Ok(None);
            }
            let mut fold_plans = if fold_index == 0 {
                candidate_plans.to_vec()
            } else {
                Vec::new()
            };
            let mut fold_training = Vec::<(usize, MultiMarketBacktestOutcome)>::new();
            if fold_index == 0 {
                fold_training.extend(candidates.iter().filter_map(|candidate| {
                    candidate.outcome.as_ref().and_then(|outcome| {
                        spec.search
                            .as_ref()
                            .and_then(|plan| plan.max_training_drawdown_bps)
                            .is_none_or(|limit| outcome.max_drawdown_bps <= limit)
                            .then(|| (candidate.ordinal, outcome.clone()))
                    })
                }));
            } else {
                let window = window_for_range(timeline, fold.train)?;
                let fold_local_search = spec
                    .search
                    .as_ref()
                    .is_some_and(|search| search.fold_local_search);
                let mut generator = if fold_local_search {
                    spec.search
                        .clone()
                        .map(|mut search| {
                            let seed = search_seed(search.seed, fold_index);
                            search.seed = seed;
                            MultiMarketSearchGenerator::new(
                                search,
                                seed,
                                active_parameter_space.to_vec(),
                                combinations.to_vec(),
                            )
                        })
                        .transpose()?
                } else {
                    None
                };
                for ordinal in 1..=if generator.is_some() {
                    total
                } else {
                    candidate_plans.len()
                } {
                    if cancellation.is_some_and(CancellationToken::is_cancelled) {
                        return Ok(None);
                    }
                    let plan = match generator.as_mut() {
                        Some(search) => search.next(ordinal, target_kind)?,
                        None => candidate_plans
                            .get(ordinal - 1)
                            .cloned()
                            .ok_or_else(|| invariant("多市场折内候选计划数量不足"))?,
                    };
                    if generator.is_some() {
                        fold_plans.push(plan.clone());
                    }
                    let candidate = run_training_candidate(
                        spec,
                        &plan,
                        root,
                        Some(fold_index + 1),
                        "train",
                        callback_parameter_search,
                        Some(window),
                        context,
                        cancellation,
                        make_injected,
                    )?;
                    let Some(candidate) = candidate else {
                        return Ok(None);
                    };
                    if let Some(search) = generator.as_mut() {
                        search.observe(&plan, Some(&candidate));
                    }
                    if let Some(outcome) = &candidate.outcome {
                        if spec
                            .search
                            .as_ref()
                            .and_then(|search| search.max_training_drawdown_bps)
                            .is_none_or(|limit| outcome.max_drawdown_bps <= limit)
                        {
                            fold_training.push((candidate.ordinal, outcome.clone()));
                        }
                    }
                    if let Some(error) = candidate.error {
                        fold_candidate_failures.push(MultiMarketExperimentFoldCandidateFailure {
                            fold: fold_index + 1,
                            ordinal: plan.ordinal,
                            error,
                        });
                    }
                }
                if let (Some(generator), Some(search)) = (generator, spec.search.as_ref()) {
                    initial_fold_searches.push(MultiMarketExperimentFoldSearchReport {
                        fold: fold_index + 1,
                        seed: generator.seed,
                        train_range: range_to_u64(fold.train)?,
                        test_range: range_to_u64(fold.test)?,
                        trials: generator.trials,
                    });
                    debug_assert!(search.fold_local_search);
                }
            }
            fold_training.sort_by(|left, right| {
                right
                    .1
                    .return_bps
                    .cmp(&left.1.return_bps)
                    .then_with(|| left.1.max_drawdown_bps.cmp(&right.1.max_drawdown_bps))
                    .then_with(|| left.0.cmp(&right.0))
            });
            let (selected_ordinal, selected_training) =
                fold_training.into_iter().next().ok_or_else(|| {
                    AppError::new(
                        AppErrorCategory::FidelityInsufficient,
                        format!("多市场实验第 {} 折训练窗没有成功候选", fold_index + 1),
                    )
                })?;
            let selected_plan = if fold_index > 0
                && spec
                    .search
                    .as_ref()
                    .is_some_and(|search| search.fold_local_search)
            {
                fold_plans
                    .iter()
                    .find(|plan| plan.ordinal == selected_ordinal)
            } else {
                candidate_plans
                    .iter()
                    .find(|plan| plan.ordinal == selected_ordinal)
            }
            .ok_or_else(|| invariant("多市场实验选中的候选计划不存在"))?;
            let (test_candidate, candidate_error) = build_candidate_spec(
                spec,
                selected_plan,
                root,
                Some(fold_index + 1),
                "oos",
                callback_parameter_search,
            )?;
            if let Some(error) = candidate_error {
                return Err(error);
            }
            let injected = make_injected(&test_candidate, &selected_plan.parameters)?;
            let test_window = window_for_range(timeline, fold.test)?;
            let test_outcome = run_inner_with_window(
                &test_candidate,
                context,
                cancellation,
                injected,
                Some(test_window),
                Some(&selected_plan.parameters),
            )?
            .ok_or_else(|| invariant("多市场样本外执行意外取消"))?;
            fold_reports.push(to_experiment_fold_report(
                fold_index + 1,
                *fold,
                selected_ordinal,
                selected_plan,
                !spec.strategy_kinds.is_empty(),
                spec.search
                    .as_ref()
                    .and_then(|search| search.max_training_drawdown_bps),
                &selected_training,
                &test_outcome,
            )?);
        }
        out_of_sample = Some(summarize_folds(fold_reports));
    }
    Ok(Some(FoldExecutionResult {
        ranking,
        out_of_sample,
        fold_candidate_failures,
        fold_searches: initial_fold_searches,
    }))
}
