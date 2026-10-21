use crate::*;

pub(crate) fn run_binance_market_worker(
    context: qx_runtime::WorkerContext,
    worker: WorkerConfig,
    pipeline_storage: PipelineStorage,
    config: RuntimeConfig,
) -> Result<(), String> {
    qx_service::venue_market_host::run_binance_market_worker(
        context,
        worker,
        pipeline_storage,
        config,
    )
}
pub(crate) fn run_binance_user_worker(
    context: qx_runtime::WorkerContext,
    worker: WorkerConfig,
    pipeline_storage: PipelineStorage,
    runtime_config_path: PathBuf,
) -> Result<(), String> {
    let initial_auth = load_binance_worker_auth(&worker)?;
    let venue = new_binance_venue(&worker, initial_auth)?;
    let runtime_config = read_runtime_config(&runtime_config_path)?;
    qx_service::binance_user_stream_host::run_binance_user_stream_worker(
        context,
        worker,
        pipeline_storage,
        runtime_config,
        runtime_config_path,
        venue,
        load_binance_worker_auth,
    )
}
