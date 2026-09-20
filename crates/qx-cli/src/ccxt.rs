//! 公共 CCXT 连接器：行情拉取、市场规格转换、余额/持仓/资金流事实映射与 worker。

use super::*;

fn ccxt_event_log_name(worker: &WorkerConfig) -> String {
    let venue = worker.venue_id.as_deref().unwrap_or("unknown");
    let prefix = if venue.to_ascii_lowercase().contains("binance") {
        "binance"
    } else {
        "ccxt"
    };
    format!(
        "{prefix}-{}-{venue}-events",
        worker.account_id.as_deref().unwrap_or("unknown")
    )
}

fn ccxt_market_event_log_name(worker: &WorkerConfig) -> String {
    format!("ccxt-market-{}-events", worker.id)
}

pub(crate) fn validate_ccxt_worker_binding(
    worker: &WorkerConfig,
    ccxt_config_path: &Path,
) -> Result<(), String> {
    let expected = worker
        .venue_id
        .as_deref()
        .ok_or_else(|| format!("CCXT worker {} 缺少 venue_id", worker.id))?;
    let payload = std::fs::read_to_string(ccxt_config_path).map_err(|error| {
        format!(
            "读取 CCXT 配置失败 path={} error={error}",
            ccxt_config_path.display()
        )
    })?;
    let config: serde_json::Value =
        serde_json::from_str::<serde_json::Value>(&payload).map_err(|error| {
            format!(
                "解析 CCXT 配置失败 path={} error={error}",
                ccxt_config_path.display()
            )
        })?;
    let configured = config
        .get("exchange_id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            format!(
                "CCXT 配置缺少 exchange_id path={}",
                ccxt_config_path.display()
            )
        })?;
    if !configured.eq_ignore_ascii_case(expected) {
        return Err(format!(
            "CCXT worker {} venue_id={} 与配置 exchange_id={} 不一致",
            worker.id, expected, configured
        ));
    }
    if let Some(credentials) = config
        .get("credential_env")
        .filter(|value| !value.is_null())
    {
        let credentials = credentials.as_object().ok_or_else(|| {
            format!(
                "CCXT 配置 credential_env 必须是对象或 null path={}",
                ccxt_config_path.display()
            )
        })?;
        for key in ["api_key", "secret", "password", "uid"] {
            if let Some(value) = credentials.get(key) {
                if !value.is_string() || value.as_str().is_some_and(|value| value.trim().is_empty())
                {
                    return Err(format!(
                        "CCXT 配置 credential_env.{key} 必须是非空环境变量名 path={}",
                        ccxt_config_path.display()
                    ));
                }
            }
        }
    }
    Ok(())
}

