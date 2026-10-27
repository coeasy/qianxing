use crate::ccxt_live_bars::{closed_live_frame, write_live_bar_snapshot, LiveStrategyBarSpec};
use crate::ccxt_stream_retry::CcxtChannelBudgets;
use crate::exchange_gateway::{ExchangeGatewayPort, GatewayBusinessHealthStatus};
use crate::market_bridges::{
    bridge_market_quote_to_paper, ccxt_market_event_log_name, observe_paper_market_bridges,
    open_paper_market_bridges, PaperMarketQuote,
};
use crate::pipeline::PipelineStorage;
use crate::pipeline_metrics_report::PipelineMetricsReporter;
use crate::worker_host;
use qx_adapter::{CcxtCallOutcome, CcxtProcessClient};
use qx_core::{ExactDecimal, InstrumentId, MarketDepthLevel, Price, Quantity};
use qx_datastruct::BarFrame;
use qx_guanxing::QuoteTick;
use qx_runtime::{
    worker_settlement_currency, RuntimeConfig, RuntimeEventEnvelope, ServiceStatus, WorkerConfig,
    WorkerContext, WorkerRole,
};
use std::time::{SystemTime, UNIX_EPOCH};

/// 一轮轮询的健康结论：本轮任何一次 ticker / OHLCV 调用失败都不能被"healthy"覆盖。
///
/// 判定此前直接写在 worker 循环末尾的 `context.mark(Ready, ...)` 里：全部标的都失败的
/// worker 每轮仍然报 Ready + `ccxt live market healthy`，运维侧看到的永远是健康。
pub fn ccxt_market_cycle_health(
    failures: u32,
    attempted: u32,
    quotes: u64,
    bars_written: u64,
) -> (ServiceStatus, String) {
    if failures == 0 {
        return (
            ServiceStatus::Ready,
            format!("ccxt live market healthy quotes={quotes} bar_snapshots={bars_written}"),
        );
    }
    (
        ServiceStatus::Degraded,
        format!(
            "ccxt live market 本轮 {failures}/{attempted} 次行情调用失败 quotes={quotes} bar_snapshots={bars_written}"
        ),
    )
}

pub struct CcxtMarketWorkerInput {
    pub context: WorkerContext,
    pub worker: WorkerConfig,
    pub pipeline_storage: PipelineStorage,
    pub ccxt_config_path: String,
    pub python: String,
    pub runtime_config: RuntimeConfig,
    pub live_specs: Vec<LiveStrategyBarSpec>,
    pub orderbook_instruments: Vec<InstrumentId>,
    pub once: bool,
}

