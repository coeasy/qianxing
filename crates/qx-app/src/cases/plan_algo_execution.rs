use super::{attach, guard::guard_panics};
use crate::context::{CallerCapability, RunContext};
use crate::error::{AppError, AppErrorCategory};
use crate::spec::{
    AlgoExecutionMethod, AlgoExecutionPlan, AlgoExecutionSpec, ALGO_EXECUTION_PLAN_SCHEMA_VERSION,
};

/// Shared Rust execution planner. It emits an immutable child schedule only;
/// submitting any child remains the normal Risk/OMS/Venue workflow's job.
pub fn plan_algo_execution(
    spec: &AlgoExecutionSpec,
    context: &RunContext,
) -> Result<AlgoExecutionPlan, AppError> {
    context.require(CallerCapability::Research, "算法拆单计划")?;
    let correlation_id = context.correlation_id().to_string();
    let result = guard_panics(&correlation_id, || {
        spec.validate()?;
        let children = match spec.method {
            AlgoExecutionMethod::Twap => qx_zhenlu::plan_twap(
                &spec.parent_id,
                spec.total_quantity_raw,
                spec.start_ts,
                spec.end_ts,
                spec.child_count.ok_or_else(|| {
                    AppError::new(
                        AppErrorCategory::InternalInvariant,
                        "validated TWAP spec is missing child_count",
                    )
                })?,
            ),
            AlgoExecutionMethod::Vwap => qx_zhenlu::plan_vwap(
                &spec.parent_id,
                spec.total_quantity_raw,
                spec.start_ts,
                spec.end_ts,
                &spec.volume_profile,
            ),
        }
        .map_err(|error| AppError::new(AppErrorCategory::InvalidInput, error.to_string()))?;
        Ok(AlgoExecutionPlan {
            schema_version: ALGO_EXECUTION_PLAN_SCHEMA_VERSION,
            method: spec.method,
            parent_id: spec.parent_id.clone(),
            children,
        })
    });
    result.map_err(|error| attach(error, &correlation_id))
}
