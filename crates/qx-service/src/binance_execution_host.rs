//! Binance execution worker queue, submit, recovery and control-plane lifecycle.

use crate::cancel_order_host::{execute_prepared_cancel_order, prepare_cancel_order};
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
use crate::risk_host::execute_submit_order_with_worker_risk;
use crate::spread_recovery_host::{
    command_is_final, is_fail_closed_rejection, is_retryable_spread_submission,
    open_spread_group_store, pending_spread_recovery_groups_for_account,
    recover_spread_groups_for_venue, recovery_order_validator, spread_recovery_poll_delay,
    sync_in_flight_spread_orders, sync_spread_group_after_order, sync_spread_groups_after_cancel,
    SpreadRecoveryContext,
};
use crate::worker_host::run_polling_worker;
use qx_adapter::BinanceSpotVenue;
use qx_control::{CommandKind, CommandStatus, ControlCommand};
use qx_core::{VenueFamily, VenueId};
use qx_runtime::{
    account_worker_settlement_currency, lease_clock, required_account_event_log, RuntimeConfig,
    ServiceStatus, WorkerConfig, WorkerContext, WorkerRole,
};
use qx_storage::{ControlCommandQueueBackend, StorageError};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Dependencies for one Binance execution worker. Venue creation remains injectable so the
/// service owns lifecycle and execution semantics while the facade supplies its connector.
pub struct BinanceExecutionWorkerInput<F> {
    pub context: WorkerContext,
    pub worker: WorkerConfig,
    pub pipeline_storage: PipelineStorage,
    pub control_store: ControlStateBackend,
    pub queue: Arc<dyn ControlCommandQueueBackend>,
    pub runtime_config: RuntimeConfig,
    pub runtime_config_path: PathBuf,
    pub dedicated_spread_recovery: bool,
    pub once: bool,
    pub create_venue: F,
}

pub fn binance_submit_matches_worker(command: &ControlCommand, worker: &WorkerConfig) -> bool {
    let expected_account = worker.account_id.as_deref().unwrap_or_default();
    qx_control::order_from_submit_command(command)
        .map(|order| order.account_id == expected_account && order.instrument.venue.is_binance())
        .unwrap_or(false)
}

pub fn binance_cancel_matches_worker(command: &ControlCommand, worker: &WorkerConfig) -> bool {
    if command.kind != CommandKind::CancelOrder {
        return false;
    }
    let Ok(target) = qx_control::cancel_order_target_from_command(command) else {
        return false;
    };
    let expected_account = worker.account_id.as_deref().unwrap_or_default();
    let expected_venue = worker.venue_id.as_deref().unwrap_or("BINANCE");
    target.account_id == expected_account
        && VenueId::new(target.venue_id).is_binance()
        && VenueId::new(expected_venue).is_binance()
}

fn binance_execution_command_matches_worker(
    command: &ControlCommand,
    worker: &WorkerConfig,
) -> bool {
    match &command.kind {
        CommandKind::SubmitOrder => binance_submit_matches_worker(command, worker),
        CommandKind::CancelOrder => binance_cancel_matches_worker(command, worker),
        _ => false,
    }
}

