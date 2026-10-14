//! Same-account multi-market L1/L2 application use case.

use super::{attach, guard::guard_panics};
use crate::{
    AppError, AppErrorCategory, CallerCapability, MultiMarketDepthBacktestOutcome,
    MultiMarketDepthBacktestSpec, RunContext, StrategyArtifactIdentity,
};
use qx_strategy::Strategy;
use std::collections::BTreeMap;

mod workflow;
use workflow::{run_inner, run_inner_with_strategies, InjectedStrategy};

pub(super) fn run_candidate(
    spec: &MultiMarketDepthBacktestSpec,
    context: &RunContext,
    cancellation: Option<&crate::run_handle::CancellationToken>,
    strategy: Option<(StrategyArtifactIdentity, Box<dyn Strategy>)>,
) -> Result<Option<MultiMarketDepthBacktestOutcome>, AppError> {
    let strategies = strategy.into_iter().collect();
    let injected = validate_injected(spec, strategies)?;
    run_inner_with_strategies(spec, context, cancellation, injected)
}

pub fn run_multi_market_depth_backtest(
    spec: &MultiMarketDepthBacktestSpec,
    context: &RunContext,
) -> Result<MultiMarketDepthBacktestOutcome, AppError> {
    run_multi_market_depth_backtest_with_strategy_slots(spec, Vec::new(), context)
}

pub(crate) fn run_multi_market_depth_backtest_with_strategy_slots(
    spec: &MultiMarketDepthBacktestSpec,
    strategies: Vec<(StrategyArtifactIdentity, Box<dyn Strategy>)>,
    context: &RunContext,
) -> Result<MultiMarketDepthBacktestOutcome, AppError> {
    context.require(CallerCapability::Research, "多市场共享账户 L1/L2 盘口回测")?;
    let correlation_id = context.correlation_id().to_string();
    guard_panics(&correlation_id, || {
        let injected = validate_injected(spec, strategies)?;
        run_inner_with_strategies(spec, context, None, injected)
    })
    .map_err(|error| attach(error, &correlation_id))?
    .ok_or_else(|| {
        attach(
            AppError::new(
                AppErrorCategory::InternalInvariant,
                "同步多市场盘口回测意外返回取消状态",
            ),
            &correlation_id,
        )
    })
}

impl MultiMarketDepthBacktestSpec {
    pub fn start(self, context: RunContext) -> crate::RunHandle<MultiMarketDepthBacktestOutcome> {
        let run_id = self.run_id.clone();
        crate::RunHandle::spawn(run_id, move |cancellation| {
            context.require(CallerCapability::Research, "多市场共享账户 L1/L2 盘口回测")?;
            let correlation_id = context.correlation_id().to_string();
            guard_panics(&correlation_id, || {
                run_inner(&self, &context, Some(&cancellation))
            })
            .map_err(|error| attach(error, &correlation_id))
        })
    }

    pub fn start_with_strategy_slots(
        self,
        strategies: Vec<(StrategyArtifactIdentity, Box<dyn Strategy>)>,
        context: RunContext,
    ) -> Result<crate::RunHandle<MultiMarketDepthBacktestOutcome>, AppError> {
        context.require(CallerCapability::Research, "多市场多语言策略 L1/L2 回测")?;
        self.validate()?;
        let injected = validate_injected(&self, strategies)?;
        let run_id = self.run_id.clone();
        Ok(crate::RunHandle::spawn(run_id, move |cancellation| {
            let correlation_id = context.correlation_id().to_string();
            guard_panics(&correlation_id, || {
                run_inner_with_strategies(&self, &context, Some(&cancellation), injected)
            })
            .map_err(|error| attach(error, &correlation_id))
        }))
    }
}

fn validate_injected(
    spec: &MultiMarketDepthBacktestSpec,
    strategies: Vec<(StrategyArtifactIdentity, Box<dyn Strategy>)>,
) -> Result<BTreeMap<String, InjectedStrategy>, AppError> {
    spec.validate()?;
    let mut injected = BTreeMap::new();
    for (identity, strategy) in strategies {
        let market = spec
            .markets
            .iter()
            .find(|market| market.strategy.strategy_id == identity.strategy_id)
            .ok_or_else(|| {
                invalid(format!(
                    "外部策略身份不属于多市场盘口策略槽: {}",
                    identity.strategy_id
                ))
            })?;
        identity.validate_for_slot(&market.strategy.strategy_id, &market.strategy)?;
        let id = identity.strategy_id.clone();
        if injected
            .insert(id.clone(), InjectedStrategy { identity, strategy })
            .is_some()
        {
            return Err(invalid(format!("多市场盘口自定义策略身份重复: {id}")));
        }
    }
    Ok(injected)
}

fn invalid(message: impl Into<String>) -> AppError {
    AppError::new(AppErrorCategory::InvalidInput, message)
}
