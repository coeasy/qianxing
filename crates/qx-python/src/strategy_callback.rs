//! Python authored decisions adapted to the shared Rust Strategy contract.

use super::{app_error, research_context, NativeRun, PyRunHandle};
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyModule};
use qx_app::{
    run_depth_backtest_with_strategy, run_depth_backtest_with_strategy_slots,
    run_multi_leg_backtest_with_strategy, start_depth_backtest_with_strategy,
    start_depth_backtest_with_strategy_slots, BacktestSpec, DepthBacktestSpec,
    MultiLegBacktestSpec, RunDepthExperimentSpec, RunExperimentSpec, RunMultiLegExperimentSpec,
    StrategyArtifactIdentity,
};
use qx_strategy::{MarketEvent, Strategy, StrategyContext, StrategyDecision};
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StrategyIdentityDocument {
    strategy_id: String,
    strategy_version: String,
    source_sha256: String,
}

fn strategy_slots(
    identities_json: &str,
    callbacks: Vec<Py<PyAny>>,
) -> PyResult<Vec<(StrategyArtifactIdentity, Box<dyn Strategy>)>> {
    let identities = serde_json::from_str::<Vec<StrategyIdentityDocument>>(identities_json)
        .map_err(|error| PyValueError::new_err(format!("invalid strategy identities: {error}")))?;
    if identities.len() != callbacks.len() {
        return Err(PyValueError::new_err(format!(
            "strategy identity count {} does not match callback count {}",
            identities.len(),
            callbacks.len()
        )));
    }
    Ok(identities
        .into_iter()
        .zip(callbacks)
        .map(|(identity, callback)| {
            (
                StrategyArtifactIdentity {
                    strategy_id: identity.strategy_id,
                    strategy_version: identity.strategy_version,
                    source_sha256: identity.source_sha256,
                },
                Box::new(PythonBacktestStrategy { callback }) as Box<dyn Strategy>,
            )
        })
        .collect())
}

struct PythonBacktestStrategy {
    callback: Py<PyAny>,
}

impl Strategy for PythonBacktestStrategy {
    fn on_event(
        &mut self,
        context: &StrategyContext,
        event: &MarketEvent,
    ) -> Result<StrategyDecision, String> {
        let context_json = serde_json::to_string(context).map_err(|error| error.to_string())?;
        let event_json = serde_json::to_string(event).map_err(|error| error.to_string())?;
        Python::attach(|py| {
            let response = self
                .callback
                .bind(py)
                .call_method1("_qx_on_event", (context_json, event_json))
                .and_then(|value| value.extract::<String>())
                .map_err(|error| format!("Python strategy callback failed: {error}"))?;
            serde_json::from_str(&response).map_err(|error| {
                format!("Python strategy returned invalid StrategyDecision JSON: {error}")
            })
        })
    }
}

#[pyfunction]
fn app_run_backtest_with_strategy(
    spec_json: &str,
    strategy_id: &str,
    strategy_version: &str,
    source_sha256: &str,
    callback: Py<PyAny>,
) -> PyResult<String> {
    let spec = BacktestSpec::from_json(spec_json).map_err(app_error)?;
    let identity = StrategyArtifactIdentity {
        strategy_id: strategy_id.to_string(),
        strategy_version: strategy_version.to_string(),
        source_sha256: source_sha256.to_string(),
    };
    spec.run_with_strategy(
        &identity,
        PythonBacktestStrategy { callback },
        &research_context(&spec.run_id),
    )
    .and_then(|outcome| outcome.to_json())
    .map_err(app_error)
}

#[pyfunction]
fn app_start_backtest_with_strategy(
    spec_json: &str,
    strategy_id: &str,
    strategy_version: &str,
    source_sha256: &str,
    callback: Py<PyAny>,
) -> PyResult<PyRunHandle> {
    let spec = BacktestSpec::from_json(spec_json).map_err(app_error)?;
    let context = research_context(&spec.run_id);
    let identity = StrategyArtifactIdentity {
        strategy_id: strategy_id.to_string(),
        strategy_version: strategy_version.to_string(),
        source_sha256: source_sha256.to_string(),
    };
    let handle = spec
        .start_with_strategy(identity, PythonBacktestStrategy { callback }, context)
        .map_err(app_error)?;
    Ok(PyRunHandle {
        inner: NativeRun::Bar(handle),
    })
}

