//! Python authored decisions adapted to the shared Rust Strategy contract.

use super::{app_error, research_context, NativeRun, PyRunHandle};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyModule};
use qx_app::{
    run_depth_backtest_with_strategy, start_depth_backtest_with_strategy, BacktestSpec,
    DepthBacktestSpec, StrategyArtifactIdentity,
};
use qx_strategy::{MarketEvent, Strategy, StrategyContext, StrategyDecision};

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

pub(super) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(app_run_backtest_with_strategy, module)?)?;
    module.add_function(wrap_pyfunction!(app_start_backtest_with_strategy, module)?)?;
    module.add_function(wrap_pyfunction!(
        app_run_depth_backtest_with_strategy,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(
        app_start_depth_backtest_with_strategy,
        module
    )?)
}
