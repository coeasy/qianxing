//! Shared preparation for guided, held-out strategy research.
//!
//! The guided preset reuses the existing versioned experiment schemas and result types. This
//! boundary enforces its extra promise: a held-out split is required, and single-market runs
//! include an effectiveness baseline by default.

use super::run_experiment::{RunExperimentSpec, EXPERIMENT_BASELINE_NAMES};
use super::run_multi_market_experiment::RunMultiMarketExperimentSpec;
use crate::{AppError, AppErrorCategory};
use serde_json::Value;

const DEFAULT_SINGLE_MARKET_BASELINE: &str = "buy-and-hold";
const EFFECTIVENESS_BASELINES: &[&str] = &["buy-and-hold", "zero-signal"];

/// Validate and canonicalize a guided research request using the owning experiment schemas.
///
/// Multi-market experiments keep their strict schema and currently have no cross-market
/// baseline. They still require held-out folds, but their ranking is not benchmark-relative
/// effectiveness evidence.
pub fn prepare_quick_research_spec_v1(payload: &str) -> Result<String, AppError> {
    let value: Value = serde_json::from_str(payload)
        .map_err(|error| invalid(format!("Quick Research JSON is invalid: {error}")))?;
    let is_multi_market = value
        .get("base")
        .and_then(Value::as_object)
        .is_some_and(|base| base.contains_key("markets"));

    if is_multi_market {
        let spec = RunMultiMarketExperimentSpec::from_json(payload)?;
        if spec.split_plan.is_none() {
            return Err(invalid(
                "Quick Research requires an explicit split_plan with held-out test Bars",
            ));
        }
        return spec.to_json();
    }

    let mut spec = RunExperimentSpec::from_json(payload)?;
    if spec.split_plan.is_none() {
        return Err(invalid(
            "Quick Research requires an explicit split_plan with held-out test Bars",
        ));
    }
    if spec.baselines.is_empty() {
        spec.baselines
            .push(DEFAULT_SINGLE_MARKET_BASELINE.to_string());
    }
    if !spec
        .baselines
        .iter()
        .any(|baseline| EFFECTIVENESS_BASELINES.contains(&baseline.as_str()))
    {
        return Err(invalid(format!(
            "Quick Research requires an effectiveness baseline: {}",
            EFFECTIVENESS_BASELINES.join(" or ")
        )));
    }
    spec.validate()?;
    if spec
        .baselines
        .iter()
        .any(|baseline| !EXPERIMENT_BASELINE_NAMES.contains(&baseline.as_str()))
    {
        return Err(invalid("Quick Research contains an unsupported baseline"));
    }
    spec.to_json()
}

fn invalid(message: impl Into<String>) -> AppError {
    AppError::new(AppErrorCategory::InvalidInput, message)
}
