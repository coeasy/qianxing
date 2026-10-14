use pyo3::prelude::*;

#[pyfunction]
fn app_screen_ashare_bars(payload: &str) -> PyResult<String> {
    qx_app::screen_bar_frames(payload).map_err(crate::app_error)
}

pub(crate) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(app_screen_ashare_bars, module)?)
}
