use crate::*;

pub(crate) fn ccxt_submit_matches_worker(command: &ControlCommand, worker: &WorkerConfig) -> bool {
    let Some(expected_account) = worker.account_id.as_deref() else {
        return false;
    };
    let Some(expected_venue) = worker.venue_id.as_deref() else {
        return false;
    };
    order_from_submit_command(command)
        .map(|order| {
            order.account_id == expected_account
                && order
                    .instrument
                    .venue
                    .as_str()
                    .eq_ignore_ascii_case(expected_venue)
        })
        .unwrap_or(false)
}

/// 通过公共 Python CCXT Worker 执行已审计 SubmitOrder。
///
/// `worker.endpoint` 约定为 CCXT JSON 配置文件路径，Python 解释器使用
/// `QX_PYTHON` 环境变量或默认 `python`。Rust 侧只负责命令队列、EventLog、
/// OMS 和 Ledger，交易所协议全部由公共 CCXT 完成。
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
    if worker.role != WorkerRole::Execution {
        return Err(format!("worker {} 不是 CCXT Execution worker", worker.id));
    }
    let settlement_currency = account_worker_currency_from_path(&runtime_config_path, &worker)?;
    let python = python_interpreter();
    let owner = worker.id.clone();
    context.mark(
        qx_runtime::ServiceStatus::Ready,
        format!(
            "ccxt execution queue polling venue={}",
            worker.venue_id.as_deref().unwrap_or("")
        ),
        Some(runtime_timestamp_ms()),
    )?;
    while !context.should_stop() {
        let now = runtime_timestamp_ms();
        // 命令队列租约/入队时间在秒域，控制面审计戳保持毫秒（见 `lease_clock`）。
        let lease_now = lease_clock(now);
        let control = control_store.load()?;
        let venue_id = worker.venue_id.as_deref().unwrap_or("ccxt");
        if !dedicated_spread_recovery
            && has_pending_spread_recovery(&pipeline_storage.root, venue_id)?
        {
            let mut recovery_pipeline = pipeline_storage
                .open(
                    required_account_event_log(&worker)?,
                    settlement_currency.clone(),
                )
                .map_err(|error| format!("打开 CCXT 多腿恢复 EventLog 失败: {error}"))?;
            let client = CcxtProcessClient::spawn(&python, &ccxt_config_path, None)
                .map_err(|error| format!("启动公共 CCXT 恢复 Worker 失败: {error}"))?;
            let recovery_venue = CcxtProcessVenue::new(venue_id, Box::new(client));
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
                && ccxt_submit_matches_worker(command, &worker)
        }) {
            queue
                .enqueue_command(command.clone(), lease_now)
                .map_err(|error| format!("补入 CCXT SubmitOrder 队列失败: {error:?}"))?;
        }
        for queued in queue
            .available_commands(lease_now)
            .map_err(|error| format!("读取 CCXT SubmitOrder 队列失败: {error:?}"))?
        {
            if context.should_stop() {
                break;
            }
            let command = queued.command.clone();
            if !ccxt_submit_matches_worker(&command, &worker) {
                continue;
            }
            let lease = match queue.claim_command(command.command_id, &owner, lease_now, 30) {
                Ok(lease) => lease,
                Err(StorageError::LeaseHeld { .. }) => continue,
                Err(error) => return Err(format!("领取 CCXT SubmitOrder 租约失败: {error:?}")),
            };
            if command_is_final(&control, command.command_id) {
                queue
                    .ack_command_at(command.command_id, &owner, lease.fencing_token, lease_now)
                    .map_err(|error| format!("清理已终态 CCXT SubmitOrder 失败: {error:?}"))?;
                continue;
            }
            let action = if command.dry_run {
                Ok("DRY_RUN_VALIDATED".into())
            } else if let Err(reason) =
                require_worker_risk_spec(&worker, &command, Some(&runtime_config_path))
            {
                Err(reason)
            } else {
                // 跨腿屏障由 `qx-execution` 网关自行判定；CLI 只注入存储根解析出的组快照。
                let spread_store = open_spread_group_store(&pipeline_storage.root)?;
                let mut pipeline = pipeline_storage
                    .open(
                        required_account_event_log(&worker)?,
                        settlement_currency.clone(),
                    )
                    .map_err(|error| format!("打开 CCXT EventLog 失败: {error}"))?;
                let client = CcxtProcessClient::spawn(&python, &ccxt_config_path, None)
                    .map_err(|error| format!("启动公共 CCXT Worker 失败: {error}"))?;
                let mut venue = CcxtProcessVenue::new(
                    worker.venue_id.clone().unwrap_or_else(|| "ccxt".into()),
                    Box::new(client),
                );
                let mut source_seq = 0_u64;
                let validator =
                    recovery_order_validator(&worker, &pipeline, Some(&runtime_config_path));
                let requested_order = order_from_submit_command(&command)
                    .map_err(|error| format!("CCXT 订单载荷非法: {error:?}"))?;
                if requested_order.limit.is_none() && worker.instrument_spec_path.is_some() {
                    let ticker = venue
                        .stream_call(serde_json::json!({
                            "op": "fetch_ticker",
                            "instrument": requested_order.instrument.to_string(),
                        }))
                        .map_err(|error| format!("CCXT 风控参考价查询失败: {error:?}"))?;
                    let ticker = ticker
                        .get("ticker")
                        .ok_or_else(|| "CCXT ticker 响应缺少 ticker".to_string())?;
                    let bid = Price::from_raw(raw_json_i128(ticker, "bid_raw")?);
                    let ask = Price::from_raw(raw_json_i128(ticker, "ask_raw")?);
                    let bid_qty = Quantity::from_raw(raw_json_i128(ticker, "bid_qty_raw")?);
                    let ask_qty = Quantity::from_raw(raw_json_i128(ticker, "ask_qty_raw")?);
                    let event_ts = ticker
                        .get("timestamp_ms")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(now);
                    source_seq = source_seq.saturating_add(1);
                    pipeline
                        .ingest(RuntimeEventEnvelope::venue(
                            RuntimeExternalEvent::MarketQuote {
                                instrument: requested_order.instrument.clone(),
                                bid,
                                ask,
                                bid_qty,
                                ask_qty,
                            },
                            event_ts,
                            now,
                            source_seq,
                            format!("{}:risk-ticker:{}", worker.id, requested_order.client_id),
                        ))
                        .map_err(|error| format!("写入 CCXT 风控参考行情失败: {error:?}"))?;
                }
                let result = execute_submit_order_with_worker_risk(
                    &command,
                    &worker,
                    &mut venue,
                    &mut pipeline,
                    now,
                    &mut source_seq,
                    Some(&runtime_config_path),
                    Some(&spread_store),
                );
                if is_fail_closed_rejection(&result) {
                    // 网关在写入任何事实之前就拒绝了：没有腿订单可归约、没有敞口要
                    // 补偿，原样把 FAIL_CLOSED 原因交给控制面记录。
                    drop(venue);
                } else {
                    let latest_pipeline = pipeline_storage
                        .open(
                            required_account_event_log(&worker)?,
                            settlement_currency.clone(),
                        )
                        .map_err(|error| format!("刷新 CCXT 多腿订单组 EventLog 失败: {error}"))?;
                    sync_spread_group_after_order(
                        &pipeline_storage.root,
                        &latest_pipeline,
                        &command,
                        now,
                    )?;
                    let (venue, recovery) = recover_spread_groups_for_venue(
                        SpreadRecoveryContext {
                            root: &pipeline_storage.root,
                            venue_id: worker.venue_id.as_deref().unwrap_or("ccxt"),
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
                .map_err(|error| format!("回写 CCXT SubmitOrder 终态失败: {error}"))?;
            let record =
                record_result.map_err(|error| format!("CCXT SubmitOrder 执行失败: {error:?}"))?;
            queue
                .ack_command_at(command.command_id, &owner, lease.fencing_token, lease_now)
                .map_err(|error| format!("确认 CCXT SubmitOrder 失败: {error:?}"))?;
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
    if worker.role != WorkerRole::UserStream {
        return Err(format!("worker {} 不是 CCXT UserStream worker", worker.id));
    }
    let python = python_interpreter();
    let log_name = required_account_event_log(&worker)?;
    let mut pipeline = pipeline_storage
        .open(
            log_name,
            account_worker_currency_from_path(&runtime_config_path, &worker)?,
        )
        .map_err(|error| format!("打开 CCXT UserStream EventLog 失败: {error}"))?;
    context.mark(
        qx_runtime::ServiceStatus::Ready,
        format!(
            "ccxt pro user stream venue={}",
            worker.venue_id.as_deref().unwrap_or("")
        ),
        Some(runtime_timestamp_ms()),
    )?;
    let client = CcxtProcessClient::spawn(&python, &ccxt_config_path, None)
        .map_err(|error| format!("启动公共 CCXT Pro Worker 失败: {error}"))?;
    // 空闲回话上限取自身读窗的 4/5：让"这一窗没有事件"先答回来，读窗到期就重新只
    // 表示子进程真的卡住 —— 而不是一个没有成交的账户在十个窗口后被具名放弃。
    let watch_idle_ms = client.timeout_ms().saturating_mul(4) / 5;
    let mut venue = CcxtProcessVenue::new(
        worker.venue_id.clone().unwrap_or_else(|| "ccxt".into()),
        Box::new(client),
    );
    let mut source_seq = pipeline
        .log()
        .events()
        .last()
        .map(|event| event.source_seq)
        .unwrap_or(0);
    let mut reconnect_streak = 0_u32;
    let mut idle_windows = 0_u64;
    loop {
        if context.should_stop() {
            break;
        }
        pipeline
            .refresh()
            .map_err(|error| format!("刷新 CCXT UserStream EventLog 失败: {error:?}"))?;
        let received_ts = runtime_timestamp_ms();
        let result = match venue.stream_call(serde_json::json!({
            "op": "watch_orders",
            "instrument": null,
            "received_ts": received_ts,
            "wait_ms": watch_idle_ms,
        })) {
            Ok(result) => {
                // 子进程答上了话：连胜清零，长期健康的流不会累积历史故障（V11 K1）。
                reconnect_streak = 0;
                result
            }
            Err(error) => {
                let detail = format!("{error:?}");
                if detail.contains("[unsupported]") || detail.contains("[authentication]") {
                    return Err(format!("CCXT Pro 用户流不可用: {detail}"));
                }
                reconnect_streak = reconnect_streak.saturating_add(1);
                context.mark(
                    qx_runtime::ServiceStatus::Degraded,
                    format!(
                        "ccxt pro user stream reconnecting failures={reconnect_streak}: {detail}"
                    ),
                    Some(received_ts),
                )?;
                if reconnect_streak >= CCXT_DEAD_CYCLE_BUDGET {
                    context.mark(
                        qx_runtime::ServiceStatus::Failed,
                        format!(
                            "ccxt pro user stream 连续 {reconnect_streak} 次重连全部失败，预算已用尽: {detail}"
                        ),
                        Some(received_ts),
                    )?;
                    return Err(format!(
                        "CCXT Pro 用户流连续 {reconnect_streak} 次重连失败，已放弃重连公共 Worker"
                    ));
                }
                thread::sleep(ccxt_respawn_delay(reconnect_streak));
                let replacement = CcxtProcessClient::spawn(&python, &ccxt_config_path, None)
                    .map_err(|spawn_error| {
                        format!("重连公共 CCXT Pro Worker 失败: {spawn_error}")
                    })?;
                venue.replace_rpc(Box::new(replacement));
                continue;
            }
        };
        let event = result
            .get("event")
            .ok_or_else(|| "CCXT Pro watch_orders 响应缺少 event".to_string())?;
        if event.get("stream").and_then(serde_json::Value::as_str) != Some("orders") {
            return Err("CCXT Pro 用户流返回了非 orders 事件".into());
        }
        // 子进程答上了话、只是这一窗没有订单事件：不占重连预算，否则一个当天没有
        // 成交的账户会在几个 30 秒窗口后被具名放弃；也不清 idle 计数之外的东西。
        if ccxt_watch_reply_is_idle(event) {
            idle_windows = idle_windows.saturating_add(1);
            context.mark(
                qx_runtime::ServiceStatus::Degraded,
                format!("ccxt pro user stream idle consecutive_windows={idle_windows}"),
                Some(received_ts),
            )?;
            if once {
                break;
            }
            continue;
        }
        idle_windows = 0;
        let events = event
            .get("events")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| "CCXT Pro orders 事件缺少 events".to_string())?;
        // 有 event、既没有 events 也不带 idle 的空回话必须节奏化：子进程秒答一张空表时，
        // 这条链上再没有第二处等待，循环就成了吃满一颗 CPU 的热转。
        if events.is_empty() {
            thread::sleep(Duration::from_millis(1_000));
        }
        let mut matched = 0_usize;
        let mut reduced = 0_usize;
        let mut reconcile_errors = 0_usize;
        for update in events {
            let remote_id = update
                .get("order_id")
                .and_then(serde_json::Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| "CCXT Pro order 事件缺少 order_id".to_string())?;
            let client_order_id = update
                .get("client_order_id")
                .and_then(serde_json::Value::as_str)
                .and_then(|value| value.parse::<u64>().ok());
            let local = pipeline.orders().into_iter().find(|order| {
                client_order_id == Some(order.client_id)
                    || pipeline
                        .venue_order_id(order.client_id)
                        .as_deref()
                        .is_some_and(|value| value == remote_id)
            });
            let Some(order) = local else {
                // 未知 remote order 不允许自动注册或下单补偿；交给 Reconcile worker
                // 按交易所 open orders/账单做完整快照核对。
                continue;
            };
            matched = matched.saturating_add(1);
            venue
                .restore_order(order.clone(), remote_id)
                .map_err(|error| format!("恢复 CCXT Pro remote order 失败: {error:?}"))?;
            let event_ts = update
                .get("timestamp_ms")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(received_ts);
            let venue_events = match venue.sync_order(order.client_id, event_ts) {
                Ok(events) => events,
                Err(error) => {
                    reconcile_errors = reconcile_errors.saturating_add(1);
                    let reason = format!("{error:?}");
                    EventLogReconcilePort::new(&mut pipeline, &worker.id, received_ts, &mut source_seq)
                        .at_event_ts(event_ts)
                        .with_tag("sync-error")
                        .require_reconcile(order.client_id, &reason)
                        .map_err(|ingest_error| {
                            format!(
                                "归约 CCXT Pro order 失败 {error:?}，且无法写入 ReconcileRequired: {ingest_error}"
                            )
                        })?;
                    continue;
                }
            };
            let reduced_events = if let Some(spec) =
                load_worker_instrument_spec(&worker, &order, Some(&runtime_config_path))?
            {
                ingest_venue_events_with_spec(
                    &mut pipeline,
                    venue_events,
                    &worker.id,
                    received_ts,
                    &mut source_seq,
                    &spec,
                )?
            } else {
                ingest_venue_events(
                    &mut pipeline,
                    venue_events,
                    &worker.id,
                    received_ts,
                    &mut source_seq,
                )?
            };
            reduced = reduced.saturating_add(reduced_events);
        }
        context.mark(
            qx_runtime::ServiceStatus::Ready,
            format!(
                "ccxt pro orders matched={matched} reduced={reduced} reconcile_errors={reconcile_errors}"
            ),
            Some(received_ts),
        )?;
        context.heartbeat(received_ts)?;
        if once {
            break;
        }
    }
    context.mark(
        qx_runtime::ServiceStatus::Stopped,
        format!("ccxt pro user stream stopped idle_windows={idle_windows}"),
        Some(runtime_timestamp_ms()),
    )?;
    Ok(())
}
