use super::NativeRun;
use qx_app::{AppError, RunStatus};

pub(super) fn events_json(run: &NativeRun, after_seq: u64) -> Result<String, AppError> {
    match run {
        NativeRun::Bar(handle) => handle.events_json_after(after_seq),
        NativeRun::Depth(handle) => handle.events_json_after(after_seq),
        NativeRun::DepthExperiment(handle) => handle.events_json_after(after_seq),
        NativeRun::Experiment(handle) => handle.events_json_after(after_seq),
        NativeRun::MultiLeg(handle) => handle.events_json_after(after_seq),
        NativeRun::MultiLegExperiment(handle) => handle.events_json_after(after_seq),
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
