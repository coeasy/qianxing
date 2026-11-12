//! Shared live CancelOrder resolution and execution over the authoritative account OMS.

use crate::pipeline::PipelineStorage;
use crate::spread_recovery_host::open_spread_group_store;
use qx_control::{
    amend_order_target_from_command, cancel_order_target_from_command, AmendOrderTarget,
    CancelOrderTarget, ControlCommand,
};
use qx_core::{Order, VenueFamily};
use qx_execution::{BorrowedVenuePort, ExecutionGateway};
use qx_runtime::{required_account_event_log, LiveEventPipeline, WorkerConfig};
use qx_zhenlu::Venue;
use std::path::Path;

pub struct PreparedCancelOrder {
    pub target: CancelOrderTarget,
    pub order: Order,
    pub pipeline: LiveEventPipeline,
}

pub struct PreparedAmendOrder {
    pub target: AmendOrderTarget,
    pub order: Order,
    pub pipeline: LiveEventPipeline,
}

pub fn prepare_amend_order(
    command: &ControlCommand,
    worker: &WorkerConfig,
    expected_venue: &str,
    pipeline_storage: &PipelineStorage,
    settlement_currency: &str,
) -> Result<PreparedAmendOrder, String> {
    let target = amend_order_target_from_command(command)
        .map_err(|error| format!("AmendOrder 命令无效: {error:?}"))?;
    if worker.account_id.as_deref() != Some(target.account_id.as_str())
        || !target.venue_id.eq_ignore_ascii_case(expected_venue)
        || worker
            .venue_id
            .as_deref()
            .is_some_and(|venue| !venue.eq_ignore_ascii_case(expected_venue))
    {
        return Err("AmendOrder 账户或交易所与执行 worker 不一致".into());
    }
    let log_name = required_account_event_log(worker)?;
    let pipeline = pipeline_storage
        .open(log_name, settlement_currency.to_owned())
        .map_err(|error| format!("打开改单账户 EventLog 失败: {error}"))?;
    let order = pipeline
        .orders()
        .into_iter()
        .find(|order| order.client_id == target.client_order_id)
        .ok_or_else(|| format!("账户 EventLog 不存在订单 {}", target.client_order_id))?;
    if order.account_id != target.account_id
        || !order
            .instrument
            .venue
            .as_str()
            .eq_ignore_ascii_case(&target.venue_id)
    {
        return Err("AmendOrder 路由身份与账户 EventLog 中的订单不一致".into());
    }
    Ok(PreparedAmendOrder {
        target,
        order,
        pipeline,
    })
}

/// Bind the caller's account/venue route to an order read from the worker's own account EventLog.
/// This check runs before the venue connector is created.
pub fn prepare_cancel_order(
    command: &ControlCommand,
    worker: &WorkerConfig,
    expected_venue: &str,
    pipeline_storage: &PipelineStorage,
    settlement_currency: &str,
) -> Result<PreparedCancelOrder, String> {
    let target = cancel_order_target_from_command(command)
        .map_err(|error| format!("CancelOrder 命令无效: {error:?}"))?;
    if worker.account_id.as_deref() != Some(target.account_id.as_str())
        || !target.venue_id.eq_ignore_ascii_case(expected_venue)
        || worker.venue_id.as_deref().is_some_and(|venue| {
            if expected_venue.eq_ignore_ascii_case("BINANCE") {
                VenueFamily::parse(venue) != VenueFamily::Binance
            } else {
                !venue.eq_ignore_ascii_case(expected_venue)
            }
        })
    {
        return Err("CancelOrder 账户或交易所与执行 worker 不一致".into());
    }
    let log_name = required_account_event_log(worker)?;
    let pipeline = pipeline_storage
        .open(log_name, settlement_currency.to_owned())
        .map_err(|error| format!("打开撤单账户 EventLog 失败: {error}"))?;
    let order = pipeline
        .orders()
        .into_iter()
        .find(|order| order.client_id == target.client_order_id)
        .ok_or_else(|| format!("账户 EventLog 不存在订单 {}", target.client_order_id))?;
    if order.account_id != target.account_id
        || !order
            .instrument
            .venue
            .as_str()
            .eq_ignore_ascii_case(&target.venue_id)
    {
        return Err("CancelOrder 路由身份与账户 EventLog 中的订单不一致".into());
    }
    Ok(PreparedCancelOrder {
        target,
        order,
        pipeline,
    })
}

/// Run cancellation through the same OMS/EventLog gateway used by Paper and submit workers.
pub fn execute_prepared_cancel_order<V: Venue>(
    command: &ControlCommand,
    worker: &WorkerConfig,
    pipeline_storage: &PipelineStorage,
    prepared: &mut PreparedCancelOrder,
    venue: &mut V,
    now: u64,
    source_seq: &mut u64,
) -> Result<String, String> {
    let store = open_spread_group_store(&pipeline_storage.root)?;
    let mut venue_port = BorrowedVenuePort::new(venue);
    let mut gateway = ExecutionGateway::new(
        &mut venue_port,
        &mut prepared.pipeline,
        &worker.id,
        now,
        source_seq,
    )
    .with_spread_group_store(&store);
    gateway.cancel_command(command).map(|result| result.message)
}

pub fn execute_prepared_amend_order<V: Venue>(
    command: &ControlCommand,
    worker: &WorkerConfig,
    pipeline_storage: &PipelineStorage,
    runtime_config_path: &Path,
    prepared: &mut PreparedAmendOrder,
    venue: &mut V,
    now: u64,
    source_seq: &mut u64,
) -> Result<String, String> {
    let log_name = required_account_event_log(worker)?;
    let _admission_lock =
        crate::risk_host::worker_account_admission_lock(&pipeline_storage.root, &log_name)?;
    prepared
        .pipeline
        .refresh()
        .map_err(|error| format!("刷新改单账户 EventLog 失败: {error:?}"))?;
    crate::risk_host::require_no_unresolved_live_order_operations(
        &worker.id,
        worker.account_id.as_deref(),
        worker.venue_id.as_deref(),
        &prepared.pipeline,
        Some(prepared.target.client_order_id),
    )?;
    crate::risk_host::require_fresh_live_balance_snapshot(
        &worker.id,
        worker.account_id.as_deref(),
        worker.venue_id.as_deref(),
        &prepared.pipeline,
        now,
    )?;
    let Some((risk, position)) = crate::risk_host::worker_risk_context(
        worker,
        &prepared.target.order,
        &prepared.pipeline,
        None,
        Some(runtime_config_path),
    )?
    else {
        return Err(format!(
            "FAIL_CLOSED: worker {} 缺少改单风控规格，拒绝改单",
            worker.id
        ));
    };
    let decision = risk.evaluate_order(&prepared.target.order, &position);
    if !decision.allowed {
        return Err(format!("改单风控拒绝: {}", decision.violations.join("; ")));
    }
    if let Some(spec) = risk.instrument_spec.as_ref() {
        venue
            .validate_instrument_spec(spec)
            .map_err(|error| format!("FAIL_CLOSED: 单改市场规格校验失败: {error:?}"))?;
        venue
            .validate_order_constraints(spec, &prepared.target.order)
            .map_err(|error| format!("FAIL_CLOSED: 改单不满足交易所市场过滤器: {error:?}"))?;
    }
    let mut venue_port = BorrowedVenuePort::new(venue);
    let mut gateway = ExecutionGateway::new(
        &mut venue_port,
        &mut prepared.pipeline,
        &worker.id,
        now,
        source_seq,
    );
    gateway.amend_command(command).map(|result| result.message)
}
