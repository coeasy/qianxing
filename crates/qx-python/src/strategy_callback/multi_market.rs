//! Python callbacks bound to N-market strategy slots.

use super::*;
use qx_app::MultiMarketBacktestSpec;

fn strategy_factory(
    factory: Py<PyAny>,
) -> impl FnMut(&MultiMarketBacktestSpec) -> Result<Box<dyn Strategy>, String> + Send {
    move |candidate| {
        let candidate_json = candidate.to_json().map_err(|error| error.to_string())?;
        let callback = Python::attach(|py| {
            factory
                .bind(py)
                .call1((candidate_json,))
                .map(|value| value.unbind())
                .map_err(|error| format!("Python multi-market strategy factory failed: {error}"))
        })?;
        Ok(Box::new(super::PythonBacktestStrategy { callback }) as Box<dyn Strategy>)
    }
}

#[pyfunction]
fn app_run_multi_market_experiment_with_strategy(
    spec_json: &str,
    strategy_id: &str,
    strategy_version: &str,
    source_sha256: &str,
    factory: Py<PyAny>,
) -> PyResult<String> {
    let spec = RunMultiMarketExperimentSpec::from_json(spec_json).map_err(app_error)?;
    let identity = StrategyArtifactIdentity {
        strategy_id: strategy_id.to_string(),
        strategy_version: strategy_version.to_string(),
        source_sha256: source_sha256.to_string(),
    };
    spec.run_with_strategy_factory(
        &identity,
        &mut strategy_factory(factory),
        &research_context(&spec.experiment_id),
    )
    .and_then(|result| result.to_json())
    .map_err(app_error)
}

#[pyfunction]
fn app_start_multi_market_experiment_with_strategy(
    spec_json: &str,
    strategy_id: &str,
    strategy_version: &str,
    source_sha256: &str,
    factory: Py<PyAny>,
) -> PyResult<PyRunHandle> {
    let spec = RunMultiMarketExperimentSpec::from_json(spec_json).map_err(app_error)?;
    let identity = StrategyArtifactIdentity {
        strategy_id: strategy_id.to_string(),
        strategy_version: strategy_version.to_string(),
        source_sha256: source_sha256.to_string(),
    };
    let context = research_context(&spec.experiment_id);
    let handle = spec
        .start_with_strategy_factory(identity, strategy_factory(factory), context)
        .map_err(app_error)?;
    Ok(PyRunHandle {
        inner: NativeRun::MultiMarketExperiment(handle),
    })
}

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
        app_run_multi_market_experiment_with_strategy,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(
        app_start_multi_market_experiment_with_strategy,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(
        app_run_multi_market_backtest_with_strategy_slots,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(
        app_start_multi_market_backtest_with_strategy_slots,
        module
    )?)
}
