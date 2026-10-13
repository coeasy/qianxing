//! Built-in strategy discovery projected from the shared Rust strategy definitions.

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::PyModule;
use qx_app::builtin_strategy_catalog;

#[pyfunction]
fn app_list_builtin_strategies() -> PyResult<String> {
    serde_json::to_string(&builtin_strategy_catalog()).map_err(|error| {
        PyValueError::new_err(format!("Strategy catalog serialization failed: {error}"))
    })
}

pub(super) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(app_list_builtin_strategies, module)?)?;
    Ok(())
}