fn ccxt_submit_matches_worker(command: &ControlCommand, worker: &WorkerConfig) -> bool {
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
fn run_ccxt_execution_worker(
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
    let python = std::env::var("QX_PYTHON").unwrap_or_else(|_| "python".into());
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
        let control = control_store.load()?;
        let venue_id = worker.venue_id.as_deref().unwrap_or("ccxt");
        if !dedicated_spread_recovery
            && has_pending_spread_recovery(&pipeline_storage.root, venue_id)?
        {
            let mut recovery_pipeline = pipeline_storage
                .open(
                    ccxt_event_log_name(&worker),
                    worker.settlement_currency.as_deref().unwrap_or("USDT"),
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
                .enqueue_command(command.clone(), now)
                .map_err(|error| format!("补入 CCXT SubmitOrder 队列失败: {error:?}"))?;
        }
        for queued in queue
            .available_commands(now)
            .map_err(|error| format!("读取 CCXT SubmitOrder 队列失败: {error:?}"))?
        {
            if context.should_stop() {
                break;
            }
            let command = queued.command.clone();
            if !ccxt_submit_matches_worker(&command, &worker) {
                continue;
            }
            let lease = match queue.claim_command(command.command_id, &owner, now, 30) {
                Ok(lease) => lease,
                Err(StorageError::LeaseHeld { .. }) => continue,
                Err(error) => return Err(format!("领取 CCXT SubmitOrder 租约失败: {error:?}")),
            };
            if command_is_final(&control, command.command_id) {
                queue
                    .ack_command_at(command.command_id, &owner, lease.fencing_token, now)
                    .map_err(|error| format!("清理已终态 CCXT SubmitOrder 失败: {error:?}"))?;
                continue;
            }
            let action = if command.dry_run {
                Ok("DRY_RUN_VALIDATED".into())
            } else {
                let mut pipeline = pipeline_storage
                    .open(
                        ccxt_event_log_name(&worker),
                        worker.settlement_currency.as_deref().unwrap_or("USDT"),
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
                );
                let latest_pipeline = pipeline_storage
                    .open(
                        ccxt_event_log_name(&worker),
                        worker.settlement_currency.as_deref().unwrap_or("USDT"),
                    )
                    .map_err(|error| format!("刷新 CCXT 多腿订单组 EventLog 失败: {error}"))?;
                sync_spread_group_after_order(&pipeline_storage.root, &latest_pipeline, &command)?;
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
                result
            };
            let (_, record_result) = control_store
                .transact(|plane| plane.execute(command.command_id, now, |_| action.clone()))
                .map_err(|error| format!("回写 CCXT SubmitOrder 终态失败: {error}"))?;
            let record =
                record_result.map_err(|error| format!("CCXT SubmitOrder 执行失败: {error:?}"))?;
            queue
                .ack_command_at(command.command_id, &owner, lease.fencing_token, now)
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
fn run_ccxt_user_stream_worker(
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
    let python = std::env::var("QX_PYTHON").unwrap_or_else(|_| "python".into());
    let log_name = ccxt_event_log_name(&worker);
    let mut pipeline = pipeline_storage
        .open(
            &log_name,
            worker.settlement_currency.as_deref().unwrap_or("USDT"),
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
        })) {
            Ok(result) => result,
            Err(error) => {
                let detail = format!("{error:?}");
                if detail.contains("[unsupported]") || detail.contains("[authentication]") {
                    return Err(format!("CCXT Pro 用户流不可用: {detail}"));
                }
                context.mark(
                    qx_runtime::ServiceStatus::Degraded,
                    format!("ccxt pro user stream reconnecting: {detail}"),
                    Some(received_ts),
                )?;
                thread::sleep(Duration::from_millis(500));
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
        let events = event
            .get("events")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| "CCXT Pro orders 事件缺少 events".to_string())?;
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
                    source_seq = source_seq.saturating_add(1);
                    pipeline
                        .ingest(RuntimeEventEnvelope::venue(
                            RuntimeExternalEvent::ReconcileRequired {
                                client_order_id: order.client_id,
                            },
                            event_ts,
                            received_ts,
                            source_seq,
                            format!("{}:sync-error:{}", worker.id, order.client_id),
                        ))
                        .map_err(|ingest_error| {
                            format!(
                                "归约 CCXT Pro order 失败 {error:?}，且无法写入 ReconcileRequired: {ingest_error:?}"
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
        "ccxt pro user stream stopped",
        Some(runtime_timestamp_ms()),
    )?;
    Ok(())
}

#[derive(Clone)]
struct LiveStrategyBarSpec {
    instrument: InstrumentId,
    timeframe: String,
    timeframe_ms: u64,
    max_staleness_ms: u64,
    history_limit: usize,
    closed_only: bool,
    snapshot_path: PathBuf,
}

pub(crate) fn timeframe_to_ms(timeframe: &str) -> Result<u64, String> {
    let value = timeframe.trim().to_ascii_lowercase();
    if value.len() < 2 {
        return Err(format!("CCXT timeframe 非法: {timeframe}"));
    }
    let (number, unit) = value.split_at(value.len() - 1);
    let number = number
        .parse::<u64>()
        .map_err(|_| format!("CCXT timeframe 数值非法: {timeframe}"))?;
    if number == 0 {
        return Err(format!("CCXT timeframe 必须为正数: {timeframe}"));
    }
    let unit_ms = match unit {
        "s" => 1_000,
        "m" => 60_000,
        "h" => 3_600_000,
        "d" => 86_400_000,
        "w" => 604_800_000,
        _ => return Err(format!("不支持的 CCXT timeframe 单位: {timeframe}")),
    };
    number
        .checked_mul(unit_ms)
        .ok_or_else(|| format!("CCXT timeframe 溢出: {timeframe}"))
}

fn live_strategy_bar_specs(
    config: &RuntimeConfig,
    runtime_config_path: &Path,
    worker: &WorkerConfig,
) -> Result<Vec<LiveStrategyBarSpec>, String> {
    let strategies = if config.strategies.is_empty() {
        vec![config.strategy.clone()]
    } else {
        config.strategies.clone()
    };
    let mut specs = Vec::new();
    for strategy in strategies {
        if !strategy.live_enabled {
            continue;
        }
        let instrument_text = strategy
            .instrument
            .as_deref()
            .ok_or_else(|| "实时策略必须配置 instrument".to_string())?;
        let instrument = InstrumentId::parse(instrument_text)
            .ok_or_else(|| format!("实时策略 instrument 非法: {instrument_text}"))?;
        let timeframe_ms = timeframe_to_ms(&strategy.live_timeframe)?;
        let max_staleness_ms = strategy
            .live_max_staleness_ms
            .unwrap_or_else(|| timeframe_ms.saturating_mul(3).max(timeframe_ms));
        let primary_matches = worker.symbols.iter().any(|symbol| {
            InstrumentId::parse(symbol).is_some_and(|configured| configured == instrument)
        });
        if primary_matches {
            let configured_path = strategy
                .bars_snapshot_path
                .as_deref()
                .ok_or_else(|| "实时策略必须配置 bars_snapshot_path".to_string())?;
            specs.push(LiveStrategyBarSpec {
                instrument,
                timeframe: strategy.live_timeframe.clone(),
                timeframe_ms,
                max_staleness_ms,
                history_limit: strategy.live_history_limit,
                closed_only: strategy.live_closed_only,
                snapshot_path: resolve_runtime_relative_path(runtime_config_path, configured_path),
            });
        }
        if let (Some(reference_text), Some(reference_path)) = (
            strategy.builtin_reference_instrument.as_deref(),
            strategy.builtin_reference_bars_snapshot_path.as_deref(),
        ) {
            let reference = InstrumentId::parse(reference_text)
                .ok_or_else(|| format!("实时策略对冲腿 instrument 非法: {reference_text}"))?;
            if worker.symbols.iter().any(|symbol| {
                InstrumentId::parse(symbol).is_some_and(|configured| configured == reference)
            }) {
                specs.push(LiveStrategyBarSpec {
                    instrument: reference,
                    timeframe: strategy.live_timeframe.clone(),
                    timeframe_ms,
                    max_staleness_ms,
                    history_limit: strategy.live_history_limit,
                    closed_only: strategy.live_closed_only,
                    snapshot_path: resolve_runtime_relative_path(
                        runtime_config_path,
                        reference_path,
                    ),
                });
            }
        }
    }
    specs.sort_by(|left, right| {
        left.instrument
            .to_string()
            .cmp(&right.instrument.to_string())
            .then(left.snapshot_path.cmp(&right.snapshot_path))
    });
    specs.dedup_by(|left, right| {
        left.instrument == right.instrument && left.snapshot_path == right.snapshot_path
    });
    Ok(specs)
}

pub(crate) fn live_frame_is_fresh(
    frame: &BarFrame,
    timeframe_ms: u64,
    closed_only: bool,
    max_staleness_ms: u64,
    now: u64,
) -> bool {
    let Some(&last_ts) = frame.ts.last() else {
        return false;
    };
    let freshness_ts = if closed_only {
        let Some(close_ts) = last_ts.checked_add(timeframe_ms) else {
            return false;
        };
        if close_ts > now {
            return false;
        }
        close_ts
    } else {
        if last_ts > now {
            return false;
        }
        last_ts
    };
    now.saturating_sub(freshness_ts) <= max_staleness_ms
}

fn closed_live_frame(
    frame: BarFrame,
    spec: &LiveStrategyBarSpec,
    now: u64,
) -> Result<Option<BarFrame>, String> {
    if frame.instrument != spec.instrument {
        return Err(format!(
            "实时 OHLCV instrument 不一致: expected={} actual={}",
            spec.instrument, frame.instrument
        ));
    }
    let visible = frame
        .ts
        .iter()
        .enumerate()
        .filter(|(_, ts)| {
            !spec.closed_only
                || ts
                    .checked_add(spec.timeframe_ms)
                    .is_some_and(|close_ts| close_ts <= now)
        })
        .collect::<Vec<_>>();
    if visible.is_empty() {
        return Ok(None);
    }
    let start = visible.len().saturating_sub(spec.history_limit);
    let visible = &visible[start..];
    let result = BarFrame {
        instrument: frame.instrument,
        source: frame.source,
        ts: visible.iter().map(|(_, ts)| **ts).collect(),
        open_raw: visible
            .iter()
            .map(|(index, _)| frame.open_raw[*index])
            .collect(),
        high_raw: visible
            .iter()
            .map(|(index, _)| frame.high_raw[*index])
            .collect(),
        low_raw: visible
            .iter()
            .map(|(index, _)| frame.low_raw[*index])
            .collect(),
        close_raw: visible
            .iter()
            .map(|(index, _)| frame.close_raw[*index])
            .collect(),
        volume_raw: visible
            .iter()
            .map(|(index, _)| frame.volume_raw[*index])
            .collect(),
    };
    result
        .validate()
        .map_err(|error| format!("实时 BarFrame 校验失败: {error:?}"))?;
    if !live_frame_is_fresh(
        &result,
        spec.timeframe_ms,
        spec.closed_only,
        spec.max_staleness_ms,
        now,
    ) {
        return Ok(None);
    }
    Ok(Some(result))
}

fn write_live_bar_snapshot(path: &Path, frame: &BarFrame) -> Result<u64, String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("实时 BarFrame 路径没有父目录: {}", path.display()))?;
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("创建实时 BarFrame 目录失败 {}: {error}", parent.display()))?;
    let payload = frame.to_json();
    let temporary = path.with_extension(format!("barframe.tmp.{}", std::process::id()));
    std::fs::write(&temporary, payload)
        .map_err(|error| format!("写入实时 BarFrame 临时文件失败: {error}"))?;
    if let Err(error) = std::fs::rename(&temporary, path) {
        // Windows 无法直接覆盖已有文件；只在替换失败时删除旧快照，
        // Strategy worker 遇到短暂缺文件会等待下一轮，不会读取半份 JSON。
        let _ = std::fs::remove_file(path);
        std::fs::rename(&temporary, path).map_err(|replacement| {
            format!(
                "提交实时 BarFrame 失败 {}: initial={error}; replacement={replacement}",
                path.display()
            )
        })?;
    }
    Ok(frame.digest())
}

/// 使用公共 CCXT REST ticker 和 OHLCV 轮询接入统一行情 EventLog，并将闭合
/// BarFrame 原子写入策略快照。Strategy worker 通过快照摘要触发幂等 JobQueue，
/// 因而不依赖 Scheduler 的固定周期，也不会因为未闭合 K 线反复下单。
fn run_ccxt_market_worker(
    context: qx_runtime::WorkerContext,
    worker: WorkerConfig,
    pipeline_storage: PipelineStorage,
    ccxt_config_path: String,
    runtime_config_path: PathBuf,
    once: bool,
) -> Result<(), String> {
    if worker.role != WorkerRole::MarketData {
        return Err(format!("worker {} 不是 CCXT MarketData worker", worker.id));
    }
    if worker.symbols.is_empty() {
        return Err(format!("worker {} 至少需要一个 CCXT instrument", worker.id));
    }
    let runtime_config = read_runtime_config(&runtime_config_path)?;
    let live_specs = live_strategy_bar_specs(&runtime_config, &runtime_config_path, &worker)?;
    let python = std::env::var("QX_PYTHON").unwrap_or_else(|_| "python".into());
    let mut client = CcxtProcessClient::spawn(&python, &ccxt_config_path, None)
        .map_err(|error| format!("启动公共 CCXT MarketData Worker 失败: {error}"))?;
    let mut pipeline = pipeline_storage
        .open(
            ccxt_market_event_log_name(&worker),
            worker.settlement_currency.as_deref().unwrap_or("USDT"),
        )
        .map_err(|error| format!("创建 CCXT 行情事件管线失败: {error}"))?;
    let mut paper_bridges = open_paper_market_bridges(&pipeline_storage, &runtime_config.workers)?;
    let instruments = worker
        .symbols
        .iter()
        .map(|symbol| {
            InstrumentId::parse(symbol)
                .ok_or_else(|| format!("worker {} instrument 非法: {symbol}", worker.id))
        })
        .collect::<Result<Vec<_>, _>>()?;
    context.mark(
        qx_runtime::ServiceStatus::Ready,
        format!(
            "ccxt ticker/ohlcv polling instruments={} live_snapshots={}",
            instruments.len(),
            live_specs.len()
        ),
        Some(runtime_timestamp_ms()),
    )?;
    let mut source_seq = 0_u64;
    let mut quotes = 0_u64;
    let mut bars_written = 0_u64;
    while !context.should_stop() {
        let cycle_now = runtime_timestamp_ms();
        for instrument in &instruments {
            let result = match client.call(serde_json::json!({
                "op": "fetch_ticker",
                "instrument": instrument.to_string(),
            })) {
                Ok(result) => result,
                Err(error) => {
                    context.mark(
                        qx_runtime::ServiceStatus::Degraded,
                        format!("CCXT ticker 暂时失败 {}: {error}; reconnecting", instrument),
                        Some(cycle_now),
                    )?;
                    client = CcxtProcessClient::spawn(&python, &ccxt_config_path, None).map_err(
                        |spawn_error| format!("重启公共 CCXT Worker 失败: {spawn_error}"),
                    )?;
                    thread::sleep(Duration::from_millis(500));
                    continue;
                }
            };
            let ticker = result
                .get("ticker")
                .ok_or_else(|| format!("CCXT ticker 响应缺少 ticker: {instrument}"))?;
            let bid = raw_json_i128(ticker, "bid_raw")?;
            let ask = raw_json_i128(ticker, "ask_raw")?;
            let bid_qty = raw_json_i128(ticker, "bid_qty_raw")?;
            let ask_qty = raw_json_i128(ticker, "ask_qty_raw")?;
            if bid <= 0 || ask <= 0 || bid > ask || bid_qty <= 0 || ask_qty <= 0 {
                return Err(format!("CCXT ticker 买卖价非法: {instrument}"));
            }
            let ts = ticker
                .get("timestamp_ms")
                .and_then(serde_json::Value::as_u64)
                .ok_or_else(|| format!("CCXT ticker 缺少 timestamp_ms: {instrument}"))?;
            let received_ts = runtime_timestamp_ms();
            source_seq = source_seq.saturating_add(1);
            pipeline
                .ingest(RuntimeEventEnvelope::market_quote(
                    instrument.clone(),
                    QuoteTick::new(
                        ts,
                        qx_core::Price::from_raw(bid),
                        qx_core::Quantity::from_raw(bid_qty),
                        qx_core::Price::from_raw(ask),
                        qx_core::Quantity::from_raw(ask_qty),
                        source_seq,
                    ),
                    received_ts,
                    source_seq,
                    format!("{}:ticker:{}", worker.id, source_seq),
                ))
                .map_err(|error| format!("CCXT 行情事实归约失败: {error:?}"))?;
            bridge_market_quote_to_paper(
                &mut paper_bridges,
                PaperMarketQuote {
                    source_worker_id: &worker.id,
                    instrument,
                    bid: qx_core::Price::from_raw(bid),
                    bid_qty: qx_core::Quantity::from_raw(bid_qty),
                    ask: qx_core::Price::from_raw(ask),
                    ask_qty: qx_core::Quantity::from_raw(ask_qty),
                    event_ts: ts,
                    receive_ts: received_ts,
                    source_seq,
                },
            )?;
            quotes = quotes.saturating_add(1);
            context.heartbeat(received_ts)?;
        }
        for spec in &live_specs {
            let start_ms = cycle_now.saturating_sub(
                spec.timeframe_ms
                    .saturating_mul(spec.history_limit.saturating_add(2) as u64),
            );
            let result = match client.call(serde_json::json!({
                "op": "fetch_ohlcv",
                "instrument": spec.instrument.to_string(),
                "timeframe": spec.timeframe,
                "start_ms": start_ms,
                "end_ms": cycle_now,
                "limit": spec.history_limit.saturating_add(2),
            })) {
                Ok(result) => result,
                Err(error) => {
                    context.mark(
                        qx_runtime::ServiceStatus::Degraded,
                        format!(
                            "CCXT OHLCV 暂时失败 {}: {error}; reconnecting",
                            spec.instrument
                        ),
                        Some(cycle_now),
                    )?;
                    client = CcxtProcessClient::spawn(&python, &ccxt_config_path, None).map_err(
                        |spawn_error| format!("重启公共 CCXT Worker 失败: {spawn_error}"),
                    )?;
                    thread::sleep(Duration::from_millis(500));
                    continue;
                }
            };
            let frame_value = result
                .get("frame")
                .ok_or_else(|| format!("CCXT OHLCV 响应缺少 frame: {}", spec.instrument))?;
            let frame = BarFrame::from_json(
                &serde_json::to_string(frame_value)
                    .map_err(|error| format!("编码 CCXT OHLCV frame 失败: {error}"))?,
            )
            .map_err(|error| format!("CCXT OHLCV BarFrame 非法 {}: {error:?}", spec.instrument))?;
            if let Some(closed) = closed_live_frame(frame, spec, cycle_now)? {
                write_live_bar_snapshot(&spec.snapshot_path, &closed)?;
                bars_written = bars_written.saturating_add(1);
            }
        }
        context.mark(
            qx_runtime::ServiceStatus::Ready,
            format!(
                "ccxt live market healthy quotes={} bar_snapshots={}",
                quotes, bars_written
            ),
            Some(cycle_now),
        )?;
        if once {
            break;
        }
        thread::sleep(Duration::from_millis(1_000));
    }
    context.mark(
        qx_runtime::ServiceStatus::Stopped,
        format!("ccxt market worker stopped quotes={quotes} bar_snapshots={bars_written}"),
        Some(runtime_timestamp_ms()),
    )?;
    Ok(())
}

fn raw_json_i128(value: &serde_json::Value, key: &str) -> Result<i128, String> {
    if let Some(value) = value.get(key).and_then(serde_json::Value::as_i64) {
        return Ok(i128::from(value));
    }
    if let Some(value) = value.get(key).and_then(serde_json::Value::as_u64) {
        return Ok(i128::from(value));
    }
    if let Some(value) = value.get(key).and_then(serde_json::Value::as_str) {
        return value
            .parse::<i128>()
            .map_err(|_| format!("CCXT 字段不是定点整数: {key}"));
    }
    Err(format!("CCXT 字段不是定点整数: {key}"))
}

fn ccxt_money(value: &serde_json::Value, field: &str) -> Result<Money, String> {
    let text = value
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| value.to_string());
    Money::from_dec(&text).ok_or_else(|| format!("CCXT 余额字段非法: {field}={text}"))
}

fn ccxt_balance_facts(value: &serde_json::Value) -> Result<Vec<AccountBalance>, String> {
    let balance = value
        .get("balance")
        .ok_or_else(|| "CCXT balance 响应缺少 balance".to_string())?;
    let free = balance
        .get("free")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| "CCXT balance 缺少 free map".to_string())?;
    let used = balance
        .get("used")
        .and_then(serde_json::Value::as_object)
        .cloned()
        .unwrap_or_default();
    let debt = balance
        .get("debt")
        .or_else(|| balance.get("borrowed"))
        .and_then(serde_json::Value::as_object)
        .cloned()
        .unwrap_or_default();
    let mut assets = BTreeSet::new();
    assets.extend(free.keys().cloned());
    assets.extend(used.keys().cloned());
    assets.extend(debt.keys().cloned());
    let mut facts = Vec::new();
    for asset in assets {
        let free_amount = free
            .get(&asset)
            .map(|value| ccxt_money(value, &format!("free.{asset}")))
            .transpose()?
            .unwrap_or(Money::ZERO);
        let locked_amount = used
            .get(&asset)
            .map(|value| ccxt_money(value, &format!("used.{asset}")))
            .transpose()?
            .unwrap_or(Money::ZERO);
        let borrowed_amount = debt
            .get(&asset)
            .map(|value| ccxt_money(value, &format!("debt.{asset}")))
            .transpose()?
            .unwrap_or(Money::ZERO);
        if free_amount.is_zero() && locked_amount.is_zero() && borrowed_amount.is_zero() {
            continue;
        }
        facts.push(AccountBalance {
            asset,
            free: free_amount,
            locked: locked_amount,
            borrowed: borrowed_amount,
        });
    }
    Ok(facts)
}

