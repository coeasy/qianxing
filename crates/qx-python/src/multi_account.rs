use crate::app_error;
use pyo3::prelude::*;
use pyo3::types::PyModule;
use qx_app::{run_multi_account_backtest, CallerCapability, MultiAccountBacktestSpec, RunContext};

#[pyfunction]
fn app_run_multi_account_backtest(spec_json: &str) -> PyResult<String> {
    let spec = MultiAccountBacktestSpec::from_json(spec_json).map_err(app_error)?;
    let context = RunContext::new(CallerCapability::Research, &spec.batch_id);
    run_multi_account_backtest(&spec, &context)
        .and_then(|result| result.to_json())
        .map_err(app_error)
}

pub(crate) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(app_run_multi_account_backtest, module)?)
}
