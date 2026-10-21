//! CCXT SubmitOrder queue and multi-leg recovery lifecycle.

use crate::ccxt_market_host::raw_json_i128;
use crate::configured_backends::ControlStateBackend;
use crate::pipeline::PipelineStorage;
use crate::pipeline_metrics_report::PipelineMetricsReporter;
use crate::risk_host::execute_submit_order_with_worker_risk;
use crate::spread_recovery_host::{
    command_is_final, is_fail_closed_rejection, open_spread_group_store,
    pending_spread_recovery_groups, recover_spread_groups_for_venue, recovery_order_validator,
    spread_recovery_poll_delay, sync_spread_group_after_order, SpreadRecoveryContext,
};
use crate::worker_host::run_polling_worker;
use crate::worker_market_spec::load_worker_market_spec_for_instrument;
use qx_adapter::{CcxtProcessClient, CcxtProcessVenue};
use qx_control::{CommandKind, CommandStatus, ControlCommand};
use qx_core::{Price, Quantity, VenueFamily};
use qx_runtime::{
    account_worker_settlement_currency, lease_clock, required_account_event_log, RuntimeConfig,
    RuntimeEventEnvelope, RuntimeExternalEvent, ServiceStatus, WorkerConfig, WorkerContext,
    WorkerRole,
};
use qx_storage::{ControlCommandQueueBackend, StorageError};
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
    now: u64,
) -> Result<String, String> {
    qx_control::order_from_submit_command(command)
        .map_err(|error| format!("CCXT 订单载荷非法: {error:?}"))?;
    if !ccxt_submit_matches_worker(command, worker) {
        return Err("订单 account_id 或 instrument venue 与 worker 拓扑不一致".into());
    }
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
    let requested_order = qx_control::order_from_submit_command(command)
        .map_err(|error| format!("CCXT 订单载荷非法: {error:?}"))?;
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
            .map_err(|error| {
                crate::paper_execution_host::terminal_submit_rejection(format!(
                    "写入 CCXT 风控参考行情失败: {error:?}"
                ))
            })?;
    }
    let result = execute_submit_order_with_worker_risk(
        command,
        worker,
        &pipeline_storage.root,
        &mut venue,
        &mut pipeline,
        now,
        &mut source_seq,
        Some(runtime_config_path),
        Some(&spread_store),
    );
    if is_fail_closed_rejection(&result) {
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
    run_polling_worker(&context, once, || {
        let now = runtime_timestamp_ms();
        let lease_now = lease_clock(now);
        let control = control_store.load()?;
        if !dedicated_spread_recovery && now >= recovery_next_at {
            let pending_before = pending_spread_recovery_groups(&pipeline_storage.root, venue_id)?;
            if pending_before > 0 {
                let mut recovery_pipeline = pipeline_storage
                    .open(log_name.clone(), settlement_currency.clone())
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
                metrics.absorb(&recovery_pipeline);
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
                require_worker_risk_spec(&worker, &command, &runtime_config_path)
            {
                Err(reason)
            } else {
                execute_ccxt_queued_order(CcxtQueuedOrderInput {
                    worker: &worker,
                    pipeline_storage: &pipeline_storage,
                    settlement_currency: &settlement_currency,
                    python: &python,
                    ccxt_config_path: &ccxt_config_path,
                    runtime_config_path: &runtime_config_path,
                    now,
                    command: &command,
                    metrics: &mut metrics,
                })?
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
    })?;
    metrics.publish(false, runtime_timestamp_ms());
    Ok(())
}

struct CcxtQueuedOrderInput<'a> {
    worker: &'a WorkerConfig,
    pipeline_storage: &'a PipelineStorage,
    settlement_currency: &'a str,
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
        command,
        worker,
        &pipeline_storage.root,
        &mut venue,
        &mut pipeline,
        now,
        &mut source_seq,
        Some(runtime_config_path),
        Some(&spread_store),
    );
    if !is_fail_closed_rejection(&result) {
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
