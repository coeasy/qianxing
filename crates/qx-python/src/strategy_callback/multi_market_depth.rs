//! Python callbacks bound to L2 slots in the same-account market portfolio.

use super::*;
use qx_app::MultiMarketDepthBacktestSpec;

#[pyfunction]
fn app_run_multi_market_depth_backtest_with_strategy_slots(
    spec_json: &str,
    identities_json: &str,
    callbacks: Vec<Py<PyAny>>,
) -> PyResult<String> {
    let spec = MultiMarketDepthBacktestSpec::from_json(spec_json).map_err(app_error)?;
    let strategies = strategy_slots(identities_json, callbacks)?;
    qx_app::run_multi_market_depth_backtest_with_strategy_slots(
        &spec,
        strategies,
        &research_context(&spec.run_id),
    )
    .and_then(|outcome| outcome.to_json())
    .map_err(app_error)
}

#[pyfunction]
fn app_start_multi_market_depth_backtest_with_strategy_slots(
    spec_json: &str,
    identities_json: &str,
    callbacks: Vec<Py<PyAny>>,
) -> PyResult<PyRunHandle> {
    let spec = MultiMarketDepthBacktestSpec::from_json(spec_json).map_err(app_error)?;
    let strategies = strategy_slots(identities_json, callbacks)?;
    let context = research_context(&spec.run_id);
    let handle = spec
        .start_with_strategy_slots(strategies, context)
        .map_err(app_error)?;
    Ok(PyRunHandle {
        inner: NativeRun::MultiMarketDepth(handle),
    })
}

pub(super) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(
        app_run_multi_market_depth_backtest_with_strategy_slots,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(
        app_start_multi_market_depth_backtest_with_strategy_slots,
        module
    )?)
}
