use crate::*;

pub(crate) use qx_service::binance_execution_host::binance_submit_matches_worker;

/// 执行一条经过 ControlPlane 审计的 Binance SubmitOrder 命令。
///
/// 该入口是显式副作用命令：配置、命令权限、订单账户/交易所和 EventLog
/// 都先校验；订单事实先写入 `OrderSubmitted`，Venue 返回的 Accepted/Fill
/// 再按同一管线归约。网络返回未知时，EventLog 会保留 Submitted 状态，
/// 后续只能通过对账恢复，绝不自动重试补单。
pub(crate) fn validate_binance_submit_worker(worker: &WorkerConfig) -> Result<&str, String> {
    if !worker.enabled
        || !matches!(
            worker.role,
            WorkerRole::UserStream | WorkerRole::Execution | WorkerRole::Reconciler
        )
    {
        return Err(format!(
            "worker {} 必须是已启用的 Binance 用户流、执行或对账角色",
            worker.id
        ));
    }
    if VenueFamily::parse_option(worker.venue_id.as_deref()) != Some(VenueFamily::Binance) {
        return Err(format!("worker {} 不是 Binance Venue", worker.id));
    }
    worker
        .account_id
        .as_deref()
        .ok_or_else(|| format!("worker {} 缺少 account_id", worker.id))
}

pub(crate) fn run_binance_submit_order(
    path: &Path,
    worker_id: &str,
    command_path: &Path,
) -> Result<(), String> {
    let config = read_runtime_config(path)?;
    let mut worker = config
        .workers
        .iter()
        .find(|worker| worker.id == worker_id)
        .cloned()
        .ok_or_else(|| format!("找不到 worker: {worker_id}"))?;
    resolve_worker_runtime_paths(&mut worker, path);
    validate_binance_submit_worker(&worker)?;
    // 与 worker 入口同一条闸门：一次性提交也不能把 A 股段收下就什么都不做（V11 Q65）。
    reject_ashare_rules_on_submit_path(path, Some(&worker), "binance-submit-order")?;
    let settlement_currency = account_worker_settlement_currency(&config, &worker)?;
    let command: ControlCommand = serde_json::from_str(
        &std::fs::read_to_string(command_path)
            .map_err(|error| format!("读取 SubmitOrder 命令失败: {error}"))?,
    )
    .map_err(|error| format!("SubmitOrder 命令 JSON 无效: {error}"))?;
    order_from_submit_command(&command)
        .map_err(|error| format!("SubmitOrder 订单载荷非法: {error:?}"))?;
    let pipeline_storage = PipelineStorage::from_config(&config)?;
    let now = runtime_timestamp_ms();
    let store = configured_control_store(&config)?;
    let outcome =
        qx_service::one_shot_submit::run_one_shot_submit(&store, command.clone(), now, || {
            if !binance_submit_matches_worker(&command, &worker) {
                // 拓扑一致性排在风控规格之前：这道判据不读盘、不碰凭据，错了就该最先露出来。
                Err("订单 account_id 或 instrument venue 与 worker 拓扑不一致".into())
            } else if let Err(reason) = require_worker_risk_spec(&worker, &command, Some(path)) {
                Err(reason)
            } else {
                qx_service::binance_execution_host::execute_binance_one_shot_order(
                    &command,
                    &worker,
                    &pipeline_storage,
                    &settlement_currency,
                    path,
                    &config.environment,
                    now,
                    |worker| {
                        let auth = load_binance_worker_auth(worker)?;
                        new_binance_venue(worker, auth)
                    },
                )
            }
        })?;
    println!(
        "[执行 · SubmitOrder] accepted={:?} final={:?} command_id={} result={}",
        outcome.accepted.status,
        outcome.terminal.status,
        command.command_id,
        outcome.terminal.result_code
    );
    outcome.into_result_code().map(|_| ())
}

/// 运行持久化控制命令队列的 Binance 执行 worker。
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_binance_execution_worker(
    context: qx_runtime::WorkerContext,
    worker: WorkerConfig,
    pipeline_storage: PipelineStorage,
    control_store: ControlStateBackend,
    queue: Arc<dyn ControlCommandQueueBackend>,
    runtime_config_path: PathBuf,
    dedicated_spread_recovery: bool,
    once: bool,
) -> Result<(), String> {
    let runtime_config = read_runtime_config(&runtime_config_path)?;
    qx_service::binance_execution_host::run_binance_execution_worker(
        qx_service::binance_execution_host::BinanceExecutionWorkerInput {
            context,
            worker,
            pipeline_storage,
            control_store,
            queue,
            runtime_config,
            runtime_config_path,
            dedicated_spread_recovery,
            once,
            create_venue: |worker: &WorkerConfig| {
                let auth = load_binance_worker_auth(worker)?;
                new_binance_venue(worker, auth)
            },
        },
    )
}
