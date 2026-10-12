//! Built-in strategy discovery projected from the shared Rust strategy definitions.

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::PyModule;
use qx_strategy::BuiltinStrategyKind;
use serde::Serialize;

#[derive(Serialize)]
struct StrategyInfo {
    name: &'static str,
    description: &'static str,
    signal_knobs: Vec<String>,
    requires_reference_leg: bool,
}

#[pyfunction]
fn app_list_builtin_strategies() -> PyResult<String> {
    let catalog = BuiltinStrategyKind::ALL
        .into_iter()
        .map(|kind| {
            let knobs = kind.signal_knob_list();
            StrategyInfo {
                name: kind.name(),
                description: kind.description(),
                signal_knobs: if knobs == "none" {
                    Vec::new()
                } else {
                    knobs.split(',').map(str::to_owned).collect()
                },
                requires_reference_leg: kind.needs_reference_leg(),
            }
        })
        .collect::<Vec<_>>();
    serde_json::to_string(&catalog).map_err(|error| {
        PyValueError::new_err(format!("Strategy catalog serialization failed: {error}"))
    })
}

pub(super) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(app_list_builtin_strategies, module)?)?;
    Ok(())
}