pub(crate) fn ccxt_position_facts(
    value: &serde_json::Value,
    venue_id: &str,
) -> Result<Vec<VenuePositionSnapshot>, String> {
    let positions = value
        .get("positions")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "CCXT positions 响应缺少 positions 数组".to_string())?;
    let mut facts = Vec::with_capacity(positions.len());
    for position in positions {
        let symbol = position
            .get("symbol")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| "CCXT position 缺少 symbol".to_string())?;
        let instrument = InstrumentId::parse(&format!("{symbol}.{}", venue_id.to_uppercase()))
            .ok_or_else(|| format!("CCXT position symbol 不是合法 InstrumentId: {symbol}"))?;
        let contracts = raw_json_i128(position, "contracts_raw")?;
        if contracts < 0 {
            return Err(format!("CCXT position contracts_raw 不能为负: {symbol}"));
        }
        if contracts == 0 {
            continue;
        }
        let side = position
            .get("side")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("long")
            .to_ascii_lowercase();
        let quantity = if side == "short" {
            contracts
                .checked_neg()
                .ok_or_else(|| "CCXT short position 数量溢出".to_string())?
        } else {
            contracts
        };
        let optional_price = |key: &str| -> Result<Option<Price>, String> {
            let Some(value) = position.get(key) else {
                return Ok(None);
            };
            if value.is_null() {
                return Ok(None);
            }
            let raw = raw_json_i128(position, key)?;
            if raw <= 0 {
                return Err(format!("CCXT position {key} 必须为正: {symbol}"));
            }
            Ok(Some(Price::from_raw(raw)))
        };
        let optional_money = |key: &str| -> Result<Money, String> {
            let Some(value) = position.get(key) else {
                return Ok(Money::ZERO);
            };
            if value.is_null() {
                return Ok(Money::ZERO);
            }
            let raw = raw_json_i128(position, key)?;
            Ok(Money::from_raw(raw))
        };
        let leverage = position
            .get("leverage")
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| u32::try_from(value).ok());
        facts.push(VenuePositionSnapshot {
            instrument,
            quantity: Quantity::from_raw(quantity),
            average_price: optional_price("entry_price_raw")?,
            mark_price: optional_price("mark_price_raw")?,
            liquidation_price: optional_price("liquidation_price_raw")?,
            unrealized_pnl: optional_money("unrealized_pnl_raw")?,
            initial_margin: optional_money("initial_margin_raw")?,
            maintenance_margin: optional_money("maintenance_margin_raw")?,
            leverage,
            margin_mode: position
                .get("margin_mode")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            position_side: Some(side),
        });
    }
    Ok(facts)
}

