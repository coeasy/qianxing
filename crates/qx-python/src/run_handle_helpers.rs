use super::app_error;
use pyo3::prelude::*;
use qx_app::{
    AppError, BacktestOutcome, DepthBacktestOutcome, MultiAccountBacktestResult,
    MultiAccountDepthBacktestResult, MultiLegBacktestOutcome, MultiMarketBacktestOutcome,
    RunDepthExperimentResult, RunExperimentResult, RunHandle, RunMultiLegExperimentResult,
    RunStatus,
};
use std::time::{Duration, Instant};

pub(super) fn events_json(run: &NativeRun, after_seq: u64) -> Result<String, AppError> {
    match run {
        NativeRun::Bar(handle) => handle.events_json_after(after_seq),
        NativeRun::Depth(handle) => handle.events_json_after(after_seq),
        NativeRun::DepthExperiment(handle) => handle.events_json_after(after_seq),
        NativeRun::Experiment(handle) => handle.events_json_after(after_seq),
        NativeRun::MultiLeg(handle) => handle.events_json_after(after_seq),
        NativeRun::MultiMarket(handle) => handle.events_json_after(after_seq),
        NativeRun::MultiLegExperiment(handle) => handle.events_json_after(after_seq),
        NativeRun::MultiAccount(handle) => handle.events_json_after(after_seq),
        NativeRun::MultiAccountDepth(handle) => handle.events_json_after(after_seq),
    }
}

pub(super) fn status_name(status: RunStatus) -> &'static str {
    match status {
        RunStatus::Running => "running",
        RunStatus::Cancelling => "cancelling",
        RunStatus::Succeeded => "succeeded",
        RunStatus::Cancelled => "cancelled",
        RunStatus::Failed => "failed",
    }
}

pub(super) enum NativeRun {
    Bar(RunHandle<BacktestOutcome>),
    Depth(RunHandle<DepthBacktestOutcome>),
    DepthExperiment(RunHandle<RunDepthExperimentResult>),
    Experiment(RunHandle<RunExperimentResult>),
    MultiLeg(RunHandle<MultiLegBacktestOutcome>),
    MultiMarket(RunHandle<MultiMarketBacktestOutcome>),
    MultiLegExperiment(RunHandle<RunMultiLegExperimentResult>),
    MultiAccount(RunHandle<MultiAccountBacktestResult>),
    MultiAccountDepth(RunHandle<MultiAccountDepthBacktestResult>),
}

#[pyclass(name = "RunHandle")]
pub(super) struct PyRunHandle {
    pub(super) inner: NativeRun,
}

#[pymethods]
impl PyRunHandle {
    #[getter]
    fn run_id(&self) -> &str {
        match &self.inner {
            NativeRun::Bar(handle) => handle.run_id(),
            NativeRun::Depth(handle) => handle.run_id(),
            NativeRun::DepthExperiment(handle) => handle.run_id(),
            NativeRun::Experiment(handle) => handle.run_id(),
            NativeRun::MultiLeg(handle) => handle.run_id(),
            NativeRun::MultiMarket(handle) => handle.run_id(),
            NativeRun::MultiLegExperiment(handle) => handle.run_id(),
            NativeRun::MultiAccount(handle) => handle.run_id(),
            NativeRun::MultiAccountDepth(handle) => handle.run_id(),
        }
    }

    #[getter]
    fn status(&mut self) -> &'static str {
        status_name(match &mut self.inner {
            NativeRun::Bar(handle) => handle.status(),
            NativeRun::Depth(handle) => handle.status(),
            NativeRun::DepthExperiment(handle) => handle.status(),
            NativeRun::Experiment(handle) => handle.status(),
            NativeRun::MultiLeg(handle) => handle.status(),
            NativeRun::MultiMarket(handle) => handle.status(),
            NativeRun::MultiLegExperiment(handle) => handle.status(),
            NativeRun::MultiAccount(handle) => handle.status(),
            NativeRun::MultiAccountDepth(handle) => handle.status(),
        })
    }

    fn cancel(&self) {
        match &self.inner {
            NativeRun::Bar(handle) => handle.cancel(),
            NativeRun::Depth(handle) => handle.cancel(),
            NativeRun::DepthExperiment(handle) => handle.cancel(),
            NativeRun::Experiment(handle) => handle.cancel(),
            NativeRun::MultiLeg(handle) => handle.cancel(),
            NativeRun::MultiMarket(handle) => handle.cancel(),
            NativeRun::MultiLegExperiment(handle) => handle.cancel(),
            NativeRun::MultiAccount(handle) => handle.cancel(),
            NativeRun::MultiAccountDepth(handle) => handle.cancel(),
        }
    }

    fn events_json(&self, after_seq: u64) -> PyResult<String> {
        events_json(&self.inner, after_seq).map_err(app_error)
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
                NativeRun::DepthExperiment(handle) => handle.status(),
                NativeRun::Experiment(handle) => handle.status(),
                NativeRun::MultiLeg(handle) => handle.status(),
                NativeRun::MultiMarket(handle) => handle.status(),
                NativeRun::MultiLegExperiment(handle) => handle.status(),
                NativeRun::MultiAccount(handle) => handle.status(),
                NativeRun::MultiAccountDepth(handle) => handle.status(),
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
                .map(|result| result.and_then(|outcome| outcome.to_json()))
                .transpose()
                .map_err(app_error),
            NativeRun::Depth(handle) => handle
                .try_take_result()
                .map(|result| result.and_then(|outcome| outcome.to_json()))
                .transpose()
                .map_err(app_error),
            NativeRun::DepthExperiment(handle) => handle
                .try_take_result()
                .map(|result| result.and_then(|outcome| outcome.to_json()))
                .transpose()
                .map_err(app_error),
            NativeRun::Experiment(handle) => handle
                .try_take_result()
                .map(|result| result.and_then(|outcome| outcome.to_json()))
                .transpose()
                .map_err(app_error),
            NativeRun::MultiLeg(handle) => handle
                .try_take_result()
                .map(|result| result.and_then(|outcome| outcome.to_json()))
                .transpose()
                .map_err(app_error),
            NativeRun::MultiMarket(handle) => handle
                .try_take_result()
                .map(|result| result.and_then(|outcome| outcome.to_json()))
                .transpose()
                .map_err(app_error),
            NativeRun::MultiLegExperiment(handle) => handle
                .try_take_result()
                .map(|result| result.and_then(|outcome| outcome.to_json()))
                .transpose()
                .map_err(app_error),
            NativeRun::MultiAccount(handle) => handle
                .try_take_result()
                .map(|result| result.and_then(|outcome| outcome.to_json()))
                .transpose()
                .map_err(app_error),
            NativeRun::MultiAccountDepth(handle) => handle
                .try_take_result()
                .map(|result| result.and_then(|outcome| outcome.to_json()))
                .transpose()
                .map_err(app_error),
        }
    }
}