/// 使用公共 CCXT REST ticker 和 OHLCV 轮询接入统一行情 EventLog，并将闭合
/// BarFrame 原子写入策略快照。Strategy worker 通过快照摘要触发幂等 JobQueue，
/// 因而不依赖 Scheduler 的固定周期，也不会因为未闭合 K 线反复下单。
pub fn run_ccxt_market_worker(input: CcxtMarketWorkerInput) -> Result<(), String> {
    let CcxtMarketWorkerInput {
        context,
        worker,
        pipeline_storage,
        ccxt_config_path,
        python,
        runtime_config,
        live_specs,
        orderbook_instruments,
        once,
    } = input;
    if worker.role != WorkerRole::MarketData {
        return Err(format!("worker {} 不是 CCXT MarketData worker", worker.id));
    }
    if worker.symbols.is_empty() {
        return Err(format!("worker {} 至少需要一个 CCXT instrument", worker.id));
    }
    let log_name = ccxt_market_event_log_name(&worker);
    let mut client = CcxtProcessClient::spawn(&python, &ccxt_config_path, None)
        .map_err(|error| format!("启动公共 CCXT MarketData Worker 失败: {error}"))?;
    let mut pipeline = pipeline_storage
        .open(log_name.clone(), worker_settlement_currency(&worker))
        .map_err(|error| format!("创建 CCXT 行情事件管线失败: {error}"))?;
    let mut metrics = PipelineMetricsReporter::new(&runtime_config, context.id(), "");
    let mut paper_bridges = open_paper_market_bridges(&pipeline_storage, &runtime_config)?;
    let instruments = worker
        .symbols
        .iter()
        .map(|symbol| {
            InstrumentId::parse(symbol)
                .ok_or_else(|| format!("worker {} instrument 非法: {symbol}", worker.id))
        })
        .collect::<Result<Vec<_>, _>>()?;
    context.mark(
        ServiceStatus::Ready,
        format!(
            "ccxt ticker/ohlcv polling instruments={} live_snapshots={}",
            instruments.len(),
            live_specs.len()
        ),
        Some(runtime_timestamp_ms()),
    )?;
    metrics.publish_if_due(true, runtime_timestamp_ms());
    let mut source_seq = 0_u64;
    let mut quotes = 0_u64;
    let mut bars_written = 0_u64;
    let mut order_books_written = 0_u64;
    let attempted_calls =
        (instruments.len() + live_specs.len() + orderbook_instruments.len()) as u32;
    // 旧口径只在每轮把 `cycle_failures` 报出去、下一轮又清零，于是柜台彻底不可用时
    // 这个循环会无上限地重生 Python 子进程（每 500ms 一个）。预算按**连续** RPC 失败计，
    // 任一成功应答清零，超过上限即具名放弃（V13 R2 #167）。
    //
    // 但一份预算管所有标的会被健康 symbol 反复清零：一个坏 instrument 每轮排在一个好
    // instrument 后面，永远攒不到第 10 次，子进程被它无限重生（V13 R2 #202）。所以按
    // `op:instrument` 分通道记账，放弃时点名的那条就是真正死掉的那条。
    let mut reconnect_budget = CcxtChannelBudgets::market_rpc();
    worker_host::run_polling_worker(&context, once, || {
        let cycle_now = runtime_timestamp_ms();
        let mut cycle_failures = 0_u32;
        for instrument in &instruments {
            if context.should_stop() {
                break;
            }
            let channel = format!("fetch_ticker:{instrument}");
            let result = match client.call_cancellable(
                serde_json::json!({
                    "op": "fetch_ticker",
                    "instrument": instrument.to_string(),
                }),
                || context.should_stop(),
            ) {
                Ok(CcxtCallOutcome::Response(result)) => {
                    reconnect_budget.note_success(&channel);
                    result
                }
                Ok(CcxtCallOutcome::Cancelled) => return Ok(1),
                Err(error) => {
                    cycle_failures = cycle_failures.saturating_add(1);
                    let delay = reconnect_budget.note_failure(&channel)?;
                    context.mark(
                        ServiceStatus::Degraded,
                        format!(
                            "CCXT ticker 暂时失败 {}: {error}; reconnecting (本通道连续 {} 次)",
                            instrument,
                            reconnect_budget.consecutive_failures(&channel)
                        ),
                        Some(cycle_now),
                    )?;
                    client = CcxtProcessClient::spawn(&python, &ccxt_config_path, None).map_err(
                        |spawn_error| format!("重启公共 CCXT Worker 失败: {spawn_error}"),
                    )?;
                    context.sleep_ms(delay.as_millis() as u64);
                    continue;
                }
            };
            let ticker = result
                .get("ticker")
                .ok_or_else(|| format!("CCXT ticker 响应缺少 ticker: {instrument}"))?;
            let bid = raw_json_i128(ticker, "bid_raw")?;
            let ask = raw_json_i128(ticker, "ask_raw")?;
            let bid_qty = raw_json_i128(ticker, "bid_qty_raw")?;
            let ask_qty = raw_json_i128(ticker, "ask_qty_raw")?;
            let bid_exact = exact_json_decimal_or_raw(ticker, "bid_exact", "bid_raw")?;
            let ask_exact = exact_json_decimal_or_raw(ticker, "ask_exact", "ask_raw")?;
            let bid_qty_exact = exact_json_decimal_or_raw(ticker, "bid_qty_exact", "bid_qty_raw")?;
            let ask_qty_exact = exact_json_decimal_or_raw(ticker, "ask_qty_exact", "ask_qty_raw")?;
            if !bid_exact.is_positive()
                || !ask_exact.is_positive()
                || bid_exact
                    .checked_cmp(ask_exact)
                    .is_none_or(|ordering| ordering.is_gt())
                || !bid_qty_exact.is_positive()
                || !ask_qty_exact.is_positive()
            {
                return Err(format!("CCXT ticker 买卖价非法: {instrument}"));
            }
            let ts = ticker
                .get("timestamp_ms")
                .and_then(serde_json::Value::as_u64)
                .ok_or_else(|| format!("CCXT ticker 缺少 timestamp_ms: {instrument}"))?;
            let received_ts = runtime_timestamp_ms();
            source_seq = source_seq.saturating_add(1);
            pipeline
                .ingest(RuntimeEventEnvelope::market_quote(
                    instrument.clone(),
                    QuoteTick::new(
                        ts,
                        qx_core::Price::from_raw(bid),
                        qx_core::Quantity::from_raw(bid_qty),
                        qx_core::Price::from_raw(ask),
                        qx_core::Quantity::from_raw(ask_qty),
                        source_seq,
                    )
                    .with_exact_prices(bid_exact, ask_exact)
                    .with_exact_sizes(bid_qty_exact, ask_qty_exact),
                    received_ts,
                    source_seq,
                    format!("{}:ticker:{}", worker.id, source_seq),
                ))
                .map_err(|error| format!("CCXT 行情事实归约失败: {error:?}"))?;
            // The legacy Paper bridge still consumes 1e9 Price values. Keep it out of the
            // exact live event when that projection is zero; otherwise it would turn a valid
            // sub-nano venue quote into an invalid zero-price paper fact.
            if bid > 0 && ask > 0 {
                bridge_market_quote_to_paper(
                    &mut paper_bridges,
                    PaperMarketQuote {
                        source_worker_id: &worker.id,
                        instrument,
                        bid: qx_core::Price::from_raw(bid),
                        bid_qty: qx_core::Quantity::from_raw(bid_qty),
                        bid_qty_exact: Some(bid_qty_exact),
                        ask: qx_core::Price::from_raw(ask),
                        ask_qty: qx_core::Quantity::from_raw(ask_qty),
                        ask_qty_exact: Some(ask_qty_exact),
                        event_ts: ts,
                        receive_ts: received_ts,
                        source_seq,
                    },
                )?;
                observe_paper_market_bridges(&paper_bridges, &mut metrics);
            }
            quotes = quotes.saturating_add(1);
            context.heartbeat(received_ts)?;
        }
        for instrument in &orderbook_instruments {
            if context.should_stop() {
                break;
            }
            let channel = format!("fetch_order_book:{instrument}");
            let result = match client.call_cancellable(
                serde_json::json!({
                    "op": "fetch_order_book",
                    "instrument": instrument.to_string(),
                    "limit": 20,
                }),
                || context.should_stop(),
            ) {
                Ok(CcxtCallOutcome::Response(result)) => {
                    reconnect_budget.note_success(&channel);
                    result
                }
                Ok(CcxtCallOutcome::Cancelled) => return Ok(1),
                Err(error) => {
                    cycle_failures = cycle_failures.saturating_add(1);
                    let delay = reconnect_budget.note_failure(&channel)?;
                    context.mark(
                        ServiceStatus::Degraded,
                        format!(
                            "CCXT order book 暂时失败 {}: {error}; reconnecting (本通道连续 {} 次)",
                            instrument,
                            reconnect_budget.consecutive_failures(&channel)
                        ),
                        Some(cycle_now),
                    )?;
                    client = CcxtProcessClient::spawn(&python, &ccxt_config_path, None).map_err(
                        |spawn_error| format!("重启公共 CCXT Worker 失败: {spawn_error}"),
                    )?;
                    context.sleep_ms(delay.as_millis() as u64);
                    continue;
                }
            };
            let book = result
                .get("order_book")
                .ok_or_else(|| format!("CCXT order book 响应缺少 order_book: {instrument}"))?;
            let bids = parse_order_book_side(book, "bids")?;
            let asks = parse_order_book_side(book, "asks")?;
            let received_ts = runtime_timestamp_ms();
            let event_ts = book
                .get("timestamp_ms")
                .and_then(serde_json::Value::as_u64)
                .filter(|timestamp| *timestamp > 0)
                .unwrap_or(received_ts);
            source_seq = source_seq.saturating_add(1);
            let sequence = book
                .get("sequence")
                .and_then(serde_json::Value::as_u64)
                .filter(|sequence| *sequence > 0)
                .unwrap_or(source_seq);
            if latest_order_book_matches(&pipeline, instrument, &bids, &asks) {
                context.heartbeat(received_ts)?;
                continue;
            }
            pipeline
                .ingest(RuntimeEventEnvelope::market_order_book(
                    instrument.clone(),
                    sequence,
                    bids,
                    asks,
                    event_ts,
                    received_ts,
                    source_seq,
                    format!("{}:orderbook:{}", worker.id, source_seq),
                ))
                .map_err(|error| format!("CCXT 盘口事实归约失败: {error:?}"))?;
            order_books_written = order_books_written.saturating_add(1);
            context.heartbeat(received_ts)?;
        }
        for spec in &live_specs {
            if context.should_stop() {
                break;
            }
            let channel = format!("fetch_ohlcv:{}", spec.instrument);
            let start_ms = cycle_now.saturating_sub(
                spec.timeframe_ms
                    .saturating_mul(spec.history_limit.saturating_add(2) as u64),
            );
            let result = match client.call_cancellable(
                serde_json::json!({
                    "op": "fetch_ohlcv",
                    "instrument": spec.instrument.to_string(),
                    "timeframe": spec.timeframe,
                    "start_ms": start_ms,
                    "end_ms": cycle_now,
                    "limit": spec.history_limit.saturating_add(2),
                }),
                || context.should_stop(),
            ) {
                Ok(CcxtCallOutcome::Response(result)) => {
                    reconnect_budget.note_success(&channel);
                    result
                }
                Ok(CcxtCallOutcome::Cancelled) => return Ok(1),
                Err(error) => {
                    cycle_failures = cycle_failures.saturating_add(1);
                    let delay = reconnect_budget.note_failure(&channel)?;
                    context.mark(
                        ServiceStatus::Degraded,
                        format!(
                            "CCXT OHLCV 暂时失败 {}: {error}; reconnecting (本通道连续 {} 次)",
                            spec.instrument,
                            reconnect_budget.consecutive_failures(&channel)
                        ),
                        Some(cycle_now),
                    )?;
                    client = CcxtProcessClient::spawn(&python, &ccxt_config_path, None).map_err(
                        |spawn_error| format!("重启公共 CCXT Worker 失败: {spawn_error}"),
                    )?;
                    context.sleep_ms(delay.as_millis() as u64);
                    continue;
                }
            };
            let frame_value = result
                .get("frame")
                .ok_or_else(|| format!("CCXT OHLCV 响应缺少 frame: {}", spec.instrument))?;
            let frame = BarFrame::from_json(
                &serde_json::to_string(frame_value)
                    .map_err(|error| format!("编码 CCXT OHLCV frame 失败: {error}"))?,
            )
            .map_err(|error| format!("CCXT OHLCV BarFrame 非法 {}: {error:?}", spec.instrument))?;
            if let Some(closed) = closed_live_frame(frame, spec, cycle_now)? {
                write_live_bar_snapshot(&spec.snapshot_path, &closed)?;
                bars_written = bars_written.saturating_add(1);
            }
        }
        let (status, detail) =
            ccxt_market_cycle_health(cycle_failures, attempted_calls, quotes, bars_written);
        let detail = format!("{detail} orderbook_snapshots={order_books_written}");
        let business_status = if attempted_calls == 0 {
            GatewayBusinessHealthStatus::Unobserved
        } else if cycle_failures == 0 {
            GatewayBusinessHealthStatus::Healthy
        } else if cycle_failures >= attempted_calls {
            GatewayBusinessHealthStatus::Unhealthy
        } else {
            GatewayBusinessHealthStatus::Degraded
        };
        metrics.observe_gateway_port_business_status(
            ExchangeGatewayPort::MarketData,
            business_status,
            cycle_now,
        );
        metrics.observe(&pipeline);
        metrics.publish_if_due(true, cycle_now);
        context.mark(status, detail, Some(cycle_now))?;
        Ok(1_000)
    })?;
    context.mark(
        ServiceStatus::Stopped,
        format!("ccxt market worker stopped quotes={quotes} bar_snapshots={bars_written} orderbook_snapshots={order_books_written}"),
        Some(runtime_timestamp_ms()),
    )?;
    metrics.publish(false, runtime_timestamp_ms());
    Ok(())
}

