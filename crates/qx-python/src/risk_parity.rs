use pyo3::prelude::*;
use pyo3::types::PyModule;
use qx_app::{plan_risk_parity, AppError, RiskParitySpec};

fn app_error(error: AppError) -> PyErr {
    crate::QxAppError::new_err(error.to_json())
}

#[pyfunction]
fn app_plan_risk_parity(spec_json: &str) -> PyResult<String> {
    let spec = RiskParitySpec::from_json(spec_json).map_err(app_error)?;
    plan_risk_parity(&spec, &crate::research_context(&spec.portfolio_id))
        .and_then(|result| result.to_json())
        .map_err(app_error)
}

pub(crate) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(app_plan_risk_parity, module)?)
}
