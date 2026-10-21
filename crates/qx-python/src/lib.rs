//! Python native boundary; JSON/Arrow, application cases, and providers delegate to shared Rust crates, preserving CLI/HTTP documents.

use pyo3::exceptions::{PyException, PyIndexError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyCapsule, PyModule, PyTuple};
use qx_app::{
    compare_runs, plan_algo_execution, run_backtest, run_depth_backtest, run_depth_experiment,
    run_experiment, run_multi_leg_backtest, run_multi_leg_experiment, run_multi_market_backtest,
    run_multi_market_depth_backtest, validate_dataset, verify_depth_run, verify_run,
    AlgoExecutionSpec, AppError, BacktestOutcome, BacktestSpec, CallerCapability, CompareRunsSpec,
    DatasetSpec, DepthBacktestSpec, MultiLegBacktestSpec, MultiMarketBacktestSpec,
    MultiMarketDepthBacktestSpec, RunContext, RunDepthExperimentSpec, RunExperimentSpec,
    RunMultiLegExperimentSpec, RunMultiMarketDepthExperimentSpec, RunMultiMarketExperimentSpec,
};
use qx_datastruct::{ArrowArray, ArrowSchema, BarFrame, FrameError};
use std::ffi::c_void;
use std::ptr::NonNull;

mod ashare_corporate_actions;
mod ashare_screen;
mod capital_allocation;
mod error_classification;
mod multi_account;
mod numeric_bridge;
mod provider_bridge;
mod risk_parity;
mod run_handle_helpers;
mod strategy_callback;
mod strategy_catalog;
fn frame_error(error: FrameError) -> PyErr {
    PyValueError::new_err(format!("BarFrame error: {error:?}"))
}
pyo3::create_exception!(
    _qianxing_native,
    QxAppError,
    PyException,
    "应用层用例失败。异常文本就是 AppError 的 JSON 文档（category/action/retry/safe_to_retry/correlation_id/source_code/message），与 `qx-cli app` 的 stderr 那行、`POST /app/*` 的响应体逐字节相同。"
);

pub(crate) fn app_error(error: AppError) -> PyErr {
    QxAppError::new_err(error.to_json())
}

/// 调用上下文：与 CLI/HTTP 两侧同形（R 档 + 身份串当 correlation id）。
/// Python cannot truthfully provide the CLI-only `QX_GIT_COMMIT` identity.
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

#[derive(serde::Deserialize)]
struct DatasetBundleBuildRequest {
    bundle_id: String,
    version: String,
    source: String,
    components: Vec<qx_data::DatasetComponentManifest>,
}

