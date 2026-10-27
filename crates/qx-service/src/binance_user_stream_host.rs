//! Binance private user-stream worker lifecycle.

use crate::exchange_gateway::{ExchangeGatewayPort, GatewayBusinessHealthStatus};
use crate::pipeline::PipelineStorage;
use crate::pipeline_metrics_report::PipelineMetricsReporter;
use crate::venue_market_host::configured_binance_ws_endpoint;
use crate::worker_market_spec::worker_report_spec;
use qx_adapter::{
    run_binance_user_stream_with_config_loader, BinanceSpotAuth, BinanceSpotVenue,
    BinanceStreamRetryPolicy, BinanceUserStreamRunConfig,
};
use qx_core::VenueFamily;
use qx_execution::{ingest_venue_events, ingest_venue_events_with_spec};
use qx_runtime::{
    account_worker_settlement_currency, required_account_event_log, RuntimeConfig, ServiceStatus,
    WorkerConfig, WorkerContext, WorkerRole,
};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Own the Binance UserStream worker's EventLog, event reduction, metrics, reconnect loop and
/// cancellation lifecycle. The facade supplies credentials and the configured venue instance.
pub fn run_binance_user_stream_worker<F>(
    context: WorkerContext,
    worker: WorkerConfig,
    pipeline_storage: PipelineStorage,
    runtime_config: RuntimeConfig,
    runtime_config_path: PathBuf,
    mut venue: BinanceSpotVenue,
    mut load_auth: F,
) -> Result<(), String>
where
    F: FnMut(&WorkerConfig) -> Result<BinanceSpotAuth, String>,
{
    if worker.role != WorkerRole::UserStream
        || VenueFamily::parse_option(worker.venue_id.as_deref()) != Some(VenueFamily::Binance)
    {
        return Err(format!(
            "worker {} 不是 Binance UserStream worker",
            worker.id
        ));
    }
    let policy =
        BinanceStreamRetryPolicy::new(10, Duration::from_secs(1), Duration::from_secs(30))?;
    let log_name = required_account_event_log(&worker)?;
    let mut pipeline = pipeline_storage
        .open(
            log_name.clone(),
            account_worker_settlement_currency(&runtime_config, &worker)?,
        )
        .map_err(|error| format!("创建用户流事件管线失败: {error}"))?;
    let metrics = Arc::new(Mutex::new(PipelineMetricsReporter::new(
        &runtime_config,
        context.id(),
        &log_name,
    )));
    venue
        .restore_orders(pipeline.orders())
        .map_err(|error| format!("恢复用户流订单状态失败: {error:?}"))?;

    let (host, port) = configured_binance_ws_endpoint(&worker)?;
    let run_config = BinanceUserStreamRunConfig::with_port(
        host,
        port,
        worker.id.clone(),
        Duration::from_secs(10),
        policy,
    )?;
    let stop_context = context.clone();
    let sleep_context = context.clone();
    let sleep_metrics = Arc::clone(&metrics);
    let event_context = context.clone();
    let mut source_seq = 0_u64;
    let mut should_stop = move || stop_context.should_stop();
    let mut sleep = move |delay: Duration| {
        sleep_context.sleep_ms(delay.as_millis() as u64);
        if !sleep_context.should_stop() {
            let now = runtime_timestamp_ms();
            let _ = sleep_context.heartbeat(now);
            if let Ok(mut metrics) = sleep_metrics.lock() {
                metrics.observe_gateway_port_business_status(
                    ExchangeGatewayPort::PrivateOrderStream,
                    GatewayBusinessHealthStatus::Degraded,
                    now,
                );
                metrics.publish_if_due(true, now);
            }
        }
    };
    let event_worker = worker.clone();
    let event_config_path = runtime_config_path.clone();
    let event_metrics = Arc::clone(&metrics);
    let mut on_event = move |payload: &str| -> Result<(), String> {
        pipeline
            .refresh()
            .map_err(|error| format!("刷新共享订单 EventLog 失败: {error:?}"))?;
        venue
            .refresh_orders(pipeline.orders())
            .map_err(|error| format!("刷新用户流订单状态失败: {error:?}"))?;
        let events = match venue.ingest_user_event(payload) {
            Ok(events) => events,
            Err(error) => {
                // An expired listen key closes the current session so the adapter reconnects;
                // the reconciler confirms state from REST before submissions resume.
                if payload.contains("listenKeyExpired") {
                    venue.reconnect();
                }
                let now = runtime_timestamp_ms();
                if let Ok(mut metrics) = event_metrics.lock() {
                    metrics.observe_gateway_port_business_status(
                        ExchangeGatewayPort::PrivateOrderStream,
                        GatewayBusinessHealthStatus::Degraded,
                        now,
                    );
                    metrics.publish_if_due(true, now);
                }
                return Err(format!("Binance 用户事件处理失败: {error:?}"));
            }
        };
        let received_ts = runtime_timestamp_ms();
        let spec = worker_report_spec(&event_worker, &events, &pipeline, Some(&event_config_path))?;
        let event_count = match &spec {
            Some(spec) => ingest_venue_events_with_spec(
                &mut pipeline,
                events,
                &event_worker.id,
                received_ts,
                &mut source_seq,
                spec,
            )?,
            None => ingest_venue_events(
                &mut pipeline,
                events,
                &event_worker.id,
                received_ts,
                &mut source_seq,
            )?,
        };
        event_context.heartbeat(received_ts)?;
        if let Ok(mut metrics) = event_metrics.lock() {
            metrics.observe_gateway_port_business_status(
                ExchangeGatewayPort::PrivateOrderStream,
                GatewayBusinessHealthStatus::Healthy,
                received_ts,
            );
            metrics.observe(&pipeline);
            metrics.publish_if_due(true, received_ts);
        }
        if event_count != 0 {
            event_context.mark(
                ServiceStatus::Ready,
                format!("user events={event_count}"),
                Some(received_ts),
            )?;
        }
        Ok(())
    };
    if let Ok(mut metrics) = metrics.lock() {
        metrics.publish_if_due(true, runtime_timestamp_ms());
    }
    let report = run_binance_user_stream_with_config_loader(
        || load_auth(&worker),
        run_config,
        &mut should_stop,
        &mut sleep,
        &mut on_event,
    );
    drop(on_event);
    if let Ok(metrics) = metrics.lock() {
        metrics.publish(false, runtime_timestamp_ms());
    }
    let report = report?;
    context.mark(
        ServiceStatus::Stopped,
        format!("user stream stopped events={}", report.events),
        Some(runtime_timestamp_ms()),
    )?;
    Ok(())
}

fn runtime_timestamp_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}