pub(crate) fn ccxt_funding_fact(
    value: &serde_json::Value,
    venue_id: &str,
) -> Result<(FundingRateSnapshot, u64), String> {
    let funding = value
        .get("funding")
        .ok_or_else(|| "CCXT funding 响应缺少 funding".to_string())?;
    let symbol = funding
        .get("symbol")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "CCXT funding 缺少 symbol".to_string())?;
    let instrument = InstrumentId::parse(&format!("{symbol}.{}", venue_id.to_uppercase()))
        .ok_or_else(|| format!("CCXT funding symbol 不是合法 InstrumentId: {symbol}"))?;
    let rate = raw_json_i128(funding, "funding_rate_bps")?;
    let rate = i64::try_from(rate).map_err(|_| "CCXT funding rate 超出 i64".to_string())?;
    let timestamp = funding
        .get("timestamp_ms")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    Ok((
        FundingRateSnapshot {
            instrument,
            funding_rate_bps: rate,
            next_funding_timestamp_ms: funding
                .get("next_funding_timestamp_ms")
                .and_then(serde_json::Value::as_u64),
        },
        timestamp,
    ))
}

pub(crate) fn ccxt_cashflow_facts(
    value: &serde_json::Value,
    account_id: &str,
    venue_id: &str,
) -> Result<Vec<(AccountCashflow, u64)>, String> {
    let rows = value
        .get("cashflows")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "CCXT cashflow 响应缺少 cashflows 数组".to_string())?;
    let mut facts = Vec::with_capacity(rows.len());
    for row in rows {
        let external_id = row
            .get("external_id")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| "CCXT cashflow 缺少 external_id".to_string())?;
        let currency = row
            .get("currency")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| format!("CCXT cashflow {external_id} 缺少 currency"))?
            .to_ascii_uppercase();
        let kind = match row
            .get("kind")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
        {
            "funding" => CashflowKind::Funding,
            "interest" => CashflowKind::Interest,
            "settlement" => CashflowKind::Settlement,
            "transfer" => CashflowKind::Transfer,
            other => return Err(format!("CCXT cashflow {external_id} kind 非法: {other}")),
        };
        let amount_raw = raw_json_i128(row, "amount_raw")?;
        let timestamp = row
            .get("timestamp_ms")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        facts.push((
            AccountCashflow {
                account_id: account_id.into(),
                venue_id: venue_id.into(),
                currency,
                kind,
                amount: Money::from_raw(amount_raw),
                external_id: external_id.into(),
            },
            timestamp,
        ));
    }
    Ok(facts)
}

fn cashflow_kind_key(kind: CashflowKind) -> &'static str {
    match kind {
        CashflowKind::Funding => "funding",
        CashflowKind::Interest => "interest",
        CashflowKind::Settlement => "settlement",
        CashflowKind::Transfer => "transfer",
    }
}

