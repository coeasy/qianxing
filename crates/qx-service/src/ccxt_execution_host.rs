//! CCXT SubmitOrder queue and multi-leg recovery lifecycle.

use crate::cancel_order_host::{
    execute_prepared_amend_order, execute_prepared_cancel_order, prepare_amend_order,
    prepare_cancel_order,
};
use crate::ccxt_market_host::{exact_json_decimal_or_raw, raw_json_i128};
use crate::configured_backends::ControlStateBackend;
use crate::execution_capability_gate::{
    require_account_query_execution_capability, require_active_orders_execution_capability,
    require_and_record_submit_command_execution_capability,
    require_and_record_submit_order_group_execution_capability,
    require_cancel_order_execution_capability, require_order_execution_capability,
    require_order_group_execution_capability, require_submit_command_execution_capability,
    AccountQueryPurpose, ExecutionConnectorProfile,
};
use crate::gateway_execution_probe::GatewayExecutionProbeVenue;
use crate::pipeline::PipelineStorage;
use crate::pipeline_metrics_report::PipelineMetricsReporter;
use crate::risk_host::execute_submit_order_group_with_worker_risk;
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

pub fn ccxt_amend_matches_worker(command: &ControlCommand, worker: &WorkerConfig) -> bool {
    let Some(expected_account) = worker.account_id.as_deref() else {
        return false;
    };
    let Some(expected_venue) = worker.venue_id.as_deref() else {
        return false;
    };
    qx_control::amend_order_target_from_command(command)
        .map(|target| {
            target.account_id == expected_account
                && target.venue_id.eq_ignore_ascii_case(expected_venue)
        })
        .unwrap_or(false)
}

pub fn ccxt_submit_order_group_matches_worker(
    command: &ControlCommand,
    worker: &WorkerConfig,
) -> bool {
    let (Some(expected_account), Some(expected_venue)) =
        (worker.account_id.as_deref(), worker.venue_id.as_deref())
    else {
        return false;
    };
    qx_control::order_group_from_submit_command(command)
        .map(|group| {
            !group.legs.is_empty()
                && group.legs.iter().all(|leg| {
                    leg.order.account_id == expected_account
                        && leg
                            .order
                            .instrument
                            .venue
                            .as_str()
                            .eq_ignore_ascii_case(expected_venue)
                })
        })
        .unwrap_or(false)
}

pub fn ccxt_cancel_order_group_matches_worker(
    command: &ControlCommand,
    worker: &WorkerConfig,
) -> bool {
    let (Some(expected_account), Some(expected_venue)) =
        (worker.account_id.as_deref(), worker.venue_id.as_deref())
    else {
        return false;
    };
    qx_control::cancel_order_group_target_from_command(command)
        .map(|target| {
            target.account_id == expected_account
                && target.venue_id.eq_ignore_ascii_case(expected_venue)
        })
        .unwrap_or(false)
}

