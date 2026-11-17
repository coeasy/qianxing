//! Continuous stepped-grid proposals and adaptive Parzen density search.

use super::rng::SplitMix64;
use super::*;
use crate::{AppError, AppErrorCategory};

#[derive(Clone, Debug)]
pub(in crate::cases) struct ContinuousSearchState {
    rng: SplitMix64,
    pub(super) seen: BTreeSet<BTreeMap<String, i128>>,
    domain_cardinality: u128,
}

impl ContinuousSearchState {
    pub(in crate::cases) fn new(
        ranges: &[ExperimentParameterRange],
        seed: u64,
    ) -> Result<Self, AppError> {
        Ok(Self {
            rng: SplitMix64::new(seed),
            seen: BTreeSet::new(),
            domain_cardinality: continuous_domain_cardinality(ranges).ok_or_else(|| {
                AppError::new(
                    AppErrorCategory::InvalidInput,
                    "连续参数值域为空或总候选数量溢出",
                )
            })?,
        })
    }

    pub(in crate::cases) fn next(
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
            .filter_map(|flat| candidate_from_flat_index(ranges, flat))
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
) -> Option<BTreeMap<String, i128>> {
    let mut candidate = BTreeMap::new();
    for range in ranges.iter().rev() {
        let count = u128::from(range.point_count()?);
        let index = (flat % count) as u64;
        flat /= count;
        candidate.insert(range.name.clone(), range.value_at(index)?);
    }
    Some(candidate)
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

pub(super) fn propose_tpe(
    dimensions: &[ExperimentParameterSpace],
    candidates: &[BTreeMap<String, i128>],
    remaining: &[usize],
    observed: &[(usize, i32)],
    smooth_neighbors: bool,
) -> Option<usize> {
    let mut ranked = observed.to_vec();
    ranked.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    let good_len = ranked.len().div_ceil(5).max(1).min(ranked.len() - 1);
    let good: BTreeSet<usize> = ranked[..good_len].iter().map(|item| item.0).collect();
    let bad: BTreeSet<usize> = ranked[good_len..].iter().map(|item| item.0).collect();
    let parzen_densities = if smooth_neighbors {
        Some(
            dimensions
                .iter()
                .map(|dimension| {
                    Some((
                        dimension.name.clone(),
                        (
                            ordinal_parzen_densities(dimension, &good, candidates)?,
                            ordinal_parzen_densities(dimension, &bad, candidates)?,
                        ),
                    ))
                })
                .collect::<Option<BTreeMap<_, _>>>()?,
        )
    } else {
        None
    };

    remaining
        .iter()
        .copied()
        .filter_map(|index| {
            let candidate = candidates.get(index)?;
            let score = dimensions.iter().try_fold(0.0, |score, dimension| {
                let contribution = if smooth_neighbors {
                    let (good_density, bad_density) =
                        parzen_densities.as_ref()?.get(&dimension.name)?;
                    let good_density = *good_density.get(index)?;
                    let bad_density = *bad_density.get(index)?;
                    good_density.ln() - bad_density.ln()
                } else {
                    let value = *candidate.get(&dimension.name)?;
                    let good_count = good
                        .iter()
                        .filter(|seen| {
                            candidates
                                .get(**seen)
                                .and_then(|candidate| candidate.get(&dimension.name))
                                == Some(&value)
                        })
                        .count();
                    let bad_count = bad
                        .iter()
                        .filter(|seen| {
                            candidates
                                .get(**seen)
                                .and_then(|candidate| candidate.get(&dimension.name))
                                == Some(&value)
                        })
                        .count();
                    let categories = dimension.values.len() as f64;
                    ((good_count as f64 + 1.0) / (good.len() as f64 + categories)).ln()
                        - ((bad_count as f64 + 1.0) / (bad.len() as f64 + categories)).ln()
                };
                Some(score + contribution)
            })?;
            Some((index, score))
        })
        .max_by(|left, right| {
            left.1
                .total_cmp(&right.1)
                // Prefer the lower canonical grid index on ties.
                .then_with(|| right.0.cmp(&left.0))
        })
        .map(|item| item.0)
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
) -> Option<Vec<f64>> {
    let mut ordered = dimension.values.clone();
    ordered.sort_unstable();
    let positions = observed
        .iter()
        .map(|index| {
            let seen = candidates.get(*index)?.get(&dimension.name)?;
            Some(ordered.binary_search(seen).ok()? as f64)
        })
        .collect::<Option<Vec<_>>>()?;
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
            let value = candidate.get(&dimension.name)?;
            let position = ordered.binary_search(value).ok()?;
            Some((*position_density.get(position)? + prior) / denominator)
        })
        .collect()
}

pub(super) fn propose_pareto_tpe(
    dimensions: &[ExperimentParameterSpace],
    candidates: &[BTreeMap<String, i128>],
    remaining: &[usize],
    observed_returns: &[(usize, i32)],
    observed_drawdowns: &[(usize, u32)],
) -> Option<usize> {
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
}
