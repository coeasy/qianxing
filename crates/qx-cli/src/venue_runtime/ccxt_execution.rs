use crate::*;

pub(crate) use qx_service::ccxt_execution_host::ccxt_submit_matches_worker;

/// 通过公共 Python CCXT Worker 执行已审计 SubmitOrder。
///
/// CLI 仅负责读取运行时配置和连接器设置；队列、租约、执行、恢复与终态回写
/// 由 qx-service 持有。
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_ccxt_execution_worker(
    context: qx_runtime::WorkerContext,
    worker: WorkerConfig,
    pipeline_storage: PipelineStorage,
    control_store: ControlStateBackend,
    queue: Arc<dyn ControlCommandQueueBackend>,
    ccxt_config_path: String,
    runtime_config_path: PathBuf,
    dedicated_spread_recovery: bool,
    once: bool,
) -> Result<(), String> {
    let runtime_config = read_runtime_config(&runtime_config_path)?;
    let python = python_interpreter();
    qx_service::ccxt_execution_host::run_ccxt_execution_worker(
        qx_service::ccxt_execution_host::CcxtExecutionWorkerInput {
            context,
            worker,
            pipeline_storage,
            control_store,
            queue,
            runtime_config,
            python,
            ccxt_config_path,
            runtime_config_path,
            dedicated_spread_recovery,
            once,
        },
    )
}

/// 使用公共 CCXT Pro `watch_orders` 接收账户订单变化，再通过同一 CCXT
/// REST `fetch_order` 补齐成交/费用并归约到 Runtime/EventLog。Pro 流只负责
/// 唤醒和提供 remote id，不直接修改本地订单或 Ledger。
pub(crate) fn run_ccxt_user_stream_worker(
    context: qx_runtime::WorkerContext,
    worker: WorkerConfig,
    pipeline_storage: PipelineStorage,
    ccxt_config_path: String,
    runtime_config_path: PathBuf,
    once: bool,
) -> Result<(), String> {
    let python = python_interpreter();
    let runtime_config = read_runtime_config(&runtime_config_path)?;
    qx_service::ccxt_user_stream_host::run_ccxt_user_stream_worker(
        qx_service::ccxt_user_stream_host::CcxtUserStreamWorkerInput {
            context,
            worker,
            pipeline_storage,
            runtime_config,
            python,
            ccxt_config_path,
            runtime_config_path,
            once,
        },
    )
}
