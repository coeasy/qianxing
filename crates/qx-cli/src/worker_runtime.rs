//! CCXT 与 Binance worker 启动的可恢复执行路径。

use crate::{
    configured_command_queue, configured_control_store, dedicated_spread_recovery_configured,
    read_runtime_config, reject_ashare_rules_on_submit_path, resolve_ccxt_config_path,
    resolve_worker_runtime_paths, run_binance_execution_worker, run_binance_market_worker,
    run_binance_reconcile_worker, run_binance_spread_recovery_worker, run_binance_user_worker,
    run_ccxt_execution_worker, run_ccxt_market_worker, run_ccxt_reconcile_worker,
    run_ccxt_spread_recovery_worker, run_ccxt_user_stream_worker, validate_ccxt_worker_binding,
    validate_worker_market_specs, venue_worker, PipelineStorage, VenueEntry,
};
use qx_runtime::WorkerRole;
use std::path::Path;

pub(crate) fn run_binance_worker_inner(
    path: &Path,
    worker_id: &str,
    once: bool,
) -> Result<(), String> {
    let config = read_runtime_config(path)?;
    let mut worker = venue_worker(&config, worker_id, &VenueEntry::BINANCE)?;
    validate_worker_market_specs(path, &worker)?;
    // 提交路径不认 A 股段：配了就当场拒，而不是收下配置再按无交易制度下单（V11 Q65）。
    reject_ashare_rules_on_submit_path(path, Some(&worker), "binance-worker")?;
    resolve_worker_runtime_paths(&mut worker, path);
    if once && matches!(worker.role, WorkerRole::MarketData | WorkerRole::UserStream) {
        return Err("binance-worker --once 只支持 execution 或 reconciler worker".into());
    }
    let pipeline_root = Path::new(&config.storage.data_dir).to_path_buf();
    let pipeline_storage = PipelineStorage::from_config(&config)?;
    // Paper 行情桥要按账户身份判定记账币种，所以这里带全量 worker；桥自身再按角色过滤。
    let bridge_config = config.clone();
    let runtime_config_path = path.to_path_buf();
    let control_store = configured_control_store(&config)?;
    let command_queue = configured_command_queue(&config, &pipeline_root)?;
    let dedicated_spread_recovery = dedicated_spread_recovery_configured(&config, &worker);
    let worker_role = worker.role;
    let registered_id = worker.id.clone();
    let worker_for_run = worker.clone();
    qx_service::worker_host::run_worker(
        config,
        &registered_id,
        worker_role,
        "Binance",
        move |context| match worker_role {
            WorkerRole::MarketData => run_binance_market_worker(
                context,
                worker_for_run.clone(),
                pipeline_storage.clone(),
                bridge_config,
            ),
            WorkerRole::UserStream => run_binance_user_worker(
                context,
                worker_for_run.clone(),
                pipeline_storage.clone(),
                runtime_config_path,
            ),
            WorkerRole::Execution => run_binance_execution_worker(
                context,
                worker_for_run,
                pipeline_storage.clone(),
                control_store,
                command_queue,
                runtime_config_path,
                dedicated_spread_recovery,
                once,
            ),
            WorkerRole::SpreadRecovery => run_binance_spread_recovery_worker(
                context,
                worker_for_run,
                pipeline_storage,
                runtime_config_path,
                once,
            ),
            WorkerRole::Reconciler => run_binance_reconcile_worker(
                context,
                worker_for_run,
                &pipeline_root,
                pipeline_storage,
                &runtime_config_path,
                once,
            ),
            _ => Err("unsupported Binance worker role".into()),
        },
    )
}

pub(crate) fn run_ccxt_worker_inner(
    path: &Path,
    worker_id: &str,
    ccxt_config_path: &Path,
    once: bool,
) -> Result<(), String> {
    let config = read_runtime_config(path)?;
    let worker = venue_worker(&config, worker_id, &VenueEntry::CCXT)?;
    validate_worker_market_specs(path, &worker)?;
    reject_ashare_rules_on_submit_path(path, Some(&worker), "ccxt-worker")?;
    let dedicated_spread_recovery = dedicated_spread_recovery_configured(&config, &worker);
    let root = Path::new(&config.storage.data_dir).to_path_buf();
    let pipeline_storage = PipelineStorage::from_config(&config)?;
    let runtime_config_path = path.to_path_buf();
    let control_store = configured_control_store(&config)?;
    let queue = configured_command_queue(&config, &root)?;
    let ccxt_config_path = resolve_ccxt_config_path(path, &ccxt_config_path.to_string_lossy());
    validate_ccxt_worker_binding(&worker, Path::new(&ccxt_config_path))?;
    let registered_id = worker.id.clone();
    let worker_role = worker.role;
    qx_service::worker_host::run_worker(
        config,
        &registered_id,
        worker_role,
        "CCXT",
        move |context| match worker_role {
            WorkerRole::MarketData => run_ccxt_market_worker(
                context,
                worker,
                pipeline_storage.clone(),
                ccxt_config_path,
                runtime_config_path.clone(),
                once,
            ),
            WorkerRole::UserStream => run_ccxt_user_stream_worker(
                context,
                worker,
                pipeline_storage.clone(),
                ccxt_config_path,
                runtime_config_path.clone(),
                once,
            ),
            WorkerRole::Reconciler => run_ccxt_reconcile_worker(
                context,
                worker,
                &root,
                pipeline_storage.clone(),
                ccxt_config_path,
                runtime_config_path.clone(),
                once,
            ),
            WorkerRole::SpreadRecovery => run_ccxt_spread_recovery_worker(
                context,
                worker,
                pipeline_storage,
                ccxt_config_path,
                runtime_config_path,
                once,
            ),
            WorkerRole::Execution => run_ccxt_execution_worker(
                context,
                worker,
                pipeline_storage,
                control_store,
                queue,
                ccxt_config_path,
                runtime_config_path,
                dedicated_spread_recovery,
                once,
            ),
            _ => Err("unsupported CCXT worker role".into()),
        },
    )
}
