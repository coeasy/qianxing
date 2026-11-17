//! Python callbacks bound to N-market strategy slots.

use super::*;
use qx_app::MultiMarketBacktestSpec;

fn strategy_factory(
    factory: Py<PyAny>,
    pass_parameters: bool,
) -> impl FnMut(
    &MultiMarketBacktestSpec,
    &std::collections::BTreeMap<String, i128>,
) -> Result<Box<dyn Strategy>, String>
       + Send {
    move |candidate, parameters| {
        let candidate_json = candidate.to_json().map_err(|error| error.to_string())?;
        let parameters_json =
            serde_json::to_string(parameters).map_err(|error| error.to_string())?;
        let callback = Python::attach(|py| {
            let factory = factory.bind(py);
            let result = if pass_parameters {
                factory.call1((candidate_json, parameters_json))
            } else {
                factory.call1((candidate_json,))
            };
            result
                .map(|value| value.unbind())
                .map_err(|error| format!("Python multi-market strategy factory failed: {error}"))
        })?;
        Ok(Box::new(super::PythonBacktestStrategy { callback }) as Box<dyn Strategy>)
    }
}

fn has_python_only_search_ranges(spec: &RunMultiMarketExperimentSpec) -> bool {
    const BUILTIN_PARAMETERS: &[&str] = &[
        "fast_window",
        "slow_window",
        "period",
        "threshold_bps",
        "quantity_raw",
        "target_notional_raw",
        "target_available_margin_bps",
        "target_volatility_bps",
        "target_kelly_fraction_bps",
    ];
    spec.search.as_ref().is_some_and(|search| {
        search
            .continuous_parameter_space
            .iter()
            .any(|range| !BUILTIN_PARAMETERS.contains(&range.name.as_str()))
    })
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
    let uses_parameter_factory =
        !spec.callback_parameter_space.is_empty() || has_python_only_search_ranges(&spec);
    let mut adapted_factory = strategy_factory(factory, uses_parameter_factory);
    let context = research_context(&spec.experiment_id);
    let result = if uses_parameter_factory {
        spec.run_with_parameter_factory(&identity, &mut adapted_factory, &context)
    } else {
        let mut legacy_factory = |candidate: &MultiMarketBacktestSpec| {
            adapted_factory(candidate, &std::collections::BTreeMap::new())
        };
        spec.run_with_strategy_factory(&identity, &mut legacy_factory, &context)
    };
    result
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
    let uses_parameter_factory =
        !spec.callback_parameter_space.is_empty() || has_python_only_search_ranges(&spec);
    let handle = if uses_parameter_factory {
        spec.start_with_parameter_factory(identity, strategy_factory(factory, true), context)
            .map_err(app_error)?
    } else {
        let mut adapted_factory = strategy_factory(factory, false);
        let legacy_factory = move |candidate: &MultiMarketBacktestSpec| {
            adapted_factory(candidate, &std::collections::BTreeMap::new())
        };
        spec.start_with_strategy_factory(identity, legacy_factory, context)
            .map_err(app_error)?
    };
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
