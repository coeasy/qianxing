//! Python 原生边界。
//!
//! Python 只获得不可变 JSON 结果或 Arrow C Data Interface capsule；Rust
//! `BarFrame` 的所有权和校验仍留在 `qx-datastruct`，不会把可变 Python 对象
//! 引入 Kernel 热路径。
//!
//! ## `app_*`：应用层用例的 Python 门面（T2-2 / 退出门 G1）
//!
//! `app_*` 函数与 `qx-cli app` 子命令、`POST /app/*` 路由调的是**同一组 `qx-app`
//! 用例**。它们的进出都是字符串：传一份 spec/outcome JSON，换回一份结果 JSON；失败时抛
//! [`QxAppError`]，**异常文本就是 `AppError::to_json()` 的原文**——于是「同一 use case 三入口
//! 结果哈希相同、错误 code 与 correlation id 相同」在 Python 这一侧也是构造性的。
//!
//! 用专门的异常类型而不是 `ValueError`：调用方要能只捕获应用层用例的失败，并按文档里的
//! `category` 分支（`InvalidInput` 改输入、`DataUnavailable` 去取数据……），而不是去猜字符串。

use pyo3::exceptions::{PyException, PyIndexError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyCapsule, PyModule, PyTuple};
use qx_app::{
    compare_runs, run_backtest, run_depth_backtest, run_experiment, validate_dataset,
    verify_depth_run, verify_run, AppError, BacktestOutcome, BacktestSpec, CallerCapability,
    CompareRunsSpec, DatasetSpec, DepthBacktestOutcome, DepthBacktestSpec, RunContext,
    RunExperimentResult, RunExperimentSpec, RunHandle, RunStatus,
};
use qx_datastruct::{ArrowArray, ArrowSchema, BarFrame, FrameError};
use std::ffi::c_void;
use std::ptr::NonNull;
use std::time::{Duration, Instant};

fn frame_error(error: FrameError) -> PyErr {
    PyValueError::new_err(format!("BarFrame error: {error:?}"))
}

pyo3::create_exception!(
    _qianxing_native,
    QxAppError,
    PyException,
    "应用层用例失败。异常文本就是 AppError 的 JSON 文档（category/action/retry/safe_to_retry/correlation_id/source_code/message），与 `qx-cli app` 的 stderr 那行、`POST /app/*` 的响应体逐字节相同。"
);

fn app_error(error: AppError) -> PyErr {
    QxAppError::new_err(error.to_json())
}

/// 调用上下文：与 CLI/HTTP 两侧同形（R 档 + 身份串当 correlation id）。
///
/// 刻意**不**带 `with_code_commit`：Python 侧拿不到 CLI 的 `QX_GIT_COMMIT`，写一个看起来像
/// 提交号的字符串只会让产物里那一格撒谎（落点仍是 `qx-app` 的包版本）。
fn research_context(correlation_id: &str) -> RunContext {
    RunContext::new(CallerCapability::Research, correlation_id)
}

#[pyfunction]
fn app_validate_dataset(spec_json: &str) -> PyResult<String> {
    let spec = DatasetSpec::from_json(spec_json).map_err(app_error)?;
    validate_dataset(&spec, &research_context(&spec.dataset_id))
        .and_then(|verdict| verdict.to_json())
        .map_err(app_error)
}

#[pyfunction]
fn app_run_backtest(spec_json: &str) -> PyResult<String> {
    let spec = BacktestSpec::from_json(spec_json).map_err(app_error)?;
    run_backtest(&spec, &research_context(&spec.run_id))
        .and_then(|outcome| outcome.to_json())
        .map_err(app_error)
}

#[pyfunction]
fn app_start_backtest(spec_json: &str) -> PyResult<PyRunHandle> {
    let spec = BacktestSpec::from_json(spec_json).map_err(app_error)?;
    let context = research_context(&spec.run_id);
    Ok(PyRunHandle {
        inner: NativeRun::Bar(spec.start(context)),
    })
}

#[pyfunction]
fn app_verify_run(outcome_json: &str) -> PyResult<String> {
    let outcome = BacktestOutcome::from_json(outcome_json).map_err(app_error)?;
    verify_run(&outcome, &research_context(&outcome.run_id))
        .and_then(|result| result.to_json())
        .map_err(app_error)
}

#[pyfunction]
fn app_compare_runs(spec_json: &str) -> PyResult<String> {
    let spec = CompareRunsSpec::from_json(spec_json).map_err(app_error)?;
    compare_runs(&spec, &research_context("compare-runs"))
        .and_then(|result| result.to_json())
        .map_err(app_error)
}

#[pyfunction]
fn app_run_experiment(spec_json: &str) -> PyResult<String> {
    let spec = RunExperimentSpec::from_json(spec_json).map_err(app_error)?;
    run_experiment(&spec, &research_context(&spec.experiment_id))
        .and_then(|result| result.to_json())
        .map_err(app_error)
}

#[pyfunction]
fn app_start_experiment(spec_json: &str) -> PyResult<PyRunHandle> {
    let spec = RunExperimentSpec::from_json(spec_json).map_err(app_error)?;
    let context = research_context(&spec.experiment_id);
    Ok(PyRunHandle {
        inner: NativeRun::Experiment(spec.start(context)),
    })
}

#[pyfunction]
fn app_run_depth_backtest(spec_json: &str) -> PyResult<String> {
    let spec = DepthBacktestSpec::from_json(spec_json).map_err(app_error)?;
    run_depth_backtest(&spec, &research_context(&spec.run_id))
        .and_then(|outcome| outcome.to_json())
        .map_err(app_error)
}

