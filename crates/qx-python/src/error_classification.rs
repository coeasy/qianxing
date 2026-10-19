use pyo3::prelude::*;
use pyo3::types::PyModule;

#[pyfunction]
fn classify_ccxt_error(class_name: &str) -> &'static str {
    qx_core::classify_ccxt_exception(class_name).category
}

#[pyfunction]
fn ccxt_error_may_retry(category: &str) -> bool {
    qx_core::classify_ccxt_category(category)
        .retryability
        .may_retry_eventually()
}

pub(crate) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(classify_ccxt_error, module)?)?;
    module.add_function(wrap_pyfunction!(ccxt_error_may_retry, module)?)?;
    Ok(())
}
