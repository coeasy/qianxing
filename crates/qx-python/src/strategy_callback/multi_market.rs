//! Python callbacks bound to N-market strategy slots.

use super::*;
use qx_app::MultiMarketBacktestSpec;

#[pyfunction]
fn app_run_multi_market_backtest_with_strategy_slots(
    spec_json: &str,
    identities_json: &str,
    callbacks: Vec<Py<PyAny>>,
) -> PyResult<String> {
    let spec = MultiMarketBacktestSpec::from_json(spec_json).map_err(app_error)?;
    let strategies = strategy_slots(identities_json, callbacks)?;
    spec.run_with_strategy_slots(strategies, &research_context(&spec.run_id))
        .and_then(|outcome| outcome.to_json())
        .map_err(app_error)
}

#[pyfunction]
fn app_start_multi_market_backtest_with_strategy_slots(
    spec_json: &str,
    identities_json: &str,
    callbacks: Vec<Py<PyAny>>,
) -> PyResult<PyRunHandle> {
    let spec = MultiMarketBacktestSpec::from_json(spec_json).map_err(app_error)?;
    let context = research_context(&spec.run_id);
    let strategies = strategy_slots(identities_json, callbacks)?;
    let handle = spec
        .start_with_strategy_slots(strategies, context)
        .map_err(app_error)?;
    Ok(PyRunHandle {
        inner: NativeRun::MultiMarket(handle),
    })
}

pub(super) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(
        app_run_multi_market_backtest_with_strategy_slots,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(
        app_start_multi_market_backtest_with_strategy_slots,
        module
    )?)
}
