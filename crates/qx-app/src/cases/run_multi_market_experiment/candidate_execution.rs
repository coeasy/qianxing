//! First-fold candidate proposal, execution, and adaptive-search feedback.

use super::search::MultiMarketSearchGenerator;
use super::*;

pub(super) struct InitialCandidateRequest<'a, F> {
    pub(super) spec: &'a RunMultiMarketExperimentSpec,
    pub(super) total: usize,
    pub(super) target_kind: BuiltinStrategyKind,
    pub(super) root: &'a std::path::Path,
    pub(super) candidate_plans: Vec<CandidatePlan>,
    pub(super) active_parameter_space: &'a [ExperimentParameterSpace],
    pub(super) combinations: &'a [BTreeMap<String, i128>],
    pub(super) first_fold: Option<FoldRanges>,
    pub(super) first_training_window: Option<MultiMarketExecutionWindow>,
    pub(super) callback_parameter_search: bool,
    pub(super) context: &'a RunContext,
    pub(super) cancellation: Option<&'a CancellationToken>,
    pub(super) make_injected: &'a mut F,
}

pub(super) struct InitialCandidateResult {
    pub(super) candidate_plans: Vec<CandidatePlan>,
    pub(super) candidates: Vec<MultiMarketExperimentCandidate>,
    pub(super) fold_searches: Vec<MultiMarketExperimentFoldSearchReport>,
}

pub(super) fn execute_initial_candidates<F>(
    request: InitialCandidateRequest<'_, F>,
) -> Result<Option<InitialCandidateResult>, AppError>
where
    F: FnMut(
        &MultiMarketBacktestSpec,
        &BTreeMap<String, i128>,
    ) -> Result<BTreeMap<String, InjectedMarketStrategy>, AppError>,
{
    let InitialCandidateRequest {
        spec,
        total,
        target_kind,
        root,
        mut candidate_plans,
        active_parameter_space,
        combinations,
        first_fold,
        first_training_window,
        callback_parameter_search,
        context,
        cancellation,
        make_injected,
    } = request;
    let mut candidates = Vec::with_capacity(total);
    let mut first_search = spec
        .search
        .clone()
        .map(|search| {
            MultiMarketSearchGenerator::new(
                search,
                spec.search.as_ref().map_or(0, |plan| plan.seed),
                active_parameter_space.to_vec(),
                combinations.to_vec(),
            )
        })
        .transpose()?;
    for ordinal in 1..=total {
        if cancellation.is_some_and(CancellationToken::is_cancelled) {
            return Ok(None);
        }
        let plan = match first_search.as_mut() {
            Some(search) => search.next(ordinal, target_kind)?,
            None => candidate_plans
                .get(ordinal - 1)
                .cloned()
                .ok_or_else(|| invariant("多市场实验候选计划数量不足"))?,
        };
        if first_search.is_some() {
            candidate_plans.push(plan.clone());
        }
        let candidate = run_training_candidate(
            spec,
            &plan,
            root,
            first_fold.map(|_| 1),
            "train",
            callback_parameter_search,
            first_training_window,
            context,
            cancellation,
            make_injected,
        )?;
        let Some(candidate) = candidate else {
            return Ok(None);
        };
        if let Some(search) = first_search.as_mut() {
            search.observe(&plan, Some(&candidate));
        }
        candidates.push(candidate);
    }
    let mut fold_searches = Vec::new();
    if let (Some(search), Some(fold), Some(generator)) =
        (spec.search.as_ref(), first_fold, first_search.take())
    {
        fold_searches.push(MultiMarketExperimentFoldSearchReport {
            fold: 1,
            seed: generator.seed,
            train_range: range_to_u64(fold.train)?,
            test_range: range_to_u64(fold.test)?,
            trials: generator.trials,
        });
        debug_assert_eq!(search.budget, total);
    }
    Ok(Some(InitialCandidateResult {
        candidate_plans,
        candidates,
        fold_searches,
    }))
}