#[pyfunction]
fn app_run_backtest_with_strategy_slots(
    spec_json: &str,
    identities_json: &str,
    callbacks: Vec<Py<PyAny>>,
) -> PyResult<String> {
    let spec = BacktestSpec::from_json(spec_json).map_err(app_error)?;
    let strategies = strategy_slots(identities_json, callbacks)?;
    spec.run_with_strategy_slots(strategies, &research_context(&spec.run_id))
        .and_then(|outcome| outcome.to_json())
        .map_err(app_error)
}

#[pyfunction]
fn app_start_backtest_with_strategy_slots(
    spec_json: &str,
    identities_json: &str,
    callbacks: Vec<Py<PyAny>>,
) -> PyResult<PyRunHandle> {
    let spec = BacktestSpec::from_json(spec_json).map_err(app_error)?;
    let context = research_context(&spec.run_id);
    let strategies = strategy_slots(identities_json, callbacks)?;
    let handle = spec
        .start_with_strategy_slots(strategies, context)
        .map_err(app_error)?;
    Ok(PyRunHandle {
        inner: NativeRun::Bar(handle),
    })
}

#[pyfunction]
fn app_run_depth_backtest_with_strategy(
    spec_json: &str,
    strategy_id: &str,
    strategy_version: &str,
    source_sha256: &str,
    callback: Py<PyAny>,
) -> PyResult<String> {
    let spec = DepthBacktestSpec::from_json(spec_json).map_err(app_error)?;
    let identity = StrategyArtifactIdentity {
        strategy_id: strategy_id.to_string(),
        strategy_version: strategy_version.to_string(),
        source_sha256: source_sha256.to_string(),
    };
    run_depth_backtest_with_strategy(
        &spec,
        &identity,
        Box::new(PythonBacktestStrategy { callback }),
        &research_context(&spec.run_id),
    )
    .and_then(|outcome| outcome.to_json())
    .map_err(app_error)
}

#[pyfunction]
fn app_start_depth_backtest_with_strategy(
    spec_json: &str,
    strategy_id: &str,
    strategy_version: &str,
    source_sha256: &str,
    callback: Py<PyAny>,
) -> PyResult<PyRunHandle> {
    let spec = DepthBacktestSpec::from_json(spec_json).map_err(app_error)?;
    let context = research_context(&spec.run_id);
    let identity = StrategyArtifactIdentity {
        strategy_id: strategy_id.to_string(),
        strategy_version: strategy_version.to_string(),
        source_sha256: source_sha256.to_string(),
    };
    let handle = start_depth_backtest_with_strategy(
        spec,
        identity,
        Box::new(PythonBacktestStrategy { callback }),
        context,
    )
    .map_err(app_error)?;
    Ok(PyRunHandle {
        inner: NativeRun::Depth(handle),
    })
}

#[pyfunction]
fn app_run_depth_backtest_with_strategy_slots(
    spec_json: &str,
    identities_json: &str,
    callbacks: Vec<Py<PyAny>>,
) -> PyResult<String> {
    let spec = DepthBacktestSpec::from_json(spec_json).map_err(app_error)?;
    let strategies = strategy_slots(identities_json, callbacks)?;
    run_depth_backtest_with_strategy_slots(&spec, strategies, &research_context(&spec.run_id))
        .and_then(|outcome| outcome.to_json())
        .map_err(app_error)
}

#[pyfunction]
fn app_start_depth_backtest_with_strategy_slots(
    spec_json: &str,
    identities_json: &str,
    callbacks: Vec<Py<PyAny>>,
) -> PyResult<PyRunHandle> {
    let spec = DepthBacktestSpec::from_json(spec_json).map_err(app_error)?;
    let context = research_context(&spec.run_id);
    let strategies = strategy_slots(identities_json, callbacks)?;
    let handle =
        start_depth_backtest_with_strategy_slots(spec, strategies, context).map_err(app_error)?;
    Ok(PyRunHandle {
        inner: NativeRun::Depth(handle),
    })
}

#[pyfunction]
fn app_run_multi_leg_backtest_with_strategy(
    spec_json: &str,
    strategy_id: &str,
    strategy_version: &str,
    source_sha256: &str,
    callback: Py<PyAny>,
) -> PyResult<String> {
    let spec = MultiLegBacktestSpec::from_json(spec_json).map_err(app_error)?;
    let identity = StrategyArtifactIdentity {
        strategy_id: strategy_id.to_string(),
        strategy_version: strategy_version.to_string(),
        source_sha256: source_sha256.to_string(),
    };
    run_multi_leg_backtest_with_strategy(
        &spec,
        &identity,
        Box::new(PythonBacktestStrategy { callback }),
        &research_context(&spec.run_id),
    )
    .and_then(|outcome| outcome.to_json())
    .map_err(app_error)
}

