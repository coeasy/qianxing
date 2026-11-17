//! Discrete/continuous proposal coordination for multi-market experiments.

use super::*;

#[derive(Clone)]
pub(super) struct CandidatePlan {
    pub(super) ordinal: usize,
    pub(super) kind: BuiltinStrategyKind,
    pub(super) parameters: BTreeMap<String, i128>,
    pub(super) proposal: Option<String>,
    pub(super) search_index: Option<usize>,
}

pub(super) struct MultiMarketSearchGenerator {
    plan: ExperimentSearchPlan,
    pub(super) seed: u64,
    dimensions: Vec<ExperimentParameterSpace>,
    combinations: Vec<BTreeMap<String, i128>>,
    ranges: Vec<ExperimentParameterRange>,
    discrete: Option<SearchState>,
    continuous: Option<ContinuousSearchState>,
    pub(super) trials: Vec<ExperimentSearchTrial>,
    observed_scores: Vec<(usize, i32)>,
    observed_drawdowns: Vec<(usize, u32)>,
}

impl MultiMarketSearchGenerator {
    pub(super) fn new(
        plan: ExperimentSearchPlan,
        seed: u64,
        dimensions: Vec<ExperimentParameterSpace>,
        combinations: Vec<BTreeMap<String, i128>>,
    ) -> Result<Self, AppError> {
        let ranges = plan.continuous_parameter_space.clone();
        let continuous = if ranges.is_empty() {
            None
        } else {
            Some(ContinuousSearchState::new(&ranges, seed)?)
        };
        let discrete = ranges
            .is_empty()
            .then(|| SearchState::new(combinations.len(), seed));
        Ok(Self {
            plan,
            seed,
            dimensions,
            combinations,
            ranges,
            discrete,
            continuous,
            trials: Vec::new(),
            observed_scores: Vec::new(),
            observed_drawdowns: Vec::new(),
        })
    }

    pub(super) fn next(
        &mut self,
        ordinal: usize,
        kind: BuiltinStrategyKind,
    ) -> Result<CandidatePlan, AppError> {
        let objective = if self.plan.algorithm == ExperimentSearchAlgorithm::ParetoTpe {
            ExperimentSearchObjective::ParetoReturnDrawdown
        } else {
            ExperimentSearchObjective::Return
        };
        let (parameters, proposal, search_index) = if self.ranges.is_empty() {
            let eligible_scores = self
                .observed_scores
                .iter()
                .copied()
                .filter(|(index, _)| {
                    self.plan.max_training_drawdown_bps.is_none_or(|limit| {
                        self.observed_drawdowns
                            .iter()
                            .any(|(drawdown_index, drawdown)| {
                                drawdown_index == index && *drawdown <= limit
                            })
                    })
                })
                .collect::<Vec<_>>();
            let eligible_drawdowns = self
                .observed_drawdowns
                .iter()
                .copied()
                .filter(|(_, drawdown)| {
                    self.plan
                        .max_training_drawdown_bps
                        .is_none_or(|limit| *drawdown <= limit)
                })
                .collect::<Vec<_>>();
            let (index, proposal) = self
                .discrete
                .as_mut()
                .and_then(|state| {
                    state.next_with_objective(
                        self.plan.algorithm,
                        objective,
                        4,
                        &self.dimensions,
                        &self.combinations,
                        &eligible_scores,
                        &eligible_drawdowns,
                    )
                })
                .ok_or_else(|| invariant("多市场自适应搜索提前耗尽离散候选"))?;
            (
                self.combinations[index].clone(),
                proposal.to_string(),
                Some(index),
            )
        } else {
            let eligible_trials = self
                .trials
                .iter()
                .filter(|trial| {
                    self.plan.max_training_drawdown_bps.is_none_or(|limit| {
                        trial
                            .training_max_drawdown_bps
                            .is_some_and(|drawdown| drawdown <= limit)
                    })
                })
                .cloned()
                .collect::<Vec<_>>();
            let (parameters, proposal) = self
                .continuous
                .as_mut()
                .and_then(|state| {
                    state.next(
                        self.plan.algorithm,
                        objective,
                        4,
                        &self.ranges,
                        &eligible_trials,
                    )
                })
                .ok_or_else(|| invariant("多市场自适应搜索提前耗尽连续候选"))?;
            (parameters, proposal.to_string(), None)
        };
        Ok(CandidatePlan {
            ordinal,
            kind,
            parameters,
            proposal: Some(proposal),
            search_index,
        })
    }

    pub(super) fn observe(
        &mut self,
        candidate: &CandidatePlan,
        outcome: Option<&MultiMarketExperimentCandidate>,
    ) {
        let training_score_bps = outcome
            .and_then(|candidate| candidate.outcome.as_ref())
            .map(|run| run.return_bps);
        let training_max_drawdown_bps = outcome
            .and_then(|candidate| candidate.outcome.as_ref())
            .map(|run| run.max_drawdown_bps);
        self.trials.push(ExperimentSearchTrial {
            ordinal: candidate.ordinal,
            proposal: candidate
                .proposal
                .clone()
                .unwrap_or_else(|| "random".into()),
            parameters: candidate.parameters.clone(),
            training_score_bps,
            training_max_drawdown_bps,
            training_robustness_score_bps: None,
        });
        if let (Some(index), Some(score), Some(drawdown)) = (
            candidate.search_index,
            training_score_bps,
            training_max_drawdown_bps,
        ) {
            self.observed_scores.push((index, score));
            self.observed_drawdowns.push((index, drawdown));
        }
    }
}
