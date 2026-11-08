use super::super::run_multi_market_backtest::{validate_injected, InjectedMarketStrategy};
use super::{
    invalid, invariant, AppError, MultiMarketBacktestSpec, RunMultiMarketExperimentSpec, Strategy,
    StrategyArtifactIdentity,
};
use std::collections::BTreeMap;

pub(super) type StrategyFactory<'a> = dyn FnMut(&MultiMarketBacktestSpec, &BTreeMap<String, i128>) -> Result<Box<dyn Strategy>, String>
    + 'a;

pub(super) fn bind_injected_strategy(
    experiment: &RunMultiMarketExperimentSpec,
    candidate: &MultiMarketBacktestSpec,
    parameters: &BTreeMap<String, i128>,
    identity: Option<&StrategyArtifactIdentity>,
    factory: Option<&mut StrategyFactory<'_>>,
) -> Result<BTreeMap<String, InjectedMarketStrategy>, AppError> {
    match (identity, factory) {
        (Some(identity), Some(factory)) => {
            let strategy = factory(candidate, parameters).map_err(invalid)?;
            // Bind custom strategy identity against the original built-in slot contract.
            let mut binding_spec = candidate.clone();
            let binding_target = binding_spec
                .markets
                .iter_mut()
                .flat_map(|market| &mut market.strategies)
                .find(|slot| slot.strategy.strategy_id == experiment.target_strategy_id)
                .ok_or_else(|| invariant("validated target strategy slot disappeared"))?;
            let base_target = experiment
                .base
                .markets
                .iter()
                .flat_map(|market| &market.strategies)
                .find(|slot| slot.strategy.strategy_id == experiment.target_strategy_id)
                .ok_or_else(|| invariant("validated target strategy slot disappeared"))?;
            binding_target.strategy = crate::BuiltinStrategySpec::new(
                base_target.strategy.kind,
                base_target.strategy.strategy_id.clone(),
            );
            validate_injected(&binding_spec, vec![(identity.clone(), strategy)])
        }
        (None, None) => Ok(BTreeMap::new()),
        _ => Err(invariant("多市场实验策略工厂和身份必须同时提供")),
    }
}
