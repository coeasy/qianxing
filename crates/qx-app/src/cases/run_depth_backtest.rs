//! L1 Tick and L2 order-book application workflow.
//!
//! This use case only validates/assembles the request and persists evidence. The
//! Rust Tick, order-book matching, risk, OMS, ledger, and replay implementations
//! remain the single source of trading semantics.

use super::{attach, guard::guard_panics};
use crate::{
    AppError, AppErrorCategory, CallerCapability, DepthBacktestOutcome, DepthBacktestSpec,
    RunContext,
};
use qx_strategy::Strategy;

mod artifacts;
pub(crate) use artifacts::DEPTH_RUNTIME_VERSION;
mod canonical_spec;
mod custom_strategy;
mod portfolio;
pub(crate) mod quality;
use custom_strategy::CallbackStrategy;
pub(super) use custom_strategy::InjectedDepthStrategy;
use custom_strategy::{run_custom_strategies, start_custom_strategies};
mod workflow;
pub(super) use workflow::{run_inner, run_inner_with_strategy};

const DEFAULT_ACCOUNT_ID: &str = "app";

/// Execute a versioned L1/L2 run through the existing Rust engines and persist
/// the same four evidence artifacts used by Bar backtests.
pub fn run_depth_backtest(
    spec: &DepthBacktestSpec,
    context: &RunContext,
) -> Result<DepthBacktestOutcome, AppError> {
    context.require(CallerCapability::Research, "Tick/OrderBook 回测")?;
    let correlation_id = context.correlation_id().to_string();
    guard_panics(&correlation_id, || run_inner(spec, context, None))
        .map_err(|error| attach(error, &correlation_id))?
        .ok_or_else(|| {
            attach(
                AppError::new(
                    AppErrorCategory::InternalInvariant,
                    "同步 Tick/OrderBook 回测未产生报告",
                ),
                &correlation_id,
            )
        })
}

/// Run caller-authored decisions through the shared Rust Tick/OrderBook engines.
pub fn run_depth_backtest_with_strategy(
    spec: &DepthBacktestSpec,
    identity: &super::StrategyArtifactIdentity,
    strategy: Box<dyn Strategy>,
    context: &RunContext,
) -> Result<DepthBacktestOutcome, AppError> {
    run_depth_backtest_with_strategy_slots(spec, vec![(identity.clone(), strategy)], context)
}

/// Run caller-authored strategies in declared same-account Tick/OrderBook portfolio slots.
pub fn run_depth_backtest_with_strategy_slots(
    spec: &DepthBacktestSpec,
    strategies: Vec<(super::StrategyArtifactIdentity, Box<dyn Strategy>)>,
    context: &RunContext,
) -> Result<DepthBacktestOutcome, AppError> {
    run_custom_strategies(spec, strategies, context)
}

pub(super) fn run_inner_with_experiment_strategy(
    spec: &DepthBacktestSpec,
    context: &RunContext,
    cancellation: Option<&crate::run_handle::CancellationToken>,
    identity: super::StrategyArtifactIdentity,
    strategy: Box<dyn Strategy>,
) -> Result<Option<DepthBacktestOutcome>, AppError> {
    run_inner_with_strategy(
        spec,
        context,
        cancellation,
        vec![InjectedDepthStrategy {
            identity,
            strategy,
            experiment_parameters: true,
        }],
    )
}

/// Start a cancellable custom strategy on the shared Tick/OrderBook lifecycle.
pub fn start_depth_backtest_with_strategy(
    spec: DepthBacktestSpec,
    identity: super::StrategyArtifactIdentity,
    strategy: Box<dyn Strategy>,
    context: RunContext,
) -> Result<crate::RunHandle<DepthBacktestOutcome>, AppError> {
    start_depth_backtest_with_strategy_slots(spec, vec![(identity, strategy)], context)
}

/// Start a cancellable Tick/OrderBook run with multiple caller-authored strategy slots.
pub fn start_depth_backtest_with_strategy_slots(
    spec: DepthBacktestSpec,
    strategies: Vec<(super::StrategyArtifactIdentity, Box<dyn Strategy>)>,
    context: RunContext,
) -> Result<crate::RunHandle<DepthBacktestOutcome>, AppError> {
    start_custom_strategies(spec, strategies, context)
}

impl DepthBacktestSpec {
    /// Start a cooperative worker for this Tick/OrderBook specification.
    pub fn start(self, context: RunContext) -> crate::RunHandle<DepthBacktestOutcome> {
        let run_id = context.correlation_id().to_string();
        crate::RunHandle::spawn(run_id, move |cancellation| {
            context.require(CallerCapability::Research, "Tick/OrderBook 回测")?;
            let correlation_id = context.correlation_id().to_string();
            guard_panics(&correlation_id, || {
                run_inner(&self, &context, Some(&cancellation))
            })
            .map_err(|error| attach(error, &correlation_id))
        })
    }
}

pub(super) fn invalid_input(message: impl std::fmt::Display) -> AppError {
    AppError::new(
        AppErrorCategory::InvalidInput,
        format!("Tick/OrderBook 输入或策略非法: {message}"),
    )
}
