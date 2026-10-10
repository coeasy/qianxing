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
    if VenueFamily::parse_option(worker.venue_id.as_deref()) != Some(VenueFamily::Binance) {
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
        .map(|order| order.account_id == expected_account && order.instrument.venue.is_binance())
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
    order_from_submit_command(command)
        .map_err(|error| format!("SubmitOrder 订单载荷非法: {error:?}"))?;
    validate_binance_submit_worker(worker)?;
    // 账户/交易所一致性只有一份判据：常驻循环用它分派命令，一次性提交也用它兜底。
    // 这里原先就地再写一遍 `account_id != expected_account || !is_binance()`，是同一规则的
    // 第二份实现——改口径时漏改一侧就会一边拦、一边放行。
    if !binance_submit_matches_worker(command, worker) {
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
    } else if !binance_submit_matches_worker(&command, &worker) {
        // 拓扑一致性排在风控规格之前：这道判据不读盘、不碰凭据，错了就该最先露出来。
        // 命令已经写了 Accepted，所以这里不能 `?` 提前退出，必须让上面那条终态回写
        // 把拒绝落成 Failed（同 `request_id` 重投才会撞幂等闸门而不是永久停在 Accepted）。
        Err("订单 account_id 或 instrument venue 与 worker 拓扑不一致".into())
    } else if let Err(reason) = require_worker_risk_spec(&worker, &command, Some(path)) {
        Err(reason)
    } else {
        binance_submit_action(
            &command,
            &worker,
            &pipeline_storage,
            &settlement_currency,
            path,
            now,
        )
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

/// Accepted 之后到终态回写之间不允许提前退出函数：失败按「这一手有没有可能已经进交易所」分两类。
///
/// - 还没写入任何事实（EventLog 名解析不了、日志或订单组存储打不开）：作为 `Err` 返回，让控制面
///   当场记下终态。这些位置原先是 `?`，函数直接退出，命令永远停在 `Accepted`，同 `request_id`
///   重投只会撞幂等闸门（V13 第三十一遍 ② #273，与 `paper-submit-order` 同一缺陷类）。
/// - 成交之后才失败（订单组快照同步、对冲恢复）：只在 stderr 告警，终态仍归交易所那一手的裁决 ——
///   把已经报出去的订单写成 `Failed` 比留着不管更危险，修复走恢复 worker。
fn binance_submit_action(
    command: &ControlCommand,
    worker: &WorkerConfig,
    pipeline_storage: &PipelineStorage,
    settlement_currency: &str,
    runtime_config_path: &Path,
    now: u64,
) -> Result<String, String> {
    let log_name = binance_event_log_name(worker).map_err(|error| {
        terminal_submit_rejection(format!("解析 Binance 执行 EventLog 名失败: {error}"))
    })?;
    let mut pipeline = pipeline_storage
        .open(log_name.clone(), settlement_currency.to_string())
        .map_err(|error| terminal_submit_rejection(format!("打开执行 EventLog 失败: {error}")))?;
    // 凭证与网关装配失败原本就是 `action` 的值，这里保持原文案不变。
    let auth = load_binance_worker_auth(worker);
    let mut venue = auth.and_then(|auth| new_binance_venue(worker, auth))?;
    // 屏障判定已下沉 `qx-execution` 网关（V10 §6.1）：CLI 不再预跑一遍，只把
    // 存储根解析出的组快照注入提交路径。
    let spread_store = open_spread_group_store(&pipeline_storage.root)
        .map_err(|error| terminal_submit_rejection(format!("打开多腿订单组存储失败: {error}")))?;
    let mut source_seq = 0_u64;
    let validator = recovery_order_validator(worker, &pipeline, Some(runtime_config_path));
    let result = execute_binance_submit_effect(
        command,
        worker,
        &mut pipeline,
        &mut venue,
        now,
        &mut source_seq,
        Some(runtime_config_path),
        Some(&spread_store),
    );
    if is_fail_closed_rejection(&result) {
        // 网关在写入任何事实之前就拒绝了：这条腿没有 EventLog 事实可
        // 归约，也没有敞口需要补偿，原样把 FAIL_CLOSED 原因交控制面。
        return result;
    }
    match pipeline_storage.open(log_name, settlement_currency.to_string()) {
        Ok(latest_pipeline) => {
            if let Err(error) = sync_spread_group_after_order(
                &pipeline_storage.root,
                &latest_pipeline,
                command,
                now,
            ) {
                eprintln!("[SpreadGroupSync] {error}");
            }
            match recover_spread_groups_for_venue(
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
            ) {
                Ok((_, recovery)) => {
                    for message in recovery {
                        eprintln!("[HedgeRecovery] {message}");
                    }
                }
                Err(error) => eprintln!("[HedgeRecovery] {error}"),
            }
        }
        Err(error) => {
            eprintln!("[SpreadGroupSync] 刷新 Binance 多腿订单组 EventLog 失败: {error}");
        }
    }
    result
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
    // 内联恢复扫描按共享节律自节流：循环仍每 100ms 扫一次队列，但两次恢复扫描之间的
    // 间隔走 `spread_recovery_poll_delay`，分组原地不动时不会每秒起灭 10 次 venue 重建。
    let mut recovery_stalls = 0_u32;
    let mut recovery_next_at = 0_u64;
    while !context.should_stop() {
        let now = runtime_timestamp_ms();
        // 命令队列租约/入队时间在秒域，控制面审计戳保持毫秒（见 `lease_clock`）。
        let lease_now = lease_clock(now);
        let control = control_store.load()?;
        let venue_id = worker.venue_id.as_deref().unwrap_or("BINANCE");
        if !dedicated_spread_recovery && now >= recovery_next_at {
            let pending_before = pending_spread_recovery_groups(&pipeline_storage.root, venue_id)?;
            if pending_before > 0 {
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
                let validator = recovery_order_validator(
                    &worker,
                    &recovery_pipeline,
                    Some(&runtime_config_path),
                );
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
                let pending_after =
                    pending_spread_recovery_groups(&pipeline_storage.root, venue_id)?;
                recovery_stalls = if pending_after >= pending_before {
                    recovery_stalls.saturating_add(1)
                } else {
                    0
                };
            } else {
                recovery_stalls = 0;
            }
            recovery_next_at = now.saturating_add(
                u64::try_from(spread_recovery_poll_delay(recovery_stalls).as_millis())
                    .unwrap_or(u64::MAX),
            );
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
