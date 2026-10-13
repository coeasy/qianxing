//! Caller-authored strategy entrypoints for the shared multi-leg Bar workflow.

use super::attach;
use super::guard::guard_panics;
use super::run_multi_leg_backtest::{custom_strategy, run_inner, InjectedMultiLegStrategy};
use crate::{
    AppError, AppErrorCategory, CallerCapability, MultiLegBacktestOutcome, MultiLegBacktestSpec,
    RunContext, StrategyArtifactIdentity,
};
use qx_strategy::Strategy;

/// Execute caller-authored decisions through both shared Rust Bar engines.
pub fn run_multi_leg_backtest_with_strategy(
    spec: &MultiLegBacktestSpec,
    identity: &StrategyArtifactIdentity,
    strategy: Box<dyn Strategy>,
    context: &RunContext,
) -> Result<MultiLegBacktestOutcome, AppError> {
    context.require(CallerCapability::Research, "多语言策略双腿 Bar 回测")?;
    spec.validate()?;
    custom_strategy::validate_identity(spec, identity)?;
    let correlation_id = context.correlation_id().to_string();
    guard_panics(&correlation_id, || {
        run_inner(
            spec,
            context,
            None,
            Some(InjectedMultiLegStrategy {
                identity: identity.clone(),
                strategy,
            }),
            None,
        )
    })
    .map_err(|error| attach(error, &correlation_id))?
    .ok_or_else(|| {
        attach(
            AppError::new(
                AppErrorCategory::InternalInvariant,
                "同步多语言策略双腿回测未产生报告",
            ),
            &correlation_id,
        )
    })
}

impl MultiLegBacktestSpec {
    /// Start a caller-authored strategy through the cancellable Rust multi-leg lifecycle.
    pub fn start_with_strategy<S: Strategy + 'static>(
        self,
        identity: StrategyArtifactIdentity,
        strategy: S,
        context: RunContext,
    ) -> Result<crate::RunHandle<MultiLegBacktestOutcome>, AppError> {
        start(self, identity, Box::new(strategy), context)
    }
}

/// Start a cancellable caller-authored strategy on the shared two-leg workflow.
fn start(
    spec: MultiLegBacktestSpec,
    identity: StrategyArtifactIdentity,
    strategy: Box<dyn Strategy>,
    context: RunContext,
) -> Result<crate::RunHandle<MultiLegBacktestOutcome>, AppError> {
    context.require(CallerCapability::Research, "多语言策略双腿 Bar 回测")?;
    spec.validate()?;
    custom_strategy::validate_identity(&spec, &identity)?;
    let run_id = context.correlation_id().to_string();
    Ok(crate::RunHandle::spawn(run_id, move |cancellation| {
        let correlation_id = context.correlation_id().to_string();
        guard_panics(&correlation_id, || {
            run_inner(
                &spec,
                &context,
                Some(&cancellation),
                Some(InjectedMultiLegStrategy { identity, strategy }),
                None,
            )
        })
        .map_err(|error| attach(error, &correlation_id))
    }))
}