fn ingest_ccxt_cashflows(
    pipeline: &mut LiveEventPipeline,
    value: &serde_json::Value,
    account_id: &str,
    venue_id: &str,
    worker_id: &str,
    received_ts: u64,
    source_seq: &mut u64,
) -> Result<usize, String> {
    let facts = ccxt_cashflow_facts(value, account_id, venue_id)?;
    let mut ingested = 0_usize;
    for (cashflow, event_ts) in facts {
        *source_seq = (*source_seq).saturating_add(1);
        let event_ts = if event_ts == 0 { received_ts } else { event_ts };
        let correlation_id = format!(
            "{}:cashflow:{}:{}:{}:{}",
            worker_id,
            cashflow_kind_key(cashflow.kind),
            cashflow.currency,
            cashflow.external_id,
            account_id
        );
        let receipt = pipeline
            .ingest(RuntimeEventEnvelope::venue(
                RuntimeExternalEvent::AccountCashflow { cashflow },
                event_ts,
                received_ts,
                *source_seq,
                correlation_id,
            ))
            .map_err(|error| format!("CCXT 现金流水事实归约失败: {error:?}"))?;
        if !receipt.deduplicated {
            ingested = ingested.saturating_add(1);
        }
    }
    Ok(ingested)
}

fn ccxt_error_is_optional_derivatives_capability(error: &str) -> bool {
    let error = error.to_ascii_lowercase();
    error.contains("unsupported")
        || error.contains("not supported")
        || error.contains("非合约市场")
        || error.contains("fetchpositions")
}

/// 将 CCXT `fetch_open_orders` 的结果与本地订单索引做只读核对。
///
/// 这里故意不把远端未知订单注册进 OMS，也不根据单次查询自动撤单或平仓。
/// 远端订单可能来自进程崩溃前尚未写入 Accepted 事实、人工操作或其他系统；
/// 正确的 fail-safe 行为是保留原始证据并让对账服务降级，交由人工确认归属。
pub(crate) fn ccxt_open_order_issues(
    value: &serde_json::Value,
    local_orders: &[Order],
    known_remote_orders: &BTreeMap<String, (u64, OrderStatus)>,
    venue_id: &str,
) -> Result<Vec<serde_json::Value>, String> {
    let orders = value
        .get("orders")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "CCXT fetch_open_orders 返回缺少 orders 数组".to_string())?;
    let mut issues = Vec::new();
    for remote in orders {
        let object = remote
            .as_object()
            .ok_or_else(|| "CCXT open order 返回项必须是 object".to_string())?;
        let remote_order_id = object
            .get("order_id")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| "CCXT open order 缺少 order_id".to_string())?;
        let symbol = object
            .get("symbol")
            .or_else(|| object.get("instrument"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        let status = object
            .get("status")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown");
        let client_order_id = object
            .get("client_order_id")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let client_order_id_u64 = client_order_id.and_then(|value| value.parse::<u64>().ok());
        let local_by_client_id = client_order_id_u64.and_then(|client_id| {
            local_orders
                .iter()
                .find(|order| order.client_id == client_id)
        });
        let local_by_remote_id = known_remote_orders.get(remote_order_id);

        if let Some((local_client_id, local_status)) = local_by_remote_id {
            if local_status.is_terminal() {
                issues.push(serde_json::json!({
                    "kind": "remote_open_local_terminal",
                    "source": "ccxt.fetch_open_orders",
                    "venue_id": venue_id,
                    "remote_order_id": remote_order_id,
                    "client_order_id": local_client_id,
                    "instrument": symbol,
                    "status": status,
                    "local_status": format!("{local_status:?}"),
                    "observed": true,
                }));
            }
            continue;
        }

        let kind = if let Some(local) = local_by_client_id {
            serde_json::json!({
                "kind": "remote_open_unmapped_local_order",
                "source": "ccxt.fetch_open_orders",
                "venue_id": venue_id,
                "remote_order_id": remote_order_id,
                "client_order_id": local.client_id,
                "instrument": symbol,
                "status": status,
                "local_status": format!("{:?}", local.status),
                "observed": true,
            })
        } else {
            serde_json::json!({
                "kind": "unknown_remote_open_order",
                "source": "ccxt.fetch_open_orders",
                "venue_id": venue_id,
                "remote_order_id": remote_order_id,
                "client_order_id": client_order_id,
                "instrument": symbol,
                "status": status,
                "observed": true,
            })
        };
        issues.push(kind);
    }
    Ok(issues)
}

