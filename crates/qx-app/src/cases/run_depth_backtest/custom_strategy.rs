//! Caller-authored strategy entrypoints for the shared Tick and OrderBook engines.

use super::run_inner_with_strategy;
use crate::cases::{attach, guard::guard_panics};
use crate::{
    AppError, AppErrorCategory, CallerCapability, DepthBacktestOutcome, DepthBacktestSpec,
    RunContext, StrategyArtifactIdentity,
};
use qx_strategy::{Strategy, StrategyContext};

pub(in crate::cases) struct InjectedDepthStrategy {
    pub(super) identity: StrategyArtifactIdentity,
    pub(super) strategy: Box<dyn Strategy>,
}

pub(super) struct CallbackStrategy(pub(super) Box<dyn Strategy>);

impl Strategy for CallbackStrategy {
    fn on_init(&mut self, context: &StrategyContext) -> Result<(), String> {
        self.0.on_init(context)
    }

    fn on_event(
        &mut self,
        context: &StrategyContext,
        event: &qx_strategy::MarketEvent,
    ) -> Result<qx_strategy::StrategyDecision, String> {
        self.0.on_event(context, event)
    }

    fn on_order_update(
        &mut self,
        context: &StrategyContext,
        update: &str,
    ) -> Result<Option<qx_strategy::StrategyDecision>, String> {
        self.0.on_order_update(context, update)
    }

    fn on_stop(&mut self) -> Result<(), String> {
        self.0.on_stop()
    }
}

/// Execute caller-authored decisions through the shared Rust Tick/OrderBook risk and matching path.
pub(super) fn run_custom_strategy(
    spec: &DepthBacktestSpec,
    identity: &StrategyArtifactIdentity,
    strategy: Box<dyn Strategy>,
    context: &RunContext,
) -> Result<DepthBacktestOutcome, AppError> {
    context.require(CallerCapability::Research, "多语言策略 Tick/OrderBook 回测")?;
    let correlation_id = context.correlation_id().to_string();
    guard_panics(&correlation_id, || {
        run_inner_with_strategy(
            spec,
            context,
            None,
            Some(InjectedDepthStrategy {
                identity: identity.clone(),
                strategy,
            }),
        )
    })
    .map_err(|error| attach(error, &correlation_id))?
    .ok_or_else(|| {
        attach(
            AppError::new(
                AppErrorCategory::InternalInvariant,
                "同步多语言策略深度回测未产生报告",
            ),
            &correlation_id,
        )
    })
}

/// Start a cancellable custom strategy on the shared Tick/OrderBook run lifecycle.
pub(super) fn start_custom_strategy(
    spec: DepthBacktestSpec,
    identity: StrategyArtifactIdentity,
    strategy: Box<dyn Strategy>,
    context: RunContext,
) -> Result<crate::RunHandle<DepthBacktestOutcome>, AppError> {
    context.require(CallerCapability::Research, "多语言策略 Tick/OrderBook 回测")?;
    spec.validate()?;
    identity.validate_for_depth(&spec)?;
    let run_id = context.correlation_id().to_string();
    Ok(crate::RunHandle::spawn(run_id, move |cancellation| {
        let correlation_id = context.correlation_id().to_string();
        guard_panics(&correlation_id, || {
            run_inner_with_strategy(
                &spec,
                &context,
                Some(&cancellation),
                Some(InjectedDepthStrategy { identity, strategy }),
            )
        })
        .map_err(|error| attach(error, &correlation_id))
    }))
}
