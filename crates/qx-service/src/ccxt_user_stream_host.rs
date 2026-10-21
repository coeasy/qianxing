//! CCXT Pro account order stream lifecycle and EventLog reduction.

use crate::ccxt_stream_retry::{
    ccxt_idle_poll_delay, ccxt_watch_reply_is_idle, CcxtReconnectBudget,
};
use crate::pipeline::PipelineStorage;
use crate::pipeline_metrics_report::PipelineMetricsReporter;
use crate::worker_market_spec::load_worker_market_spec_for_instrument;
use qx_adapter::{CcxtCallOutcome, CcxtProcessClient, CcxtProcessVenue};
use qx_core::VenueFamily;
use qx_execution::{ingest_venue_events, ingest_venue_events_with_spec, EventLogReconcilePort};
use qx_runtime::{
    account_worker_settlement_currency, required_account_event_log, RuntimeConfig, ServiceStatus,
    WorkerConfig, WorkerContext, WorkerRole,
};
use std::path::PathBuf;
use std::time::Instant;

pub struct CcxtUserStreamWorkerInput {
    pub context: WorkerContext,
    pub worker: WorkerConfig,
    pub pipeline_storage: PipelineStorage,
    pub runtime_config: RuntimeConfig,
    pub python: String,
    pub ccxt_config_path: String,
    pub runtime_config_path: PathBuf,
    pub once: bool,
}

/// Run the CCXT Pro `watch_orders` stream, enrich known updates through REST, and reduce them into
/// the canonical account EventLog. Process spawning stays injectable at the interpreter/config edge.
pub fn run_ccxt_user_stream_worker(input: CcxtUserStreamWorkerInput) -> Result<(), String> {
    let CcxtUserStreamWorkerInput {
        context,
        worker,
        pipeline_storage,
        runtime_config,
        python,
        ccxt_config_path,
        runtime_config_path,
        once,
    } = input;
    let venue_id = validate_ccxt_user_stream_worker(&worker)?;
    let log_name = required_account_event_log(&worker)?;
    let mut pipeline = pipeline_storage
        .open(
            log_name.clone(),
            account_worker_settlement_currency(&runtime_config, &worker)?,
        )
        .map_err(|error| format!("打开 CCXT UserStream EventLog 失败: {error}"))?;
    let mut metrics = PipelineMetricsReporter::new(&runtime_config, context.id(), &log_name);
    metrics.publish_if_due(true, runtime_timestamp_ms());
    context.mark(
        ServiceStatus::Ready,
        format!("ccxt pro user stream venue={venue_id}"),
        Some(runtime_timestamp_ms()),
    )?;

    let client = CcxtProcessClient::spawn(&python, &ccxt_config_path, None)
        .map_err(|error| format!("启动公共 CCXT Pro Worker 失败: {error}"))?;
    // Reply before 4/5 of the driver's read window so empty but healthy accounts stay cancellable.
    let watch_idle_ms = qx_adapter::ccxt_idle_window_ms(client.timeout_ms());
    let mut venue = CcxtProcessVenue::new(venue_id, Box::new(client));
    let mut source_seq = pipeline
        .log()
        .events()
        .last()
        .map(|event| event.source_seq)
        .unwrap_or(0);
    let mut reconnect_budget = CcxtReconnectBudget::default();
    let mut idle_windows = 0_u64;
    loop {
        if context.should_stop() {
            break;
        }
        pipeline
            .refresh()
            .map_err(|error| format!("刷新 CCXT UserStream EventLog 失败: {error:?}"))?;
        let received_ts = runtime_timestamp_ms();
        metrics.observe(&pipeline);
        metrics.publish_if_due(true, received_ts);
        let stream_call_started = Instant::now();
        let result = match venue.stream_call_cancellable(
            serde_json::json!({
                "op": "watch_orders",
                "instrument": null,
                "received_ts": received_ts,
                "wait_ms": watch_idle_ms,
            }),
            &mut || context.should_stop(),
        ) {
            Ok(CcxtCallOutcome::Response(result)) => result,
            Ok(CcxtCallOutcome::Cancelled) => break,
            Err(error) => {
                let detail = format!("{error:?}");
                if detail.contains("[unsupported]") || detail.contains("[authentication]") {
                    return Err(format!("CCXT Pro 用户流不可用: {detail}"));
                }
                let backoff = reconnect_budget
                    .note_failure()
                    .map_err(|gave_up| format!("{gave_up}；最后一次故障: {detail}"))?;
                context.mark(
                    ServiceStatus::Degraded,
                    format!(
                        "ccxt pro user stream reconnecting failures={}: {detail}",
                        reconnect_budget.consecutive_failures()
                    ),
                    Some(received_ts),
                )?;
                context.sleep_ms(backoff.as_millis() as u64);
                if context.should_stop() {
                    break;
                }
                let replacement = CcxtProcessClient::spawn(&python, &ccxt_config_path, None)
                    .map_err(|spawn_error| {
                        format!("重连公共 CCXT Pro Worker 失败: {spawn_error}")
                    })?;
                venue.replace_rpc(Box::new(replacement));
                continue;
            }
        };
        reconnect_budget.note_success();
        let event = result
            .get("event")
            .ok_or_else(|| "CCXT Pro watch_orders 响应缺少 event".to_string())?;
        if event.get("stream").and_then(serde_json::Value::as_str) != Some("orders") {
            return Err("CCXT Pro 用户流返回了非 orders 事件".into());
        }
        if ccxt_watch_reply_is_idle(event) {
            idle_windows = idle_windows.saturating_add(1);
            context.mark(
                ServiceStatus::Degraded,
                format!("ccxt pro user stream idle consecutive_windows={idle_windows}"),
                Some(received_ts),
            )?;
            metrics.observe(&pipeline);
            metrics.publish_if_due(true, received_ts);
            if once {
                break;
            }
            let idle_poll_delay = ccxt_idle_poll_delay(stream_call_started.elapsed());
            if !idle_poll_delay.is_zero() {
                context.sleep_ms(idle_poll_delay.as_millis() as u64);
            }
            continue;
        }
        idle_windows = 0;
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
                // Unknown remote orders are never auto-registered or resubmitted; reconciliation owns discovery.
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
                    EventLogReconcilePort::new(
                        &mut pipeline,
                        &worker.id,
                        received_ts,
                        &mut source_seq,
                    )
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
            let reduced_events = if let Some(spec) = load_worker_market_spec_for_instrument(
                &worker,
                &order.instrument,
                Some(&runtime_config_path),
            )? {
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
            ServiceStatus::Ready,
            format!(
                "ccxt pro orders matched={matched} reduced={reduced} reconcile_errors={reconcile_errors}"
            ),
            Some(received_ts),
        )?;
        context.heartbeat(received_ts)?;
        metrics.observe(&pipeline);
        metrics.publish_if_due(true, received_ts);
        if once {
            break;
        }
    }
    context.mark(
        ServiceStatus::Stopped,
        format!("ccxt pro user stream stopped idle_windows={idle_windows}"),
        Some(runtime_timestamp_ms()),
    )?;
    metrics.publish(false, runtime_timestamp_ms());
    Ok(())
}

