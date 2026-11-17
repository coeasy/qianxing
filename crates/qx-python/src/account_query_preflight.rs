//! Python boundary for the shared Rust account-query capability decision.

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::PyModule;
use qx_app::{
    evaluate_account_query_capability_preflight_v1, AccountQueryCapabilityPreflightSpecV1,
    CallerCapability, RunContext,
};
use serde_json::Value;

const CAPABILITY_CATALOG: &str = include_str!("capability_catalog.generated.json");

#[pyfunction]
fn app_account_query_capability_preflight_v1(payload_json: &str) -> PyResult<String> {
    let spec: AccountQueryCapabilityPreflightSpecV1 =
        serde_json::from_str(payload_json).map_err(|error| {
            PyValueError::new_err(format!("Invalid account query preflight: {error}"))
        })?;
    let catalog: Value = serde_json::from_str(CAPABILITY_CATALOG).map_err(|error| {
        PyValueError::new_err(format!("Invalid embedded capability catalog: {error}"))
    })?;
    let context = RunContext::new(
        CallerCapability::Live,
        "python-account-query-capability-preflight",
    );
    let decision = evaluate_account_query_capability_preflight_v1(&spec, &catalog, &context)
        .map_err(|error| {
            PyValueError::new_err(format!("Account query preflight failed: {error}"))
        })?;
    serde_json::to_string(&decision)
        .map_err(|error| PyValueError::new_err(format!("Encode account query decision: {error}")))
}

pub(super) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(
        app_account_query_capability_preflight_v1,
        module
    )?)?;
    Ok(())
}
