//! Deterministic random, categorical TPE, and adaptive ordinal Parzen TPE over a finite grid.
//!
//! Candidate proposals consume only training-window scores. Test windows are never passed to
//! this module; they remain owned by `experiment_split::evaluate_out_of_sample`.

use super::run_experiment::{
    ExecutionCostSensitivityResult, ExperimentCandidateResult, ExperimentParameterSpace,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExperimentSearchAlgorithm {
    Random,
    Tpe,
    /// Gaussian Parzen density over ordered grid positions with a data-derived bandwidth.
    TpeKde,
    ParetoTpe,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExperimentSearchObjective {
    #[default]
    Return,
    ParetoReturnDrawdown,
}

/// Training-window-only interpretation of an evaluated parameter neighborhood.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExperimentNeighborhoodAssessment {
    /// At least one adjacent grid candidate has not been evaluated.
    IncompleteNeighborhood,
    /// The parameter grid has no adjacent values around the selected center.
    NoNeighbors,
    /// All adjacent candidates were attempted, but none produced a score.
    NoSuccessfulNeighbors,
    /// The center is profitable and every adjacent candidate also scored positively.
    StablePositive,
    /// The center is profitable but an adjacent candidate failed or scored non-positive.
    FragilePositive,
    /// At least one scored adjacent candidate or the center exceeds the declared drawdown budget.
    DrawdownBudgetExceeded,
    /// A scored adjacent candidate or the center lacks the drawdown value needed by the budget.
    DrawdownBudgetUnverified,
    /// The selected center did not produce a positive training score.
    NoPositiveTrainingEdge,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExperimentSearchPlan {
    pub algorithm: ExperimentSearchAlgorithm,
    pub seed: u64,
    /// Hard budget; it cannot exceed the size of the finite validated parameter grid.
    pub budget: usize,
    /// Optional hard maximum drawdown on the first training window, in basis points.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_training_drawdown_bps: Option<u32>,
    /// Rebuild each walk-forward fold's dataset and run an independent seeded search.
    #[serde(default, skip_serializing_if = "is_false")]
    pub fold_local_search: bool,
    /// Maximum ordered parameter-grid distance included in neighborhood stability reporting.
    /// Omitted values preserve the historical radius-1 report.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub neighborhood_radius: Option<usize>,
    /// Optional inclusive integer ranges for adaptive proposals. When present, the legacy
    /// categorical `RunExperimentSpec.parameter_space` must be empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub continuous_parameter_space: Vec<ExperimentParameterRange>,
    /// Stop a candidate after an explicit warmup prefix once the hard training drawdown budget
    /// is already breached. The drawdown metric is monotone over completed training bars.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub early_pruning: Option<ExperimentEarlyPruningPlan>,
    /// Prefer the worst return across equal contiguous pieces of each training window.
    /// This is computed from training equity only and never includes purge/test bars.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub robustness_subwindows: Option<usize>,
    /// Require the worst training subwindow return to meet this floor before OOS selection.
    /// This is a selection gate, not an objective score and never reads test-window data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimum_training_robustness_score_bps: Option<i32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExperimentEarlyPruningPlan {
    /// Do not inspect the drawdown budget until this many training bars have completed.
    pub min_training_bars: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExperimentCandidatePruning {
    pub processed_bars: usize,
    pub observed_max_drawdown_bps: u32,
    pub max_training_drawdown_bps: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct DrawdownPruningWindow {
    pub min_bars: usize,
    pub training_bars: usize,
    pub max_drawdown_bps: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum SearchBacktestAttempt<T> {
    Completed(Option<T>),
    Pruned(ExperimentCandidatePruning),
}

/// Inclusive, stepped integer domain for a single built-in strategy parameter.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExperimentParameterRange {
    pub name: String,
    pub min: i128,
    pub max: i128,
    pub step: i128,
}

impl ExperimentParameterRange {
    pub(super) fn point_count(&self) -> Option<u64> {
        if self.min < 0 || self.max <= self.min || self.step <= 0 {
            return None;
        }
        let span = self.max.checked_sub(self.min)?;
        let points = span.checked_div(self.step)?.checked_add(1)?;
        let points = u64::try_from(points).ok()?;
        // Exact conversion to/from f64 ordinal coordinates is needed by the Parzen proposal.
        (2..=(1_u64 << 53)).contains(&points).then_some(points)
    }

    pub(super) fn value_at(&self, index: u64) -> Option<i128> {
        if index >= self.point_count()? {
            return None;
        }
        self.step
            .checked_mul(i128::from(index))
            .and_then(|offset| self.min.checked_add(offset))
    }

    fn index_of(&self, value: i128) -> Option<u64> {
        let offset = value.checked_sub(self.min)?;
        if offset < 0 || offset % self.step != 0 {
            return None;
        }
        let index = u64::try_from(offset / self.step).ok()?;
        (index < self.point_count()?).then_some(index)
    }
}

pub(super) fn continuous_domain_cardinality(ranges: &[ExperimentParameterRange]) -> Option<u128> {
    if ranges.is_empty() {
        return None;
    }
    ranges.iter().try_fold(1_u128, |count, range| {
        Some(count.saturating_mul(u128::from(range.point_count()?)))
    })
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExperimentSearchTrial {
    pub ordinal: usize,
    pub proposal: String,
    pub parameters: BTreeMap<String, i128>,
    /// Full-interval return on the scored training window. Failed candidates have no score.
    pub training_score_bps: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub training_max_drawdown_bps: Option<u32>,
    /// Minimum return over the configured contiguous training subwindows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub training_robustness_score_bps: Option<i32>,
}

/// Return the theoretical one-dimensional neighborhood around a parameter center.
/// The caller matches these parameter maps against evaluated trials; absent maps are gaps.
pub(super) fn neighborhood_parameter_sets(
    dimensions: &[ExperimentParameterSpace],
    grid: &[BTreeMap<String, i128>],
    ranges: &[ExperimentParameterRange],
    center: &BTreeMap<String, i128>,
    radius: usize,
) -> Vec<BTreeMap<String, i128>> {
    if ranges.is_empty() {
        return grid
            .iter()
            .filter(|candidate| is_neighborhood_candidate(center, candidate, dimensions, radius))
            .cloned()
            .collect();
    }
    let mut possible = Vec::with_capacity(ranges.len().saturating_mul(radius).saturating_mul(2));
    for range in ranges {
        let Some(center_value) = center.get(&range.name).copied() else {
            return Vec::new();
        };
        let Some(point_count) = range.point_count() else {
            return Vec::new();
        };
        let Some(center_index) = range.index_of(center_value) else {
            return Vec::new();
        };
        for distance in 1..=radius {
            let Ok(distance) = u64::try_from(distance) else {
                return Vec::new();
            };
            for neighbor_index in [
                center_index.checked_sub(distance),
                center_index
                    .checked_add(distance)
                    .filter(|index| *index < point_count),
            ]
            .into_iter()
            .flatten()
            {
                let Some(value) = range.value_at(neighbor_index) else {
                    return Vec::new();
                };
                let mut neighbor = center.clone();
                neighbor.insert(range.name.clone(), value);
                possible.push(neighbor);
            }
        }
    }
    possible
}

impl ExperimentSearchTrial {
    /// The proposal/selection objective. Existing plans retain total training return semantics.
    pub fn selection_score_bps(&self) -> Option<i32> {
        self.training_robustness_score_bps
            .or(self.training_score_bps)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExperimentSearchReport {
    pub algorithm: ExperimentSearchAlgorithm,
    #[serde(default)]
    pub objective: ExperimentSearchObjective,
    pub seed: u64,
    pub budget: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_training_drawdown_bps: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub early_pruning: Option<ExperimentEarlyPruningPlan>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub robustness_subwindows: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimum_training_robustness_score_bps: Option<i32>,
    /// Aggregate result of any per-scenario training cost-degradation gates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_robustness_passed: Option<bool>,
    /// Aggregate result of configured per-scenario neighborhood cost gates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub neighborhood_cost_robustness_passed: Option<bool>,
    pub trials: Vec<ExperimentSearchTrial>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub neighborhood: Option<ExperimentNeighborhoodReport>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fold_searches: Vec<ExperimentFoldSearchReport>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExperimentFoldSearchReport {
    pub fold: usize,
    pub seed: u64,
    pub train_range: [u64; 2],
    pub test_range: [u64; 2],
    pub trials: Vec<ExperimentSearchTrial>,
    pub candidates: Vec<ExperimentCandidateResult>,
    pub selected_ordinal: Option<usize>,
    pub selected_run_manifest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub neighborhood: Option<ExperimentNeighborhoodReport>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cost_sensitivity: Vec<ExecutionCostSensitivityResult>,
}

/// First-order finite-grid stability around the best evaluated training candidate.
/// Scores are training-window only; unevaluated candidates remain visible in coverage counts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExperimentNeighborhoodReport {
    pub center_ordinal: usize,
    pub center_parameters: BTreeMap<String, i128>,
    pub radius: usize,
    pub possible_neighbors: usize,
    pub evaluated_neighbors: usize,
    pub successful_neighbors: usize,
    pub failed_neighbors: usize,
    pub mean_training_score_bps: Option<i32>,
    pub min_training_score_bps: Option<i32>,
    pub max_training_score_bps: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assessment: Option<ExperimentNeighborhoodAssessment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drawdown_budget_bps: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub center_drawdown_bps: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_neighbor_drawdown_bps: Option<u32>,
    #[serde(default, skip_serializing_if = "is_zero_usize")]
    pub drawdown_budget_exceeded_neighbors: usize,
    #[serde(default, skip_serializing_if = "is_zero_usize")]
    pub drawdown_budget_unverified_neighbors: usize,
}

fn is_zero_usize(value: &usize) -> bool {
    *value == 0
}

struct NeighborhoodDrawdownFacts {
    budget_bps: Option<u32>,
    center_bps: Option<u32>,
    max_neighbor_bps: Option<u32>,
    exceeded_neighbors: usize,
    unverified_neighbors: usize,
    assessment: Option<ExperimentNeighborhoodAssessment>,
}

fn neighborhood_drawdown_facts(
    center: &ExperimentSearchTrial,
    neighbors: &[&ExperimentSearchTrial],
    budget_bps: Option<u32>,
) -> NeighborhoodDrawdownFacts {
    let center_bps = center.training_max_drawdown_bps;
    let max_neighbor_bps = neighbors
        .iter()
        .filter_map(|trial| trial.training_max_drawdown_bps)
        .max();
    let exceeded_neighbors = budget_bps.map_or(0, |budget| {
        neighbors
            .iter()
            .filter(|trial| {
                trial
                    .training_max_drawdown_bps
                    .is_some_and(|drawdown| drawdown > budget)
            })
            .count()
    });
    let unverified_neighbors = budget_bps.map_or(0, |_| {
        neighbors
            .iter()
            .filter(|trial| {
                trial.selection_score_bps().is_some() && trial.training_max_drawdown_bps.is_none()
            })
            .count()
    });
    let assessment = budget_bps.and_then(|budget| {
        if center_bps.is_some_and(|drawdown| drawdown > budget) || exceeded_neighbors > 0 {
            Some(ExperimentNeighborhoodAssessment::DrawdownBudgetExceeded)
        } else if center_bps.is_none() || unverified_neighbors > 0 {
            Some(ExperimentNeighborhoodAssessment::DrawdownBudgetUnverified)
        } else {
            None
        }
    });
    NeighborhoodDrawdownFacts {
        budget_bps,
        center_bps,
        max_neighbor_bps,
        exceeded_neighbors,
        unverified_neighbors,
        assessment,
    }
}

fn best_neighborhood_center(
    trials: &[ExperimentSearchTrial],
    drawdown_budget_bps: Option<u32>,
) -> Option<(&ExperimentSearchTrial, i32)> {
    let scored = trials
        .iter()
        .filter_map(|trial| trial.selection_score_bps().map(|score| (trial, score)))
        .collect::<Vec<_>>();
    let compare = |(left, left_score): &(&ExperimentSearchTrial, i32),
                   (right, right_score): &(&ExperimentSearchTrial, i32)| {
        left_score
            .cmp(right_score)
            .then_with(|| {
                right
                    .training_max_drawdown_bps
                    .cmp(&left.training_max_drawdown_bps)
            })
            .then_with(|| right.ordinal.cmp(&left.ordinal))
    };
    let eligible = scored
        .iter()
        .copied()
        .filter(|(trial, _)| {
            drawdown_budget_bps.is_none_or(|budget| {
                trial
                    .training_max_drawdown_bps
                    .is_some_and(|drawdown| drawdown <= budget)
            })
        })
        .max_by(compare);

    // Keep a diagnostic center when every scored candidate misses the drawdown gate. The
    // report will mark that center as exceeded or unverified instead of disappearing.
    eligible.or_else(|| scored.into_iter().max_by(compare))
}

#[cfg(test)]
pub(super) fn neighborhood_report(
    dimensions: &[ExperimentParameterSpace],
    grid: &[BTreeMap<String, i128>],
    trials: &[ExperimentSearchTrial],
) -> Option<ExperimentNeighborhoodReport> {
    neighborhood_report_with_budget(dimensions, grid, trials, None)
}

#[cfg(test)]
pub(super) fn neighborhood_report_with_budget(
    dimensions: &[ExperimentParameterSpace],
    grid: &[BTreeMap<String, i128>],
    trials: &[ExperimentSearchTrial],
    drawdown_budget_bps: Option<u32>,
) -> Option<ExperimentNeighborhoodReport> {
    neighborhood_report_with_radius_and_budget(dimensions, grid, trials, 1, drawdown_budget_bps)
}

pub(super) fn neighborhood_report_with_radius_and_budget(
    dimensions: &[ExperimentParameterSpace],
    grid: &[BTreeMap<String, i128>],
    trials: &[ExperimentSearchTrial],
    radius: usize,
    drawdown_budget_bps: Option<u32>,
) -> Option<ExperimentNeighborhoodReport> {
    let center = best_neighborhood_center(trials, drawdown_budget_bps)?;
    let (center_trial, _) = center;
    let possible = grid
        .iter()
        .filter(|candidate| {
            is_neighborhood_candidate(&center_trial.parameters, candidate, dimensions, radius)
        })
        .collect::<Vec<_>>();
    let evaluated = possible
        .iter()
        .filter_map(|candidate| trials.iter().find(|trial| trial.parameters == **candidate))
        .collect::<Vec<_>>();
    let scores = evaluated
        .iter()
        .filter_map(|trial| trial.selection_score_bps())
        .collect::<Vec<_>>();
    let mean = if scores.is_empty() {
        None
    } else {
        Some(
            (scores.iter().map(|score| i64::from(*score)).sum::<i64>() / scores.len() as i64)
                as i32,
        )
    };
    let failed_neighbors = evaluated.len().saturating_sub(scores.len());
    let drawdown = neighborhood_drawdown_facts(center_trial, &evaluated, drawdown_budget_bps);
    let assessment = if possible.is_empty() {
        ExperimentNeighborhoodAssessment::NoNeighbors
    } else if evaluated.len() < possible.len() {
        ExperimentNeighborhoodAssessment::IncompleteNeighborhood
    } else if scores.is_empty() {
        ExperimentNeighborhoodAssessment::NoSuccessfulNeighbors
    } else if center.1 <= 0 {
        ExperimentNeighborhoodAssessment::NoPositiveTrainingEdge
    } else if let Some(drawdown_assessment) = drawdown.assessment {
        drawdown_assessment
    } else if failed_neighbors == 0 && scores.iter().all(|score| *score > 0) {
        ExperimentNeighborhoodAssessment::StablePositive
    } else {
        ExperimentNeighborhoodAssessment::FragilePositive
    };
    Some(ExperimentNeighborhoodReport {
        center_ordinal: center_trial.ordinal,
        center_parameters: center_trial.parameters.clone(),
        radius,
        possible_neighbors: possible.len(),
        evaluated_neighbors: evaluated.len(),
        successful_neighbors: scores.len(),
        failed_neighbors,
        mean_training_score_bps: mean,
        min_training_score_bps: scores.iter().copied().min(),
        max_training_score_bps: scores.iter().copied().max(),
        assessment: Some(assessment),
        drawdown_budget_bps: drawdown.budget_bps,
        center_drawdown_bps: drawdown.center_bps,
        max_neighbor_drawdown_bps: drawdown.max_neighbor_bps,
        drawdown_budget_exceeded_neighbors: drawdown.exceeded_neighbors,
        drawdown_budget_unverified_neighbors: drawdown.unverified_neighbors,
    })
}

/// First-order neighbor coverage for a bounded stepped-integer search space.
/// Only evaluated training-window observations contribute to score summaries.
#[cfg(test)]
pub(super) fn continuous_neighborhood_report(
    ranges: &[ExperimentParameterRange],
    trials: &[ExperimentSearchTrial],
) -> Option<ExperimentNeighborhoodReport> {
    continuous_neighborhood_report_with_budget(ranges, trials, None)
}

#[cfg(test)]
pub(super) fn continuous_neighborhood_report_with_budget(
    ranges: &[ExperimentParameterRange],
    trials: &[ExperimentSearchTrial],
    drawdown_budget_bps: Option<u32>,
) -> Option<ExperimentNeighborhoodReport> {
    continuous_neighborhood_report_with_radius_and_budget(ranges, trials, 1, drawdown_budget_bps)
}

pub(super) fn continuous_neighborhood_report_with_radius_and_budget(
    ranges: &[ExperimentParameterRange],
    trials: &[ExperimentSearchTrial],
    radius: usize,
    drawdown_budget_bps: Option<u32>,
) -> Option<ExperimentNeighborhoodReport> {
    if ranges.is_empty() {
        return None;
    }
    let center = best_neighborhood_center(trials, drawdown_budget_bps)?;
    let (center_trial, center_score) = center;
    let mut possible = Vec::with_capacity(ranges.len().saturating_mul(radius).saturating_mul(2));
    for range in ranges {
        let center_value = *center_trial.parameters.get(&range.name)?;
        let point_count = range.point_count()?;
        let center_index = range.index_of(center_value)?;
        for distance in 1..=radius {
            let distance = u64::try_from(distance).ok()?;
            for neighbor_index in [
                center_index.checked_sub(distance),
                center_index
                    .checked_add(distance)
                    .filter(|index| *index < point_count),
            ]
            .into_iter()
            .flatten()
            {
                let value = range.value_at(neighbor_index)?;
                let mut neighbor = center_trial.parameters.clone();
                neighbor.insert(range.name.clone(), value);
                possible.push(neighbor);
            }
        }
    }
    let evaluated = possible
        .iter()
        .filter_map(|candidate| trials.iter().find(|trial| trial.parameters == *candidate))
        .collect::<Vec<_>>();
    let scores = evaluated
        .iter()
        .filter_map(|trial| trial.selection_score_bps())
        .collect::<Vec<_>>();
    let mean = if scores.is_empty() {
        None
    } else {
        let total = scores.iter().map(|score| i64::from(*score)).sum::<i64>();
        Some((total / scores.len() as i64) as i32)
    };
    let failed_neighbors = evaluated.len().saturating_sub(scores.len());
    let drawdown = neighborhood_drawdown_facts(center_trial, &evaluated, drawdown_budget_bps);
    let assessment = if possible.is_empty() {
        ExperimentNeighborhoodAssessment::NoNeighbors
    } else if evaluated.len() < possible.len() {
        ExperimentNeighborhoodAssessment::IncompleteNeighborhood
    } else if scores.is_empty() {
        ExperimentNeighborhoodAssessment::NoSuccessfulNeighbors
    } else if center_score <= 0 {
        ExperimentNeighborhoodAssessment::NoPositiveTrainingEdge
    } else if let Some(drawdown_assessment) = drawdown.assessment {
        drawdown_assessment
    } else if failed_neighbors == 0 && scores.iter().all(|score| *score > 0) {
        ExperimentNeighborhoodAssessment::StablePositive
    } else {
        ExperimentNeighborhoodAssessment::FragilePositive
    };
    Some(ExperimentNeighborhoodReport {
        center_ordinal: center_trial.ordinal,
        center_parameters: center_trial.parameters.clone(),
        radius,
        possible_neighbors: possible.len(),
        evaluated_neighbors: evaluated.len(),
        successful_neighbors: scores.len(),
        failed_neighbors,
        mean_training_score_bps: mean,
        min_training_score_bps: scores.iter().copied().min(),
        max_training_score_bps: scores.iter().copied().max(),
        assessment: Some(assessment),
        drawdown_budget_bps: drawdown.budget_bps,
        center_drawdown_bps: drawdown.center_bps,
        max_neighbor_drawdown_bps: drawdown.max_neighbor_bps,
        drawdown_budget_exceeded_neighbors: drawdown.exceeded_neighbors,
        drawdown_budget_unverified_neighbors: drawdown.unverified_neighbors,
    })
}

fn is_neighborhood_candidate(
    center: &BTreeMap<String, i128>,
    candidate: &BTreeMap<String, i128>,
    dimensions: &[ExperimentParameterSpace],
    radius: usize,
) -> bool {
    let mut changed_dimensions = 0usize;
    for dimension in dimensions {
        let Some(center_value) = center.get(&dimension.name) else {
            return false;
        };
        let Some(candidate_value) = candidate.get(&dimension.name) else {
            return false;
        };
        let mut ordered_values = dimension.values.clone();
        ordered_values.sort_unstable();
        let Some(center_index) = ordered_values
            .iter()
            .position(|value| value == center_value)
        else {
            return false;
        };
        let Some(candidate_index) = ordered_values
            .iter()
            .position(|value| value == candidate_value)
        else {
            return false;
        };
        if center_index != candidate_index {
            if center_index.abs_diff(candidate_index) > radius {
                return false;
            }
            changed_dimensions += 1;
            if changed_dimensions > 1 {
                return false;
            }
        }
    }
    changed_dimensions == 1
}

#[cfg(test)]
mod assessment_tests {
    use super::*;

    fn space() -> Vec<ExperimentParameterSpace> {
        vec![
            ExperimentParameterSpace {
                name: "fast_window".into(),
                values: vec![1, 2, 3],
            },
            ExperimentParameterSpace {
                name: "slow_window".into(),
                values: vec![4, 5, 6],
            },
        ]
    }

    fn candidate(fast: i128, slow: i128) -> BTreeMap<String, i128> {
        BTreeMap::from([("fast_window".into(), fast), ("slow_window".into(), slow)])
    }

    fn trial(
        ordinal: usize,
        parameters: BTreeMap<String, i128>,
        score: Option<i32>,
    ) -> ExperimentSearchTrial {
        ExperimentSearchTrial {
            ordinal,
            proposal: "random".into(),
            parameters,
            training_score_bps: score,
            training_max_drawdown_bps: None,
            training_robustness_score_bps: None,
        }
    }

    fn trial_with_drawdown(
        ordinal: usize,
        parameters: BTreeMap<String, i128>,
        score: i32,
        drawdown_bps: u32,
    ) -> ExperimentSearchTrial {
        let mut value = trial(ordinal, parameters, Some(score));
        value.training_max_drawdown_bps = Some(drawdown_bps);
        value
    }

    fn full_neighborhood(
        neighbor_scores: [Option<i32>; 4],
        center_score: i32,
    ) -> ExperimentNeighborhoodReport {
        let center = candidate(2, 5);
        let neighbors = [
            candidate(1, 5),
            candidate(3, 5),
            candidate(2, 4),
            candidate(2, 6),
        ];
        let grid = std::iter::once(center.clone())
            .chain(neighbors.iter().cloned())
            .collect::<Vec<_>>();
        let trials = std::iter::once(trial(1, center, Some(center_score)))
            .chain(
                neighbors
                    .into_iter()
                    .zip(neighbor_scores)
                    .enumerate()
                    .map(|(index, (parameters, score))| trial(index + 2, parameters, score)),
            )
            .collect::<Vec<_>>();
        neighborhood_report(&space(), &grid, &trials).unwrap()
    }

    #[test]
    fn assessment_distinguishes_stable_fragile_incomplete_and_no_edge_neighborhoods() {
        assert_eq!(
            full_neighborhood([Some(40), Some(30), Some(20), Some(10)], 100).assessment,
            Some(ExperimentNeighborhoodAssessment::StablePositive)
        );
        assert_eq!(
            full_neighborhood([Some(40), Some(30), Some(0), Some(10)], 100).assessment,
            Some(ExperimentNeighborhoodAssessment::FragilePositive)
        );
        assert_eq!(
            full_neighborhood([Some(40), None, Some(20), Some(10)], 100).assessment,
            Some(ExperimentNeighborhoodAssessment::FragilePositive)
        );
        let center = candidate(2, 5);
        let neighbors = [
            candidate(1, 5),
            candidate(3, 5),
            candidate(2, 4),
            candidate(2, 6),
        ];
        let grid = std::iter::once(center.clone())
            .chain(neighbors.iter().cloned())
            .collect::<Vec<_>>();
        let partial = [
            trial(1, center, Some(100)),
            trial(2, neighbors[0].clone(), Some(40)),
            trial(3, neighbors[1].clone(), Some(30)),
        ];
        assert_eq!(
            neighborhood_report(&space(), &grid, &partial)
                .unwrap()
                .assessment,
            Some(ExperimentNeighborhoodAssessment::IncompleteNeighborhood)
        );
        assert_eq!(
            full_neighborhood([Some(-4), Some(-3), Some(-2), Some(-1)], 0).assessment,
            Some(ExperimentNeighborhoodAssessment::NoPositiveTrainingEdge)
        );
        assert_eq!(
            full_neighborhood([None, None, None, None], 100).assessment,
            Some(ExperimentNeighborhoodAssessment::NoSuccessfulNeighbors)
        );
    }

    #[test]
    fn configurable_grid_radius_includes_only_one_dimension_changes_within_radius() {
        let dimensions = vec![
            ExperimentParameterSpace {
                name: "fast_window".into(),
                values: vec![1, 2, 3, 4, 5],
            },
            ExperimentParameterSpace {
                name: "slow_window".into(),
                values: vec![1, 2, 3, 4, 5],
            },
        ];
        let center = candidate(3, 3);
        let neighbors = [
            candidate(1, 3),
            candidate(2, 3),
            candidate(4, 3),
            candidate(5, 3),
            candidate(3, 1),
            candidate(3, 2),
            candidate(3, 4),
            candidate(3, 5),
            candidate(1, 1),
        ];
        let grid = std::iter::once(center.clone())
            .chain(neighbors.iter().cloned())
            .collect::<Vec<_>>();
        let trials = std::iter::once(trial(1, center, Some(100)))
            .chain(
                neighbors
                    .iter()
                    .cloned()
                    .enumerate()
                    .map(|(index, parameters)| trial(index + 2, parameters, Some(50))),
            )
            .collect::<Vec<_>>();
        let report =
            neighborhood_report_with_radius_and_budget(&dimensions, &grid, &trials, 2, None)
                .unwrap();
        assert_eq!(report.radius, 2);
        assert_eq!(report.possible_neighbors, 8);
        assert_eq!(report.evaluated_neighbors, 8);
        assert_eq!(
            report.assessment,
            Some(ExperimentNeighborhoodAssessment::StablePositive)
        );
    }

    #[test]
    fn neighborhood_drawdown_budget_marks_risky_neighbors_and_reports_the_counts() {
        let center = candidate(2, 5);
        let neighbors = [
            candidate(1, 5),
            candidate(3, 5),
            candidate(2, 4),
            candidate(2, 6),
        ];
        let grid = std::iter::once(center.clone())
            .chain(neighbors.iter().cloned())
            .collect::<Vec<_>>();
        let trials = std::iter::once(trial_with_drawdown(1, center, 100, 50))
            .chain(
                neighbors
                    .into_iter()
                    .enumerate()
                    .map(|(index, parameters)| {
                        trial_with_drawdown(index + 2, parameters, 40, [80, 120, 90, 100][index])
                    }),
            )
            .collect::<Vec<_>>();

        let report = neighborhood_report_with_budget(&space(), &grid, &trials, Some(100)).unwrap();
        assert_eq!(report.drawdown_budget_bps, Some(100));
        assert_eq!(report.center_drawdown_bps, Some(50));
        assert_eq!(report.max_neighbor_drawdown_bps, Some(120));
        assert_eq!(report.drawdown_budget_exceeded_neighbors, 1);
        assert_eq!(report.drawdown_budget_unverified_neighbors, 0);
        assert_eq!(
            report.assessment,
            Some(ExperimentNeighborhoodAssessment::DrawdownBudgetExceeded)
        );
    }

    #[test]
    fn neighborhood_center_matches_drawdown_eligible_search_selection() {
        let dimensions = vec![ExperimentParameterSpace {
            name: "period".into(),
            values: vec![1, 2, 3],
        }];
        let point = |period| BTreeMap::from([("period".into(), period)]);
        let grid = vec![point(1), point(2), point(3)];
        let trials = vec![
            trial_with_drawdown(1, point(1), 120, 220),
            trial_with_drawdown(2, point(2), 100, 100),
            trial_with_drawdown(3, point(3), 80, 90),
        ];

        let report =
            neighborhood_report_with_radius_and_budget(&dimensions, &grid, &trials, 1, Some(150))
                .unwrap();

        assert_eq!(report.center_ordinal, 2);
        assert_eq!(report.center_parameters["period"], 2);
        assert_eq!(report.center_drawdown_bps, Some(100));
    }

    #[test]
    fn assessment_marks_single_point_parameter_grid_as_no_neighbors() {
        let dimensions = vec![ExperimentParameterSpace {
            name: "period".into(),
            values: vec![5],
        }];
        let center = BTreeMap::from([("period".into(), 5)]);
        let report = neighborhood_report(
            &dimensions,
            std::slice::from_ref(&center),
            &[trial(1, center.clone(), Some(100))],
        )
        .unwrap();
        assert_eq!(
            report.assessment,
            Some(ExperimentNeighborhoodAssessment::NoNeighbors)
        );
    }

    #[test]
    fn legacy_neighborhood_json_without_assessment_remains_readable() {
        let payload = r#"{
            "center_ordinal":1,
            "center_parameters":{"period":5},
            "radius":1,
            "possible_neighbors":2,
            "evaluated_neighbors":1,
            "successful_neighbors":1,
            "failed_neighbors":0,
            "mean_training_score_bps":100,
            "min_training_score_bps":100,
            "max_training_score_bps":100
        }"#;
        let report: ExperimentNeighborhoodReport =
            serde_json::from_str(payload).expect("legacy result remains readable");
        assert_eq!(report.assessment, None);
    }
}

#[derive(Clone, Debug)]
pub(super) struct SearchState {
    rng: SplitMix64,
    remaining: Vec<usize>,
}

impl SearchState {
    pub(super) fn new(candidate_count: usize, seed: u64) -> Self {
        Self {
            rng: SplitMix64(seed),
            remaining: (0..candidate_count).collect(),
        }
    }

    #[cfg(test)]
    pub(super) fn next(
        &mut self,
        algorithm: ExperimentSearchAlgorithm,
        startup_trials: usize,
        dimensions: &[ExperimentParameterSpace],
        candidates: &[BTreeMap<String, i128>],
        observed_scores: &[(usize, i32)],
    ) -> Option<(usize, &'static str)> {
        self.next_with_objective(
            algorithm,
            if algorithm == ExperimentSearchAlgorithm::ParetoTpe {
                ExperimentSearchObjective::ParetoReturnDrawdown
            } else {
                ExperimentSearchObjective::Return
            },
            startup_trials,
            dimensions,
            candidates,
            observed_scores,
            &[],
        )
    }

    // Each input controls a separate search dimension or observed metric.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn next_with_objective(
        &mut self,
        algorithm: ExperimentSearchAlgorithm,
        objective: ExperimentSearchObjective,
        startup_trials: usize,
        dimensions: &[ExperimentParameterSpace],
        candidates: &[BTreeMap<String, i128>],
        observed_scores: &[(usize, i32)],
        observed_drawdowns: &[(usize, u32)],
    ) -> Option<(usize, &'static str)> {
        if self.remaining.is_empty() {
            return None;
        }
        let adaptive = matches!(
            algorithm,
            ExperimentSearchAlgorithm::Tpe
                | ExperimentSearchAlgorithm::TpeKde
                | ExperimentSearchAlgorithm::ParetoTpe
        ) && observed_scores.len() >= startup_trials;
        let selected = if adaptive {
            match objective {
                ExperimentSearchObjective::Return => propose_tpe(
                    dimensions,
                    candidates,
                    &self.remaining,
                    observed_scores,
                    algorithm == ExperimentSearchAlgorithm::TpeKde,
                ),
                ExperimentSearchObjective::ParetoReturnDrawdown => propose_pareto_tpe(
                    dimensions,
                    candidates,
                    &self.remaining,
                    observed_scores,
                    observed_drawdowns,
                ),
            }
        } else {
            let index = self.rng.index(self.remaining.len());
            self.remaining[index]
        };
        self.remaining.retain(|candidate| *candidate != selected);
        let proposal = match (algorithm, adaptive) {
            (ExperimentSearchAlgorithm::Random, _) => "random",
            (ExperimentSearchAlgorithm::Tpe, true) => "tpe",
            (ExperimentSearchAlgorithm::Tpe, false) => "tpe_random_startup",
            (ExperimentSearchAlgorithm::TpeKde, true) => "tpe_kde",
            (ExperimentSearchAlgorithm::TpeKde, false) => "tpe_kde_random_startup",
            (ExperimentSearchAlgorithm::ParetoTpe, true) => "pareto_tpe",
            (ExperimentSearchAlgorithm::ParetoTpe, false) => "pareto_tpe_random_startup",
        };
        Some((selected, proposal))
    }
}

/// Seeded adaptive proposals over stepped integer ranges. Ordinal coordinates are exact up to
/// 2^53 points per dimension (validated by `ExperimentParameterRange::point_count`).
#[derive(Clone, Debug)]
pub(super) struct ContinuousSearchState {
    rng: SplitMix64,
    seen: BTreeSet<BTreeMap<String, i128>>,
    domain_cardinality: u128,
}

impl ContinuousSearchState {
    pub(super) fn new(ranges: &[ExperimentParameterRange], seed: u64) -> Self {
        Self {
            rng: SplitMix64(seed),
            seen: BTreeSet::new(),
            domain_cardinality: continuous_domain_cardinality(ranges)
                .expect("continuous parameter ranges are validated before execution"),
        }
    }

    pub(super) fn next(
        &mut self,
        algorithm: ExperimentSearchAlgorithm,
        objective: ExperimentSearchObjective,
        startup_trials: usize,
        ranges: &[ExperimentParameterRange],
        observed: &[ExperimentSearchTrial],
    ) -> Option<(BTreeMap<String, i128>, &'static str)> {
        if self.seen.len() as u128 >= self.domain_cardinality {
            return None;
        }
        let scored = observed
            .iter()
            .filter_map(|trial| {
                trial
                    .selection_score_bps()
                    .map(|score| (trial, score, trial.training_max_drawdown_bps))
            })
            .collect::<Vec<_>>();
        let adaptive =
            algorithm != ExperimentSearchAlgorithm::Random && scored.len() >= startup_trials;
        let (proposal, label) = if adaptive {
            let good_ordinals = continuous_good_trials(&scored, objective);
            let good = scored
                .iter()
                .filter(|(trial, _, _)| good_ordinals.contains(&trial.ordinal))
                .map(|(trial, _, _)| *trial)
                .collect::<Vec<_>>();
            let bad = scored
                .iter()
                .filter(|(trial, _, _)| !good_ordinals.contains(&trial.ordinal))
                .map(|(trial, _, _)| *trial)
                .collect::<Vec<_>>();
            if good.is_empty() || bad.is_empty() {
                (
                    self.random_unique(ranges)?,
                    continuous_random_label(algorithm),
                )
            } else {
                let mut best: Option<(f64, BTreeMap<String, i128>)> = None;
                for _ in 0..128 {
                    let candidate = sample_from_good(&mut self.rng, ranges, &good, algorithm)?;
                    if self.seen.contains(&candidate) {
                        continue;
                    }
                    let score = ranges
                        .iter()
                        .map(|range| {
                            let value = candidate[&range.name];
                            let good_density = continuous_density(range, value, &good, algorithm);
                            let bad_density = continuous_density(range, value, &bad, algorithm);
                            (good_density + 1e-12).ln() - (bad_density + 1e-12).ln()
                        })
                        .sum::<f64>();
                    if best.as_ref().is_none_or(|(best_score, best_candidate)| {
                        score.total_cmp(best_score).is_gt()
                            || (score.total_cmp(best_score).is_eq() && candidate < *best_candidate)
                    }) {
                        best = Some((score, candidate));
                    }
                }
                match best {
                    Some((_, candidate)) => (candidate, continuous_adaptive_label(algorithm)),
                    None => (
                        self.random_unique(ranges)?,
                        continuous_random_label(algorithm),
                    ),
                }
            }
        } else {
            (
                self.random_unique(ranges)?,
                continuous_random_label(algorithm),
            )
        };
        if !self.seen.insert(proposal.clone()) {
            // The random fallback has a bounded exhaustive prefix, so duplicate proposals are
            // an invariant failure rather than an unbounded retry loop.
            return None;
        }
        Some((proposal, label))
    }

    fn random_unique(
        &mut self,
        ranges: &[ExperimentParameterRange],
    ) -> Option<BTreeMap<String, i128>> {
        for _ in 0..4096 {
            let candidate = sample_uniform(&mut self.rng, ranges)?;
            if !self.seen.contains(&candidate) {
                return Some(candidate);
            }
        }
        // Domains are bounded to at most 256 trials, so scanning a fixed prefix guarantees a
        // unique result without relying on a probabilistic retry loop, even for tiny domains.
        (0..1_000_000_u128)
            .take_while(|flat| *flat < self.domain_cardinality)
            .map(|flat| candidate_from_flat_index(ranges, flat))
            .find(|candidate| !self.seen.contains(candidate))
    }
}

fn continuous_good_trials(
    scored: &[(&ExperimentSearchTrial, i32, Option<u32>)],
    objective: ExperimentSearchObjective,
) -> BTreeSet<usize> {
    let mut ranked = scored.to_vec();
    if objective == ExperimentSearchObjective::ParetoReturnDrawdown
        && ranked.iter().all(|(_, _, drawdown)| drawdown.is_some())
    {
        let metrics = ranked
            .iter()
            .map(|(trial, ret, drawdown)| (trial.ordinal, (*ret, drawdown.unwrap_or_default())))
            .collect::<BTreeMap<_, _>>();
        let mut remaining = metrics.keys().copied().collect::<BTreeSet<_>>();
        let mut front_rank = BTreeMap::<usize, usize>::new();
        let mut rank = 0usize;
        while !remaining.is_empty() {
            let front = remaining
                .iter()
                .copied()
                .filter(|candidate| {
                    !remaining.iter().copied().any(|other| {
                        other != *candidate && pareto_dominates(metrics[&other], metrics[candidate])
                    })
                })
                .collect::<Vec<_>>();
            if front.is_empty() {
                break;
            }
            for ordinal in front {
                front_rank.insert(ordinal, rank);
                remaining.remove(&ordinal);
            }
            rank += 1;
        }
        ranked.sort_by(|(left, left_ret, left_dd), (right, right_ret, right_dd)| {
            front_rank[&left.ordinal]
                .cmp(&front_rank[&right.ordinal])
                .then_with(|| right_ret.cmp(left_ret))
                .then_with(|| left_dd.cmp(right_dd))
                .then_with(|| left.ordinal.cmp(&right.ordinal))
        });
    } else {
        ranked.sort_by(|(left, left_ret, _), (right, right_ret, _)| {
            right_ret
                .cmp(left_ret)
                .then_with(|| left.ordinal.cmp(&right.ordinal))
        });
    }
    let good_len = ranked
        .len()
        .div_ceil(5)
        .max(1)
        .min(ranked.len().saturating_sub(1));
    ranked
        .iter()
        .take(good_len)
        .map(|(trial, _, _)| trial.ordinal)
        .collect()
}

fn pareto_dominates(left: (i32, u32), right: (i32, u32)) -> bool {
    left.0 >= right.0 && left.1 <= right.1 && (left.0 > right.0 || left.1 < right.1)
}

fn continuous_random_label(algorithm: ExperimentSearchAlgorithm) -> &'static str {
    match algorithm {
        ExperimentSearchAlgorithm::Random => "random_continuous",
        ExperimentSearchAlgorithm::Tpe => "tpe_continuous_random_startup",
        ExperimentSearchAlgorithm::TpeKde => "tpe_kde_continuous_random_startup",
        ExperimentSearchAlgorithm::ParetoTpe => "pareto_tpe_continuous_random_startup",
    }
}

fn continuous_adaptive_label(algorithm: ExperimentSearchAlgorithm) -> &'static str {
    match algorithm {
        ExperimentSearchAlgorithm::Random => "random_continuous",
        ExperimentSearchAlgorithm::Tpe => "tpe_continuous",
        ExperimentSearchAlgorithm::TpeKde => "tpe_kde_continuous",
        ExperimentSearchAlgorithm::ParetoTpe => "pareto_tpe_continuous",
    }
}

fn sample_uniform(
    rng: &mut SplitMix64,
    ranges: &[ExperimentParameterRange],
) -> Option<BTreeMap<String, i128>> {
    ranges
        .iter()
        .map(|range| {
            let count = range.point_count()?;
            let index = rng.index_u64(count);
            Some((range.name.clone(), range.value_at(index)?))
        })
        .collect()
}

fn candidate_from_flat_index(
    ranges: &[ExperimentParameterRange],
    mut flat: u128,
) -> BTreeMap<String, i128> {
    let mut candidate = BTreeMap::new();
    for range in ranges.iter().rev() {
        let count = u128::from(range.point_count().expect("validated range"));
        let index = (flat % count) as u64;
        flat /= count;
        candidate.insert(
            range.name.clone(),
            range
                .value_at(index)
                .expect("ordinal is within validated range"),
        );
    }
    candidate
}

fn sample_from_good(
    rng: &mut SplitMix64,
    ranges: &[ExperimentParameterRange],
    good: &[&ExperimentSearchTrial],
    algorithm: ExperimentSearchAlgorithm,
) -> Option<BTreeMap<String, i128>> {
    let mut candidate = BTreeMap::new();
    for range in ranges {
        let count = range.point_count()?;
        let center_trial = good[rng.index(good.len())];
        let center_index = range.index_of(*center_trial.parameters.get(&range.name)?)?;
        let center = if count <= 1 {
            0.0
        } else {
            center_index as f64 / (count - 1) as f64
        };
        let centers = good
            .iter()
            .filter_map(|trial| range.index_of(*trial.parameters.get(&range.name)?))
            .map(|index| {
                if count <= 1 {
                    0.0
                } else {
                    index as f64 / (count - 1) as f64
                }
            })
            .collect::<Vec<_>>();
        let bandwidth = bandwidth(&centers, count, algorithm);
        let sampled = (center + gaussian(rng) * bandwidth).clamp(0.0, 1.0);
        let index = if count <= 1 {
            0
        } else {
            (sampled * (count - 1) as f64).round() as u64
        };
        candidate.insert(range.name.clone(), range.value_at(index)?);
    }
    Some(candidate)
}

fn continuous_density(
    range: &ExperimentParameterRange,
    value: i128,
    trials: &[&ExperimentSearchTrial],
    algorithm: ExperimentSearchAlgorithm,
) -> f64 {
    let Some(count) = range.point_count() else {
        return 0.0;
    };
    let Some(index) = range.index_of(value) else {
        return 0.0;
    };
    let position = if count <= 1 {
        0.0
    } else {
        index as f64 / (count - 1) as f64
    };
    let centers = trials
        .iter()
        .filter_map(|trial| range.index_of(*trial.parameters.get(&range.name)?))
        .map(|index| {
            if count <= 1 {
                0.0
            } else {
                index as f64 / (count - 1) as f64
            }
        })
        .collect::<Vec<_>>();
    let sigma = bandwidth(&centers, count, algorithm);
    let density = centers
        .iter()
        .map(|center| {
            gaussian_kernel(position, *center, sigma)
                + gaussian_kernel(position, -*center, sigma)
                + gaussian_kernel(position, 2.0 - *center, sigma)
        })
        .sum::<f64>()
        / centers.len().max(1) as f64;
    density + 1e-9
}

fn bandwidth(positions: &[f64], point_count: u64, algorithm: ExperimentSearchAlgorithm) -> f64 {
    if algorithm == ExperimentSearchAlgorithm::Tpe {
        return 0.15;
    }
    let mean = positions.iter().sum::<f64>() / positions.len().max(1) as f64;
    let variance = if positions.len() > 1 {
        positions
            .iter()
            .map(|value| (value - mean).powi(2))
            .sum::<f64>()
            / (positions.len() - 1) as f64
    } else {
        0.0
    };
    let min_bandwidth = if point_count <= 1 {
        0.5
    } else {
        (1.0 / (point_count - 1) as f64).max(0.05)
    };
    (1.06 * variance.sqrt() * (positions.len().max(1) as f64).powf(-0.2)).clamp(min_bandwidth, 0.5)
}

fn gaussian_kernel(value: f64, center: f64, sigma: f64) -> f64 {
    let normalized = (value - center) / sigma;
    (-0.5 * normalized * normalized).exp() / sigma
}

fn gaussian(rng: &mut SplitMix64) -> f64 {
    let u1 = (rng.next_f64()).max(f64::MIN_POSITIVE);
    let u2 = rng.next_f64();
    (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
}

fn propose_tpe(
    dimensions: &[ExperimentParameterSpace],
    candidates: &[BTreeMap<String, i128>],
    remaining: &[usize],
    observed: &[(usize, i32)],
    smooth_neighbors: bool,
) -> usize {
    let mut ranked = observed.to_vec();
    ranked.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    let good_len = ranked.len().div_ceil(5).max(1).min(ranked.len() - 1);
    let good: BTreeSet<usize> = ranked[..good_len].iter().map(|item| item.0).collect();
    let bad: BTreeSet<usize> = ranked[good_len..].iter().map(|item| item.0).collect();
    let parzen_densities = smooth_neighbors.then(|| {
        dimensions
            .iter()
            .map(|dimension| {
                (
                    dimension.name.clone(),
                    (
                        ordinal_parzen_densities(dimension, &good, candidates),
                        ordinal_parzen_densities(dimension, &bad, candidates),
                    ),
                )
            })
            .collect::<BTreeMap<_, _>>()
    });

    remaining
        .iter()
        .copied()
        .map(|index| {
            let candidate = &candidates[index];
            let score = dimensions
                .iter()
                .map(|dimension| {
                    if smooth_neighbors {
                        let (good_density, bad_density) = parzen_densities
                            .as_ref()
                            .and_then(|densities| densities.get(&dimension.name))
                            .expect("Parzen densities are built for KDE proposals");
                        let good_density = good_density[index];
                        let bad_density = bad_density[index];
                        return good_density.ln() - bad_density.ln();
                    }
                    let value = candidate[&dimension.name];
                    let good_count = good
                        .iter()
                        .filter(|seen| candidates[**seen][&dimension.name] == value)
                        .count();
                    let bad_count = bad
                        .iter()
                        .filter(|seen| candidates[**seen][&dimension.name] == value)
                        .count();
                    let categories = dimension.values.len() as f64;
                    ((good_count as f64 + 1.0) / (good.len() as f64 + categories)).ln()
                        - ((bad_count as f64 + 1.0) / (bad.len() as f64 + categories)).ln()
                })
                .sum::<f64>();
            (index, score)
        })
        .max_by(|left, right| {
            left.1
                .total_cmp(&right.1)
                // Prefer the lower canonical grid index on ties.
                .then_with(|| right.0.cmp(&left.0))
        })
        .map(|item| item.0)
        .expect("remaining candidates is non-empty")
}

/// Gaussian Parzen density over sorted candidate positions. Silverman's bandwidth is
/// estimated from the observed positions, clamped to half a grid step at the narrow end,
/// and each kernel is normalized over the finite domain to avoid edge bias. A uniform
/// pseudocount keeps every unobserved position explorable. Ordinal coordinates avoid i128
/// subtraction overflow; proposals remain restricted to the validated candidate grid.
fn ordinal_parzen_densities(
    dimension: &ExperimentParameterSpace,
    observed: &BTreeSet<usize>,
    candidates: &[BTreeMap<String, i128>],
) -> Vec<f64> {
    let mut ordered = dimension.values.clone();
    ordered.sort_unstable();
    let positions = observed
        .iter()
        .map(|index| {
            let seen = candidates[*index][&dimension.name];
            ordered
                .binary_search(&seen)
                .expect("observed value belongs to validated parameter grid") as f64
        })
        .collect::<Vec<_>>();
    let mean = positions.iter().sum::<f64>() / positions.len().max(1) as f64;
    let variance = if positions.len() > 1 {
        positions
            .iter()
            .map(|position| (position - mean).powi(2))
            .sum::<f64>()
            / (positions.len() - 1) as f64
    } else {
        0.0
    };
    let bandwidth = (1.06 * variance.sqrt() * (positions.len().max(1) as f64).powf(-0.2))
        .clamp(0.5, (ordered.len() as f64 / 2.0).max(0.5));
    let mut position_density = vec![0.0; ordered.len()];
    for center in positions {
        let weights = (0..ordered.len())
            .map(|position| {
                let normalized = (position as f64 - center) / bandwidth;
                (-0.5 * normalized * normalized).exp()
            })
            .collect::<Vec<_>>();
        let normalizer = weights.iter().sum::<f64>().max(f64::MIN_POSITIVE);
        for (density, weight) in position_density.iter_mut().zip(weights) {
            *density += weight / normalizer;
        }
    }
    let prior = 1.0 / ordered.len() as f64;
    let denominator = (observed.len() + 1) as f64;
    candidates
        .iter()
        .map(|candidate| {
            let position = ordered
                .binary_search(&candidate[&dimension.name])
                .expect("candidate value belongs to validated parameter grid");
            (position_density[position] + prior) / denominator
        })
        .collect()
}

fn propose_pareto_tpe(
    dimensions: &[ExperimentParameterSpace],
    candidates: &[BTreeMap<String, i128>],
    remaining: &[usize],
    observed_returns: &[(usize, i32)],
    observed_drawdowns: &[(usize, u32)],
) -> usize {
    let drawdowns = observed_drawdowns
        .iter()
        .copied()
        .collect::<BTreeMap<_, _>>();
    let mut ranked = observed_returns
        .iter()
        .filter_map(|(index, ret)| {
            drawdowns
                .get(index)
                .map(|drawdown| (*index, *ret, *drawdown))
        })
        .collect::<Vec<_>>();
    let mut unranked = ranked.iter().map(|item| item.0).collect::<BTreeSet<_>>();
    let metrics = ranked
        .iter()
        .map(|(index, ret, drawdown)| (*index, (*ret, *drawdown)))
        .collect::<BTreeMap<_, _>>();
    let mut front_rank = BTreeMap::<usize, usize>::new();
    let mut rank = 0usize;
    while !unranked.is_empty() {
        let frontier = unranked
            .iter()
            .copied()
            .filter(|candidate| {
                let (candidate_return, candidate_drawdown) = metrics[candidate];
                !unranked.iter().copied().any(|other| {
                    if other == *candidate {
                        return false;
                    }
                    let (other_return, other_drawdown) = metrics[&other];
                    other_return >= candidate_return
                        && other_drawdown <= candidate_drawdown
                        && (other_return > candidate_return || other_drawdown < candidate_drawdown)
                })
            })
            .collect::<Vec<_>>();
        if frontier.is_empty() {
            break;
        }
        for index in &frontier {
            front_rank.insert(*index, rank);
            unranked.remove(index);
        }
        rank += 1;
    }
    ranked.sort_by(|left, right| {
        front_rank[&left.0]
            .cmp(&front_rank[&right.0])
            .then_with(|| right.1.cmp(&left.1))
            .then_with(|| left.2.cmp(&right.2))
            .then_with(|| left.0.cmp(&right.0))
    });
    let good_len = ranked
        .len()
        .div_ceil(5)
        .max(1)
        .min(ranked.len().saturating_sub(1));
    let good = ranked[..good_len]
        .iter()
        .map(|item| item.0)
        .collect::<BTreeSet<_>>();
    let bad = ranked[good_len..]
        .iter()
        .map(|item| item.0)
        .collect::<BTreeSet<_>>();
    remaining
        .iter()
        .copied()
        .map(|index| {
            let candidate = &candidates[index];
            let score = dimensions
                .iter()
                .map(|dimension| {
                    let value = candidate[&dimension.name];
                    let good_count = good
                        .iter()
                        .filter(|seen| candidates[**seen][&dimension.name] == value)
                        .count();
                    let bad_count = bad
                        .iter()
                        .filter(|seen| candidates[**seen][&dimension.name] == value)
                        .count();
                    let categories = dimension.values.len() as f64;
                    ((good_count as f64 + 1.0) / (good.len() as f64 + categories)).ln()
                        - ((bad_count as f64 + 1.0) / (bad.len() as f64 + categories)).ln()
                })
                .sum::<f64>();
            (index, score)
        })
        .max_by(|left, right| {
            left.1
                .total_cmp(&right.1)
                .then_with(|| right.0.cmp(&left.0))
        })
        .map(|item| item.0)
        .expect("remaining candidates is non-empty")
}

#[derive(Clone, Debug)]
struct SplitMix64(u64);

impl SplitMix64 {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut value = self.0;
        value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        value ^ (value >> 31)
    }

    fn index(&mut self, len: usize) -> usize {
        (self.next() % len as u64) as usize
    }

    fn index_u64(&mut self, len: u64) -> u64 {
        self.next() % len
    }

    fn next_f64(&mut self) -> f64 {
        ((self.next() >> 11) as f64) / ((1_u64 << 53) as f64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn space() -> Vec<ExperimentParameterSpace> {
        vec![
            ExperimentParameterSpace {
                name: "fast_window".into(),
                values: vec![1, 2, 3],
            },
            ExperimentParameterSpace {
                name: "slow_window".into(),
                values: vec![4, 5, 6],
            },
        ]
    }

    fn candidates(dimensions: &[ExperimentParameterSpace]) -> Vec<BTreeMap<String, i128>> {
        let mut values = vec![BTreeMap::new()];
        for dimension in dimensions {
            let mut next = Vec::new();
            for current in values {
                for value in &dimension.values {
                    let mut candidate = current.clone();
                    candidate.insert(dimension.name.clone(), *value);
                    next.push(candidate);
                }
            }
            values = next;
        }
        values
    }

    #[test]
    fn random_proposals_are_seeded_and_never_repeat() {
        let dimensions = space();
        let candidates = candidates(&dimensions);
        let run = |seed| {
            let mut state = SearchState::new(candidates.len(), seed);
            (0..8)
                .map(|_| {
                    state
                        .next(
                            ExperimentSearchAlgorithm::Random,
                            5,
                            &dimensions,
                            &candidates,
                            &[],
                        )
                        .unwrap()
                        .0
                })
                .collect::<Vec<_>>()
        };
        let first = run(42);
        assert_eq!(first, run(42));
        assert_eq!(first.iter().copied().collect::<BTreeSet<_>>().len(), 8);
        assert_ne!(first, run(43));
    }

    #[test]
    fn tpe_uses_observed_training_scores_after_startup() {
        let dimensions = space();
        let candidates = candidates(&dimensions);
        let mut state = SearchState::new(candidates.len(), 7);
        // The fast_window=3 category is consistently associated with the top training trials.
        let good = candidates
            .iter()
            .enumerate()
            .filter(|(_, candidate)| candidate["fast_window"] == 3)
            .take(1)
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        let bad = candidates
            .iter()
            .enumerate()
            .filter(|(_, candidate)| candidate["fast_window"] != 3)
            .take(4)
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        let observed = good
            .iter()
            .map(|index| (*index, 100))
            .chain(bad.iter().map(|index| (*index, -10)))
            .collect::<Vec<_>>();
        for index in good.iter().chain(bad.iter()) {
            state.remaining.retain(|remaining| remaining != index);
        }
        let (next, proposal) = state
            .next(
                ExperimentSearchAlgorithm::Tpe,
                5,
                &dimensions,
                &candidates,
                &observed,
            )
            .unwrap();
        assert_eq!(proposal, "tpe");
        assert_eq!(candidates[next]["fast_window"], 3);
    }

    #[test]
    fn kde_tpe_prefers_an_unseen_neighbor_of_training_winner() {
        let dimensions = vec![ExperimentParameterSpace {
            name: "period".into(),
            values: vec![10, 20, 30, 40, 50],
        }];
        let candidates = candidates(&dimensions);
        let observed = [(2, 100), (0, -100)];
        let remaining = [1, 3, 4];
        // Categorical TPE ties on every unseen value. The Parzen estimate
        // favors 40 near the good observation at 30 and away from the bad 10.
        assert_eq!(
            propose_tpe(&dimensions, &candidates, &remaining, &observed, false),
            1
        );
        assert_eq!(
            propose_tpe(&dimensions, &candidates, &remaining, &observed, true),
            3
        );
        let mut state = SearchState::new(candidates.len(), 20261022);
        state.remaining = remaining.to_vec();
        assert_eq!(
            state.next(
                ExperimentSearchAlgorithm::TpeKde,
                2,
                &dimensions,
                &candidates,
                &observed,
            ),
            Some((3, "tpe_kde"))
        );
    }

    #[test]
    fn continuous_proposals_are_seeded_unique_and_learn_training_density() {
        let ranges = vec![ExperimentParameterRange {
            name: "period".into(),
            min: 1,
            max: 101,
            step: 1,
        }];
        let observed = vec![
            ExperimentSearchTrial {
                ordinal: 1,
                proposal: "random_continuous".into(),
                parameters: BTreeMap::from([("period".into(), 91)]),
                training_score_bps: Some(100),
                training_max_drawdown_bps: Some(100),
                training_robustness_score_bps: None,
            },
            ExperimentSearchTrial {
                ordinal: 2,
                proposal: "random_continuous".into(),
                parameters: BTreeMap::from([("period".into(), 11)]),
                training_score_bps: Some(-20),
                training_max_drawdown_bps: Some(500),
                training_robustness_score_bps: None,
            },
            ExperimentSearchTrial {
                ordinal: 3,
                proposal: "random_continuous".into(),
                parameters: BTreeMap::from([("period".into(), 21)]),
                training_score_bps: Some(-10),
                training_max_drawdown_bps: Some(400),
                training_robustness_score_bps: None,
            },
            ExperimentSearchTrial {
                ordinal: 4,
                proposal: "random_continuous".into(),
                parameters: BTreeMap::from([("period".into(), 31)]),
                training_score_bps: Some(0),
                training_max_drawdown_bps: Some(300),
                training_robustness_score_bps: None,
            },
        ];
        let run = |seed| {
            let mut state = ContinuousSearchState::new(&ranges, seed);
            state
                .seen
                .extend(observed.iter().map(|trial| trial.parameters.clone()));
            let mut history = observed.clone();
            let mut proposed = Vec::new();
            for _ in 0..12 {
                let (parameters, proposal) = state
                    .next(
                        ExperimentSearchAlgorithm::TpeKde,
                        ExperimentSearchObjective::Return,
                        4,
                        &ranges,
                        &history,
                    )
                    .unwrap();
                let value = parameters["period"];
                proposed.push((value, proposal));
                history.push(ExperimentSearchTrial {
                    ordinal: history.len() + 1,
                    proposal: proposal.into(),
                    parameters,
                    training_score_bps: Some(0),
                    training_max_drawdown_bps: Some(200),
                    training_robustness_score_bps: None,
                });
            }
            proposed
        };
        let first = run(42);
        assert_eq!(first, run(42));
        assert_ne!(first, run(43));
        assert!(first.iter().all(|(value, _)| (1..=101).contains(value)));
        assert_eq!(first[0].1, "tpe_kde_continuous");
        assert!(
            first[0].0 >= 70,
            "proposal should concentrate near the training winner: {first:?}"
        );
        assert_eq!(
            first
                .iter()
                .map(|(value, _)| *value)
                .collect::<BTreeSet<_>>()
                .len(),
            first.len()
        );
    }

    #[test]
    fn stepped_integer_neighborhood_reports_full_positive_coverage() {
        let ranges = vec![ExperimentParameterRange {
            name: "period".into(),
            min: 1,
            max: 9,
            step: 2,
        }];
        let trial = |ordinal, period, score| ExperimentSearchTrial {
            ordinal,
            proposal: "tpe_kde_continuous".into(),
            parameters: BTreeMap::from([("period".into(), period)]),
            training_score_bps: Some(score),
            training_max_drawdown_bps: Some(100),
            training_robustness_score_bps: None,
        };
        let report = continuous_neighborhood_report(
            &ranges,
            &[trial(1, 3, 20), trial(2, 5, 100), trial(3, 7, 30)],
        )
        .expect("range center has a valid first-order neighborhood");
        assert_eq!(report.center_parameters["period"], 5);
        assert_eq!(report.possible_neighbors, 2);
        assert_eq!(report.evaluated_neighbors, 2);
        assert_eq!(report.successful_neighbors, 2);
        assert_eq!(report.mean_training_score_bps, Some(25));
        assert_eq!(
            report.assessment,
            Some(ExperimentNeighborhoodAssessment::StablePositive)
        );
    }

    #[test]
    fn stepped_integer_neighborhood_center_matches_drawdown_eligible_selection() {
        let ranges = vec![ExperimentParameterRange {
            name: "period".into(),
            min: 1,
            max: 5,
            step: 2,
        }];
        let trial = |ordinal, period, score, drawdown| ExperimentSearchTrial {
            ordinal,
            proposal: "tpe_kde_continuous".into(),
            parameters: BTreeMap::from([("period".into(), period)]),
            training_score_bps: Some(score),
            training_max_drawdown_bps: Some(drawdown),
            training_robustness_score_bps: None,
        };

        let report = continuous_neighborhood_report_with_radius_and_budget(
            &ranges,
            &[
                trial(1, 1, 120, 220),
                trial(2, 3, 100, 100),
                trial(3, 5, 80, 90),
            ],
            1,
            Some(150),
        )
        .unwrap();

        assert_eq!(report.center_ordinal, 2);
        assert_eq!(report.center_parameters["period"], 3);
        assert_eq!(report.center_drawdown_bps, Some(100));
    }

    #[test]
    fn stepped_integer_neighborhood_expands_by_configured_radius() {
        let ranges = vec![ExperimentParameterRange {
            name: "period".into(),
            min: 1,
            max: 9,
            step: 2,
        }];
        let trial = |ordinal, period, score| ExperimentSearchTrial {
            ordinal,
            proposal: "tpe_kde_continuous".into(),
            parameters: BTreeMap::from([("period".into(), period)]),
            training_score_bps: Some(score),
            training_max_drawdown_bps: Some(100),
            training_robustness_score_bps: None,
        };
        let report = continuous_neighborhood_report_with_radius_and_budget(
            &ranges,
            &[
                trial(1, 1, 10),
                trial(2, 3, 20),
                trial(3, 5, 100),
                trial(4, 7, 30),
                trial(5, 9, 5),
            ],
            2,
            None,
        )
        .unwrap();
        assert_eq!(report.center_parameters["period"], 5);
        assert_eq!(report.radius, 2);
        assert_eq!(report.possible_neighbors, 4);
        assert_eq!(report.evaluated_neighbors, 4);
        assert_eq!(report.mean_training_score_bps, Some(16));
    }

    #[test]
    fn stepped_integer_neighborhood_distinguishes_incomplete_and_failed_neighbors() {
        let ranges = vec![ExperimentParameterRange {
            name: "period".into(),
            min: 1,
            max: 5,
            step: 1,
        }];
        let trial = |ordinal, period, score| ExperimentSearchTrial {
            ordinal,
            proposal: "tpe_kde_continuous".into(),
            parameters: BTreeMap::from([("period".into(), period)]),
            training_score_bps: score,
            training_max_drawdown_bps: score.map(|_| 100),
            training_robustness_score_bps: None,
        };
        let incomplete = continuous_neighborhood_report(
            &ranges,
            &[trial(1, 2, Some(100)), trial(2, 1, Some(10))],
        )
        .expect("training center is available");
        assert_eq!(incomplete.possible_neighbors, 2);
        assert_eq!(incomplete.evaluated_neighbors, 1);
        assert_eq!(
            incomplete.assessment,
            Some(ExperimentNeighborhoodAssessment::IncompleteNeighborhood)
        );

        let failed = continuous_neighborhood_report(
            &ranges,
            &[
                trial(1, 2, Some(100)),
                trial(2, 1, Some(10)),
                trial(3, 3, None),
            ],
        )
        .expect("training center is available");
        assert_eq!(failed.evaluated_neighbors, 2);
        assert_eq!(failed.successful_neighbors, 1);
        assert_eq!(failed.failed_neighbors, 1);
        assert_eq!(
            failed.assessment,
            Some(ExperimentNeighborhoodAssessment::FragilePositive)
        );
    }

    #[test]
    fn continuous_ranges_keep_large_i128_values_exact_and_reject_overflow_domains() {
        let range = ExperimentParameterRange {
            name: "quantity_raw".into(),
            min: i128::MAX - 1_000,
            max: i128::MAX,
            step: 1,
        };
        assert_eq!(range.point_count(), Some(1_001));
        assert_eq!(range.value_at(1_000), Some(i128::MAX));
        assert_eq!(range.index_of(i128::MAX), Some(1_000));
        assert_eq!(
            ExperimentParameterRange {
                name: "period".into(),
                min: 1,
                max: i128::MAX,
                step: 1,
            }
            .point_count(),
            None,
            "ordinal float coordinates must stay exactly representable"
        );
    }
}
