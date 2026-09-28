use crate::*;

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
    if worker
        .venue_id
        .as_deref()
        .map(|venue| venue.to_ascii_lowercase().contains("binance"))
        != Some(true)
    {
        return Err(format!("worker {} 不是 Binance Venue", worker.id));
    }
    worker
        .account_id
        .as_deref()
        .ok_or_else(|| format!("worker {} 缺少 account_id", worker.id))
}

pub(crate) fn binance_submit_matches_worker(
    command: &ControlCommand,
    worker: &WorkerConfig,
) -> bool {
    let expected_account = worker.account_id.as_deref().unwrap_or_default();
    order_from_submit_command(command)
        .map(|order| {
            order.account_id == expected_account
                && order
                    .instrument
                    .venue
                    .as_str()
                    .eq_ignore_ascii_case("BINANCE")
        })
        .unwrap_or(false)
}

#[allow(clippy::too_many_arguments)] // V10 P0a/P1b：风控与组存储形参由编译器逐个点名，不合并成参数包。
pub(crate) fn execute_binance_submit_effect(
    command: &ControlCommand,
    worker: &WorkerConfig,
    pipeline: &mut LiveEventPipeline,
    venue: &mut BinanceSpotVenue,
    now: u64,
    source_seq: &mut u64,
    runtime_config_path: Option<&Path>,
    spread_store: Option<&dyn SpreadOrderGroupStore>,
) -> Result<String, String> {
    let requested_order = order_from_submit_command(command)
        .map_err(|error| format!("SubmitOrder 订单载荷非法: {error:?}"))?;
    let expected_account = validate_binance_submit_worker(worker)?;
    if requested_order.account_id != expected_account
        || !requested_order
            .instrument
            .venue
            .as_str()
            .eq_ignore_ascii_case("BINANCE")
    {
        return Err("订单 account_id 或 instrument venue 与 worker 拓扑不一致".into());
    }

    venue
        .restore_orders(pipeline.orders())
        .map_err(|error| format!("装载待提交订单失败: {error:?}"))?;
    execute_submit_order_with_worker_risk(
        command,
        worker,
        venue,
        pipeline,
        now,
        source_seq,
        runtime_config_path,
        spread_store,
    )
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
    let (_, accepted_result) = store
        .transact(|plane| plane.submit_as(command.clone(), Permission::Trading, now))
        .map_err(|error| format!("持久化 SubmitOrder Accepted 失败: {error:?}"))?;
    let accepted = accepted_result
        .map_err(|error| format!("SubmitOrder 未通过控制面权限/幂等校验: {error:?}"))?;

    let action = if command.dry_run {
        Ok("DRY_RUN_VALIDATED".into())
    } else if let Err(reason) = require_worker_risk_spec(&worker, &command, Some(path)) {
        Err(reason)
    } else {
        let mut pipeline = pipeline_storage
            .open(
                binance_event_log_name(&worker)?,
                settlement_currency.clone(),
            )
            .map_err(|error| format!("打开执行 EventLog 失败: {error}"))?;
        let auth = load_binance_worker_auth(&worker);
        // 屏障判定已下沉 `qx-execution` 网关（V10 §6.1）：CLI 不再预跑一遍，只把
        // 存储根解析出的组快照注入提交路径。
        let spread_store = open_spread_group_store(&pipeline_storage.root)?;
        match auth.and_then(|auth| new_binance_venue(&worker, auth)) {
            Ok(mut venue) => {
                let mut source_seq = 0_u64;
                let validator = recovery_order_validator(&worker, &pipeline, Some(path));
                let result = execute_binance_submit_effect(
                    &command,
                    &worker,
                    &mut pipeline,
                    &mut venue,
                    now,
                    &mut source_seq,
                    Some(path),
                    Some(&spread_store),
                );
                if is_fail_closed_rejection(&result) {
                    // 网关在写入任何事实之前就拒绝了：这条腿没有 EventLog 事实可
                    // 归约，也没有敞口需要补偿，原样把 FAIL_CLOSED 原因交控制面。
                    drop(venue);
                } else {
                    let latest_pipeline = pipeline_storage
                        .open(
                            binance_event_log_name(&worker)?,
                            settlement_currency.clone(),
                        )
                        .map_err(|error| {
                            format!("刷新 Binance 多腿订单组 EventLog 失败: {error}")
                        })?;
                    sync_spread_group_after_order(
                        &pipeline_storage.root,
                        &latest_pipeline,
                        &command,
                        now,
                    )?;
                    let (venue, recovery) = recover_spread_groups_for_venue(
                        SpreadRecoveryContext {
                            root: &pipeline_storage.root,
                            venue_id: worker.venue_id.as_deref().unwrap_or("BINANCE"),
                            accept_any_venue: false,
                            order_validator: Some(&validator),
                            pipeline: &mut pipeline,
                            worker_id: &worker.id,
                            now,
                            source_seq: &mut source_seq,
                        },
                        venue,
                    )?;
                    for message in recovery {
                        eprintln!("[HedgeRecovery] {message}");
                    }
                    drop(venue);
                }
                result
            }
            Err(error) => Err(error),
        }
    };
    let (_, record_result) = store
        .transact(|plane| plane.execute(command.command_id, now, |_| action.clone()))
        .map_err(|error| format!("持久化 SubmitOrder 终态失败: {error:?}"))?;
    let record = record_result.map_err(|error| format!("执行控制命令失败: {error:?}"))?;
    println!(
        "[执行 · SubmitOrder] accepted={:?} final={:?} command_id={} result={}",
        accepted.status, record.status, command.command_id, record.result_code
    );
    if record.status == qx_control::CommandStatus::Failed {
        return Err(record.result_code);
    }
    Ok(())
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
    let owner = worker.id.clone();
    let settlement_currency = account_worker_currency_from_path(&runtime_config_path, &worker)?;
    context.mark(
        qx_runtime::ServiceStatus::Ready,
        "execution queue polling",
        Some(runtime_timestamp_ms()),
    )?;
    while !context.should_stop() {
        let now = runtime_timestamp_ms();
        // 命令队列租约/入队时间在秒域，控制面审计戳保持毫秒（见 `lease_clock`）。
        let lease_now = lease_clock(now);
        let control = control_store.load()?;
        let venue_id = worker.venue_id.as_deref().unwrap_or("BINANCE");
        if !dedicated_spread_recovery
            && has_pending_spread_recovery(&pipeline_storage.root, venue_id)?
        {
            let mut recovery_pipeline = pipeline_storage
                .open(
                    binance_event_log_name(&worker)?,
                    settlement_currency.clone(),
                )
                .map_err(|error| format!("打开 Binance 多腿恢复 EventLog 失败: {error}"))?;
            let auth = load_binance_worker_auth(&worker)?;
            let mut recovery_venue = new_binance_venue(&worker, auth)?;
            recovery_venue
                .restore_orders(recovery_pipeline.orders())
                .map_err(|error| format!("恢复 Binance 多腿订单状态失败: {error:?}"))?;
            let mut recovery_seq = recovery_pipeline
                .log()
                .events()
                .last()
                .map(|event| event.source_seq)
                .unwrap_or(0);
            let validator =
                recovery_order_validator(&worker, &recovery_pipeline, Some(&runtime_config_path));
            let (_, diagnostics) = recover_spread_groups_for_venue(
                SpreadRecoveryContext {
                    root: &pipeline_storage.root,
                    venue_id,
                    accept_any_venue: false,
                    order_validator: Some(&validator),
                    pipeline: &mut recovery_pipeline,
                    worker_id: &worker.id,
                    now,
                    source_seq: &mut recovery_seq,
                },
                recovery_venue,
            )?;
            for message in diagnostics {
                eprintln!("[HedgeRecovery] {message}");
            }
        }
        for command in control.pending().filter(|command| {
            matches!(&command.kind, CommandKind::SubmitOrder)
                && binance_submit_matches_worker(command, &worker)
        }) {
            queue
                .enqueue_command(command.clone(), lease_now)
                .map_err(|error| format!("补入 SubmitOrder 队列失败: {error:?}"))?;
        }
        for queued in queue
            .available_commands(lease_now)
            .map_err(|error| format!("读取 SubmitOrder 队列失败: {error:?}"))?
        {
            if context.should_stop() {
                break;
            }
            let command = queued.command.clone();
            if !binance_submit_matches_worker(&command, &worker) {
                continue;
            }
            let lease = match queue.claim_command(command.command_id, &owner, lease_now, 30) {
                Ok(lease) => lease,
                Err(qx_storage::StorageError::LeaseHeld { .. }) => continue,
                Err(error) => return Err(format!("领取 SubmitOrder 租约失败: {error:?}")),
            };
            if command_is_final(&control, command.command_id) {
                queue
                    .ack_command_at(command.command_id, &owner, lease.fencing_token, lease_now)
                    .map_err(|error| format!("清理已终态 SubmitOrder 失败: {error:?}"))?;
                continue;
            }
            let action = if command.dry_run {
                Ok("DRY_RUN_VALIDATED".into())
            } else if let Err(reason) =
                require_worker_risk_spec(&worker, &command, Some(&runtime_config_path))
            {
                Err(reason)
            } else {
                let mut pipeline = pipeline_storage
                    .open(
                        binance_event_log_name(&worker)?,
                        settlement_currency.clone(),
                    )
                    .map_err(|error| format!("打开执行 EventLog 失败: {error}"))?;
                let auth = load_binance_worker_auth(&worker)?;
                let mut venue = new_binance_venue(&worker, auth)?;
                venue
                    .restore_orders(pipeline.orders())
                    .map_err(|error| format!("恢复执行订单状态失败: {error:?}"))?;
                // 屏障判定已下沉网关；这里只注入由存储根解析出的组快照。
                let spread_store = open_spread_group_store(&pipeline_storage.root)?;
                let mut source_seq = 0_u64;
                let validator =
                    recovery_order_validator(&worker, &pipeline, Some(&runtime_config_path));
                let result = execute_binance_submit_effect(
                    &command,
                    &worker,
                    &mut pipeline,
                    &mut venue,
                    now,
                    &mut source_seq,
                    Some(&runtime_config_path),
                    Some(&spread_store),
                );
                if is_fail_closed_rejection(&result) {
                    // 未写入任何事实：无腿可归约、无敞口可补偿，保留 FAIL_CLOSED 文案。
                    drop(venue);
                } else {
                    let latest_pipeline = pipeline_storage
                        .open(
                            binance_event_log_name(&worker)?,
                            settlement_currency.clone(),
                        )
                        .map_err(|error| {
                            format!("刷新 Binance 多腿订单组 EventLog 失败: {error}")
                        })?;
                    sync_spread_group_after_order(
                        &pipeline_storage.root,
                        &latest_pipeline,
                        &command,
                        now,
                    )?;
                    let (venue, recovery) = recover_spread_groups_for_venue(
                        SpreadRecoveryContext {
                            root: &pipeline_storage.root,
                            venue_id: worker.venue_id.as_deref().unwrap_or("BINANCE"),
                            accept_any_venue: false,
                            order_validator: Some(&validator),
                            pipeline: &mut pipeline,
                            worker_id: &worker.id,
                            now,
                            source_seq: &mut source_seq,
                        },
                        venue,
                    )?;
                    for message in recovery {
                        eprintln!("[HedgeRecovery] {message}");
                    }
                    drop(venue);
                }
                result
            };
            let (_, record_result) = control_store
                .transact(|plane| plane.execute(command.command_id, now, |_| action.clone()))
                .map_err(|error| format!("回写 SubmitOrder 终态失败: {error:?}"))?;
            let record = record_result.map_err(|error| format!("执行控制命令失败: {error:?}"))?;
            queue
                .ack_command_at(command.command_id, &owner, lease.fencing_token, lease_now)
                .map_err(|error| format!("确认 SubmitOrder 队列失败: {error:?}"))?;
            context.mark(
                if record.status == qx_control::CommandStatus::Executed {
                    qx_runtime::ServiceStatus::Ready
                } else {
                    qx_runtime::ServiceStatus::Degraded
                },
                format!(
                    "command_id={} status={:?}",
                    command.command_id, record.status
                ),
                Some(now),
            )?;
        }
        context.heartbeat(now)?;
        if once {
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}