/// Execute one explicitly accepted Binance SubmitOrder command, including EventLog reduction and
/// spread recovery. The facade only provides the authenticated Venue constructor.
pub fn execute_binance_one_shot_order<F>(
    command: &ControlCommand,
    worker: &WorkerConfig,
    pipeline_storage: &PipelineStorage,
    settlement_currency: &str,
    runtime_config_path: &Path,
    runtime_environment: &str,
    now: u64,
    mut create_venue: F,
) -> Result<String, String>
where
    F: FnMut(&WorkerConfig) -> Result<BinanceSpotVenue, String>,
{
    let order = qx_control::order_from_submit_command(command)
        .map_err(|error| format!("SubmitOrder 订单载荷非法: {error:?}"))?;
    if !binance_submit_matches_worker(command, worker) {
        return Err("订单 account_id 或 instrument venue 与 worker 拓扑不一致".into());
    }
    require_order_execution_capability(
        &order,
        worker,
        runtime_environment,
        runtime_config_path,
        ExecutionConnectorProfile::BinanceSpot,
    )?;
    let log_name = required_account_event_log(worker).map_err(|error| {
        crate::paper_execution_host::terminal_submit_rejection(format!(
            "解析 Binance 执行 EventLog 名失败: {error}"
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
        ExecutionConnectorProfile::BinanceSpot,
        false,
        &mut pipeline,
        now,
    )?;
    require_and_record_submit_command_execution_capability(
        command,
        worker,
        runtime_environment,
        runtime_config_path,
        ExecutionConnectorProfile::BinanceSpot,
        &mut pipeline,
        now,
    )?;
    let mut venue = create_venue(worker)?;
    let spread_store = open_spread_group_store(&pipeline_storage.root).map_err(|error| {
        crate::paper_execution_host::terminal_submit_rejection(format!(
            "打开多腿订单组存储失败: {error}"
        ))
    })?;
    let mut source_seq = 0_u64;
    let validator = recovery_order_validator(worker, &pipeline, Some(runtime_config_path));
    venue
        .restore_orders(pipeline.orders())
        .map_err(|error| format!("装载待提交订单失败: {error:?}"))?;
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
    if is_fail_closed_rejection(&result) || is_retryable_spread_submission(&result) {
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
                    account_id: worker.account_id.as_deref(),
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

pub fn run_binance_execution_worker<F>(
    mut input: BinanceExecutionWorkerInput<F>,
) -> Result<(), String>
where
    F: FnMut(&WorkerConfig) -> Result<BinanceSpotVenue, String>,
{
    let BinanceExecutionWorkerInput {
        context,
        worker,
        pipeline_storage,
        control_store,
        queue,
        runtime_config,
        runtime_config_path,
        dedicated_spread_recovery,
        once,
        create_venue,
    } = &mut input;
    if !worker.enabled
        || worker.role != WorkerRole::Execution
        || VenueFamily::parse_option(worker.venue_id.as_deref()) != Some(VenueFamily::Binance)
    {
        return Err(format!(
            "worker {} 不是 Binance Execution worker",
            worker.id
        ));
    }
    let owner = worker.id.clone();
    let settlement_currency = account_worker_settlement_currency(runtime_config, worker)?;
    let log_name = required_account_event_log(worker)?;
    let mut metrics = PipelineMetricsReporter::new(runtime_config, context.id(), &log_name);
    context.mark(
        ServiceStatus::Ready,
        "execution queue polling",
        Some(runtime_timestamp_ms()),
    )?;
    metrics.publish_if_due(true, runtime_timestamp_ms());

    // Recovery scans use their own bounded cadence; the queue remains responsive at 100ms.
    let mut recovery_stalls = 0_u32;
    let mut recovery_next_at = 0_u64;
    let worker_result = run_polling_worker(context, *once, || {
        let now = runtime_timestamp_ms();
        let lease_now = lease_clock(now);
        let control = control_store.load()?;
        let venue_id = worker.venue_id.as_deref().unwrap_or("BINANCE");

        if !*dedicated_spread_recovery && now >= recovery_next_at {
            let pending_before = pending_spread_recovery_groups_for_account(
                &pipeline_storage.root,
                venue_id,
                worker.account_id.as_deref(),
            )?;
            if pending_before > 0 {
                let mut recovery_pipeline = pipeline_storage
                    .open(log_name.clone(), settlement_currency.clone())
                    .map_err(|error| format!("打开 Binance 多腿恢复 EventLog 失败: {error}"))?;
                let recovery_orders = recovery_pipeline.orders();
                require_active_orders_execution_capability(
                    recovery_orders.iter(),
                    worker,
                    &runtime_config.environment,
                    runtime_config_path,
                    ExecutionConnectorProfile::BinanceSpot,
                    true,
                    &mut recovery_pipeline,
                    now,
                )?;
                let mut recovery_venue = create_venue(worker)?;
                recovery_venue
                    .restore_orders(recovery_pipeline.orders())
                    .map_err(|error| format!("恢复 Binance 多腿订单状态失败: {error:?}"))?;
                let recovery_venue = GatewayExecutionProbeVenue::new(recovery_venue, &mut metrics);
                let mut recovery_seq = recovery_pipeline
                    .log()
                    .events()
                    .last()
                    .map(|event| event.source_seq)
                    .unwrap_or(0);
                let validator =
                    recovery_order_validator(worker, &recovery_pipeline, Some(runtime_config_path));
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

        for command in control.pending().filter(|command| {
            matches!(&command.kind, CommandKind::SubmitOrder)
                && binance_submit_matches_worker(command, worker)
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
            if !binance_execution_command_matches_worker(&command, worker) {
                continue;
            }
            let lease = match queue.claim_command(command.command_id, &owner, lease_now, 30) {
                Ok(lease) => lease,
                Err(StorageError::LeaseHeld { .. }) => continue,
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
            } else if command.kind == CommandKind::CancelOrder {
                execute_binance_queued_cancel(
                    worker,
                    pipeline_storage,
                    &settlement_currency,
                    &runtime_config.environment,
                    runtime_config_path,
                    now,
                    &command,
                    &mut metrics,
                    create_venue,
                    *dedicated_spread_recovery,
                )?
            } else {
                if let Err(reason) = require_worker_risk_spec(worker, &command, runtime_config_path)
                {
                    Err(reason)
                } else if let Err(reason) = require_submit_command_execution_capability(
                    &command,
                    worker,
                    &runtime_config.environment,
                    runtime_config_path,
                    ExecutionConnectorProfile::BinanceSpot,
                ) {
                    Err(reason)
                } else {
                    execute_binance_queued_order(BinanceQueuedOrderInput {
                        worker,
                        pipeline_storage,
                        settlement_currency: &settlement_currency,
                        runtime_environment: &runtime_config.environment,
                        runtime_config_path,
                        now,
                        command: &command,
                        metrics: &mut metrics,
                        create_venue,
                    })?
                }
            };
            if is_retryable_spread_submission(&action) {
                // Keep the accepted command and queue item pending. Its lease expires and the
                // worker retries after the other leg's durable venue result is synchronized.
                continue;
            }
            let (_, record_result) = control_store
                .transact(|plane| plane.execute(command.command_id, now, |_| action.clone()))
                .map_err(|error| format!("回写 SubmitOrder 终态失败: {error:?}"))?;
            let record = record_result.map_err(|error| format!("执行控制命令失败: {error:?}"))?;
            queue
                .ack_command_at(command.command_id, &owner, lease.fencing_token, lease_now)
                .map_err(|error| format!("确认 SubmitOrder 队列失败: {error:?}"))?;
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

struct BinanceQueuedOrderInput<'a, F> {
    worker: &'a WorkerConfig,
    pipeline_storage: &'a PipelineStorage,
    settlement_currency: &'a str,
    runtime_environment: &'a str,
    runtime_config_path: &'a Path,
    now: u64,
    command: &'a ControlCommand,
    metrics: &'a mut PipelineMetricsReporter,
    create_venue: &'a mut F,
}

fn execute_binance_queued_order<F>(
    input: BinanceQueuedOrderInput<'_, F>,
) -> Result<Result<String, String>, String>
where
    F: FnMut(&WorkerConfig) -> Result<BinanceSpotVenue, String>,
{
    let BinanceQueuedOrderInput {
        worker,
        pipeline_storage,
        settlement_currency,
        runtime_environment,
        runtime_config_path,
        now,
        command,
        metrics,
        create_venue,
    } = input;
    let log_name = required_account_event_log(worker)?;
    let mut pipeline = pipeline_storage
        .open(log_name, settlement_currency.to_string())
        .map_err(|error| format!("打开执行 EventLog 失败: {error}"))?;
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
        ExecutionConnectorProfile::BinanceSpot,
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
        ExecutionConnectorProfile::BinanceSpot,
        &mut pipeline,
        now,
    ) {
        return Ok(Err(reason));
    }
    let mut venue = create_venue(worker)?;
    venue
        .restore_orders(pipeline.orders())
        .map_err(|error| format!("恢复执行订单状态失败: {error:?}"))?;
    let mut venue = GatewayExecutionProbeVenue::new(venue, metrics);
    let spread_store = open_spread_group_store(&pipeline_storage.root)?;
    let mut source_seq = 0_u64;
    let validator = recovery_order_validator(worker, &pipeline, Some(runtime_config_path));
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
    if !is_fail_closed_rejection(&result) && !is_retryable_spread_submission(&result) {
        let latest_pipeline = pipeline_storage
            .open(
                required_account_event_log(worker)?,
                settlement_currency.to_string(),
            )
            .map_err(|error| format!("刷新 Binance 多腿订单组 EventLog 失败: {error}"))?;
        sync_spread_group_after_order(&pipeline_storage.root, &latest_pipeline, command, now)?;
        let (_, recovery) = recover_spread_groups_for_venue(
            SpreadRecoveryContext {
                root: &pipeline_storage.root,
                account_id: worker.account_id.as_deref(),
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
    } else {
        drop(venue);
    }
    metrics.absorb(&pipeline);
    Ok(result)
}

#[allow(clippy::too_many_arguments)]
fn execute_binance_queued_cancel<F>(
    worker: &WorkerConfig,
    pipeline_storage: &PipelineStorage,
    settlement_currency: &str,
    runtime_environment: &str,
    runtime_config_path: &Path,
    now: u64,
    command: &ControlCommand,
    metrics: &mut PipelineMetricsReporter,
    create_venue: &mut F,
    dedicated_spread_recovery: bool,
) -> Result<Result<String, String>, String>
where
    F: FnMut(&WorkerConfig) -> Result<BinanceSpotVenue, String>,
{
    let mut prepared = prepare_cancel_order(
        command,
        worker,
        "BINANCE",
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
    require_cancel_order_execution_capability(
        &prepared.order,
        worker,
        runtime_environment,
        runtime_config_path,
        ExecutionConnectorProfile::BinanceSpot,
    )?;
    let mut venue = create_venue(worker)?;
    venue
        .restore_orders(prepared.pipeline.orders())
        .map_err(|error| format!("恢复 Binance 撤单订单状态失败: {error:?}"))?;
    let mut venue = GatewayExecutionProbeVenue::new(venue, metrics);
    let mut source_seq = 0_u64;
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
        .map_err(|error| format!("刷新 Binance 撤单 EventLog 失败: {error}"))?;
    sync_spread_groups_after_cancel(
        &pipeline_storage.root,
        &latest_pipeline,
        prepared.target.client_order_id,
    )?;
    if !dedicated_spread_recovery {
        let validator =
            recovery_order_validator(worker, &prepared.pipeline, Some(runtime_config_path));
        let (_, recovery) = recover_spread_groups_for_venue(
            SpreadRecoveryContext {
                root: &pipeline_storage.root,
                account_id: worker.account_id.as_deref(),
                venue_id: "BINANCE",
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

fn require_worker_risk_spec(
    worker: &WorkerConfig,
    command: &ControlCommand,
    runtime_config_path: &Path,
) -> Result<(), String> {
    let order = qx_control::order_from_submit_command(command)
        .map_err(|error| format!("SubmitOrder 订单载荷非法: {error:?}"))?;
    if crate::worker_market_spec::load_worker_market_spec_for_instrument(
        worker,
        &order.instrument,
        Some(runtime_config_path),
    )?
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