#[pyfunction]
fn app_start_multi_leg_backtest_with_strategy(
    spec_json: &str,
    strategy_id: &str,
    strategy_version: &str,
    source_sha256: &str,
    callback: Py<PyAny>,
) -> PyResult<PyRunHandle> {
    let spec = MultiLegBacktestSpec::from_json(spec_json).map_err(app_error)?;
    let context = research_context(&spec.run_id);
    let identity = StrategyArtifactIdentity {
        strategy_id: strategy_id.to_string(),
        strategy_version: strategy_version.to_string(),
        source_sha256: source_sha256.to_string(),
    };
    let handle = spec
        .start_with_strategy(identity, PythonBacktestStrategy { callback }, context)
        .map_err(app_error)?;
    Ok(PyRunHandle {
        inner: NativeRun::MultiLeg(handle),
    })
}

fn python_strategy_factory(
    factory: Py<PyAny>,
) -> impl FnMut(&MultiLegBacktestSpec) -> Result<Box<dyn Strategy>, String> + Send {
    move |candidate| {
        let candidate_json = candidate.to_json().map_err(|error| error.to_string())?;
        let callback = Python::attach(|py| {
            factory
                .bind(py)
                .call1((candidate_json,))
                .map(|value| value.unbind())
                .map_err(|error| format!("Python strategy factory failed: {error}"))
        })?;
        Ok(Box::new(PythonBacktestStrategy { callback }) as Box<dyn Strategy>)
    }
}

fn python_bar_strategy_factory(
    factory: Py<PyAny>,
) -> impl FnMut(&BacktestSpec) -> Result<Box<dyn Strategy>, String> + Send {
    move |candidate| {
        let candidate_json = candidate.to_json().map_err(|error| error.to_string())?;
        let callback = Python::attach(|py| {
            factory
                .bind(py)
                .call1((candidate_json,))
                .map(|value| value.unbind())
                .map_err(|error| format!("Python strategy factory failed: {error}"))
        })?;
        Ok(Box::new(PythonBacktestStrategy { callback }) as Box<dyn Strategy>)
    }
}

fn python_depth_strategy_factory(
    factory: Py<PyAny>,
) -> impl FnMut(&DepthBacktestSpec) -> Result<Box<dyn Strategy>, String> + Send {
    move |candidate| {
        let candidate_json = serde_json::to_string(candidate).map_err(|error| error.to_string())?;
        let callback = Python::attach(|py| {
            factory
                .bind(py)
                .call1((candidate_json,))
                .map(|value| value.unbind())
                .map_err(|error| format!("Python depth strategy factory failed: {error}"))
        })?;
        Ok(Box::new(PythonBacktestStrategy { callback }) as Box<dyn Strategy>)
    }
}

#[pyfunction]
fn app_run_experiment_with_strategy(
    spec_json: &str,
    strategy_id: &str,
    strategy_version: &str,
    source_sha256: &str,
    factory: Py<PyAny>,
) -> PyResult<String> {
    let spec = RunExperimentSpec::from_json(spec_json).map_err(app_error)?;
    let identity = StrategyArtifactIdentity {
        strategy_id: strategy_id.to_string(),
        strategy_version: strategy_version.to_string(),
        source_sha256: source_sha256.to_string(),
    };
    let mut factory = python_bar_strategy_factory(factory);
    spec.run_with_strategy_factory(
        &identity,
        &mut factory,
        &research_context(&spec.experiment_id),
    )
    .and_then(|result| result.to_json())
    .map_err(app_error)
}

#[pyfunction]
fn app_start_experiment_with_strategy(
    spec_json: &str,
    strategy_id: &str,
    strategy_version: &str,
    source_sha256: &str,
    factory: Py<PyAny>,
) -> PyResult<PyRunHandle> {
    let spec = RunExperimentSpec::from_json(spec_json).map_err(app_error)?;
    let context = research_context(&spec.experiment_id);
    let identity = StrategyArtifactIdentity {
        strategy_id: strategy_id.to_string(),
        strategy_version: strategy_version.to_string(),
        source_sha256: source_sha256.to_string(),
    };
    let handle = spec
        .start_with_strategy_factory(identity, python_bar_strategy_factory(factory), context)
        .map_err(app_error)?;
    Ok(PyRunHandle {
        inner: NativeRun::Experiment(handle),
    })
}

