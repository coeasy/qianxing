use crate::*;

pub(crate) fn run_ccxt_reconcile_worker(
    context: qx_runtime::WorkerContext,
    worker: WorkerConfig,
    pipeline_root: &Path,
    pipeline_storage: PipelineStorage,
    ccxt_config_path: String,
    runtime_config_path: PathBuf,
    once: bool,
) -> Result<(), String> {
    let runtime_config = read_runtime_config(&runtime_config_path)?;
    let python = python_interpreter();
    qx_service::ccxt_reconcile_host::run_ccxt_reconcile_worker(
        qx_service::ccxt_reconcile_host::CcxtReconcileWorkerInput {
            context,
            worker,
            pipeline_root: pipeline_root.to_path_buf(),
            pipeline_storage,
            runtime_config,
            python,
            ccxt_config_path,
            runtime_config_path,
            once,
        },
    )
}

#[cfg(test)]
pub(crate) use qx_service::ccxt_reconcile::{
    ccxt_local_order_issue, ccxt_reconcile_round, ccxt_reconcile_service_status,
};
