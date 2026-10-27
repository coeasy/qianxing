//! Venue market-data worker hosts and their supervised polling/stream loops.

use crate::exchange_gateway::{ExchangeGatewayPort, GatewayBusinessHealthStatus};
use crate::market_bridges::{
    bridge_market_quote_to_paper, observe_paper_market_bridges, open_paper_market_bridges,
    worker_scoped_event_log_name, PaperMarketQuote,
};
use crate::pipeline::PipelineStorage;
use crate::pipeline_metrics_report::PipelineMetricsReporter;
use qx_adapter::{BinanceQuotePoll, BinanceSpotMarketStream};
use qx_core::{InstrumentId, VenueFamily};
use qx_runtime::{
    worker_settlement_currency, RuntimeConfig, RuntimeEventEnvelope, ServiceStatus, WorkerConfig,
    WorkerContext, WorkerRole,
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Run the Binance public market stream for one configured MarketData worker.
///
/// The market event log and Paper mirrors are service-owned. The caller is responsible for
/// selecting the configured worker and supervising this runner's returned result.
pub fn run_binance_market_worker(
    context: WorkerContext,
    worker: WorkerConfig,
    pipeline_storage: PipelineStorage,
    config: RuntimeConfig,
) -> Result<(), String> {
    if worker.role != WorkerRole::MarketData
        || VenueFamily::parse_option(worker.venue_id.as_deref()) != Some(VenueFamily::Binance)
    {
        return Err(format!(
            "worker {} 不是 Binance MarketData worker",
            worker.id
        ));
    }
    if worker.symbols.len() != 1 {
        return Err(format!(
            "当前 Binance market worker 要求恰好一个 symbol，实际 {}",
            worker.symbols.len()
        ));
    }
    let instrument = InstrumentId::parse(&worker.symbols[0])
        .ok_or_else(|| format!("worker {} instrument 非法", worker.id))?;
    let (host, port) = configured_binance_ws_endpoint(&worker)?;
    let log_name = worker_scoped_event_log_name("", &worker.id);
    let mut pipeline = pipeline_storage
        .open(log_name, worker_settlement_currency(&worker))
        .map_err(|error| format!("创建行情事件管线失败: {error}"))?;
    let mut metrics = PipelineMetricsReporter::new(&config, context.id(), "");
    let mut paper_bridges = open_paper_market_bridges(&pipeline_storage, &config)?;
    let mut stream = BinanceSpotMarketStream::connect_with_endpoint(
        instrument.clone(),
        host,
        port,
        Duration::from_secs(10),
    )?;
    context.mark(
        ServiceStatus::Ready,
        "market stream connected",
        Some(runtime_timestamp_ms()),
    )?;
    metrics.publish_if_due(true, runtime_timestamp_ms());
    let mut quotes = 0_u64;
    let mut idle_windows = 0_u64;
    while !context.should_stop() {
        let poll_now = runtime_timestamp_ms();
        let poll = match stream.recv_quote(poll_now) {
            Ok(poll) => poll,
            Err(error) => {
                metrics.observe_gateway_port_business_status(
                    ExchangeGatewayPort::MarketData,
                    GatewayBusinessHealthStatus::Unhealthy,
                    poll_now,
                );
                metrics.publish(false, poll_now);
                return Err(format!("Binance market stream 读取行情失败: {error}"));
            }
        };
        match poll {
            BinanceQuotePoll::Quote(quote) => {
                idle_windows = 0;
                let received_ts = runtime_timestamp_ms();
                pipeline
                    .ingest(RuntimeEventEnvelope::market_quote(
                        instrument.clone(),
                        quote,
                        received_ts,
                        quote.source_seq,
                        format!("{}:quote:{}", worker.id, quote.source_seq),
                    ))
                    .map_err(|error| format!("行情事实归约失败: {error:?}"))?;
                metrics.observe_gateway_port_business_status(
                    ExchangeGatewayPort::MarketData,
                    GatewayBusinessHealthStatus::Healthy,
                    received_ts,
                );
                metrics.observe(&pipeline);
                metrics.publish_if_due(true, received_ts);
                bridge_market_quote_to_paper(
                    &mut paper_bridges,
                    PaperMarketQuote {
                        source_worker_id: &worker.id,
                        instrument: &instrument,
                        bid: quote.bid,
                        bid_qty: quote.bid_qty,
                        bid_qty_exact: quote.bid_qty_exact,
                        ask: quote.ask,
                        ask_qty: quote.ask_qty,
                        ask_qty_exact: quote.ask_qty_exact,
                        event_ts: quote.ts,
                        receive_ts: received_ts,
                        source_seq: quote.source_seq,
                    },
                )?;
                observe_paper_market_bridges(&paper_bridges, &mut metrics);
                quotes = quotes.saturating_add(1);
                context.heartbeat(received_ts)?;
            }
            // An idle read window is healthy-but-degraded and remains cancellable.
            BinanceQuotePoll::Idle => {
                idle_windows = idle_windows.saturating_add(1);
                let now = runtime_timestamp_ms();
                metrics.observe_gateway_port_business_status(
                    ExchangeGatewayPort::MarketData,
                    GatewayBusinessHealthStatus::Degraded,
                    now,
                );
                metrics.publish_if_due(true, now);
                context.mark(
                    ServiceStatus::Degraded,
                    format!("market stream idle consecutive_windows={idle_windows}"),
                    Some(now),
                )?;
            }
            BinanceQuotePoll::Closed => {
                let now = runtime_timestamp_ms();
                metrics.observe_gateway_port_business_status(
                    ExchangeGatewayPort::MarketData,
                    GatewayBusinessHealthStatus::Unhealthy,
                    now,
                );
                break;
            }
        }
    }
    let _ = stream.close();
    context.mark(
        ServiceStatus::Stopped,
        format!("market stream stopped quotes={quotes} idle_windows={idle_windows}"),
        Some(runtime_timestamp_ms()),
    )?;
    metrics.publish(false, runtime_timestamp_ms());
    Ok(())
}

pub fn configured_binance_ws_endpoint(worker: &WorkerConfig) -> Result<(String, u16), String> {
    let endpoint = worker
        .endpoint
        .as_deref()
        .ok_or_else(|| format!("worker {} 缺少 WebSocket endpoint", worker.id))?;
    let remainder = endpoint
        .strip_prefix("wss://")
        .or_else(|| endpoint.strip_prefix("ws://"))
        .ok_or_else(|| format!("worker {} endpoint 必须使用 ws:// 或 wss://", worker.id))?;
    let authority = remainder
        .split('/')
        .next()
        .filter(|authority| !authority.is_empty())
        .ok_or_else(|| format!("worker {} endpoint 缺少主机", worker.id))?;
    let (host, port) = authority
        .rsplit_once(':')
        .filter(|(_, port)| !port.is_empty() && port.chars().all(|ch| ch.is_ascii_digit()))
        .map(|(host, port)| (host, port.parse::<u16>().unwrap_or_default()))
        .unwrap_or((authority, 443));
    if host.is_empty()
        || host.contains(':')
        || host.contains('\r')
        || host.contains('\n')
        || port == 0
    {
        return Err(format!("worker {} endpoint 主机非法", worker.id));
    }
    Ok((host.to_string(), port))
}

fn runtime_timestamp_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

#[cfg(test)]
mod tests {
    use super::configured_binance_ws_endpoint;
    use qx_runtime::{WorkerConfig, WorkerRole};

    fn worker(endpoint: Option<&str>) -> WorkerConfig {
        WorkerConfig {
            id: "market-binance".into(),
            role: WorkerRole::MarketData,
            enabled: true,
            account_id: None,
            venue_id: Some("binance".into()),
            endpoint: endpoint.map(str::to_owned),
            symbols: vec!["BTCUSDT.BINANCE".into()],
            settlement_currency: None,
            credential_env: None,
            credential_files: None,
            instrument_spec_path: None,
            paper_initial_cash_raw: None,
            max_order_notional_raw: None,
            max_position_notional_raw: None,
        }
    }

    #[test]
    fn websocket_endpoint_uses_tls_default_port_and_explicit_port() {
        assert_eq!(
            configured_binance_ws_endpoint(&worker(Some("wss://stream.binance.com/ws"))).unwrap(),
            ("stream.binance.com".into(), 443)
        );
        assert_eq!(
            configured_binance_ws_endpoint(&worker(Some("ws://localhost:9443/stream"))).unwrap(),
            ("localhost".into(), 9443)
        );
    }

    #[test]
    fn websocket_endpoint_fails_closed_for_missing_or_invalid_scheme() {
        assert!(configured_binance_ws_endpoint(&worker(None)).is_err());
        assert!(configured_binance_ws_endpoint(&worker(Some("https://example.com/ws"))).is_err());
        assert!(configured_binance_ws_endpoint(&worker(Some("ws://example.com:0/ws"))).is_err());
    }
}
