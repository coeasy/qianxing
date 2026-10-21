//! Test-only compatibility adapters for risk scenarios that share the CLI test harness.

use crate::{LiveEventPipeline, Order, RiskContext, WorkerConfig};
use qx_risk::OrderRiskPosition;
use std::path::Path;

pub(crate) fn require_fresh_live_balance_snapshot(
    worker: &WorkerConfig,
    pipeline: &LiveEventPipeline,
    now: u64,
) -> Result<(), String> {
    qx_service::risk_host::require_fresh_live_balance_snapshot(
        &worker.id,
        worker.account_id.as_deref(),
        worker.venue_id.as_deref(),
        pipeline,
        now,
    )
}

pub(crate) fn worker_risk_context(
    worker: &WorkerConfig,
    order: &Order,
    pipeline: &LiveEventPipeline,
    runtime_config_path: Option<&Path>,
) -> Result<Option<(RiskContext, OrderRiskPosition)>, String> {
    qx_service::risk_host::worker_risk_context(worker, order, pipeline, None, runtime_config_path)
}

pub(crate) fn worker_risk_context_with_config(
    worker: &WorkerConfig,
    order: &Order,
    pipeline: &LiveEventPipeline,
    runtime_config: &crate::RuntimeConfig,
    runtime_config_path: Option<&Path>,
) -> Result<Option<(RiskContext, OrderRiskPosition)>, String> {
    qx_service::risk_host::worker_risk_context(
        worker,
        order,
        pipeline,
        Some(runtime_config),
        runtime_config_path,
    )
}
