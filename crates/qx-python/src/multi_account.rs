use crate::{app_error, NativeRun, PyRunHandle};
use pyo3::prelude::*;
use pyo3::types::PyModule;
use qx_app::{
    run_multi_account_backtest, run_multi_account_depth_backtest, CallerCapability,
    MultiAccountBacktestSpec, MultiAccountDepthBacktestSpec, RunContext,
};

#[pyfunction]
fn app_run_multi_account_backtest(spec_json: &str) -> PyResult<String> {
    let spec = MultiAccountBacktestSpec::from_json(spec_json).map_err(app_error)?;
    let context = RunContext::new(CallerCapability::Research, &spec.batch_id);
    run_multi_account_backtest(&spec, &context)
        .and_then(|result| result.to_json())
        .map_err(app_error)
}

#[pyfunction]
fn app_start_multi_account_backtest(spec_json: &str) -> PyResult<PyRunHandle> {
    let spec = MultiAccountBacktestSpec::from_json(spec_json).map_err(app_error)?;
    let context = RunContext::new(CallerCapability::Research, &spec.batch_id);
    Ok(PyRunHandle {
        inner: NativeRun::MultiAccount(spec.start(context)),
    })
}

#[pyfunction]
fn app_run_multi_account_depth_backtest(spec_json: &str) -> PyResult<String> {
    let spec = MultiAccountDepthBacktestSpec::from_json(spec_json).map_err(app_error)?;
    let context = RunContext::new(CallerCapability::Research, &spec.batch_id);
    run_multi_account_depth_backtest(&spec, &context)
        .and_then(|result| result.to_json())
        .map_err(app_error)
}

#[pyfunction]
fn app_start_multi_account_depth_backtest(spec_json: &str) -> PyResult<PyRunHandle> {
    let spec = MultiAccountDepthBacktestSpec::from_json(spec_json).map_err(app_error)?;
    let context = RunContext::new(CallerCapability::Research, &spec.batch_id);
    Ok(PyRunHandle {
        inner: NativeRun::MultiAccountDepth(spec.start(context)),
    })
}

pub(crate) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(app_run_multi_account_backtest, module)?)?;
    module.add_function(wrap_pyfunction!(app_start_multi_account_backtest, module)?)?;
    module.add_function(wrap_pyfunction!(
        app_run_multi_account_depth_backtest,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(
        app_start_multi_account_depth_backtest,
        module
    )?)
}
