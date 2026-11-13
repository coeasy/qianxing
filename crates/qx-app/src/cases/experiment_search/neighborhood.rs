//! Discrete and continuous training-neighborhood coverage and drawdown assessment.

use super::*;

pub(in crate::cases) fn neighborhood_parameter_sets(
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

pub(in crate::cases) fn neighborhood_report_with_radius_and_budget(
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

pub(in crate::cases) fn continuous_neighborhood_report_with_radius_and_budget(
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