pub fn raw_json_i128(value: &serde_json::Value, key: &str) -> Result<i128, String> {
    if let Some(value) = value.get(key).and_then(serde_json::Value::as_i64) {
        return Ok(i128::from(value));
    }
    if let Some(value) = value.get(key).and_then(serde_json::Value::as_u64) {
        return Ok(i128::from(value));
    }
    if let Some(value) = value.get(key).and_then(serde_json::Value::as_str) {
        return value
            .parse::<i128>()
            .map_err(|_| format!("CCXT 字段不是定点整数: {key}"));
    }
    Err(format!("CCXT 字段不是定点整数: {key}"))
}

fn parse_order_book_side(
    order_book: &serde_json::Value,
    side: &str,
) -> Result<Vec<MarketDepthLevel>, String> {
    let levels = order_book
        .get(side)
        .and_then(serde_json::Value::as_array)
        .filter(|levels| !levels.is_empty() && levels.len() <= 100)
        .ok_or_else(|| format!("CCXT order book {side} 必须包含 1..=100 个价格档位"))?;
    levels
        .iter()
        .enumerate()
        .map(|(index, level)| {
            let price_raw = raw_json_i128(level, "price_raw")?;
            let qty_raw = raw_json_i128(level, "qty_raw")?;
            let price_exact = exact_json_decimal_or_raw(level, "price_exact", "price_raw")?;
            let quantity_exact = exact_json_decimal_or_raw(level, "qty_exact", "qty_raw")?;
            let result = MarketDepthLevel {
                price: Price::from_raw(price_raw),
                quantity: Quantity::from_raw(qty_raw),
                price_exact: Some(price_exact),
                quantity_exact: Some(quantity_exact),
            };
            result
                .validate()
                .map_err(|error| format!("CCXT order book {side}[{index}] 非法: {error}"))?;
            Ok(result)
        })
        .collect()
}

