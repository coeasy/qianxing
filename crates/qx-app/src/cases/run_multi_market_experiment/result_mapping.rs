//! Multi-market experiment result ranking and fold/failure DTO projection.

use super::*;

pub(super) fn rank_candidates(
    candidates: &[MultiMarketExperimentCandidate],
) -> Vec<MultiMarketExperimentRankedCandidate> {
    let mut ranked = candidates
        .iter()
        .filter_map(|candidate| {
            candidate.outcome.as_ref().map(|outcome| {
                (
                    candidate.ordinal,
                    outcome.run_id.clone(),
                    outcome.return_bps,
                    outcome.max_drawdown_bps,
                    outcome.shared_result_hash.clone(),
                )
            })
        })
        .collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        right
            .2
            .cmp(&left.2)
            .then_with(|| left.3.cmp(&right.3))
            .then_with(|| left.0.cmp(&right.0))
    });
    ranked
        .into_iter()
        .enumerate()
        .map(
            |(index, (ordinal, run_id, return_bps, max_drawdown_bps, shared_result_hash))| {
                MultiMarketExperimentRankedCandidate {
                    rank: index + 1,
                    ordinal,
                    run_id,
                    return_bps,
                    max_drawdown_bps,
                    shared_result_hash,
                }
            },
        )
        .collect()
}

#[allow(clippy::too_many_arguments)]
pub(super) fn to_experiment_fold_report(
    fold_number: usize,
    ranges: FoldRanges,
    selected_ordinal: usize,
    selected_plan: &CandidatePlan,
    selected_strategy_kind: bool,
    max_training_drawdown_bps: Option<u32>,
    training: &crate::MultiMarketBacktestOutcome,
    test: &crate::MultiMarketBacktestOutcome,
) -> Result<ExperimentFoldReport, AppError> {
    let range = |value: [usize; 2]| -> Result<[u64; 2], AppError> {
        Ok([
            u64::try_from(value[0]).map_err(|_| invalid("实验折索引超过 u64 范围"))?,
            u64::try_from(value[1]).map_err(|_| invalid("实验折索引超过 u64 范围"))?,
        ])
    };
    Ok(ExperimentFoldReport {
        fold: fold_number,
        train_range: range(ranges.train)?,
        test_range: range(ranges.test)?,
        purge_range: (ranges.purge[0] < ranges.purge[1])
            .then(|| range(ranges.purge))
            .transpose()?,
        embargo_range: (ranges.embargo[0] < ranges.embargo[1])
            .then(|| range(ranges.embargo))
            .transpose()?,
        selected_ordinal,
        selected_strategy_kind: selected_strategy_kind.then(|| selected_plan.kind.name().into()),
        selected_parameters: selected_plan.parameters.clone(),
        in_sample_return_bps: training.return_bps,
        selected_training_robustness_score_bps: None,
        selected_training_max_drawdown_bps: Some(training.max_drawdown_bps),
        max_training_drawdown_bps,
        out_of_sample_return_bps: test.return_bps,
    })
}

pub(super) fn failed_candidate(
    ordinal: usize,
    kind: BuiltinStrategyKind,
    selected_kind: bool,
    parameters: BTreeMap<String, i128>,
    error: AppError,
) -> MultiMarketExperimentCandidate {
    MultiMarketExperimentCandidate {
        ordinal,
        strategy_kind: selected_kind.then(|| kind.name().to_string()),
        parameters,
        outcome: None,
        error: Some(error.to_json()),
    }
}
