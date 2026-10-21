use crate::*;
#[cfg(test)]
pub(crate) use qx_service::reconcile_report::{persist_reconcile_report, ReconcileReportInput};
pub(crate) fn run_binance_reconcile_worker(
    context: qx_runtime::WorkerContext,
    worker: WorkerConfig,
    pipeline_root: &Path,
    pipeline_storage: PipelineStorage,
    runtime_config_path: &Path,
    once: bool,
) -> Result<(), String> {
    let runtime_config = read_runtime_config(runtime_config_path)?;
    qx_service::binance_reconcile_host::run_binance_reconcile_worker(
        context,
        worker,
        pipeline_root.to_path_buf(),
        pipeline_storage,
        runtime_config,
        once,
        |worker| {
            let auth = load_binance_worker_auth(worker)?;
            new_binance_venue(worker, auth)
        },
    )
}
