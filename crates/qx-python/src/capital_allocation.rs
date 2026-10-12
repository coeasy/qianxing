use pyo3::prelude::*;
use pyo3::types::PyModule;
use qx_app::{plan_capital_allocation, AppError, CapitalAllocationSpec};

fn app_error(error: AppError) -> PyErr {
    crate::QxAppError::new_err(error.to_json())
}

#[pyfunction]
fn app_plan_capital_allocation(spec_json: &str) -> PyResult<String> {
    let spec = CapitalAllocationSpec::from_json(spec_json).map_err(app_error)?;
    plan_capital_allocation(&spec, &crate::research_context(&spec.portfolio_id))
        .and_then(|result| result.to_json())
        .map_err(app_error)
}

pub(crate) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(app_plan_capital_allocation, module)?)
}