fn run_ccxt_reconcile_worker(
    context: qx_runtime::WorkerContext,
    worker: WorkerConfig,
    pipeline_root: &Path,
    pipeline_storage: PipelineStorage,
    ccxt_config_path: String,
    runtime_config_path: PathBuf,
    once: bool,
) -> Result<(), String> {
    if worker.role != WorkerRole::Reconciler {
        return Err(format!("worker {} 不是 CCXT Reconciler worker", worker.id));
    }
    let account_id = worker
        .account_id
        .clone()
        .ok_or_else(|| format!("worker {} 缺少 account_id", worker.id))?;
    let venue_id = worker
        .venue_id
        .clone()
        .ok_or_else(|| format!("worker {} 缺少 venue_id", worker.id))?;
    let python = std::env::var("QX_PYTHON").unwrap_or_else(|_| "python".into());
    let settlement_currency = worker
        .settlement_currency
        .clone()
        .unwrap_or_else(|| "USDT".into());
    let mut pipeline = pipeline_storage
        .open(ccxt_event_log_name(&worker), settlement_currency)
        .map_err(|error| format!("打开 CCXT 对账 EventLog 失败: {error}"))?;
    context.mark(
        qx_runtime::ServiceStatus::Ready,
        format!("ccxt reconcile polling venue={venue_id}"),
        Some(runtime_timestamp_ms()),
    )?;
    while !context.should_stop() {
        pipeline
            .refresh()
            .map_err(|error| format!("刷新 CCXT 对账 EventLog 失败: {error:?}"))?;
        let mut client = CcxtProcessClient::spawn(&python, &ccxt_config_path, None)
            .map_err(|error| format!("启动公共 CCXT Reconcile Worker 失败: {error}"))?;
        let balance_result = client
            .call(serde_json::json!({"op": "fetch_balance"}))
            .map_err(|error| format!("CCXT balance 对账失败: {error}"))?;
        let balances = ccxt_balance_facts(&balance_result)?;
        let received_ts = runtime_timestamp_ms();
        let mut source_seq = pipeline
            .log()
            .events()
            .last()
            .map(|event| event.source_seq)
            .unwrap_or(0);
        source_seq = source_seq.saturating_add(1);
        pipeline
            .ingest(RuntimeEventEnvelope::venue(
                RuntimeExternalEvent::AccountBalanceSnapshot {
                    account_id: account_id.clone(),
                    venue_id: venue_id.clone(),
                    balances: balances.clone(),
                },
                received_ts,
                received_ts,
                source_seq,
                format!("{}:balances:{}", worker.id, received_ts),
            ))
            .map_err(|error| format!("CCXT 余额事实归约失败: {error:?}"))?;

        let cashflow_since_ms = pipeline
            .log()
            .events()
            .iter()
            .filter_map(|event| match &event.kind {
                EventKind::AccountCashflow { .. } => Some(event.ts),
                _ => None,
            })
            .max();

        // 费率只是预估输入，真实资金费/利息/交割必须读取带外部 ID 的账单
        // 增量事实后才能进入 Ledger。优先使用统一 fetch_ledger；若交易所
        // 只暴露合约 funding history，则明确走该能力，不把缺失能力伪装成空账单。
        let mut cashflow_count = 0_usize;
        match client.call(serde_json::json!({
            "op": "fetch_ledger",
            "code": null,
            "since_ms": cashflow_since_ms,
        })) {
            Ok(cashflow_result) => {
                cashflow_count = cashflow_count.saturating_add(ingest_ccxt_cashflows(
                    &mut pipeline,
                    &cashflow_result,
                    &account_id,
                    &venue_id,
                    &worker.id,
                    received_ts,
                    &mut source_seq,
                )?);
            }
            Err(error) if ccxt_error_is_optional_derivatives_capability(&error) => {}
            Err(error) => return Err(format!("CCXT 资金流水对账失败: {error}")),
        }
        // 部分交易所的 fetch_ledger 不包含 funding 账单；合约市场再读取
        // 专用 funding history。与 ledger 同一 external_id 会由流水事实幂等去重。
        for instrument in &worker.symbols {
            match client.call(serde_json::json!({
                "op": "fetch_funding_history",
                "instrument": instrument,
                "since_ms": cashflow_since_ms,
            })) {
                Ok(history_result) => {
                    cashflow_count = cashflow_count.saturating_add(ingest_ccxt_cashflows(
                        &mut pipeline,
                        &history_result,
                        &account_id,
                        &venue_id,
                        &worker.id,
                        received_ts,
                        &mut source_seq,
                    )?);
                }
                Err(history_error)
                    if ccxt_error_is_optional_derivatives_capability(&history_error) => {}
                Err(history_error) => {
                    return Err(format!(
                        "CCXT 资金费账单对账失败 {instrument}: {history_error}"
                    ));
                }
            }
        }

        // 合约账户的持仓、保证金和未实现盈亏必须进入同一可恢复事实流。
        // 现货交易所通常不支持 fetch_positions；这类能力缺失只跳过该类快照，
        // 不能把“未支持”误判成空仓。
        let position_request = if worker.symbols.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::json!(worker.symbols)
        };
        match client.call(serde_json::json!({
            "op": "fetch_positions",
            "instruments": position_request,
        })) {
            Ok(position_result) => {
                let positions = ccxt_position_facts(&position_result, &venue_id)?;
                source_seq = source_seq.saturating_add(1);
                pipeline
                    .ingest(RuntimeEventEnvelope::venue(
                        RuntimeExternalEvent::AccountPositionSnapshot {
                            account_id: account_id.clone(),
                            venue_id: venue_id.clone(),
                            positions,
                        },
                        received_ts,
                        received_ts,
                        source_seq,
                        format!("{}:positions:{}", worker.id, received_ts),
                    ))
                    .map_err(|error| format!("CCXT 持仓事实归约失败: {error:?}"))?;
            }
            Err(error) if ccxt_error_is_optional_derivatives_capability(&error) => {}
            Err(error) => return Err(format!("CCXT 持仓对账失败: {error}")),
        }

        // 资金费率是风险输入，按 instrument 保存最新观察值；它不直接修改
        // Ledger，实际扣款已经由上面的 Cashflow 账单事实负责。
        for instrument in &worker.symbols {
            match client.call(serde_json::json!({
                "op": "fetch_funding_rate",
                "instrument": instrument,
            })) {
                Ok(funding_result) => {
                    let (snapshot, event_ts) = ccxt_funding_fact(&funding_result, &venue_id)?;
                    source_seq = source_seq.saturating_add(1);
                    pipeline
                        .ingest(RuntimeEventEnvelope::venue(
                            RuntimeExternalEvent::FundingRateSnapshot { snapshot },
                            event_ts,
                            received_ts,
                            source_seq,
                            format!("{}:funding:{}:{}", worker.id, instrument, received_ts),
                        ))
                        .map_err(|error| format!("CCXT 资金费率事实归约失败: {error:?}"))?;
                }
                Err(error) if ccxt_error_is_optional_derivatives_capability(&error) => {}
                Err(error) => {
                    return Err(format!("CCXT 资金费率对账失败 {instrument}: {error}"));
                }
            }
        }

        // 订单逐笔 fetch_order 只能覆盖本地已经知道的订单；进程在写入
        // Accepted 前崩溃、人工下单或其他系统下单，都会只存在于交易所。
        // 先拉取远端活动订单做只读发现，再交给下面的本地订单同步流程。
        let local_orders = pipeline.orders();
        let known_remote_orders = local_orders
            .iter()
            .filter_map(|order| {
                pipeline
                    .venue_order_id(order.client_id)
                    .map(|remote_id| (remote_id, (order.client_id, order.status)))
            })
            .collect::<BTreeMap<_, _>>();
        let open_order_requests = if worker.symbols.is_empty() {
            vec![None]
        } else {
            worker.symbols.iter().map(Some).collect::<Vec<_>>()
        };
        let mut open_order_issues = Vec::new();
        let mut seen_open_order_keys = BTreeSet::new();
        for instrument in open_order_requests {
            let open_orders_result = client.call(serde_json::json!({
                "op": "fetch_open_orders",
                "instrument": instrument,
            }));
            let open_orders_result = match open_orders_result {
                Ok(value) => value,
                Err(error) if ccxt_error_is_optional_derivatives_capability(&error) => continue,
                Err(error) => return Err(format!("CCXT 活动订单对账失败: {error}")),
            };
            for issue in ccxt_open_order_issues(
                &open_orders_result,
                &local_orders,
                &known_remote_orders,
                &venue_id,
            )? {
                let key = format!(
                    "{}:{}",
                    issue
                        .get("remote_order_id")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("unknown"),
                    issue
                        .get("instrument")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("")
                );
                if seen_open_order_keys.insert(key) {
                    open_order_issues.push(issue);
                }
            }
        }

        let mut venue = CcxtProcessVenue::new(venue_id.clone(), Box::new(client));
        let orders = local_orders;
        let mut updates = 0_usize;
        for order in orders
            .into_iter()
            .filter(|order| !order.status.is_terminal())
        {
            let Some(remote_id) = pipeline.venue_order_id(order.client_id) else {
                source_seq = source_seq.saturating_add(1);
                pipeline
                    .ingest(RuntimeEventEnvelope::venue(
                        RuntimeExternalEvent::ReconcileRequired {
                            client_order_id: order.client_id,
                        },
                        received_ts,
                        received_ts,
                        source_seq,
                        format!("{}:missing-remote:{}", worker.id, order.client_id),
                    ))
                    .map_err(|error| format!("CCXT 缺失远端订单对账失败: {error:?}"))?;
                continue;
            };
            venue
                .restore_order(order.clone(), remote_id)
                .map_err(|error| format!("恢复 CCXT 本地订单失败: {error:?}"))?;
            match venue.sync_order(order.client_id, received_ts) {
                Ok(events) => {
                    updates += if let Some(spec) =
                        load_worker_instrument_spec(&worker, &order, Some(&runtime_config_path))?
                    {
                        ingest_venue_events_with_spec(
                            &mut pipeline,
                            events,
                            &worker.id,
                            received_ts,
                            &mut source_seq,
                            &spec,
                        )?
                    } else {
                        ingest_venue_events(
                            &mut pipeline,
                            events,
                            &worker.id,
                            received_ts,
                            &mut source_seq,
                        )?
                    };
                }
                Err(error) => {
                    source_seq = source_seq.saturating_add(1);
                    pipeline
                        .ingest(RuntimeEventEnvelope::venue(
                            RuntimeExternalEvent::ReconcileRequired {
                                client_order_id: order.client_id,
                            },
                            received_ts,
                            received_ts,
                            source_seq,
                            format!("{}:order-error:{}", worker.id, order.client_id),
                        ))
                        .map_err(|ingest_error| {
                            format!(
                                "CCXT 订单对账失败 {error:?}，且无法写入 ReconcileRequired: {ingest_error:?}"
                            )
                        })?;
                }
            }
        }
        let snapshot = pipeline.snapshot();
        let balance_discrepancies = pipeline
            .settlement_balance_discrepancies(&account_id, &venue_id, &balances)
            .map_err(|error| format!("CCXT 账户余额对账失败: {error:?}"))?;
        let position_snapshots_count = snapshot
            .account_positions
            .get(&(account_id.clone(), venue_id.clone()))
            .map(Vec::len)
            .unwrap_or(0);
        persist_reconcile_report(ReconcileReportInput {
            pipeline_root,
            worker_id: &worker.id,
            account_id: &account_id,
            venue_id: &venue_id,
            observed_ts: received_ts,
            issues: &[],
            additional_order_issues: &open_order_issues,
            balances_count: balances.len(),
            balance_discrepancies: &balance_discrepancies,
            position_snapshots_count,
            funding_rate_snapshots_count: snapshot.funding_rates.len(),
            cashflow_count,
        })?;
        context.heartbeat(received_ts)?;
        let service_status = if open_order_issues.is_empty() && balance_discrepancies.is_empty() {
            qx_runtime::ServiceStatus::Ready
        } else {
            qx_runtime::ServiceStatus::Degraded
        };
        context.mark(
            service_status,
            format!(
                "ccxt reconciled order_updates={updates} open_order_issues={} balance_discrepancies={}",
                open_order_issues.len(),
                balance_discrepancies.len()
            ),
            Some(received_ts),
        )?;
        if once {
            break;
        }
        thread::sleep(Duration::from_secs(5));
    }
    context.mark(
        qx_runtime::ServiceStatus::Stopped,
        "ccxt reconciler stopped",
        Some(runtime_timestamp_ms()),
    )?;
    Ok(())
}

