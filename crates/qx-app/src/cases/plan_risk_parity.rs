//! Portfolio risk-parity planning; the domain solver is owned by qx-zhenlu.

use super::{attach, guard::guard_panics};
use crate::context::{CallerCapability, RunContext};
use crate::error::{AppError, AppErrorCategory};
use crate::spec::{RiskParityResult, RiskParitySpec, RISK_PARITY_RESULT_SCHEMA_VERSION};
use qx_core::Fnv1a;
use qx_zhenlu::portfolio::{plan_risk_parity as solve_risk_parity, RiskParityError};

/// Build a deterministic, long-only equal-risk-contribution rebalancing plan.
///
/// This research use case only produces target allocations and deltas. Order
/// submission still has to go through the authenticated Paper/Live workflow.
pub fn plan_risk_parity(
    spec: &RiskParitySpec,
    context: &RunContext,
) -> Result<RiskParityResult, AppError> {
    context.require(CallerCapability::Research, "组合风险平价调仓规划")?;
    let correlation_id = context.correlation_id().to_string();
    let outcome = guard_panics(&correlation_id, || plan_risk_parity_inner(spec));
    outcome.map_err(|error| attach(error, &correlation_id))
}

fn plan_risk_parity_inner(spec: &RiskParitySpec) -> Result<RiskParityResult, AppError> {
    let (portfolio_id, input) = spec.clone().into_domain()?;
    let plan = solve_risk_parity(&input).map_err(|error| {
        let (category, message) = match error {
            RiskParityError::InsufficientHistory(message) => {
                (AppErrorCategory::FidelityInsufficient, message)
            }
            RiskParityError::SolverDidNotConverge => (
                AppErrorCategory::FidelityInsufficient,
                "risk parity solver did not converge within its bounded iteration budget".into(),
            ),
            RiskParityError::InvalidInput(message) | RiskParityError::Arithmetic(message) => {
                (AppErrorCategory::InvalidInput, message)
            }
        };
        AppError::new(category, message)
    })?;
    let canonical =
        serde_json::to_string(&(RISK_PARITY_RESULT_SCHEMA_VERSION, &portfolio_id, &plan)).map_err(
            |error| {
                AppError::new(
                    AppErrorCategory::InternalInvariant,
                    format!("RiskParityResult 指纹输入序列化失败: {error}"),
                )
            },
        )?;
    let mut hash = Fnv1a::new();
    hash.write_text(&canonical);
    Ok(RiskParityResult {
        schema_version: RISK_PARITY_RESULT_SCHEMA_VERSION,
        portfolio_id,
        plan_hash: format!("{:016x}", hash.finish()),
        plan,
    })
}
