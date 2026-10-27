//! Shared live CancelOrder resolution and execution over the authoritative account OMS.

use crate::pipeline::PipelineStorage;
use crate::spread_recovery_host::open_spread_group_store;
use qx_control::{cancel_order_target_from_command, CancelOrderTarget, ControlCommand};
use qx_core::Order;
use qx_execution::{BorrowedVenuePort, ExecutionGateway};
use qx_runtime::{required_account_event_log, LiveEventPipeline, WorkerConfig};
use qx_zhenlu::Venue;

pub struct PreparedCancelOrder {
    pub target: CancelOrderTarget,
    pub order: Order,
    pub pipeline: LiveEventPipeline,
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
        || worker
            .venue_id
            .as_deref()
            .is_some_and(|venue| !venue.eq_ignore_ascii_case(expected_venue))
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
