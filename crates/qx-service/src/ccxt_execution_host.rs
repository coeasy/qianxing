//! CCXT SubmitOrder queue and multi-leg recovery lifecycle.

use crate::cancel_order_host::{execute_prepared_cancel_order, prepare_cancel_order};
use crate::ccxt_market_host::{exact_json_decimal_or_raw, raw_json_i128};
use crate::configured_backends::ControlStateBackend;
use crate::execution_capability_gate::{
    require_active_orders_execution_capability,
    require_and_record_submit_command_execution_capability,
    require_cancel_order_execution_capability, require_order_execution_capability,
    require_submit_command_execution_capability, ExecutionConnectorProfile,
};
use crate::gateway_execution_probe::GatewayExecutionProbeVenue;
use crate::pipeline::PipelineStorage;
use crate::pipeline_metrics_report::PipelineMetricsReporter;
use crate::spread_recovery_host::{
    command_is_final, is_fail_closed_rejection, is_retryable_spread_submission,
    open_spread_group_store, pending_spread_recovery_groups_for_account,
    recover_spread_groups_for_venue, recovery_order_validator, spread_recovery_poll_delay,
    sync_in_flight_spread_orders, sync_spread_group_after_order, sync_spread_groups_after_cancel,
    SpreadRecoveryContext,
};
use crate::worker_host::run_polling_worker;
use crate::worker_market_spec::load_worker_market_spec_for_instrument;
use qx_adapter::{CcxtProcessClient, CcxtProcessVenue};
use qx_control::{CommandKind, CommandStatus, ControlCommand};
use qx_core::{ExactDecimal, Price, Quantity, VenueFamily};
use qx_runtime::{
    account_worker_settlement_currency, lease_clock, required_account_event_log, RuntimeConfig,
    RuntimeEventEnvelope, RuntimeExternalEvent, ServiceStatus, WorkerConfig, WorkerContext,
    WorkerRole,
};
use qx_storage::{ControlCommandQueueBackend, StorageError};
use qx_zhenlu::Venue;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub struct CcxtExecutionWorkerInput {
    pub context: WorkerContext,
    pub worker: WorkerConfig,
    pub pipeline_storage: PipelineStorage,
    pub control_store: ControlStateBackend,
    pub queue: Arc<dyn ControlCommandQueueBackend>,
    pub runtime_config: RuntimeConfig,
    pub python: String,
    pub ccxt_config_path: String,
    pub runtime_config_path: PathBuf,
    pub dedicated_spread_recovery: bool,
    pub once: bool,
}

pub fn ccxt_submit_matches_worker(command: &ControlCommand, worker: &WorkerConfig) -> bool {
    let Some(expected_account) = worker.account_id.as_deref() else {
        return false;
    };
    let Some(expected_venue) = worker.venue_id.as_deref() else {
        return false;
    };
    qx_control::order_from_submit_command(command)
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

pub fn ccxt_cancel_matches_worker(command: &ControlCommand, worker: &WorkerConfig) -> bool {
    let Some(expected_account) = worker.account_id.as_deref() else {
        return false;
    };
    let Some(expected_venue) = worker.venue_id.as_deref() else {
        return false;
    };
    qx_control::cancel_order_target_from_command(command)
        .map(|target| {
            target.account_id == expected_account
                && target.venue_id.eq_ignore_ascii_case(expected_venue)
        })
        .unwrap_or(false)
}

fn ccxt_execution_command_matches_worker(command: &ControlCommand, worker: &WorkerConfig) -> bool {
    match &command.kind {
        CommandKind::SubmitOrder => ccxt_submit_matches_worker(command, worker),
        CommandKind::CancelOrder => ccxt_cancel_matches_worker(command, worker),
        _ => false,
    }
}

fn exact_ticker_reference_price(
    ticker: &serde_json::Value,
    now: u64,
) -> Result<ExactDecimal, String> {
    const MAX_RISK_TICKER_AGE_MS: u64 = 30_000;
    let timestamp_ms = ticker
        .get("timestamp_ms")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| "精确市价风控 ticker 缺少交易所时间戳".to_string())?;
    let age_ms = now
        .checked_sub(timestamp_ms)
        .ok_or_else(|| "精确市价风控 ticker 时间晚于本地时间".to_string())?;
    if age_ms > MAX_RISK_TICKER_AGE_MS {
        return Err(format!(
            "精确市价风控 ticker 已过期 {age_ms}ms（上限 {MAX_RISK_TICKER_AGE_MS}ms）"
        ));
    }
    let parse = |field: &str| -> Result<ExactDecimal, String> {
        let text = ticker
            .get(field)
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| format!("CCXT ticker 缺少精确 {field} 价格"))?;
        ExactDecimal::parse(text)
            .map_err(|error| format!("CCXT ticker 精确 {field} 价格非法: {error}"))
    };
    let bid = parse("bid_exact")?;
    let ask = parse("ask_exact")?;
    if !bid.is_positive() || !ask.is_positive() {
        return Err("CCXT ticker 精确 bid/ask 必须为正".into());
    }
    if bid.checked_cmp(ask).is_none_or(|ordering| ordering.is_gt()) {
        return Err("CCXT ticker 精确 bid 高于 ask".into());
    }
    Ok(ask)
}

