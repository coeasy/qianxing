use super::super::experiment_grid::apply_parameters;
use super::search::CandidatePlan;
use super::{invariant, AppError, MultiMarketBacktestSpec, RunMultiMarketExperimentSpec};

pub(super) fn build_candidate_spec(
    experiment: &RunMultiMarketExperimentSpec,
    plan: &CandidatePlan,
    root: &std::path::Path,
    fold: Option<usize>,
    phase: &str,
    callback_parameter_search: bool,
) -> Result<(MultiMarketBacktestSpec, Option<AppError>), AppError> {
    let mut candidate = experiment.base.clone();
    candidate.run_id = match fold {
        Some(fold) => format!(
            "{}-f{fold:04}-{phase}-{:04}",
            experiment.experiment_id, plan.ordinal
        ),
        None => format!("{}-{:04}", experiment.experiment_id, plan.ordinal),
    };
    candidate.output_dir = match fold {
        Some(fold) => root
            .join(format!("fold-{fold:04}"))
            .join(format!("{phase}-{:04}", plan.ordinal))
            .to_string_lossy()
            .into_owned(),
        None => root
            .join(format!("run-{:04}", plan.ordinal))
            .to_string_lossy()
            .into_owned(),
    };
    let target = candidate
        .markets
        .iter_mut()
        .flat_map(|market| &mut market.strategies)
        .find(|slot| slot.strategy.strategy_id == experiment.target_strategy_id)
        .ok_or_else(|| invariant("validated target strategy slot disappeared"))?;
    target.strategy.kind = plan.kind;
    if callback_parameter_search {
        return Ok((candidate, None));
    }
    let error = apply_parameters(&mut target.strategy, &plan.parameters).err();
    Ok((candidate, error))
}