fn validate_ccxt_user_stream_worker(worker: &WorkerConfig) -> Result<String, String> {
    if !worker.enabled
        || worker.role != WorkerRole::UserStream
        || VenueFamily::parse_option(worker.venue_id.as_deref()) != Some(VenueFamily::Other)
    {
        return Err(format!(
            "worker {} 不是已启用的 CCXT UserStream worker",
            worker.id
        ));
    }
    worker
        .venue_id
        .clone()
        .filter(|venue| !venue.trim().is_empty())
        .ok_or_else(|| format!("worker {} 缺少 venue_id", worker.id))
}

fn runtime_timestamp_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

#[cfg(test)]
mod tests {
    use super::validate_ccxt_user_stream_worker;
    use qx_runtime::WorkerConfig;

    fn worker(role: &str, enabled: bool, venue_id: &str) -> WorkerConfig {
        serde_json::from_value(serde_json::json!({
            "id": "ccxt-user-stream",
            "role": role,
            "enabled": enabled,
            "account_id": "main",
            "venue_id": venue_id
        }))
        .expect("valid worker fixture")
    }

    #[test]
    fn ccxt_user_stream_requires_enabled_role_and_non_binance_venue() {
        assert_eq!(
            validate_ccxt_user_stream_worker(&worker("user_stream", true, "okx")).unwrap(),
            "okx"
        );
        assert!(validate_ccxt_user_stream_worker(&worker("execution", true, "okx")).is_err());
        assert!(validate_ccxt_user_stream_worker(&worker("user_stream", false, "okx")).is_err());
        assert!(validate_ccxt_user_stream_worker(&worker("user_stream", true, "binance")).is_err());
    }
}