#[pyfunction]
fn app_assemble_dataset_bundle(payload: &str) -> PyResult<String> {
    let request: DatasetBundleBuildRequest =
        serde_json::from_str(payload).map_err(|error| PyValueError::new_err(error.to_string()))?;
    let bundle = qx_app::dataset_bundle::assemble_dataset_bundle(
        request.bundle_id,
        request.version,
        request.source,
        request.components,
    )
    .map_err(PyValueError::new_err)?;
    serde_json::to_string(&bundle).map_err(|error| PyValueError::new_err(error.to_string()))
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
fn app_plan_algo_execution(spec_json: &str) -> PyResult<String> {
    let spec = AlgoExecutionSpec::from_json(spec_json).map_err(app_error)?;
    plan_algo_execution(&spec, &research_context(&spec.parent_id))
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

#[pyfunction]
fn app_run_depth_experiment(spec_json: &str) -> PyResult<String> {
    let spec = RunDepthExperimentSpec::from_json(spec_json).map_err(app_error)?;
    run_depth_experiment(&spec, &research_context(&spec.experiment_id))
        .and_then(|result| result.to_json())
        .map_err(app_error)
}

#[pyfunction]
fn app_start_depth_experiment(spec_json: &str) -> PyResult<PyRunHandle> {
    let spec = RunDepthExperimentSpec::from_json(spec_json).map_err(app_error)?;
    let context = research_context(&spec.experiment_id);
    Ok(PyRunHandle {
        inner: NativeRun::DepthExperiment(spec.start(context)),
    })
}

#[pyfunction]
fn app_run_multi_leg_backtest(spec_json: &str) -> PyResult<String> {
    let spec = MultiLegBacktestSpec::from_json(spec_json).map_err(app_error)?;
    run_multi_leg_backtest(&spec, &research_context(&spec.run_id))
        .and_then(|outcome| outcome.to_json())
        .map_err(app_error)
}

#[pyfunction]
fn app_start_multi_leg_backtest(spec_json: &str) -> PyResult<PyRunHandle> {
    let spec = MultiLegBacktestSpec::from_json(spec_json).map_err(app_error)?;
    let context = research_context(&spec.run_id);
    Ok(PyRunHandle {
        inner: NativeRun::MultiLeg(spec.start(context)),
    })
}

#[pyfunction]
fn app_run_multi_market_backtest(spec_json: &str) -> PyResult<String> {
    let spec = MultiMarketBacktestSpec::from_json(spec_json).map_err(app_error)?;
    run_multi_market_backtest(&spec, &research_context(&spec.run_id))
        .and_then(|outcome| outcome.to_json())
        .map_err(app_error)
}

#[pyfunction]
fn app_start_multi_market_backtest(spec_json: &str) -> PyResult<PyRunHandle> {
    let spec = MultiMarketBacktestSpec::from_json(spec_json).map_err(app_error)?;
    let context = research_context(&spec.run_id);
    Ok(PyRunHandle {
        inner: NativeRun::MultiMarket(spec.start(context)),
    })
}

#[pyfunction]
fn app_run_multi_market_depth_backtest(spec_json: &str) -> PyResult<String> {
    let spec = MultiMarketDepthBacktestSpec::from_json(spec_json).map_err(app_error)?;
    run_multi_market_depth_backtest(&spec, &research_context(&spec.run_id))
        .and_then(|outcome| outcome.to_json())
        .map_err(app_error)
}

#[pyfunction]
fn app_start_multi_market_depth_backtest(spec_json: &str) -> PyResult<PyRunHandle> {
    let spec = MultiMarketDepthBacktestSpec::from_json(spec_json).map_err(app_error)?;
    let context = research_context(&spec.run_id);
    Ok(PyRunHandle {
        inner: NativeRun::MultiMarketDepth(spec.start(context)),
    })
}

#[pyfunction]
fn app_run_multi_leg_experiment(spec_json: &str) -> PyResult<String> {
    let spec = RunMultiLegExperimentSpec::from_json(spec_json).map_err(app_error)?;
    run_multi_leg_experiment(&spec, &research_context(&spec.experiment_id))
        .and_then(|result| result.to_json())
        .map_err(app_error)
}

#[pyfunction]
fn app_start_multi_leg_experiment(spec_json: &str) -> PyResult<PyRunHandle> {
    let spec = RunMultiLegExperimentSpec::from_json(spec_json).map_err(app_error)?;
    let context = research_context(&spec.experiment_id);
    Ok(PyRunHandle {
        inner: NativeRun::MultiLegExperiment(spec.start(context)),
    })
}

#[pyfunction]
fn app_run_multi_market_experiment(spec_json: &str) -> PyResult<String> {
    let spec = RunMultiMarketExperimentSpec::from_json(spec_json).map_err(app_error)?;
    qx_app::run_multi_market_experiment(&spec, &research_context(&spec.experiment_id))
        .and_then(|result| result.to_json())
        .map_err(app_error)
}

#[pyfunction]
fn app_start_multi_market_experiment(spec_json: &str) -> PyResult<PyRunHandle> {
    let spec = RunMultiMarketExperimentSpec::from_json(spec_json).map_err(app_error)?;
    let context = research_context(&spec.experiment_id);
    Ok(PyRunHandle {
        inner: NativeRun::MultiMarketExperiment(spec.start(context)),
    })
}

#[pyfunction]
fn app_run_multi_market_depth_experiment(spec_json: &str) -> PyResult<String> {
    let spec = RunMultiMarketDepthExperimentSpec::from_json(spec_json).map_err(app_error)?;
    qx_app::run_multi_market_depth_experiment(&spec, &research_context(&spec.experiment_id))
        .and_then(|result| result.to_json())
        .map_err(app_error)
}

#[pyfunction]
fn app_start_multi_market_depth_experiment(spec_json: &str) -> PyResult<PyRunHandle> {
    let spec = RunMultiMarketDepthExperimentSpec::from_json(spec_json).map_err(app_error)?;
    let context = research_context(&spec.experiment_id);
    Ok(PyRunHandle {
        inner: NativeRun::MultiMarketDepthExperiment(spec.start(context)),
    })
}

use run_handle_helpers::{NativeRun, PyRunHandle};

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
fn fingerprint_bars(payload: &str) -> PyResult<String> {
    let frame = BarFrame::from_json(payload).map_err(frame_error)?;
    let bars = (0..frame.ts.len())
        .map(|index| qx_data::Bar {
            instrument: frame.instrument.to_string(),
            timestamp: frame.ts[index],
            open_raw: frame.open_raw[index],
            high_raw: frame.high_raw[index],
            low_raw: frame.low_raw[index],
            close_raw: frame.close_raw[index],
            volume_raw: frame.volume_raw[index],
        })
        .collect::<Vec<_>>();
    qx_data::fingerprint_bars(&bars).map_err(PyValueError::new_err)
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
    let pointer = NonNull::from(Box::leak(Box::new(array))).cast::<c_void>();
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
    let pointer = NonNull::from(Box::leak(Box::new(schema))).cast::<c_void>();
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
    ashare_corporate_actions::register(module)?;
    error_classification::register(module)?;
    numeric_bridge::register(module)?;
    module.add_function(wrap_pyfunction!(frame_digest, module)?)?;
    module.add_function(wrap_pyfunction!(fingerprint_bars, module)?)?;
    module.add_function(wrap_pyfunction!(frame_to_json, module)?)?;
    ashare_screen::register(module)?;
    module.add_function(wrap_pyfunction!(owned_arrow_capsules, module)?)?;
    module.add_class::<OwnedArrowArray>()?;
    module.add_function(wrap_pyfunction!(owned_arrow_array, module)?)?;
    module.add_function(wrap_pyfunction!(app_validate_dataset, module)?)?;
    module.add_function(wrap_pyfunction!(app_assemble_dataset_bundle, module)?)?;
    module.add_function(wrap_pyfunction!(app_run_backtest, module)?)?;
    strategy_callback::register(module)?;
    multi_account::register(module)?;
    module.add_function(wrap_pyfunction!(app_start_backtest, module)?)?;
    module.add_function(wrap_pyfunction!(app_verify_run, module)?)?;
    module.add_function(wrap_pyfunction!(app_compare_runs, module)?)?;
    risk_parity::register(module)?;
    capital_allocation::register(module)?;
    module.add_function(wrap_pyfunction!(app_plan_algo_execution, module)?)?;
    module.add_function(wrap_pyfunction!(app_run_experiment, module)?)?;
    module.add_function(wrap_pyfunction!(app_start_experiment, module)?)?;
    module.add_function(wrap_pyfunction!(app_run_depth_backtest, module)?)?;
    module.add_function(wrap_pyfunction!(app_start_depth_backtest, module)?)?;
    module.add_function(wrap_pyfunction!(app_run_depth_experiment, module)?)?;
    module.add_function(wrap_pyfunction!(app_start_depth_experiment, module)?)?;
    module.add_function(wrap_pyfunction!(app_run_multi_leg_backtest, module)?)?;
    module.add_function(wrap_pyfunction!(app_start_multi_leg_backtest, module)?)?;
    module.add_function(wrap_pyfunction!(app_run_multi_market_backtest, module)?)?;
    module.add_function(wrap_pyfunction!(app_start_multi_market_backtest, module)?)?;
    module.add_function(wrap_pyfunction!(
        app_run_multi_market_depth_backtest,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(
        app_start_multi_market_depth_backtest,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(app_run_multi_leg_experiment, module)?)?;
    module.add_function(wrap_pyfunction!(app_start_multi_leg_experiment, module)?)?;
    module.add_function(wrap_pyfunction!(app_run_multi_market_experiment, module)?)?;
    module.add_function(wrap_pyfunction!(app_start_multi_market_experiment, module)?)?;
    module.add_function(wrap_pyfunction!(
        app_run_multi_market_depth_experiment,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(
        app_start_multi_market_depth_experiment,
        module
    )?)?;
    module.add_class::<PyRunHandle>()?;
    module.add_function(wrap_pyfunction!(app_verify_depth_run, module)?)?;
    provider_bridge::register(module)?;
    strategy_catalog::register(module)?;
    module.add("QxAppError", module.py().get_type::<QxAppError>())?;
    Ok(())
}