fn latest_order_book_matches(
    pipeline: &qx_runtime::LiveEventPipeline,
    instrument: &InstrumentId,
    bids: &[MarketDepthLevel],
    asks: &[MarketDepthLevel],
) -> bool {
    pipeline.log().events().iter().rev().find_map(|event| {
        let qx_core::EventKind::MarketOrderBook {
            instrument: current_instrument,
            bids: current_bids,
            asks: current_asks,
            ..
        } = &event.kind
        else {
            return None;
        };
        (current_instrument == instrument).then(|| current_bids == bids && current_asks == asks)
    }) == Some(true)
}

pub fn exact_json_decimal_or_raw(
    value: &serde_json::Value,
    exact_key: &str,
    raw_key: &str,
) -> Result<ExactDecimal, String> {
    if let Some(exact) = value.get(exact_key) {
        if exact.is_null() {
            return ExactDecimal::from_parts(raw_json_i128(value, raw_key)?, 9)
                .map_err(|error| format!("CCXT 字段无法转为精确十进制 {raw_key}: {error}"));
        }
        let text = exact
            .as_str()
            .ok_or_else(|| format!("CCXT 精确十进制字段不是字符串: {exact_key}"))?;
        return ExactDecimal::parse(text)
            .map_err(|error| format!("CCXT 精确十进制字段非法 {exact_key}: {error}"));
    }
    ExactDecimal::from_parts(raw_json_i128(value, raw_key)?, 9)
        .map_err(|error| format!("CCXT 字段无法转为精确十进制 {raw_key}: {error}"))
}

#[cfg(test)]
mod exact_ticker_value_tests {
    use super::*;

    #[test]
    fn exact_ticker_price_is_not_lost_when_legacy_projection_is_zero() {
        let ticker = serde_json::json!({
            "bid_raw": 0,
            "ask_raw": 0,
            "bid_exact": "0.0000000007",
            "ask_exact": "0.0000000008"
        });

        assert_eq!(
            exact_json_decimal_or_raw(&ticker, "bid_exact", "bid_raw").unwrap(),
            ExactDecimal::parse("0.0000000007").unwrap()
        );
        assert_eq!(
            exact_json_decimal_or_raw(&ticker, "ask_exact", "ask_raw").unwrap(),
            ExactDecimal::parse("0.0000000008").unwrap()
        );
    }

    #[test]
    fn legacy_ticker_without_exact_sidecar_uses_scale_nine_projection() {
        let ticker = serde_json::json!({"bid_raw": 1_250_000_000_i64});
        assert_eq!(
            exact_json_decimal_or_raw(&ticker, "bid_exact", "bid_raw").unwrap(),
            ExactDecimal::parse("1.25").unwrap()
        );
    }
}

fn runtime_timestamp_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}
