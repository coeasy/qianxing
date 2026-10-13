//! Batch independent Tick/OrderBook accounts through the shared Rust depth kernel.

use super::{attach, guard::guard_panics};
use crate::{
    AppError, AppErrorCategory, CallerCapability, MultiAccountDepthBacktestResult,
    MultiAccountDepthBacktestSpec, RunContext,
};

/// Run independent L1 Tick or L2 OrderBook accounts in stable account-id order.
pub fn run_multi_account_depth_backtest(
    spec: &MultiAccountDepthBacktestSpec,
    context: &RunContext,
) -> Result<MultiAccountDepthBacktestResult, AppError> {
    context.require(CallerCapability::Research, "多账户 Tick/OrderBook 回测")?;
    let correlation_id = context.correlation_id().to_string();
    guard_panics(&correlation_id, || {
        super::run_multi_account_backtest::run_depth_batch_inner(spec, context, None)
    })
    .map_err(|error| attach(error, &correlation_id))?
    .ok_or_else(|| {
        attach(
            AppError::new(
                AppErrorCategory::InternalInvariant,
                "同步多账户深度回测意外返回取消状态",
            ),
            &correlation_id,
        )
    })
}

impl MultiAccountDepthBacktestSpec {
    /// Start a cancellable account batch; cancellation reaches the active depth kernel.
    pub fn start(self, context: RunContext) -> crate::RunHandle<MultiAccountDepthBacktestResult> {
        let run_id = context.correlation_id().to_string();
        crate::RunHandle::spawn(run_id, move |cancellation| {
            context.require(CallerCapability::Research, "多账户 Tick/OrderBook 回测")?;
            let correlation_id = context.correlation_id().to_string();
            guard_panics(&correlation_id, || {
                super::run_multi_account_backtest::run_depth_batch_inner(
                    &self,
                    &context,
                    Some(&cancellation),
                )
            })
            .map_err(|error| attach(error, &correlation_id))
        })
    }
}
