use super::{attach, guard::guard_panics};
use crate::{
    context::{CallerCapability, RunContext},
    error::{AppError, AppErrorCategory},
    spec::{
        CapitalAllocationResult, CapitalAllocationSpec, CAPITAL_ALLOCATION_SPEC_SCHEMA_VERSION,
    },
};

pub fn plan_capital_allocation(
    spec: &CapitalAllocationSpec,
    context: &RunContext,
) -> Result<CapitalAllocationResult, AppError> {
    context.require(CallerCapability::Research, "组合资金确定性分配")?;
    let correlation_id = context.correlation_id().to_string();
    guard_panics(&correlation_id, || {
        spec.validate()?;
        let input = qx_zhenlu::portfolio::CapitalAllocationInput {
            portfolio_id: spec.portfolio_id.clone(),
            portfolio_budget_raw: spec.portfolio_budget_raw,
            accounts: spec.accounts.clone(),
            requests: spec.requests.clone(),
        };
        let allocations = qx_zhenlu::portfolio::allocate_portfolio_capital(&input).map_err(
            |error| match error {
                qx_zhenlu::portfolio::CapitalAllocationError::InvalidInput(message) => {
                    AppError::new(AppErrorCategory::InvalidInput, message)
                }
                qx_zhenlu::portfolio::CapitalAllocationError::Arithmetic(message) => {
                    AppError::new(AppErrorCategory::InternalInvariant, message)
                }
            },
        )?;
        Ok(CapitalAllocationResult {
            schema_version: CAPITAL_ALLOCATION_SPEC_SCHEMA_VERSION,
            portfolio_id: spec.portfolio_id.clone(),
            allocations,
        })
    })
    .map_err(|error| attach(error, &correlation_id))
}