/// Execute one accepted CCXT SubmitOrder command, including market reference pricing and spread
/// recovery. The CLI supplies the selected Python interpreter and relative configuration path.
#[allow(clippy::too_many_arguments)]
pub fn execute_ccxt_one_shot_order(
    command: &ControlCommand,
    worker: &WorkerConfig,
    pipeline_storage: &PipelineStorage,
    settlement_currency: &str,
    python: &str,
    ccxt_config_path: &str,
    runtime_config_path: &Path,
    runtime_environment: &str,
    now: u64,
) -> Result<String, String> {
    let requested_order = qx_control::order_from_submit_command(command)
        .map_err(|error| format!("CCXT 订单载荷非法: {error:?}"))?;
    if !ccxt_submit_matches_worker(command, worker) {
        return Err("订单 account_id 或 instrument venue 与 worker 拓扑不一致".into());
    }
    require_order_execution_capability(
        &requested_order,
        worker,
        runtime_environment,
        runtime_config_path,
        ExecutionConnectorProfile::Ccxt {
            config_path: Path::new(ccxt_config_path),
        },
    )?;
    let log_name = required_account_event_log(worker).map_err(|error| {
        crate::paper_execution_host::terminal_submit_rejection(format!(
            "解析 CCXT 执行 EventLog 名失败: {error}"
        ))
    })?;
    let mut pipeline = pipeline_storage
        .open(log_name.clone(), settlement_currency.to_string())
        .map_err(|error| {
            crate::paper_execution_host::terminal_submit_rejection(format!(
                "打开执行 EventLog 失败: {error}"
            ))
        })?;
    sync_in_flight_spread_orders(
        &pipeline_storage.root,
        &pipeline,
        command.payload.get("spread_group_id").map(String::as_str),
        worker.account_id.as_deref(),
        worker.venue_id.as_deref(),
    )?;
    let active_orders = pipeline.orders();
    require_active_orders_execution_capability(
        active_orders.iter(),
        worker,
        runtime_environment,
        runtime_config_path,
        ExecutionConnectorProfile::Ccxt {
            config_path: Path::new(ccxt_config_path),
        },
        false,
        &mut pipeline,
        now,
    )?;
    require_and_record_submit_command_execution_capability(
        command,
        worker,
        runtime_environment,
        runtime_config_path,
        ExecutionConnectorProfile::Ccxt {
            config_path: Path::new(ccxt_config_path),
        },
        &mut pipeline,
        now,
    )?;
    let client = CcxtProcessClient::spawn(python, ccxt_config_path, None).map_err(|error| {
        crate::paper_execution_host::terminal_submit_rejection(format!(
            "启动公共 CCXT Worker 失败: {error}"
        ))
    })?;
    let venue_id = worker.venue_id.clone().unwrap_or_else(|| "ccxt".into());
    let mut venue = CcxtProcessVenue::new(venue_id, Box::new(client));
    let spread_store = open_spread_group_store(&pipeline_storage.root).map_err(|error| {
        crate::paper_execution_host::terminal_submit_rejection(format!(
            "打开多腿订单组存储失败: {error}"
        ))
    })?;
    let mut source_seq = 0_u64;
    let validator = recovery_order_validator(worker, &pipeline, Some(runtime_config_path));
    let mut exact_market_reference = None;
    if requested_order.limit.is_none() && worker.instrument_spec_path.is_some() {
        let ticker = venue
            .stream_call(serde_json::json!({
                "op": "fetch_ticker",
                "instrument": requested_order.instrument.to_string(),
            }))
            .map_err(|error| {
                crate::paper_execution_host::terminal_submit_rejection(format!(
                    "CCXT 风控参考价查询失败: {error:?}"
                ))
            })?;
        let ticker = ticker.get("ticker").ok_or_else(|| {
            crate::paper_execution_host::terminal_submit_rejection(
                "CCXT ticker 响应缺少 ticker".into(),
            )
        })?;
        let bid_raw = raw_json_i128(ticker, "bid_raw")?;
        let ask_raw = raw_json_i128(ticker, "ask_raw")?;
        let bid = Price::from_raw(bid_raw);
        let ask = Price::from_raw(ask_raw);
        let bid_exact = exact_json_decimal_or_raw(ticker, "bid_exact", "bid_raw")?;
        let ask_exact = exact_json_decimal_or_raw(ticker, "ask_exact", "ask_raw")?;
        let bid_qty = Quantity::from_raw(raw_json_i128(ticker, "bid_qty_raw")?);
        let ask_qty = Quantity::from_raw(raw_json_i128(ticker, "ask_qty_raw")?);
        let bid_qty_exact = exact_json_decimal_or_raw(ticker, "bid_qty_exact", "bid_qty_raw")?;
        let ask_qty_exact = exact_json_decimal_or_raw(ticker, "ask_qty_exact", "ask_qty_raw")?;
        exact_market_reference = if requested_order
            .exact_amounts
            .is_some_and(|amounts| amounts.limit_price.is_none())
        {
            Some(exact_ticker_reference_price(ticker, now)?)
        } else {
            None
        };
        let event_ts = ticker
            .get("timestamp_ms")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(now);
        source_seq = source_seq.saturating_add(1);
        if bid_exact.is_positive()
            && ask_exact.is_positive()
            && bid_qty_exact.is_positive()
            && ask_qty_exact.is_positive()
            && bid_exact
                .checked_cmp(ask_exact)
                .is_some_and(|ordering| !ordering.is_gt())
        {
            pipeline
                .ingest(RuntimeEventEnvelope::venue(
                    RuntimeExternalEvent::MarketQuote {
                        instrument: requested_order.instrument.clone(),
                        bid,
                        ask,
                        bid_qty,
                        ask_qty,
                        bid_qty_exact: Some(bid_qty_exact),
                        ask_qty_exact: Some(ask_qty_exact),
                        bid_exact: Some(bid_exact),
                        ask_exact: Some(ask_exact),
                    },
                    event_ts,
                    now,
                    source_seq,
                    format!("{}:risk-ticker:{}", worker.id, requested_order.client_id),
                ))
                .map_err(|error| {
                    crate::paper_execution_host::terminal_submit_rejection(format!(
                        "写入 CCXT 风控参考行情失败: {error:?}"
                    ))
                })?;
        }
    }
    let result = crate::risk_host::execute_submit_order_with_worker_risk_and_market_reference(
        command,
        worker,
        &pipeline_storage.root,
        &mut venue,
        &mut pipeline,
        now,
        &mut source_seq,
        Some(runtime_config_path),
        Some(&spread_store),
        exact_market_reference,
    );
    if is_fail_closed_rejection(&result) || is_retryable_spread_submission(&result) {
        drop(venue);
    } else {
        let latest_pipeline = pipeline_storage
            .open(log_name, settlement_currency.to_string())
            .map_err(|error| {
                crate::paper_execution_host::terminal_submit_rejection(format!(
                    "刷新 CCXT 多腿订单组 EventLog 失败: {error}"
                ))
            })?;
        sync_spread_group_after_order(&pipeline_storage.root, &latest_pipeline, command, now)?;
        let (venue, recovery) = recover_spread_groups_for_venue(
            SpreadRecoveryContext {
                root: &pipeline_storage.root,
                account_id: worker.account_id.as_deref(),
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
}

pub fn run_ccxt_execution_worker(input: CcxtExecutionWorkerInput) -> Result<(), String> {
    let CcxtExecutionWorkerInput {
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
    } = input;
    let venue_id = worker
        .venue_id
        .as_deref()
        .filter(|venue| VenueFamily::parse_option(Some(venue)) == Some(VenueFamily::Other))
        .ok_or_else(|| format!("worker {} 不是有效的 CCXT Venue", worker.id))?;
    if !worker.enabled || worker.role != WorkerRole::Execution {
        return Err(format!(
            "worker {} 不是已启用的 CCXT Execution worker",
            worker.id
        ));
    }
    let settlement_currency = account_worker_settlement_currency(&runtime_config, &worker)?;
    let log_name = required_account_event_log(&worker)?;
    let mut metrics = PipelineMetricsReporter::new(&runtime_config, context.id(), &log_name);
    let owner = worker.id.clone();
    context.mark(
        ServiceStatus::Ready,
        format!("ccxt execution queue polling venue={venue_id}"),
        Some(runtime_timestamp_ms()),
    )?;
    metrics.publish_if_due(true, runtime_timestamp_ms());
    let mut recovery_stalls = 0_u32;
    let mut recovery_next_at = 0_u64;
    let worker_result = run_polling_worker(&context, once, || {
        let now = runtime_timestamp_ms();
        let lease_now = lease_clock(now);
        let control = control_store.load()?;
        if !dedicated_spread_recovery && now >= recovery_next_at {
            let pending_before = pending_spread_recovery_groups_for_account(
                &pipeline_storage.root,
                venue_id,
                worker.account_id.as_deref(),
            )?;
            if pending_before > 0 {
                let mut recovery_pipeline = pipeline_storage
                    .open(log_name.clone(), settlement_currency.clone())
                    .map_err(|error| format!("打开 CCXT 多腿恢复 EventLog 失败: {error}"))?;
                let recovery_orders = recovery_pipeline.orders();
                require_active_orders_execution_capability(
                    recovery_orders.iter(),
                    &worker,
                    &runtime_config.environment,
                    &runtime_config_path,
                    ExecutionConnectorProfile::Ccxt {
                        config_path: Path::new(&ccxt_config_path),
                    },
                    true,
                    &mut recovery_pipeline,
                    now,
                )?;
                let client = CcxtProcessClient::spawn(&python, &ccxt_config_path, None)
                    .map_err(|error| format!("启动公共 CCXT 恢复 Worker 失败: {error}"))?;
                let recovery_venue = CcxtProcessVenue::new(venue_id, Box::new(client));
                let recovery_venue = GatewayExecutionProbeVenue::new(recovery_venue, &mut metrics);
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
                        account_id: worker.account_id.as_deref(),
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
                metrics.absorb(&recovery_pipeline);
                let pending_after = pending_spread_recovery_groups_for_account(
                    &pipeline_storage.root,
                    venue_id,
                    worker.account_id.as_deref(),
                )?;
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
        for command in control
            .pending()
            .filter(|command| ccxt_execution_command_matches_worker(command, &worker))
        {
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
            if !ccxt_execution_command_matches_worker(&command, &worker) {
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
            } else if command.kind == CommandKind::CancelOrder {
                execute_ccxt_queued_cancel(
                    &worker,
                    &pipeline_storage,
                    &settlement_currency,
                    &runtime_config.environment,
                    &python,
                    &ccxt_config_path,
                    &runtime_config_path,
                    now,
                    &command,
                    &mut metrics,
                    dedicated_spread_recovery,
                )?
            } else if let Err(reason) =
                require_worker_risk_spec(&worker, &command, &runtime_config_path)
            {
                Err(reason)
            } else if let Err(reason) = require_submit_command_execution_capability(
                &command,
                &worker,
                &runtime_config.environment,
                &runtime_config_path,
                ExecutionConnectorProfile::Ccxt {
                    config_path: Path::new(&ccxt_config_path),
                },
            ) {
                Err(reason)
            } else {
                execute_ccxt_queued_order(CcxtQueuedOrderInput {
                    worker: &worker,
                    pipeline_storage: &pipeline_storage,
                    settlement_currency: &settlement_currency,
                    runtime_environment: &runtime_config.environment,
                    python: &python,
                    ccxt_config_path: &ccxt_config_path,
                    runtime_config_path: &runtime_config_path,
                    now,
                    command: &command,
                    metrics: &mut metrics,
                })?
            };
            if is_retryable_spread_submission(&action) {
                // Leave the accepted command pending; its lease expires and it retries after
                // the other leg's durable venue result is synchronized.
                continue;
            }
            let (_, record_result) = control_store
                .transact(|plane| plane.execute(command.command_id, now, |_| action.clone()))
                .map_err(|error| format!("回写 CCXT SubmitOrder 终态失败: {error}"))?;
            let record =
                record_result.map_err(|error| format!("CCXT SubmitOrder 执行失败: {error:?}"))?;
            queue
                .ack_command_at(command.command_id, &owner, lease.fencing_token, lease_now)
                .map_err(|error| format!("确认 CCXT SubmitOrder 失败: {error:?}"))?;
            context.mark(
                if record.status == CommandStatus::Executed {
                    ServiceStatus::Ready
                } else {
                    ServiceStatus::Degraded
                },
                format!(
                    "command_id={} status={:?}",
                    command.command_id, record.status
                ),
                Some(now),
            )?;
        }
        context.heartbeat(now)?;
        metrics.publish_if_due(true, now);
        Ok(100)
    });
    metrics.publish(false, runtime_timestamp_ms());
    worker_result?;
    Ok(())
}

fn execute_ccxt_queued_cancel(
    worker: &WorkerConfig,
    pipeline_storage: &PipelineStorage,
    settlement_currency: &str,
    runtime_environment: &str,
    python: &str,
    ccxt_config_path: &str,
    runtime_config_path: &Path,
    now: u64,
    command: &ControlCommand,
    metrics: &mut PipelineMetricsReporter,
    dedicated_spread_recovery: bool,
) -> Result<Result<String, String>, String> {
    let mut prepared = prepare_cancel_order(
        command,
        worker,
        worker.venue_id.as_deref().unwrap_or_default(),
        pipeline_storage,
        settlement_currency,
    )?;
    if prepared.order.status == qx_core::OrderStatus::Cancelled {
        return Ok(Ok("ALREADY_CANCELLED".into()));
    }
    if prepared.order.status.is_terminal() {
        return Ok(Err(format!(
            "订单 {} 已处于终态 {:?}，不能撤销",
            prepared.order.client_id, prepared.order.status
        )));
    }
    if let Err(reason) = require_cancel_order_execution_capability(
        &prepared.order,
        worker,
        runtime_environment,
        runtime_config_path,
        ExecutionConnectorProfile::Ccxt {
            config_path: Path::new(ccxt_config_path),
        },
    ) {
        return Ok(Err(reason));
    }

    // A new CCXT worker process has no in-memory remote-id mapping. Rebuild it from the
    // account EventLog and restore durable fee state before issuing the cancel RPC.
    let target_id = prepared.target.client_order_id;
    let target_remote_id = prepared.pipeline.venue_order_id(target_id).ok_or_else(|| {
        format!("CCXT 撤单订单 {target_id} 在账户 EventLog 中缺少 remote order id")
    })?;
    let client = CcxtProcessClient::spawn(python, ccxt_config_path, None)
        .map_err(|error| format!("启动公共 CCXT 撤单 Worker 失败: {error}"))?;
    let mut venue = CcxtProcessVenue::new(
        worker.venue_id.as_deref().unwrap_or("ccxt"),
        Box::new(client),
    );
    for order in prepared
        .pipeline
        .orders()
        .into_iter()
        .filter(|order| !order.status.is_terminal())
    {
        let Some(remote_id) = prepared.pipeline.venue_order_id(order.client_id) else {
            if order.client_id == target_id {
                return Ok(Err(format!(
                    "CCXT 撤单订单 {} 缺少 remote order id",
                    order.client_id
                )));
            }
            continue;
        };
        let fee_baseline = prepared
            .pipeline
            .fee_baseline_for_order(order.client_id)
            .map_err(|error| format!("读取 CCXT 持久手续费基线失败: {error}"))?;
        let exact_fee_baseline = prepared
            .pipeline
            .exact_fee_baseline_for_order(order.client_id)
            .map_err(|error| format!("读取 CCXT 持久精确手续费基线失败: {error}"))?;
        let seen_trade_ids = prepared
            .pipeline
            .fee_trade_ids_for_order(order.client_id)
            .map_err(|error| format!("读取 CCXT 成交费用去重状态失败: {error}"))?;
        venue
            .restore_order_with_exact_fee_state(
                order.clone(),
                remote_id,
                fee_baseline,
                exact_fee_baseline,
                seen_trade_ids,
            )
            .map_err(|error| format!("恢复 CCXT 撤单订单 {} 失败: {error:?}", order.client_id))?;
    }
    if venue
        .snapshot()
        .iter()
        .all(|order| order.client_order_id != target_id)
    {
        return Ok(Err(format!(
            "CCXT 撤单订单 {target_id} 未能从 EventLog 恢复"
        )));
    }
    if prepared.pipeline.venue_order_id(target_id).as_deref() != Some(target_remote_id.as_str()) {
        return Ok(Err(format!(
            "CCXT 撤单订单 {target_id} 的远端订单身份发生变化"
        )));
    }
    let mut venue = GatewayExecutionProbeVenue::new(venue, metrics);

    let mut source_seq = prepared
        .pipeline
        .log()
        .events()
        .last()
        .map(|event| event.source_seq)
        .unwrap_or(0);
    let result = execute_prepared_cancel_order(
        command,
        worker,
        pipeline_storage,
        &mut prepared,
        &mut venue,
        now,
        &mut source_seq,
    );
    let latest_pipeline = pipeline_storage
        .open(
            required_account_event_log(worker)?,
            settlement_currency.to_owned(),
        )
        .map_err(|error| format!("刷新 CCXT 撤单 EventLog 失败: {error}"))?;
    sync_spread_groups_after_cancel(&pipeline_storage.root, &latest_pipeline, target_id)?;
    if !dedicated_spread_recovery {
        let validator =
            recovery_order_validator(worker, &prepared.pipeline, Some(runtime_config_path));
        let (_, recovery) = recover_spread_groups_for_venue(
            SpreadRecoveryContext {
                root: &pipeline_storage.root,
                account_id: worker.account_id.as_deref(),
                venue_id: worker.venue_id.as_deref().unwrap_or("ccxt"),
                accept_any_venue: false,
                order_validator: Some(&validator),
                pipeline: &mut prepared.pipeline,
                worker_id: &worker.id,
                now,
                source_seq: &mut source_seq,
            },
            venue,
        )?;
        for message in recovery {
            eprintln!("[HedgeRecovery] {message}");
        }
    } else {
        drop(venue);
    }
    metrics.absorb(&prepared.pipeline);
    Ok(result)
}

struct CcxtQueuedOrderInput<'a> {
    worker: &'a WorkerConfig,
    pipeline_storage: &'a PipelineStorage,
    settlement_currency: &'a str,
    runtime_environment: &'a str,
    python: &'a str,
    ccxt_config_path: &'a str,
    runtime_config_path: &'a Path,
    now: u64,
    command: &'a ControlCommand,
    metrics: &'a mut PipelineMetricsReporter,
}

fn execute_ccxt_queued_order(
    input: CcxtQueuedOrderInput<'_>,
) -> Result<Result<String, String>, String> {
    let CcxtQueuedOrderInput {
        worker,
        pipeline_storage,
        settlement_currency,
        runtime_environment,
        python,
        ccxt_config_path,
        runtime_config_path,
        now,
        command,
        metrics,
    } = input;
    let log_name = required_account_event_log(worker)?;
    let spread_store = open_spread_group_store(&pipeline_storage.root)?;
    let mut pipeline = pipeline_storage
        .open(log_name, settlement_currency.to_string())
        .map_err(|error| format!("打开 CCXT EventLog 失败: {error}"))?;
    sync_in_flight_spread_orders(
        &pipeline_storage.root,
        &pipeline,
        command.payload.get("spread_group_id").map(String::as_str),
        worker.account_id.as_deref(),
        worker.venue_id.as_deref(),
    )?;
    let active_orders = pipeline.orders();
    if let Err(reason) = require_active_orders_execution_capability(
        active_orders.iter(),
        worker,
        runtime_environment,
        runtime_config_path,
        ExecutionConnectorProfile::Ccxt {
            config_path: Path::new(ccxt_config_path),
        },
        false,
        &mut pipeline,
        now,
    ) {
        return Ok(Err(reason));
    }
    if let Err(reason) = require_and_record_submit_command_execution_capability(
        command,
        worker,
        runtime_environment,
        runtime_config_path,
        ExecutionConnectorProfile::Ccxt {
            config_path: Path::new(ccxt_config_path),
        },
        &mut pipeline,
        now,
    ) {
        return Ok(Err(reason));
    }
    let client = CcxtProcessClient::spawn(python, ccxt_config_path, None)
        .map_err(|error| format!("启动公共 CCXT Worker 失败: {error}"))?;
    let mut venue = CcxtProcessVenue::new(
        worker.venue_id.as_deref().unwrap_or("ccxt"),
        Box::new(client),
    );
    let mut source_seq = 0_u64;
    let validator = recovery_order_validator(worker, &pipeline, Some(runtime_config_path));
    let requested_order = qx_control::order_from_submit_command(command)
        .map_err(|error| format!("CCXT 订单载荷非法: {error:?}"))?;
    let mut exact_market_reference = None;
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
        let bid_raw = raw_json_i128(ticker, "bid_raw")?;
        let ask_raw = raw_json_i128(ticker, "ask_raw")?;
        let bid = Price::from_raw(bid_raw);
        let ask = Price::from_raw(ask_raw);
        let bid_exact = exact_json_decimal_or_raw(ticker, "bid_exact", "bid_raw")?;
        let ask_exact = exact_json_decimal_or_raw(ticker, "ask_exact", "ask_raw")?;
        let bid_qty = Quantity::from_raw(raw_json_i128(ticker, "bid_qty_raw")?);
        let ask_qty = Quantity::from_raw(raw_json_i128(ticker, "ask_qty_raw")?);
        let bid_qty_exact = exact_json_decimal_or_raw(ticker, "bid_qty_exact", "bid_qty_raw")?;
        let ask_qty_exact = exact_json_decimal_or_raw(ticker, "ask_qty_exact", "ask_qty_raw")?;
        exact_market_reference = if requested_order
            .exact_amounts
            .is_some_and(|amounts| amounts.limit_price.is_none())
        {
            Some(exact_ticker_reference_price(ticker, now)?)
        } else {
            None
        };
        let event_ts = ticker
            .get("timestamp_ms")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(now);
        source_seq = source_seq.saturating_add(1);
        if bid_exact.is_positive()
            && ask_exact.is_positive()
            && bid_qty_exact.is_positive()
            && ask_qty_exact.is_positive()
            && bid_exact
                .checked_cmp(ask_exact)
                .is_some_and(|ordering| !ordering.is_gt())
        {
            pipeline
                .ingest(RuntimeEventEnvelope::venue(
                    RuntimeExternalEvent::MarketQuote {
                        instrument: requested_order.instrument.clone(),
                        bid,
                        ask,
                        bid_qty,
                        ask_qty,
                        bid_qty_exact: Some(bid_qty_exact),
                        ask_qty_exact: Some(ask_qty_exact),
                        bid_exact: Some(bid_exact),
                        ask_exact: Some(ask_exact),
                    },
                    event_ts,
                    now,
                    source_seq,
                    format!("{}:risk-ticker:{}", worker.id, requested_order.client_id),
                ))
                .map_err(|error| format!("写入 CCXT 风控参考行情失败: {error:?}"))?;
        }
    }
    let mut venue = GatewayExecutionProbeVenue::new(venue, metrics);
    let result = crate::risk_host::execute_submit_order_with_worker_risk_and_market_reference(
        command,
        worker,
        &pipeline_storage.root,
        &mut venue,
        &mut pipeline,
        now,
        &mut source_seq,
        Some(runtime_config_path),
        Some(&spread_store),
        exact_market_reference,
    );
    if !is_fail_closed_rejection(&result) && !is_retryable_spread_submission(&result) {
        let latest_pipeline = pipeline_storage
            .open(
                required_account_event_log(worker)?,
                settlement_currency.to_string(),
            )
            .map_err(|error| format!("刷新 CCXT 多腿订单组 EventLog 失败: {error}"))?;
        sync_spread_group_after_order(&pipeline_storage.root, &latest_pipeline, command, now)?;
        let (_, recovery) = recover_spread_groups_for_venue(
            SpreadRecoveryContext {
                root: &pipeline_storage.root,
                account_id: worker.account_id.as_deref(),
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
    } else {
        drop(venue);
    }
    metrics.absorb(&pipeline);
    Ok(result)
}

fn require_worker_risk_spec(
    worker: &WorkerConfig,
    command: &ControlCommand,
    runtime_config_path: &Path,
) -> Result<(), String> {
    let order = qx_control::order_from_submit_command(command)
        .map_err(|error| format!("SubmitOrder 订单载荷非法: {error:?}"))?;
    if load_worker_market_spec_for_instrument(worker, &order.instrument, Some(runtime_config_path))?
        .is_some()
    {
        Ok(())
    } else {
        Err(format!(
            "FAIL_CLOSED: worker {} 缺少风控配置（instrument_spec_path），拒绝提交订单",
            worker.id
        ))
    }
}

fn runtime_timestamp_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

#[cfg(test)]
mod exact_ticker_reference_tests {
    use super::*;

    #[test]
    fn preserves_fresh_sub_nano_market_risk_reference_and_rejects_stale_quote() {
        let ticker = serde_json::json!({
            "timestamp_ms": 100_000,
            "bid_exact": "0.0000000007",
            "ask_exact": "0.0000000009"
        });
        assert_eq!(
            exact_ticker_reference_price(&ticker, 105_000).unwrap(),
            ExactDecimal::parse("0.0000000009").unwrap()
        );
        assert!(exact_ticker_reference_price(&ticker, 131_000)
            .unwrap_err()
            .contains("已过期"));
    }
}

#[cfg(test)]
mod cancel_route_tests {
    use super::ccxt_cancel_matches_worker;
    use qx_control::{CommandKind, ControlCommand, Permission};
    use qx_runtime::WorkerConfig;
    use std::collections::BTreeMap;

    fn worker(account_id: &str, venue_id: &str) -> WorkerConfig {
        serde_json::from_value(serde_json::json!({
            "id": "ccxt-execution",
            "role": "execution",
            "enabled": true,
            "account_id": account_id,
            "venue_id": venue_id
        }))
        .expect("valid execution worker")
    }

    fn cancel(account_id: &str, venue_id: &str) -> ControlCommand {
        ControlCommand {
            command_id: 21,
            request_id: "cancel-21".into(),
            operator_id: "operator".into(),
            reason: "cancel route test".into(),
            kind: CommandKind::CancelOrder,
            target: "21".into(),
            payload: BTreeMap::from([
                ("account_id".into(), account_id.into()),
                ("venue_id".into(), venue_id.into()),
            ]),
            permission: Permission::Admin,
            dry_run: true,
        }
    }

    #[test]
    fn cancel_commands_route_by_account_and_case_insensitive_venue() {
        assert!(ccxt_cancel_matches_worker(
            &cancel("main", "OKX"),
            &worker("main", "okx")
        ));
        assert!(!ccxt_cancel_matches_worker(
            &cancel("other", "OKX"),
            &worker("main", "okx")
        ));
        assert!(!ccxt_cancel_matches_worker(
            &cancel("main", "BITGET"),
            &worker("main", "okx")
        ));
    }
}