#[pyfunction]
fn app_start_depth_backtest(spec_json: &str) -> PyResult<PyRunHandle> {
    let spec = DepthBacktestSpec::from_json(spec_json).map_err(app_error)?;
    let context = research_context(&spec.run_id);
    Ok(PyRunHandle {
        inner: NativeRun::Depth(spec.start(context)),
    })
}

enum NativeRun {
    Bar(RunHandle<BacktestOutcome>),
    Depth(RunHandle<DepthBacktestOutcome>),
    Experiment(RunHandle<RunExperimentResult>),
}

#[pyclass(name = "RunHandle")]
struct PyRunHandle {
    inner: NativeRun,
}

#[pymethods]
impl PyRunHandle {
    #[getter]
    fn run_id(&self) -> &str {
        match &self.inner {
            NativeRun::Bar(handle) => handle.run_id(),
            NativeRun::Depth(handle) => handle.run_id(),
            NativeRun::Experiment(handle) => handle.run_id(),
        }
    }

    #[getter]
    fn status(&mut self) -> &'static str {
        status_name(match &mut self.inner {
            NativeRun::Bar(handle) => handle.status(),
            NativeRun::Depth(handle) => handle.status(),
            NativeRun::Experiment(handle) => handle.status(),
        })
    }

    fn cancel(&self) {
        match &self.inner {
            NativeRun::Bar(handle) => handle.cancel(),
            NativeRun::Depth(handle) => handle.cancel(),
            NativeRun::Experiment(handle) => handle.cancel(),
        }
    }

    /// Wait for completion while releasing the Python GIL. A timeout returns
    /// the current status; it does not cancel the worker.
    #[pyo3(signature = (timeout_ms=None))]
    fn wait(&mut self, py: Python<'_>, timeout_ms: Option<u64>) -> &'static str {
        let deadline = timeout_ms.map(|ms| Instant::now() + Duration::from_millis(ms));
        loop {
            let status = match &mut self.inner {
                NativeRun::Bar(handle) => handle.status(),
                NativeRun::Depth(handle) => handle.status(),
                NativeRun::Experiment(handle) => handle.status(),
            };
            if matches!(
                status,
                RunStatus::Succeeded | RunStatus::Cancelled | RunStatus::Failed
            ) {
                return status_name(status);
            }
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return status_name(status);
            }
            py.detach(|| std::thread::sleep(Duration::from_millis(5)));
        }
    }

    /// Return the completed outcome JSON exactly once. `None` means the run
    /// has not finished or was cancelled; inspect `status` to distinguish.
    fn result_json(&mut self) -> PyResult<Option<String>> {
        match &mut self.inner {
            NativeRun::Bar(handle) => handle
                .try_take_result()
                .map(|result| {
                    result.and_then(|outcome| {
                        outcome.to_json().map_err(|e| {
                            AppError::new(
                                qx_app::AppErrorCategory::InternalInvariant,
                                e.to_string(),
                            )
                        })
                    })
                })
                .transpose()
                .map_err(app_error),
            NativeRun::Depth(handle) => handle
                .try_take_result()
                .map(|result| {
                    result.and_then(|outcome| {
                        outcome.to_json().map_err(|e| {
                            AppError::new(
                                qx_app::AppErrorCategory::InternalInvariant,
                                e.to_string(),
                            )
                        })
                    })
                })
                .transpose()
                .map_err(app_error),
            NativeRun::Experiment(handle) => handle
                .try_take_result()
                .map(|result| result.and_then(|outcome| outcome.to_json()))
                .transpose()
                .map_err(app_error),
        }
    }
}

fn status_name(status: RunStatus) -> &'static str {
    match status {
        RunStatus::Running => "running",
        RunStatus::Cancelling => "cancelling",
        RunStatus::Succeeded => "succeeded",
        RunStatus::Cancelled => "cancelled",
        RunStatus::Failed => "failed",
    }
}

#[pyfunction]
fn app_verify_depth_run(outcome_json: &str) -> PyResult<String> {
    let outcome = qx_app::DepthBacktestOutcome::from_json(outcome_json).map_err(app_error)?;
    verify_depth_run(&outcome, &research_context(&outcome.run_id))
        .and_then(|result| result.to_json())
        .map_err(app_error)
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
    // 应用层用例入口 + 它们专用的异常类型。
    module.add_function(wrap_pyfunction!(app_validate_dataset, module)?)?;
    module.add_function(wrap_pyfunction!(app_run_backtest, module)?)?;
    module.add_function(wrap_pyfunction!(app_start_backtest, module)?)?;
    module.add_function(wrap_pyfunction!(app_verify_run, module)?)?;
    module.add_function(wrap_pyfunction!(app_compare_runs, module)?)?;
    module.add_function(wrap_pyfunction!(app_run_experiment, module)?)?;
    module.add_function(wrap_pyfunction!(app_start_experiment, module)?)?;
    module.add_function(wrap_pyfunction!(app_run_depth_backtest, module)?)?;
    module.add_function(wrap_pyfunction!(app_start_depth_backtest, module)?)?;
    module.add_class::<PyRunHandle>()?;
    module.add_function(wrap_pyfunction!(app_verify_depth_run, module)?)?;
    module.add("QxAppError", module.py().get_type::<QxAppError>())?;
    Ok(())
}
