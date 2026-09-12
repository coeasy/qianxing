//! Python 原生边界。
//!
//! Python 只获得不可变 JSON 结果或 Arrow C Data Interface capsule；Rust
//! `BarFrame` 的所有权和校验仍留在 `qx-datastruct`，不会把可变 Python 对象
//! 引入 Kernel 热路径。

use pyo3::exceptions::{PyIndexError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyCapsule, PyModule, PyTuple};
use qx_datastruct::{ArrowArray, ArrowSchema, BarFrame, FrameError};
use std::ffi::c_void;
use std::ptr::NonNull;

fn frame_error(error: FrameError) -> PyErr {
    PyValueError::new_err(format!("BarFrame error: {error:?}"))
}

#[pyfunction]
fn frame_digest(payload: &str) -> PyResult<u64> {
    Ok(BarFrame::from_json(payload).map_err(frame_error)?.digest())
}

#[pyfunction]
fn frame_to_json(payload: &str) -> PyResult<String> {
    BarFrame::from_json(payload)
        .map_err(frame_error)
        .map(|frame| frame.to_json())
}

/// 导出一个拥有型 Arrow 列的 `(schema_capsule, array_capsule)`。
///
/// capsule 被 pyarrow 消费后会调用 C Data Interface release；若 capsule
/// 未被消费，析构器也会主动 release，确保 Python 异常路径不泄漏 Rust 所有权。
#[pyfunction]
fn owned_arrow_capsules<'py>(
    py: Python<'py>,
    payload: &str,
    column: usize,
) -> PyResult<Bound<'py, PyTuple>> {
    let (array, schema) = make_arrow_capsules(py, payload, column)?;
    PyTuple::new(py, [schema, array])
}

#[pyclass(unsendable)]
struct OwnedArrowArray {
    array: Py<PyCapsule>,
    schema: Py<PyCapsule>,
}

#[pymethods]
impl OwnedArrowArray {
    /// Arrow C Data Interface protocol consumed by pyarrow.Array.
    fn __arrow_c_array__<'py>(
        &self,
        py: Python<'py>,
        _requested_schema: Option<Py<PyAny>>,
    ) -> PyResult<(Py<PyCapsule>, Py<PyCapsule>)> {
        Ok((self.schema.clone_ref(py), self.array.clone_ref(py)))
    }
}

#[pyfunction]
fn owned_arrow_array(
    py: Python<'_>,
    payload: &str,
    column: usize,
) -> PyResult<Py<OwnedArrowArray>> {
    let (array, schema) = make_arrow_capsules(py, payload, column)?;
    Py::new(
        py,
        OwnedArrowArray {
            array: array.unbind(),
            schema: schema.unbind(),
        },
    )
}

fn make_arrow_capsules<'py>(
    py: Python<'py>,
    payload: &str,
    column: usize,
) -> PyResult<(Bound<'py, PyCapsule>, Bound<'py, PyCapsule>)> {
    let frame = BarFrame::from_json(payload).map_err(frame_error)?;
    let mut columns = frame.owned_arrow_columns().map_err(frame_error)?;
    let column_index = column;
    columns
        .get(column_index)
        .ok_or_else(|| PyIndexError::new_err("Arrow column index out of range"))?;
    // Clone only the selected owning column so the other temporary columns can
    // be released before returning. The FFI transfer itself remains owning.
    let selected = columns.swap_remove(column_index);
    let (array, schema) = selected.into_ffi();
    let array = capsule_for_array(py, array)?;
    let schema = capsule_for_schema(py, schema)?;
    Ok((array, schema))
}

fn capsule_for_array<'py>(py: Python<'py>, array: ArrowArray) -> PyResult<Bound<'py, PyCapsule>> {
    let pointer = NonNull::new(Box::into_raw(Box::new(array)).cast::<c_void>())
        .expect("Box pointer must not be null");
    // SAFETY: pointer owns a valid ArrowArray and the destructor releases and
    // deallocates it exactly once.
    unsafe {
        PyCapsule::new_with_pointer_and_destructor(
            py,
            pointer,
            c"arrow_array",
            Some(drop_array_capsule),
        )
    }
}

fn capsule_for_schema<'py>(
    py: Python<'py>,
    schema: ArrowSchema,
) -> PyResult<Bound<'py, PyCapsule>> {
    let pointer = NonNull::new(Box::into_raw(Box::new(schema)).cast::<c_void>())
        .expect("Box pointer must not be null");
    // SAFETY: pointer owns a valid ArrowSchema and the destructor releases and
    // deallocates it exactly once.
    unsafe {
        PyCapsule::new_with_pointer_and_destructor(
            py,
            pointer,
            c"arrow_schema",
            Some(drop_schema_capsule),
        )
    }
}

unsafe extern "C" fn drop_array_capsule(capsule: *mut pyo3::ffi::PyObject) {
    let pointer = unsafe { pyo3::ffi::PyCapsule_GetPointer(capsule, c"arrow_array".as_ptr()) };
    if pointer.is_null() {
        return;
    }
    let array = pointer.cast::<ArrowArray>();
    let release = unsafe { (*array).release };
    if let Some(release) = release {
        unsafe { release(array) };
    }
    unsafe { drop(Box::from_raw(array)) };
}

unsafe extern "C" fn drop_schema_capsule(capsule: *mut pyo3::ffi::PyObject) {
    let pointer = unsafe { pyo3::ffi::PyCapsule_GetPointer(capsule, c"arrow_schema".as_ptr()) };
    if pointer.is_null() {
        return;
    }
    let schema = pointer.cast::<ArrowSchema>();
    let release = unsafe { (*schema).release };
    if let Some(release) = release {
        unsafe { release(schema) };
    }
    unsafe { drop(Box::from_raw(schema)) };
}

#[pymodule]
fn _qianxing_native(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(frame_digest, module)?)?;
    module.add_function(wrap_pyfunction!(frame_to_json, module)?)?;
    module.add_function(wrap_pyfunction!(owned_arrow_capsules, module)?)?;
    module.add_class::<OwnedArrowArray>()?;
    module.add_function(wrap_pyfunction!(owned_arrow_array, module)?)?;
    Ok(())
}
