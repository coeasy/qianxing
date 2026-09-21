use crate::*;

/// 使用公共 CCXT REST ticker 和 OHLCV 轮询接入统一行情 EventLog，并将闭合
/// BarFrame 原子写入策略快照。Strategy worker 通过快照摘要触发幂等 JobQueue，
/// 因而不依赖 Scheduler 的固定周期，也不会因为未闭合 K 线反复下单。
pub(crate) fn run_ccxt_market_worker(
    context: qx_runtime::WorkerContext,
    worker: WorkerConfig,
    pipeline_storage: PipelineStorage,
    ccxt_config_path: String,
    runtime_config_path: PathBuf,
    once: bool,
) -> Result<(), String> {
    if worker.role != WorkerRole::MarketData {
        return Err(format!("worker {} 不是 CCXT MarketData worker", worker.id));
    }
    if worker.symbols.is_empty() {
        return Err(format!("worker {} 至少需要一个 CCXT instrument", worker.id));
    }
    let runtime_config = read_runtime_config(&runtime_config_path)?;
    let live_specs = live_strategy_bar_specs(&runtime_config, &runtime_config_path, &worker)?;
    let python = python_interpreter();
    let mut client = CcxtProcessClient::spawn(&python, &ccxt_config_path, None)
        .map_err(|error| format!("启动公共 CCXT MarketData Worker 失败: {error}"))?;
    let mut pipeline = pipeline_storage
        .open(
            ccxt_market_event_log_name(&worker),
            worker_settlement_currency(&worker),
        )
        .map_err(|error| format!("创建 CCXT 行情事件管线失败: {error}"))?;
    let mut paper_bridges = open_paper_market_bridges(&pipeline_storage, &runtime_config.workers)?;
    let instruments = worker
        .symbols
        .iter()
        .map(|symbol| {
            InstrumentId::parse(symbol)
                .ok_or_else(|| format!("worker {} instrument 非法: {symbol}", worker.id))
        })
        .collect::<Result<Vec<_>, _>>()?;
    context.mark(
        qx_runtime::ServiceStatus::Ready,
        format!(
            "ccxt ticker/ohlcv polling instruments={} live_snapshots={}",
            instruments.len(),
            live_specs.len()
        ),
        Some(runtime_timestamp_ms()),
    )?;
    let mut source_seq = 0_u64;
    let mut quotes = 0_u64;
    let mut bars_written = 0_u64;
    while !context.should_stop() {
        let cycle_now = runtime_timestamp_ms();
        for instrument in &instruments {
            let result = match client.call(serde_json::json!({
                "op": "fetch_ticker",
                "instrument": instrument.to_string(),
            })) {
                Ok(result) => result,
                Err(error) => {
                    context.mark(
                        qx_runtime::ServiceStatus::Degraded,
                        format!("CCXT ticker 暂时失败 {}: {error}; reconnecting", instrument),
                        Some(cycle_now),
                    )?;
                    client = CcxtProcessClient::spawn(&python, &ccxt_config_path, None).map_err(
                        |spawn_error| format!("重启公共 CCXT Worker 失败: {spawn_error}"),
                    )?;
                    thread::sleep(Duration::from_millis(500));
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
            if bid <= 0 || ask <= 0 || bid > ask || bid_qty <= 0 || ask_qty <= 0 {
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
                    ),
                    received_ts,
                    source_seq,
                    format!("{}:ticker:{}", worker.id, source_seq),
                ))
                .map_err(|error| format!("CCXT 行情事实归约失败: {error:?}"))?;
            bridge_market_quote_to_paper(
                &mut paper_bridges,
                PaperMarketQuote {
                    source_worker_id: &worker.id,
                    instrument,
                    bid: qx_core::Price::from_raw(bid),
                    bid_qty: qx_core::Quantity::from_raw(bid_qty),
                    ask: qx_core::Price::from_raw(ask),
                    ask_qty: qx_core::Quantity::from_raw(ask_qty),
                    event_ts: ts,
                    receive_ts: received_ts,
                    source_seq,
                },
            )?;
            quotes = quotes.saturating_add(1);
            context.heartbeat(received_ts)?;
        }
        for spec in &live_specs {
            let start_ms = cycle_now.saturating_sub(
                spec.timeframe_ms
                    .saturating_mul(spec.history_limit.saturating_add(2) as u64),
            );
            let result = match client.call(serde_json::json!({
                "op": "fetch_ohlcv",
                "instrument": spec.instrument.to_string(),
                "timeframe": spec.timeframe,
                "start_ms": start_ms,
                "end_ms": cycle_now,
                "limit": spec.history_limit.saturating_add(2),
            })) {
                Ok(result) => result,
                Err(error) => {
                    context.mark(
                        qx_runtime::ServiceStatus::Degraded,
                        format!(
                            "CCXT OHLCV 暂时失败 {}: {error}; reconnecting",
                            spec.instrument
                        ),
                        Some(cycle_now),
                    )?;
                    client = CcxtProcessClient::spawn(&python, &ccxt_config_path, None).map_err(
                        |spawn_error| format!("重启公共 CCXT Worker 失败: {spawn_error}"),
                    )?;
                    thread::sleep(Duration::from_millis(500));
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
        context.mark(
            qx_runtime::ServiceStatus::Ready,
            format!(
                "ccxt live market healthy quotes={} bar_snapshots={}",
                quotes, bars_written
            ),
            Some(cycle_now),
        )?;
        if once {
            break;
        }
        thread::sleep(Duration::from_millis(1_000));
    }
    context.mark(
        qx_runtime::ServiceStatus::Stopped,
        format!("ccxt market worker stopped quotes={quotes} bar_snapshots={bars_written}"),
        Some(runtime_timestamp_ms()),
    )?;
    Ok(())
}