pub(crate) fn run_ccxt_worker(
    path: &Path,
    worker_id: &str,
    ccxt_config_path: &Path,
    once: bool,
) -> Result<(), String> {
    let config = read_runtime_config(path)?;
    let worker = config
        .workers
        .iter()
        .find(|worker| worker.id == worker_id)
        .cloned()
        .ok_or_else(|| format!("找不到 worker: {worker_id}"))?;
    let dedicated_spread_recovery = dedicated_spread_recovery_configured(&config, &worker);
    if !worker.enabled
        || !matches!(
            worker.role,
            WorkerRole::MarketData
                | WorkerRole::UserStream
                | WorkerRole::Execution
                | WorkerRole::SpreadRecovery
                | WorkerRole::Reconciler
        )
    {
        return Err(format!(
            "worker {worker_id} 不是启用的 CCXT MarketData/UserStream/Execution/SpreadRecovery/Reconciler worker"
        ));
    }
    if worker
        .venue_id
        .as_deref()
        .map(|venue| venue.eq_ignore_ascii_case("paper"))
        .unwrap_or(true)
    {
        return Err(format!("worker {worker_id} 必须绑定非 paper 的 CCXT venue"));
    }
    let root = Path::new(&config.storage.data_dir).to_path_buf();
    let pipeline_storage = PipelineStorage::from_config(&config)?;
    let runtime_config_path = path.to_path_buf();
    let control_store = configured_control_store(&config)?;
    let queue = configured_command_queue(&config, &root)?;
    let ccxt_config_path = resolve_ccxt_config_path(path, &ccxt_config_path.to_string_lossy());
    validate_ccxt_worker_binding(&worker, Path::new(&ccxt_config_path))?;
    let supervisor = RuntimeSupervisor::new(config)?;
    let registered_id = worker.id.clone();
    let worker_role = worker.role;
    let handle = supervisor.spawn_worker(&registered_id, move |context| match worker_role {
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
    })?;
    handle
        .join()
        .map_err(|_| format!("CCXT worker {worker_id} panic"))?
}

