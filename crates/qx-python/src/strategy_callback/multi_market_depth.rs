//! Python callbacks bound to L2 slots in the same-account market portfolio.

use super::*;
use qx_app::{
    MultiMarketDepthBacktestSpec, RunMultiMarketDepthExperimentSpec, StrategyArtifactIdentity,
};
use qx_strategy::Strategy;

fn experiment_strategy_factory(
    factory: Py<PyAny>,
) -> impl FnMut(
    &MultiMarketDepthBacktestSpec,
    &std::collections::BTreeMap<String, i128>,
) -> Result<Box<dyn Strategy>, String>
       + Send {
    move |candidate, parameters| {
        let candidate_json = candidate.to_json().map_err(|error| error.to_string())?;
        let parameters_json = serde_json::to_string(parameters)
            .map_err(|error| format!("failed to serialize callback parameters: {error}"))?;
        let callback = Python::attach(|py| {
            factory
                .bind(py)
                .call1((candidate_json, parameters_json))
                .map(|value| value.unbind())
                .map_err(|error| format!("Python multi-market L2 strategy factory failed: {error}"))
        })?;
        Ok(Box::new(super::PythonBacktestStrategy { callback }) as Box<dyn Strategy>)
    }
}

#[pyfunction]
fn app_run_multi_market_depth_experiment_with_strategy(
    spec_json: &str,
    strategy_id: &str,
    strategy_version: &str,
    source_sha256: &str,
    factory: Py<PyAny>,
) -> PyResult<String> {
    let spec = RunMultiMarketDepthExperimentSpec::from_json(spec_json).map_err(app_error)?;
    let identity = StrategyArtifactIdentity {
        strategy_id: strategy_id.to_string(),
        strategy_version: strategy_version.to_string(),
        source_sha256: source_sha256.to_string(),
    };
    spec.run_with_strategy_factory(
        &identity,
        &mut experiment_strategy_factory(factory),
        &research_context(&spec.experiment_id),
    )
    .and_then(|result| result.to_json())
    .map_err(app_error)
}

#[pyfunction]
fn app_start_multi_market_depth_experiment_with_strategy(
    spec_json: &str,
    strategy_id: &str,
    strategy_version: &str,
    source_sha256: &str,
    factory: Py<PyAny>,
) -> PyResult<PyRunHandle> {
    let spec = RunMultiMarketDepthExperimentSpec::from_json(spec_json).map_err(app_error)?;
    let identity = StrategyArtifactIdentity {
        strategy_id: strategy_id.to_string(),
        strategy_version: strategy_version.to_string(),
        source_sha256: source_sha256.to_string(),
    };
    let context = research_context(&spec.experiment_id);
    let handle = spec
        .start_with_strategy_factory(identity, experiment_strategy_factory(factory), context)
        .map_err(app_error)?;
    Ok(PyRunHandle {
        inner: NativeRun::MultiMarketDepthExperiment(handle),
    })
}

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
        app_run_multi_market_depth_experiment_with_strategy,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(
        app_start_multi_market_depth_experiment_with_strategy,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(
        app_run_multi_market_depth_backtest_with_strategy_slots,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(
        app_start_multi_market_depth_backtest_with_strategy_slots,
        module
    )?)
}