#[pyfunction]
fn app_run_depth_experiment_with_strategy(
    spec_json: &str,
    strategy_id: &str,
    strategy_version: &str,
    source_sha256: &str,
    factory: Py<PyAny>,
) -> PyResult<String> {
    let spec = RunDepthExperimentSpec::from_json(spec_json).map_err(app_error)?;
    let identity = StrategyArtifactIdentity {
        strategy_id: strategy_id.to_string(),
        strategy_version: strategy_version.to_string(),
        source_sha256: source_sha256.to_string(),
    };
    let mut factory = python_depth_strategy_factory(factory);
    spec.run_with_strategy_factory(
        &identity,
        &mut factory,
        &research_context(&spec.experiment_id),
    )
    .and_then(|result| result.to_json())
    .map_err(app_error)
}

#[pyfunction]
fn app_start_depth_experiment_with_strategy(
    spec_json: &str,
    strategy_id: &str,
    strategy_version: &str,
    source_sha256: &str,
    factory: Py<PyAny>,
) -> PyResult<PyRunHandle> {
    let spec = RunDepthExperimentSpec::from_json(spec_json).map_err(app_error)?;
    let context = research_context(&spec.experiment_id);
    let identity = StrategyArtifactIdentity {
        strategy_id: strategy_id.to_string(),
        strategy_version: strategy_version.to_string(),
        source_sha256: source_sha256.to_string(),
    };
    let handle = spec
        .start_with_strategy_factory(identity, python_depth_strategy_factory(factory), context)
        .map_err(app_error)?;
    Ok(PyRunHandle {
        inner: NativeRun::DepthExperiment(handle),
    })
}

#[pyfunction]
fn app_run_multi_leg_experiment_with_strategy(
    spec_json: &str,
    strategy_id: &str,
    strategy_version: &str,
    source_sha256: &str,
    factory: Py<PyAny>,
) -> PyResult<String> {
    let spec = RunMultiLegExperimentSpec::from_json(spec_json).map_err(app_error)?;
    let identity = StrategyArtifactIdentity {
        strategy_id: strategy_id.to_string(),
        strategy_version: strategy_version.to_string(),
        source_sha256: source_sha256.to_string(),
    };
    let mut factory = python_strategy_factory(factory);
    spec.run_with_strategy_factory(
        &identity,
        &mut factory,
        &research_context(&spec.experiment_id),
    )
    .and_then(|result| result.to_json())
    .map_err(app_error)
}

#[pyfunction]
fn app_start_multi_leg_experiment_with_strategy(
    spec_json: &str,
    strategy_id: &str,
    strategy_version: &str,
    source_sha256: &str,
    factory: Py<PyAny>,
) -> PyResult<PyRunHandle> {
    let spec = RunMultiLegExperimentSpec::from_json(spec_json).map_err(app_error)?;
    let context = research_context(&spec.experiment_id);
    let identity = StrategyArtifactIdentity {
        strategy_id: strategy_id.to_string(),
        strategy_version: strategy_version.to_string(),
        source_sha256: source_sha256.to_string(),
    };
    let handle = spec
        .start_with_strategy_factory(identity, python_strategy_factory(factory), context)
        .map_err(app_error)?;
    Ok(PyRunHandle {
        inner: NativeRun::MultiLegExperiment(handle),
    })
}

pub(super) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(app_run_experiment_with_strategy, module)?)?;
    module.add_function(wrap_pyfunction!(
        app_start_experiment_with_strategy,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(
        app_run_depth_experiment_with_strategy,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(
        app_start_depth_experiment_with_strategy,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(app_run_backtest_with_strategy, module)?)?;
    module.add_function(wrap_pyfunction!(app_start_backtest_with_strategy, module)?)?;
    module.add_function(wrap_pyfunction!(
        app_run_backtest_with_strategy_slots,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(
        app_start_backtest_with_strategy_slots,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(
        app_run_depth_backtest_with_strategy,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(
        app_start_depth_backtest_with_strategy,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(
        app_run_depth_backtest_with_strategy_slots,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(
        app_start_depth_backtest_with_strategy_slots,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(
        app_run_multi_leg_backtest_with_strategy,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(
        app_start_multi_leg_backtest_with_strategy,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(
        app_run_multi_leg_experiment_with_strategy,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(
        app_start_multi_leg_experiment_with_strategy,
        module
    )?)
}