fn run_ccxt_spread_recovery_worker(
    context: qx_runtime::WorkerContext,
    worker: WorkerConfig,
    pipeline_storage: PipelineStorage,
    ccxt_config_path: String,
    runtime_config_path: PathBuf,
    once: bool,
) -> Result<(), String> {
    let python = std::env::var("QX_PYTHON").unwrap_or_else(|_| "python".into());
    let venue_id = worker.venue_id.clone().unwrap_or_else(|| "ccxt".into());
    context.mark(
        qx_runtime::ServiceStatus::Ready,
        format!("ccxt spread recovery scanning venue={venue_id}"),
        Some(runtime_timestamp_ms()),
    )?;
    while !context.should_stop() {
        let now = runtime_timestamp_ms();
        if has_pending_spread_recovery(&pipeline_storage.root, &venue_id)? {
            let mut pipeline = pipeline_storage
                .open(
                    ccxt_event_log_name(&worker),
                    worker.settlement_currency.as_deref().unwrap_or("USDT"),
                )
                .map_err(|error| format!("打开 CCXT 多腿恢复 EventLog 失败: {error}"))?;
            let client = CcxtProcessClient::spawn(&python, &ccxt_config_path, None)
                .map_err(|error| format!("启动公共 CCXT 恢复 Worker 失败: {error}"))?;
            let venue = CcxtProcessVenue::new(venue_id.clone(), Box::new(client));
            let mut source_seq = pipeline
                .log()
                .events()
                .last()
                .map(|event| event.source_seq)
                .unwrap_or(0);
            let validator =
                recovery_order_validator(&worker, &pipeline, Some(&runtime_config_path));
            let (_, diagnostics) = recover_spread_groups_for_venue(
                SpreadRecoveryContext {
                    root: &pipeline_storage.root,
                    venue_id: &venue_id,
                    accept_any_venue: false,
                    order_validator: Some(&validator),
                    pipeline: &mut pipeline,
                    worker_id: &worker.id,
                    now,
                    source_seq: &mut source_seq,
                },
                venue,
            )?;
            for message in diagnostics {
                eprintln!("[HedgeRecovery] {message}");
            }
        }
        context.heartbeat(now)?;
        if once {
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}

/// 下载公共 CCXT OHLCV 快照，输出为 qianxing_bridge.BarFrame JSON，作为回测
/// 的不可变输入；回测运行期间不再访问交易所。
pub(crate) fn run_ccxt_fetch_ohlcv(
    ccxt_config_path: &Path,
    instrument: &str,
    timeframe: &str,
    start_ms: u64,
    end_ms: u64,
    output_path: &Path,
) -> Result<(), String> {
    let python = std::env::var("QX_PYTHON").unwrap_or_else(|_| "python".into());
    let mut client = CcxtProcessClient::spawn(&python, &ccxt_config_path.to_string_lossy(), None)?;
    let result = client
        .call(serde_json::json!({
            "op": "fetch_ohlcv",
            "instrument": instrument,
            "timeframe": timeframe,
            "start_ms": start_ms,
            "end_ms": end_ms,
        }))
        .map_err(|error| format!("CCXT OHLCV 查询失败: {error}"))?;
    let frame = result
        .get("frame")
        .ok_or_else(|| "CCXT OHLCV 响应缺少 frame".to_string())?;
    std::fs::write(
        output_path,
        serde_json::to_string_pretty(frame)
            .map_err(|error| format!("编码 OHLCV 快照失败: {error}"))?,
    )
    .map_err(|error| format!("写入 OHLCV 快照失败 {}: {error}", output_path.display()))?;
    println!(
        "[CCXT · OHLCV] instrument={} timeframe={} output={} ✓",
        instrument,
        timeframe,
        output_path.display()
    );
    Ok(())
}

pub(crate) fn run_ccxt_market_spec(
    ccxt_config_path: &Path,
    instrument: &str,
    output_path: &Path,
) -> Result<(), String> {
    let python = std::env::var("QX_PYTHON").unwrap_or_else(|_| "python".into());
    let mut client = CcxtProcessClient::spawn(&python, &ccxt_config_path.to_string_lossy(), None)?;
    let result = client
        .call(serde_json::json!({
            "op": "resolve_market",
            "instrument": instrument,
        }))
        .map_err(|error| format!("CCXT market spec 查询失败: {error}"))?;
    let mut market = result
        .get("market")
        .cloned()
        .ok_or_else(|| "CCXT market spec 响应缺少 market".to_string())?;
    let tiers = match client.call(serde_json::json!({
        "op": "fetch_leverage_tiers",
        "instrument": instrument,
    })) {
        Ok(value) => value
            .get("tiers")
            .cloned()
            .unwrap_or_else(|| serde_json::json!([])),
        Err(error) if ccxt_error_is_optional_derivatives_capability(&error) => {
            serde_json::json!([])
        }
        Err(error) => return Err(format!("CCXT leverage tiers 查询失败: {error}")),
    };
    if let Some(object) = market.as_object_mut() {
        object.insert("leverage_tiers".into(), tiers);
    }
    std::fs::write(
        output_path,
        serde_json::to_string_pretty(&market)
            .map_err(|error| format!("编码 market spec 失败: {error}"))?,
    )
    .map_err(|error| format!("写入 market spec 失败 {}: {error}", output_path.display()))?;
    println!(
        "[CCXT · MarketSpec] instrument={} output={} ✓",
        instrument,
        output_path.display()
    );
    Ok(())
}

pub(crate) fn ccxt_market_to_spec(
    instrument: &InstrumentId,
    market: &serde_json::Value,
) -> Result<TradingInstrumentSpec, String> {
    let market_type = market
        .get("market_type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("spot");
    let product = match market_type {
        "spot" => TradingProduct::Spot,
        "margin" => TradingProduct::Margin,
        "swap" | "perpetual" => TradingProduct::Perpetual,
        "future" | "futures" => TradingProduct::Future,
        other => return Err(format!("CCXT market type 不支持: {other}")),
    };
    let base_currency = market
        .get("base")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "CCXT market 缺少 base".to_string())?;
    let quote_currency = market
        .get("quote")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "CCXT market 缺少 quote".to_string())?;
    let settlement_currency = market
        .get("settle")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(quote_currency);
    let contract_size = raw_json_i128(market, "contract_size_raw")?;
    let linear = market
        .get("linear")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(!product.is_derivative());
    let inverse = market
        .get("inverse")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let max_leverage = market
        .get("max_leverage")
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .unwrap_or(if product.is_derivative() { 100 } else { 1 });
    let spec = TradingInstrumentSpec {
        instrument: instrument.clone(),
        product,
        base_currency: base_currency.into(),
        quote_currency: quote_currency.into(),
        settlement_currency: settlement_currency.into(),
        contract_size,
        linear,
        inverse,
        // CCXT 市场快照的精度字段由不同 exchange/precisionMode 表达；
        // 若未由上游归一化，使用最小定点单位并要求部署侧覆盖。
        price_tick: raw_json_i128(market, "price_tick_raw").unwrap_or(1),
        qty_step: raw_json_i128(market, "qty_step_raw").unwrap_or(1),
        min_qty: raw_json_i128(market, "min_qty_raw").unwrap_or(1),
        max_leverage,
        maintenance_margin_bps: market
            .get("maintenance_margin_bps")
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .unwrap_or(500),
        valid_from: 0,
        valid_to: market.get("expiry_ms").and_then(serde_json::Value::as_u64),
    };
    spec.validate()
        .map_err(|error| format!("CCXT market spec 非法: {error:?}"))?;
    Ok(spec)
}

pub(crate) fn ccxt_margin_rule_from_market(market: &serde_json::Value) -> Box<dyn MarginRule> {
    let Some(rows) = market
        .get("leverage_tiers")
        .and_then(serde_json::Value::as_array)
    else {
        return Box::new(NoMargin);
    };
    let tiers = rows
        .iter()
        .filter_map(|row| {
            let max_notional = row
                .get("max_notional_raw")
                .and_then(serde_json::Value::as_i64)
                .map(i128::from)
                .or_else(|| {
                    row.get("max_notional_raw")
                        .and_then(serde_json::Value::as_u64)
                        .map(i128::from)
                })
                .unwrap_or(i128::MAX);
            let initial_bp = row
                .get("initial_margin_bps")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(0);
            let maintenance_bp = row
                .get("maintenance_margin_bps")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(0);
            let max_leverage = row
                .get("max_leverage")
                .and_then(serde_json::Value::as_u64)
                .and_then(|value| u32::try_from(value).ok());
            (max_notional > 0 && initial_bp >= 0 && maintenance_bp >= 0).then_some(MarginTier {
                max_notional,
                initial_bp,
                maintenance_bp,
                max_leverage,
            })
        })
        .collect::<Vec<_>>();
    if tiers.is_empty() {
        Box::new(NoMargin)
    } else {
        Box::new(TieredMargin { tiers })
    }
}

pub(crate) fn ccxt_worker_command(argv: &[String]) {
    let path = argv
        .get(2)
        .cloned()
        .unwrap_or_else(|| "deploy/qianxing.runtime.example.json".into());
    let worker_id = match argv.get(3).cloned() {
        Some(worker_id) => worker_id,
        None => {
            eprintln!("ccxt-worker 需要 worker-id");
            std::process::exit(2);
        }
    };
    let ccxt_config = match argv.get(4).cloned() {
        Some(config) => config,
        None => {
            eprintln!("ccxt-worker 需要 ccxt-config.json");
            std::process::exit(2);
        }
    };
    let once = argv.iter().any(|argument| argument == "--once");
    if let Err(error) = run_ccxt_worker(Path::new(&path), &worker_id, Path::new(&ccxt_config), once)
    {
        eprintln!("CCXT worker 启动/运行失败: {error}");
        std::process::exit(2);
    }
}

pub(crate) fn ccxt_fetch_ohlcv_command(argv: &[String]) {
    let ccxt_config = match argv.get(2).cloned() {
        Some(config) => config,
        None => {
            eprintln!("ccxt-fetch-ohlcv 需要 ccxt-config.json instrument start_ms end_ms output.json [timeframe]");
            std::process::exit(2);
        }
    };
    let instrument = match argv.get(3).cloned() {
        Some(instrument) => instrument,
        None => {
            eprintln!("ccxt-fetch-ohlcv 缺少 instrument");
            std::process::exit(2);
        }
    };
    let start_ms = match argv.get(4).cloned().and_then(|value| value.parse().ok()) {
        Some(value) => value,
        None => {
            eprintln!("ccxt-fetch-ohlcv start_ms 非法");
            std::process::exit(2);
        }
    };
    let end_ms = match argv.get(5).cloned().and_then(|value| value.parse().ok()) {
        Some(value) => value,
        None => {
            eprintln!("ccxt-fetch-ohlcv end_ms 非法");
            std::process::exit(2);
        }
    };
    let output = match argv.get(6).cloned() {
        Some(output) => output,
        None => {
            eprintln!("ccxt-fetch-ohlcv 缺少 output.json");
            std::process::exit(2);
        }
    };
    let timeframe = argv.get(7).cloned().unwrap_or_else(|| "1m".into());
    if let Err(error) = run_ccxt_fetch_ohlcv(
        Path::new(&ccxt_config),
        &instrument,
        &timeframe,
        start_ms,
        end_ms,
        Path::new(&output),
    ) {
        eprintln!("CCXT OHLCV 下载失败: {error}");
        std::process::exit(2);
    }
}

pub(crate) fn ccxt_market_spec_command(argv: &[String]) {
    let config = match argv.get(2).cloned() {
        Some(config) => config,
        None => {
            eprintln!("ccxt-market-spec 需要 ccxt-config.json instrument output.json");
            std::process::exit(2);
        }
    };
    let instrument = match argv.get(3).cloned() {
        Some(instrument) => instrument,
        None => {
            eprintln!("ccxt-market-spec 缺少 instrument");
            std::process::exit(2);
        }
    };
    let output = match argv.get(4).cloned() {
        Some(output) => output,
        None => {
            eprintln!("ccxt-market-spec 缺少 output.json");
            std::process::exit(2);
        }
    };
    if let Err(error) = run_ccxt_market_spec(Path::new(&config), &instrument, Path::new(&output)) {
        eprintln!("CCXT market spec 下载失败: {error}");
        std::process::exit(2);
    }
}
