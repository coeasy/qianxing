use crate::*;

#[cfg(test)]
pub(crate) use qx_service::ccxt_market_host::ccxt_market_cycle_health;

/// CLI loads environment-specific paths; qx-service owns the CCXT market worker loop.
pub(crate) fn run_ccxt_market_worker(
    context: qx_runtime::WorkerContext,
    worker: WorkerConfig,
    pipeline_storage: PipelineStorage,
    ccxt_config_path: String,
    runtime_config_path: PathBuf,
    once: bool,
) -> Result<(), String> {
    let runtime_config = read_runtime_config(&runtime_config_path)?;
    let live_specs = live_strategy_bar_specs(&runtime_config, &runtime_config_path, &worker)?;
    qx_service::ccxt_market_host::run_ccxt_market_worker(
        qx_service::ccxt_market_host::CcxtMarketWorkerInput {
            context,
            worker,
            pipeline_storage,
            ccxt_config_path,
            python: python_interpreter(),
            runtime_config,
            live_specs,
            once,
        },
    )
}