fn ccxt_execution_command_matches_worker(command: &ControlCommand, worker: &WorkerConfig) -> bool {
    match &command.kind {
        CommandKind::SubmitOrder => ccxt_submit_matches_worker(command, worker),
        CommandKind::AmendOrder => ccxt_amend_matches_worker(command, worker),
        CommandKind::CancelOrder => ccxt_cancel_matches_worker(command, worker),
        CommandKind::SubmitOrderGroup => ccxt_submit_order_group_matches_worker(command, worker),
        CommandKind::CancelOrderGroup => ccxt_cancel_order_group_matches_worker(command, worker),
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
            } else if command.kind == CommandKind::CancelOrderGroup {
                execute_ccxt_queued_cancel_order_group(
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
                )?
            } else if command.kind == CommandKind::AmendOrder {
                execute_ccxt_queued_amend(
                    &worker,
                    &pipeline_storage,
                    &settlement_currency,
                    &python,
                    &ccxt_config_path,
                    &runtime_config_path,
                    now,
                    &command,
                    &mut metrics,
                )?
            } else if command.kind == CommandKind::SubmitOrderGroup {
                execute_ccxt_queued_order_group(CcxtQueuedOrderGroupInput {
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

#[allow(clippy::too_many_arguments)] // Mirrors the queued cancellation context without hiding policy inputs.
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
    use qx_zhenlu::{FileOrderGroupStore, OrderGroupStore};

    let mut prepared = prepare_cancel_order(
        command,
        worker,
        worker.venue_id.as_deref().unwrap_or_default(),
        pipeline_storage,
        settlement_currency,
    )?;
    let log_name = required_account_event_log(worker)?;
    let _admission_lock =
        crate::risk_host::worker_account_admission_lock(&pipeline_storage.root, &log_name)?;
    prepared
        .pipeline
        .refresh()
        .map_err(|error| format!("刷新撤单前账户 EventLog 失败: {error:?}"))?;
    prepared.order = prepared
        .pipeline
        .orders()
        .into_iter()
        .find(|order| order.client_id == prepared.target.client_order_id)
        .ok_or_else(|| {
            format!(
                "刷新后账户 EventLog 不存在撤单订单 {}",
                prepared.target.client_order_id
            )
        })?;
    crate::risk_host::require_no_unresolved_live_order_operations(
        &worker.id,
        worker.account_id.as_deref(),
        worker.venue_id.as_deref(),
        &prepared.pipeline,
        Some(prepared.target.client_order_id),
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
    let mut group_store = FileOrderGroupStore::new(pipeline_storage.root.join("order-groups"))?;
    let mut triggered_oco_group_id = None;
    for group_id in group_store.group_ids()? {
        let Some(group) = group_store.load(&group_id)? else {
            continue;
        };
        if !group.legs.iter().any(|leg| {
            leg.order.client_id == prepared.target.client_order_id
                && leg.order.account_id == prepared.target.account_id
                && leg
                    .order
                    .instrument
                    .venue
                    .as_str()
                    .eq_ignore_ascii_case(worker.venue_id.as_deref().unwrap_or_default())
        }) || matches!(
            group.status,
            qx_core::OrderGroupStatus::Cancelled | qx_core::OrderGroupStatus::Rejected
        ) {
            continue;
        }
        let eligible_triggered_native_oco_child = worker.venue_id.as_deref().is_some_and(|venue| {
            venue.eq_ignore_ascii_case("OKX") || venue.eq_ignore_ascii_case("BITGET")
        }) && is_reconciled_triggered_oco_child(
            &group,
            prepared.target.client_order_id,
            prepared
                .pipeline
                .venue_order_id(prepared.target.client_order_id)
                .as_deref(),
        );
        if eligible_triggered_native_oco_child {
            if triggered_oco_group_id.is_some() {
                return Ok(Err(format!(
                    "订单 {} 出现在多个活动订单组，拒绝撤销并要求账户级对账",
                    prepared.target.client_order_id
                )));
            }
            triggered_oco_group_id = Some(group.intent.group_id.clone());
            continue;
        }
        return Ok(Err(format!(
            "订单 {} 属于活动原生订单组，必须先对账；已确认触发的 OKX/Bitget OCO 子单可直接撤销",
            prepared.target.client_order_id
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
    if let Some(group_id) = triggered_oco_group_id {
        let group = group_store
            .load(&group_id)?
            .ok_or_else(|| format!("触发 OCO 订单组 {group_id} 在撤单后消失"))?;
        if let Some(snapshot) = latest_pipeline
            .orders()
            .into_iter()
            .find(|order| order.client_id == target_id)
        {
            if let Err(error) = group_store.apply_order_snapshot(
                &group_id,
                &snapshot,
                Some(target_remote_id.clone()),
            ) {
                let _ = group_store.require_reconciliation(&group_id);
                return Err(format!(
                    "持久化已触发 OKX OCO 子单撤单结果失败，订单组已围栏待对账: {error}"
                ));
            }
        } else {
            let _ = group_store.require_reconciliation(&group_id);
            return Err(format!(
                "触发 OCO 子单 {} 撤单后不在 EventLog，订单组 {} 已围栏待对账",
                target_id, group.intent.group_id
            ));
        }
    }
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

fn is_reconciled_triggered_oco_child(
    group: &qx_core::OrderGroupState,
    client_order_id: u64,
    remote_order_id: Option<&str>,
) -> bool {
    if group.intent.kind != qx_core::OrderGroupKind::Oco
        || group.has_pending_amendment()
        || !matches!(
            group.status,
            qx_core::OrderGroupStatus::Active | qx_core::OrderGroupStatus::PartiallyFilled
        )
        || group
            .venue_group_id
            .as_deref()
            .is_none_or(|id| id.is_empty() || !id.bytes().all(|byte| byte.is_ascii_digit()))
        || group.legs.len() != 2
    {
        return false;
    }
    let Some(remote_order_id) =
        remote_order_id.filter(|id| !id.is_empty() && id.bytes().all(|byte| byte.is_ascii_digit()))
    else {
        return false;
    };
    let Some(active_leg) = group
        .legs
        .iter()
        .find(|leg| leg.order.client_id == client_order_id)
    else {
        return false;
    };
    if active_leg.venue_order_id.as_deref() != Some(remote_order_id)
        || active_leg.order.status.is_terminal()
        || !matches!(
            active_leg.role,
            qx_core::OrderGroupLegRole::TakeProfit | qx_core::OrderGroupLegRole::StopLoss
        )
    {
        return false;
    }
    group
        .legs
        .iter()
        .filter(|leg| leg.order.client_id != client_order_id)
        .count()
        == 1
        && group
            .legs
            .iter()
            .find(|leg| leg.order.client_id != client_order_id)
            .is_some_and(|peer| {
                peer.role != active_leg.role
                    && matches!(
                        peer.order.status,
                        qx_core::OrderStatus::Cancelled | qx_core::OrderStatus::Expired
                    )
                    && peer.venue_order_id.as_deref().is_none_or(|id| {
                        !id.is_empty() && id.bytes().all(|byte| byte.is_ascii_digit())
                    })
                    && peer.order.filled.raw() == 0
                    && !peer
                        .order
                        .exact_amounts
                        .is_some_and(|amounts| amounts.filled.is_positive())
            })
}

// Keep the worker, transport, request, and reporting dependencies explicit at this boundary.
#[allow(clippy::too_many_arguments)]
fn execute_ccxt_queued_amend(
    worker: &WorkerConfig,
    pipeline_storage: &PipelineStorage,
    settlement_currency: &str,
    python: &str,
    ccxt_config_path: &str,
    runtime_config_path: &Path,
    now: u64,
    command: &ControlCommand,
    metrics: &mut PipelineMetricsReporter,
) -> Result<Result<String, String>, String> {
    let mut prepared = prepare_amend_order(
        command,
        worker,
        worker.venue_id.as_deref().unwrap_or_default(),
        pipeline_storage,
        settlement_currency,
    )?;
    let target_id = prepared.target.client_order_id;
    let log_name = required_account_event_log(worker)?;
    let _admission_lock =
        crate::risk_host::worker_account_admission_lock(&pipeline_storage.root, &log_name)?;
    prepared
        .pipeline
        .refresh()
        .map_err(|error| format!("改单前刷新账户 EventLog 失败: {error:?}"))?;
    use qx_zhenlu::{FileOrderGroupStore, OrderGroupStore};
    let mut group_store = FileOrderGroupStore::new(pipeline_storage.root.join("order-groups"))?;
    let target_remote_id = prepared.pipeline.venue_order_id(target_id).ok_or_else(|| {
        format!("CCXT 改单目标 {target_id} 在账户 EventLog 中缺少 remote order id")
    })?;
    let mut amendment_group_id = None;
    for group_id in group_store.group_ids()? {
        let Some(group) = group_store.load(&group_id)? else {
            continue;
        };
        if !group.legs.iter().any(|leg| {
            leg.order.client_id == target_id
                && leg.order.account_id == prepared.target.account_id
                && leg
                    .order
                    .instrument
                    .venue
                    .as_str()
                    .eq_ignore_ascii_case(&prepared.target.venue_id)
        }) || matches!(
            group.status,
            qx_core::OrderGroupStatus::Cancelled | qx_core::OrderGroupStatus::Rejected
        ) {
            continue;
        }
        if amendment_group_id.is_some() {
            return Ok(Err(format!(
                "改单目标 {target_id} 属于多个未终结订单组，拒绝改单并要求账户级对账"
            )));
        }
        if !matches!(
            worker.venue_id.as_deref(),
            Some(venue) if venue.eq_ignore_ascii_case("OKX") || venue.eq_ignore_ascii_case("BITGET")
        ) || !is_reconciled_triggered_oco_child(&group, target_id, Some(&target_remote_id))
        {
            return Ok(Err(format!(
                "改单目标 {target_id} 属于活动原生订单组 {}；仅已对账且对侧零成交终止的 OKX/Bitget OCO 子单允许改单",
                group.intent.group_id
            )));
        }
        amendment_group_id = Some(group.intent.group_id.clone());
    }
    if prepared
        .target
        .order
        .exact_amounts
        .is_some_and(|amounts| amounts.conditional.is_some())
        && amendment_group_id.is_none()
    {
        return Ok(Err(
            "CCXT 尚不支持未触发条件单的原生改单；条件条款保持 fail-closed".into(),
        ));
    }
    let amendment_operation_id = format!("control:amend:{}:{}", command.command_id, target_id);
    if let Some(group_id) = amendment_group_id.as_deref() {
        if let Err(error) = group_store.begin_child_amendment(
            group_id,
            target_id,
            amendment_operation_id.clone(),
            &prepared.target.order,
        ) {
            return Ok(Err(format!("订单组子腿改单围栏失败: {error}")));
        }
    }
    let client = CcxtProcessClient::spawn(python, ccxt_config_path, None)
        .map_err(|error| format!("启动公共 CCXT 改单 Worker 失败: {error}"))?;
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
                    "CCXT 改单目标 {} 缺少 remote order id",
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
            .map_err(|error| format!("恢复 CCXT 改单订单 {} 失败: {error:?}", order.client_id))?;
    }
    if venue
        .snapshot()
        .iter()
        .all(|order| order.client_order_id != target_id)
    {
        return Ok(Err(format!(
            "CCXT 改单目标 {target_id} 未能从 EventLog 恢复"
        )));
    }
    if prepared.pipeline.venue_order_id(target_id).as_deref() != Some(target_remote_id.as_str()) {
        return Ok(Err(format!(
            "CCXT 改单目标 {target_id} 的远端订单身份发生变化"
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
    let result = execute_prepared_amend_order(
        command,
        worker,
        runtime_config_path,
        &mut prepared,
        &mut venue,
        now,
        &mut source_seq,
    );
    if let Some(group_id) = amendment_group_id.as_deref() {
        prepared
            .pipeline
            .refresh()
            .map_err(|error| format!("刷新改单后的账户 EventLog 失败: {error:?}"))?;
        if prepared.pipeline.pending_amendment(target_id).is_none() {
            if let Some(snapshot) = prepared
                .pipeline
                .orders()
                .into_iter()
                .find(|order| order.client_id == target_id)
            {
                if group_store
                    .confirm_child_amendment(group_id, &amendment_operation_id, &snapshot)
                    .is_err()
                {
                    let _ = group_store.abort_child_amendment(
                        group_id,
                        &amendment_operation_id,
                        &snapshot,
                    );
                }
            }
        }
        if result.is_ok() {
            let group = group_store
                .load(group_id)?
                .ok_or_else(|| format!("改单后的订单组 {group_id} 不存在"))?;
            let synchronized = !group.has_pending_amendment()
                && group
                    .legs
                    .iter()
                    .find(|leg| leg.order.client_id == target_id)
                    .is_some_and(|leg| leg.order.same_submission_identity(&prepared.target.order));
            if !synchronized {
                let _ = group_store.require_reconciliation(group_id);
                return Ok(Err(format!(
                    "订单组 {group_id} 的改单虽已处理，但组快照未能确认；订单组保持待对账"
                )));
            }
        }
    }
    Ok(result.map_err(|error| format!("CCXT 原生改单未确认: {error}")))
}

struct CcxtQueuedOrderGroupInput<'a> {
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

fn execute_ccxt_queued_order_group(
    input: CcxtQueuedOrderGroupInput<'_>,
) -> Result<Result<String, String>, String> {
    use qx_zhenlu::{FileOrderGroupStore, OrderGroupStore};

    let CcxtQueuedOrderGroupInput {
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
    let group = qx_control::order_group_from_submit_command(command)
        .map_err(|error| format!("SubmitOrderGroup 载荷非法: {error:?}"))?;
    if !ccxt_submit_order_group_matches_worker(command, worker) {
        return Ok(Err(
            "SubmitOrderGroup 账户或交易所与 CCXT worker 拓扑不匹配".into(),
        ));
    }
    let venue_id = worker.venue_id.as_deref().unwrap_or_default();
    if !(venue_id.eq_ignore_ascii_case("OKX") || venue_id.eq_ignore_ascii_case("BITGET"))
        || group.kind != qx_core::OrderGroupKind::Oco
    {
        return Ok(Err("CCXT 原生订单组当前只支持 OKX 与 Bitget UTA OCO".into()));
    }

    let log_name = required_account_event_log(worker)?;
    let mut pipeline = pipeline_storage
        .open(log_name, settlement_currency.to_owned())
        .map_err(|error| format!("打开 CCXT 订单组 EventLog 失败: {error}"))?;
    let mut group_store = FileOrderGroupStore::new(pipeline_storage.root.join("order-groups"))?;
    let mut grouped_order_ids = std::collections::BTreeSet::new();
    for group_id in group_store.group_ids()? {
        let Some(existing) = group_store.load(&group_id)? else {
            continue;
        };
        if existing.legs.iter().all(|leg| {
            leg.order.account_id == worker.account_id.as_deref().unwrap_or_default()
                && leg
                    .order
                    .instrument
                    .venue
                    .as_str()
                    .eq_ignore_ascii_case(worker.venue_id.as_deref().unwrap_or_default())
        }) {
            for leg in &existing.legs {
                grouped_order_ids.insert(leg.order.client_id);
            }
            if matches!(
                existing.status,
                qx_core::OrderGroupStatus::Submitting
                    | qx_core::OrderGroupStatus::CancelPending
                    | qx_core::OrderGroupStatus::ReconcileRequired
                    | qx_core::OrderGroupStatus::PeerCancelRequired
            ) || existing.venue_group_id.is_none()
            {
                return Ok(Err(format!(
                    "账户订单组 {} 缺少确认的交易所组身份或仍待对账，先恢复后再提交新组",
                    existing.intent.group_id
                )));
            }
        }
    }
    let active_orders = pipeline.orders();
    let ungrouped_active = active_orders
        .iter()
        .filter(|order| !grouped_order_ids.contains(&order.client_id));
    if let Err(error) = require_active_orders_execution_capability(
        ungrouped_active,
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
        return Ok(Err(error));
    }
    if let Err(error) = require_and_record_submit_order_group_execution_capability(
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
        metrics.absorb(&pipeline);
        return Ok(Err(error));
    }

    let client = CcxtProcessClient::spawn(python, ccxt_config_path, None)
        .map_err(|error| format!("启动公共 CCXT 订单组 Worker 失败: {error}"))?;
    let mut venue = CcxtProcessVenue::new(
        worker.venue_id.as_deref().unwrap_or("OKX"),
        Box::new(client),
    );
    let existing_orders = pipeline.orders();
    for order in existing_orders
        .iter()
        .filter(|order| !order.status.is_terminal())
    {
        let Some(remote_id) = pipeline.venue_order_id(order.client_id) else {
            continue;
        };
        let fee_baseline = pipeline
            .fee_baseline_for_order(order.client_id)
            .map_err(|error| format!("读取 CCXT 订单组手续费基线失败: {error}"))?;
        let exact_fee_baseline = pipeline
            .exact_fee_baseline_for_order(order.client_id)
            .map_err(|error| format!("读取 CCXT 订单组精确手续费基线失败: {error}"))?;
        let trade_ids = pipeline
            .fee_trade_ids_for_order(order.client_id)
            .map_err(|error| format!("读取 CCXT 订单组成交去重状态失败: {error}"))?;
        venue
            .restore_order_with_exact_fee_state(
                order.clone(),
                remote_id,
                fee_baseline,
                exact_fee_baseline,
                trade_ids,
            )
            .map_err(|error| format!("恢复 CCXT 订单组订单 {} 失败: {error:?}", order.client_id))?;
    }
    let source_seq = pipeline
        .log()
        .events()
        .last()
        .map(|event| event.source_seq)
        .unwrap_or(0);
    let mut source_seq = source_seq;
    let result = execute_submit_order_group_with_worker_risk(
        command,
        worker,
        &pipeline_storage.root,
        venue,
        &mut pipeline,
        now,
        &mut source_seq,
        Some(runtime_config_path),
        &mut group_store,
    );
    metrics.absorb(&pipeline);
    Ok(result)
}

#[allow(clippy::too_many_arguments)]
fn execute_ccxt_queued_cancel_order_group(
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
) -> Result<Result<String, String>, String> {
    use qx_zhenlu::{FileOrderGroupStore, OrderGroupStore};

    let target = qx_control::cancel_order_group_target_from_command(command)
        .map_err(|error| format!("CancelOrderGroup 载荷非法: {error:?}"))?;
    let is_okx = target.venue_id.eq_ignore_ascii_case("OKX");
    let is_bitget = target.venue_id.eq_ignore_ascii_case("BITGET");
    if !ccxt_cancel_order_group_matches_worker(command, worker) || !(is_okx || is_bitget) {
        return Ok(Err(
            "CancelOrderGroup 与受支持的 CCXT OCO worker 拓扑不匹配".into(),
        ));
    }
    let log_name = required_account_event_log(worker)?;
    let _admission_lock =
        crate::risk_host::worker_account_admission_lock(&pipeline_storage.root, &log_name)?;
    let mut group_store = FileOrderGroupStore::new(pipeline_storage.root.join("order-groups"))?;
    let Some(group) = group_store.load(&target.group_id)? else {
        return Ok(Err(format!("找不到持久订单组 {}", target.group_id)));
    };
    if group.intent.kind != qx_core::OrderGroupKind::Oco
        || group.legs.iter().any(|leg| {
            leg.order.account_id != target.account_id
                || !leg
                    .order
                    .instrument
                    .venue
                    .as_str()
                    .eq_ignore_ascii_case(&target.venue_id)
        })
    {
        return Ok(Err(
            "CancelOrderGroup 只接受匹配账户的 OKX/Bitget OCO".into()
        ));
    }
    if group.status == qx_core::OrderGroupStatus::Cancelled {
        return Ok(Ok("ALREADY_CANCELLED".into()));
    }
    if !matches!(
        group.status,
        qx_core::OrderGroupStatus::Active | qx_core::OrderGroupStatus::CancelPending
    ) {
        return Ok(Err(format!(
            "订单组 {} 当前状态 {:?}，必须先对账才能撤销",
            target.group_id, group.status
        )));
    }
    if group.legs.iter().any(|leg| {
        leg.venue_order_id.is_some()
            || leg.order.filled.raw() != 0
            || leg
                .order
                .exact_amounts
                .is_some_and(|amounts| amounts.filled.is_positive())
    }) {
        return Ok(Err(
            "原生 OCO 已生成子订单或存在成交，必须先按子单事实对账".into()
        ));
    }
    let mut pipeline = pipeline_storage
        .open(log_name, settlement_currency.to_owned())
        .map_err(|error| format!("打开原生 OCO 撤销 EventLog 失败: {error}"))?;
    if let Err(reason) = require_order_group_execution_capability(
        &group.intent,
        worker,
        runtime_environment,
        runtime_config_path,
        ExecutionConnectorProfile::Ccxt {
            config_path: Path::new(ccxt_config_path),
        },
        "cancel",
    ) {
        return Ok(Err(reason));
    }
    for leg in &group.legs {
        if let Err(reason) = require_cancel_order_execution_capability(
            &leg.order,
            worker,
            runtime_environment,
            runtime_config_path,
            ExecutionConnectorProfile::Ccxt {
                config_path: Path::new(ccxt_config_path),
            },
        ) {
            return Ok(Err(reason));
        }
    }
    require_account_query_execution_capability(
        worker,
        runtime_environment,
        AccountQueryPurpose::Reconcile,
        ExecutionConnectorProfile::Ccxt {
            config_path: Path::new(ccxt_config_path),
        },
    )?;
    let venue_group_id = group
        .venue_group_id
        .clone()
        .ok_or_else(|| format!("订单组 {} 缺少已确认的交易所组 ID", group.intent.group_id))?;
    let client = CcxtProcessClient::spawn(python, ccxt_config_path, None)
        .map_err(|error| format!("启动 CCXT OCO 撤销 Worker 失败: {error}"))?;
    let mut venue = CcxtProcessVenue::new(
        worker.venue_id.as_deref().unwrap_or("OKX"),
        Box::new(client),
    );
    let instrument = group.legs[0].order.instrument.to_string();
    let mut fetch_request = serde_json::json!({
        "op": "fetch_order_group",
        "kind": "oco",
        "instrument": instrument.clone(),
    });
    if is_okx {
        fetch_request["algo_id"] = serde_json::Value::String(venue_group_id.clone());
    } else {
        fetch_request["strategy_order_id"] = serde_json::Value::String(venue_group_id.clone());
    }
    let before_cancel = venue
        .stream_call(fetch_request.clone())
        .ok()
        .and_then(|response| response.get("order_group").cloned());
    let Some(before_cancel) = before_cancel else {
        require_ccxt_group_reconciliation(&group, worker, &mut group_store, &mut pipeline, now)?;
        metrics.absorb(&pipeline);
        return Ok(Err("原生 OCO 撤销前查询失败，订单组已围栏待对账".into()));
    };
    let before_state = before_cancel
        .get("state")
        .and_then(serde_json::Value::as_str);
    let before_is_untriggered = if is_okx {
        before_state == Some("live") && okx_group_is_untriggered(&before_cancel)
    } else {
        before_state == Some("live") && bitget_group_has_no_child_exposure(&before_cancel)
    };
    let already_cancelled = before_state == Some("canceled")
        && if is_okx {
            okx_group_is_untriggered(&before_cancel)
        } else {
            bitget_group_cancel_is_confirmed(&group, &before_cancel)
        };
    if before_is_untriggered {
        if group.status == qx_core::OrderGroupStatus::Active {
            group_store.begin_native_cancel(&group.intent.group_id)?;
        }
    } else if !(group.status == qx_core::OrderGroupStatus::CancelPending && already_cancelled) {
        require_ccxt_group_reconciliation(&group, worker, &mut group_store, &mut pipeline, now)?;
        metrics.absorb(&pipeline);
        return Ok(Err(format!(
            "原生 OCO 撤销前状态 {:?} 或已有成交/子单，已围栏待对账",
            before_state
        )));
    }
    if let Err(reason) =
        mark_native_group_cancel_pending(&group, worker, &mut group_store, &mut pipeline, now)
    {
        require_ccxt_group_reconciliation(&group, worker, &mut group_store, &mut pipeline, now)?;
        metrics.absorb(&pipeline);
        return Ok(Err(format!(
            "原生 OCO 子腿无法持久化撤单中状态，订单组已围栏待对账: {reason}"
        )));
    }
    let fact = if already_cancelled {
        before_cancel
    } else {
        let mut cancel_request = serde_json::json!({
            "op": "cancel_order_group",
            "kind": "oco",
            "instrument": instrument.clone(),
        });
        if is_okx {
            cancel_request["algo_id"] = serde_json::Value::String(venue_group_id.clone());
        } else {
            cancel_request["strategy_order_id"] = serde_json::Value::String(venue_group_id.clone());
        }
        let cancel_result = venue.stream_call(cancel_request);
        let ack_valid = cancel_result.as_ref().is_ok_and(|response| {
            response
                .get("order_group")
                .and_then(|group| {
                    group.get(if is_okx {
                        "algo_id"
                    } else {
                        "strategy_order_id"
                    })
                })
                .and_then(serde_json::Value::as_str)
                == Some(venue_group_id.as_str())
                && response
                    .get("order_group")
                    .and_then(|group| group.get("cancel_request_accepted"))
                    .and_then(serde_json::Value::as_str)
                    == Some("true")
        });
        let query_result = if ack_valid {
            venue.stream_call(fetch_request)
        } else {
            Err(qx_core::QxError::ReconcileRequired(
                "原生 OCO 撤销响应未知或身份不匹配".into(),
            ))
        };
        match query_result
            .ok()
            .and_then(|response| response.get("order_group").cloned())
        {
            Some(fact) => fact,
            None => {
                require_ccxt_group_reconciliation(
                    &group,
                    worker,
                    &mut group_store,
                    &mut pipeline,
                    now,
                )?;
                metrics.absorb(&pipeline);
                return Ok(Err("OKX OCO 撤销结果未知，订单组已围栏待对账".into()));
            }
        }
    };
    let confirmed_cancel = fact.get("state").and_then(serde_json::Value::as_str)
        == Some("canceled")
        && if is_okx {
            okx_group_is_untriggered(&fact)
        } else {
            bitget_group_cancel_is_confirmed(&group, &fact)
        };
    if !confirmed_cancel {
        require_ccxt_group_reconciliation(&group, worker, &mut group_store, &mut pipeline, now)?;
        metrics.absorb(&pipeline);
        return Ok(Err(format!(
            "原生 OCO 撤销后状态 {:?} 或成交/子单信息不符合未触发撤销，已待对账",
            fact.get("state").and_then(serde_json::Value::as_str)
        )));
    }

    let mut source_seq = pipeline
        .log()
        .events()
        .last()
        .map(|event| event.source_seq)
        .unwrap_or(0);
    for leg in &group.legs {
        source_seq = source_seq.saturating_add(1);
        pipeline
            .ingest(RuntimeEventEnvelope::venue(
                RuntimeExternalEvent::Cancelled {
                    client_order_id: leg.order.client_id,
                },
                now,
                now,
                source_seq,
                format!("{}:native-oco-cancel:{}", worker.id, leg.order.client_id),
            ))
            .map_err(|error| format!("写入原生 OCO 撤销事实失败: {error:?}"))?;
    }
    let orders = pipeline.orders();
    for leg in &group.legs {
        let snapshot = orders
            .iter()
            .find(|order| order.client_id == leg.order.client_id)
            .ok_or_else(|| format!("原生 OCO 腿 {} 撤销后不在 EventLog", leg.order.client_id))?;
        group_store.apply_order_snapshot(&group.intent.group_id, snapshot, None)?;
    }
    group_store.complete_native_cancel(&group.intent.group_id)?;
    metrics.absorb(&pipeline);
    Ok(Ok(if is_okx {
        "OKX_OCO_CANCELLED"
    } else {
        "BITGET_OCO_CANCELLED"
    }
    .into()))
}

fn bitget_group_has_no_child_exposure(group: &serde_json::Value) -> bool {
    group
        .get("order_ids")
        .and_then(serde_json::Value::as_array)
        .is_some_and(Vec::is_empty)
        && group
            .get("sub_orders")
            .and_then(serde_json::Value::as_array)
            .is_some_and(Vec::is_empty)
}

fn bitget_group_cancel_is_confirmed(
    order_group: &qx_core::OrderGroupState,
    remote: &serde_json::Value,
) -> bool {
    let Some(children) = remote
        .get("sub_orders")
        .and_then(serde_json::Value::as_array)
    else {
        return false;
    };
    crate::ccxt_reconcile_host::bitget_cancel_children_are_unfilled(order_group, children)
}

fn mark_native_group_cancel_pending(
    group: &qx_core::OrderGroupState,
    worker: &WorkerConfig,
    store: &mut dyn qx_zhenlu::OrderGroupStore,
    pipeline: &mut qx_runtime::LiveEventPipeline,
    now: u64,
) -> Result<(), String> {
    let mut source_seq = pipeline
        .log()
        .events()
        .last()
        .map(|event| event.source_seq)
        .unwrap_or(0);
    for leg in &group.legs {
        let Some(current) = pipeline
            .orders()
            .into_iter()
            .find(|order| order.client_id == leg.order.client_id)
        else {
            return Err(format!("订单组子腿 {} 不在 EventLog", leg.order.client_id));
        };
        if current.status == qx_core::OrderStatus::CancelPending {
            continue;
        }
        if !current
            .status
            .can_transition_to(qx_core::OrderStatus::CancelPending)
        {
            return Err(format!(
                "订单组子腿 {} 当前状态 {:?} 不能开始撤销",
                leg.order.client_id, current.status
            ));
        }
        source_seq = source_seq.saturating_add(1);
        pipeline
            .ingest(RuntimeEventEnvelope::venue(
                RuntimeExternalEvent::CancelPending {
                    client_order_id: leg.order.client_id,
                },
                now,
                now,
                source_seq,
                format!(
                    "{}:okx-oco-cancel-pending:{}",
                    worker.id, leg.order.client_id
                ),
            ))
            .map_err(|error| format!("写入 OKX OCO 撤单中事实失败: {error:?}"))?;
    }
    let orders = pipeline.orders();
    for leg in &group.legs {
        let snapshot = orders
            .iter()
            .find(|order| order.client_id == leg.order.client_id)
            .ok_or_else(|| format!("订单组子腿 {} 不在 EventLog", leg.order.client_id))?;
        store.apply_order_snapshot(&group.intent.group_id, snapshot, None)?;
    }
    Ok(())
}

fn okx_group_is_untriggered(group: &serde_json::Value) -> bool {
    let actual_size = group
        .get("actual_size")
        .and_then(serde_json::Value::as_str)
        .and_then(|text| ExactDecimal::parse(text).ok());
    group
        .get("order_ids")
        .and_then(serde_json::Value::as_array)
        .is_some_and(Vec::is_empty)
        && actual_size.is_some_and(ExactDecimal::is_zero)
}

fn require_ccxt_group_reconciliation(
    group: &qx_core::OrderGroupState,
    worker: &WorkerConfig,
    store: &mut dyn qx_zhenlu::OrderGroupStore,
    pipeline: &mut qx_runtime::LiveEventPipeline,
    now: u64,
) -> Result<(), String> {
    store.require_reconciliation(&group.intent.group_id)?;
    let mut source_seq = pipeline
        .log()
        .events()
        .last()
        .map(|event| event.source_seq)
        .unwrap_or(0);
    for leg in &group.legs {
        if pipeline
            .orders()
            .iter()
            .any(|order| order.client_id == leg.order.client_id && order.status.is_terminal())
        {
            continue;
        }
        source_seq = source_seq.saturating_add(1);
        pipeline
            .ingest(RuntimeEventEnvelope::venue(
                RuntimeExternalEvent::ReconcileRequired {
                    client_order_id: leg.order.client_id,
                },
                now,
                now,
                source_seq,
                format!("{}:okx-oco-reconcile:{}", worker.id, leg.order.client_id),
            ))
            .map_err(|error| format!("写入 OKX OCO 待对账事实失败: {error:?}"))?;
    }
    let orders = pipeline.orders();
    for leg in &group.legs {
        if let Some(snapshot) = orders
            .iter()
            .find(|order| order.client_id == leg.order.client_id)
        {
            store.record_order_snapshot_during_reconciliation(
                &group.intent.group_id,
                snapshot,
                None,
            )?;
        }
    }
    Ok(())
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
    use super::{
        bitget_group_has_no_child_exposure, ccxt_amend_matches_worker, ccxt_cancel_matches_worker,
        ccxt_cancel_order_group_matches_worker, ccxt_submit_order_group_matches_worker,
        is_reconciled_triggered_oco_child,
    };
    use qx_control::{CommandKind, ControlCommand, Permission};
    use qx_core::{
        ConditionalOrderIntent, ConditionalOrderKind, ExactDecimal, ExactOrderAmounts,
        InstrumentId, Order, OrderGroupExecution, OrderGroupIntent, OrderGroupKind,
        OrderGroupLegIntent, OrderGroupLegRole, OrderGroupState, OrderPolicy, OrderStatus, Price,
        Quantity, Side, TriggerPriceType,
    };
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

    fn amend(account_id: &str, venue_id: &str) -> ControlCommand {
        let order = Order {
            client_id: 21,
            instrument: InstrumentId::parse(&format!("BTC/USDT.{venue_id}")).unwrap(),
            side: Side::Buy,
            qty: Quantity::from_i64(2),
            limit: Some(Price::from_i64(100)),
            status: OrderStatus::Working,
            filled: Quantity::ZERO,
            account_id: account_id.into(),
            trace: None,
            policy: None,
            exact_amounts: None,
        };
        ControlCommand {
            command_id: 22,
            request_id: "amend-22".into(),
            operator_id: "operator".into(),
            reason: "amend route test".into(),
            kind: CommandKind::AmendOrder,
            target: "21".into(),
            payload: BTreeMap::from([
                ("account_id".into(), account_id.into()),
                ("venue_id".into(), venue_id.into()),
                ("order_json".into(), serde_json::to_string(&order).unwrap()),
            ]),
            permission: Permission::Admin,
            dry_run: true,
        }
    }

    fn order_group_submit(account_id: &str, venue_id: &str) -> ControlCommand {
        let instrument = InstrumentId::parse(&format!("BTC/USDT.{venue_id}")).unwrap();
        let leg = |client_id, role, kind, trigger| OrderGroupLegIntent {
            role,
            order: Order {
                client_id,
                instrument: instrument.clone(),
                side: Side::Sell,
                qty: Quantity::ZERO,
                limit: None,
                status: OrderStatus::PendingSubmit,
                filled: Quantity::ZERO,
                account_id: account_id.into(),
                trace: None,
                policy: Some(OrderPolicy::default()),
                exact_amounts: Some(ExactOrderAmounts {
                    quantity: ExactDecimal::parse("0.01").unwrap(),
                    limit_price: None,
                    filled: ExactDecimal::ZERO,
                    quote_budget: None,
                    quote_filled: ExactDecimal::ZERO,
                    market_reference_price: None,
                    conditional: Some(ConditionalOrderIntent {
                        kind,
                        trigger_price: Some(ExactDecimal::parse(trigger).unwrap()),
                        trigger_price_type: Some(TriggerPriceType::Last),
                        trailing_percent: None,
                        trailing_amount: None,
                        trailing_trigger_price: None,
                    }),
                }),
            },
        };
        let group = OrderGroupIntent::new(
            "okx-protection-1",
            OrderGroupKind::Oco,
            OrderGroupExecution::NativeRequired,
            vec![
                leg(
                    31,
                    OrderGroupLegRole::TakeProfit,
                    ConditionalOrderKind::TakeProfit,
                    "70000",
                ),
                leg(
                    32,
                    OrderGroupLegRole::StopLoss,
                    ConditionalOrderKind::StopLoss,
                    "60000",
                ),
            ],
        )
        .unwrap();
        let mut command = cancel(account_id, venue_id);
        command.kind = CommandKind::SubmitOrderGroup;
        command.target = group.group_id.clone();
        command.payload = BTreeMap::from([(
            "order_group_json".into(),
            serde_json::to_string(&group).unwrap(),
        )]);
        command
    }

    fn cancel_group(account_id: &str, venue_id: &str) -> ControlCommand {
        let mut command = cancel(account_id, venue_id);
        command.kind = CommandKind::CancelOrderGroup;
        command.target = "okx-protection-1".into();
        command
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

    #[test]
    fn amend_commands_route_by_account_and_case_insensitive_venue() {
        assert!(ccxt_amend_matches_worker(
            &amend("main", "OKX"),
            &worker("main", "okx")
        ));
        assert!(!ccxt_amend_matches_worker(
            &amend("other", "OKX"),
            &worker("main", "okx")
        ));
        assert!(!ccxt_amend_matches_worker(
            &amend("main", "BITGET"),
            &worker("main", "okx")
        ));
    }

    #[test]
    fn native_group_commands_route_to_the_exact_ccxt_account_and_venue() {
        assert!(ccxt_submit_order_group_matches_worker(
            &order_group_submit("main", "OKX"),
            &worker("main", "okx")
        ));
        assert!(!ccxt_submit_order_group_matches_worker(
            &order_group_submit("other", "OKX"),
            &worker("main", "okx")
        ));
        assert!(ccxt_submit_order_group_matches_worker(
            &order_group_submit("main", "BITGET"),
            &worker("main", "bitget")
        ));
        assert!(ccxt_cancel_order_group_matches_worker(
            &cancel_group("main", "OKX"),
            &worker("main", "okx")
        ));
        assert!(ccxt_cancel_order_group_matches_worker(
            &cancel_group("main", "BITGET"),
            &worker("main", "bitget")
        ));
    }

    #[test]
    fn bitget_oco_cancel_requires_complete_zero_fill_child_facts() {
        assert!(bitget_group_has_no_child_exposure(&serde_json::json!({
            "order_ids": [], "sub_orders": []
        })));
        assert!(!bitget_group_has_no_child_exposure(&serde_json::json!({
            "order_ids": ["1"], "sub_orders": []
        })));
    }

    #[test]
    fn only_a_reconciled_triggered_native_oco_child_can_use_single_order_cancel() {
        let submit = order_group_submit("main", "OKX");
        let intent: OrderGroupIntent =
            serde_json::from_str(submit.payload.get("order_group_json").unwrap()).unwrap();
        let mut group = OrderGroupState::from_intent(intent).unwrap();
        group.status = qx_core::OrderGroupStatus::Active;
        group.venue_group_id = Some("900001".into());
        let active = group
            .legs
            .iter_mut()
            .find(|leg| leg.role == OrderGroupLegRole::TakeProfit)
            .unwrap();
        active.order.status = OrderStatus::Submitted;
        active.venue_order_id = Some("700001".into());
        let peer = group
            .legs
            .iter_mut()
            .find(|leg| leg.role == OrderGroupLegRole::StopLoss)
            .unwrap();
        peer.order.status = OrderStatus::Cancelled;

        assert!(is_reconciled_triggered_oco_child(
            &group,
            31,
            Some("700001")
        ));
        assert!(!is_reconciled_triggered_oco_child(
            &group,
            31,
            Some("700002")
        ));
        assert!(!is_reconciled_triggered_oco_child(&group, 31, None));

        group.legs[1].order.status = OrderStatus::Submitted;
        assert!(!is_reconciled_triggered_oco_child(
            &group,
            31,
            Some("700001")
        ));

        let submit = order_group_submit("main", "BITGET");
        let intent: OrderGroupIntent =
            serde_json::from_str(submit.payload.get("order_group_json").unwrap()).unwrap();
        let mut bitget_group = OrderGroupState::from_intent(intent).unwrap();
        bitget_group.status = qx_core::OrderGroupStatus::Active;
        bitget_group.venue_group_id = Some("900002".into());
        let active = bitget_group
            .legs
            .iter_mut()
            .find(|leg| leg.role == OrderGroupLegRole::TakeProfit)
            .unwrap();
        active.order.status = OrderStatus::Submitted;
        active.venue_order_id = Some("700011".into());
        let peer = bitget_group
            .legs
            .iter_mut()
            .find(|leg| leg.role == OrderGroupLegRole::StopLoss)
            .unwrap();
        peer.order.status = OrderStatus::Cancelled;
        peer.venue_order_id = Some("700012".into());
        assert!(is_reconciled_triggered_oco_child(
            &bitget_group,
            31,
            Some("700011")
        ));
        group.legs[1].order.status = OrderStatus::Cancelled;
        group.status = qx_core::OrderGroupStatus::ReconcileRequired;
        assert!(!is_reconciled_triggered_oco_child(
            &group,
            31,
            Some("700001")
        ));
        group.status = qx_core::OrderGroupStatus::Active;
        group.legs[1].order.filled = Quantity::from_i64(1);
        assert!(!is_reconciled_triggered_oco_child(
            &group,
            31,
            Some("700001")
        ));
    }
}
