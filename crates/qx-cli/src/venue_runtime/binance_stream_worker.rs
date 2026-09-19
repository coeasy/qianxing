use crate::*;

pub(crate) fn run_binance_market_worker(
    context: qx_runtime::WorkerContext,
    worker: WorkerConfig,
    pipeline_storage: PipelineStorage,
    paper_workers: Vec<WorkerConfig>,
) -> Result<(), String> {
    if worker.symbols.len() != 1 {
        return Err(format!(
            "当前 Binance market worker 要求恰好一个 symbol，实际 {}",
            worker.symbols.len()
        ));
    }
    let instrument = InstrumentId::parse(&worker.symbols[0])
        .ok_or_else(|| format!("worker {} instrument 非法", worker.id))?;
    let (host, port) = configured_ws_endpoint(&worker)?;
    let mut pipeline = pipeline_storage
        .open(binance_event_log_name(&worker), "USDT")
        .map_err(|error| format!("创建行情事件管线失败: {error}"))?;
    let mut paper_bridges = open_paper_market_bridges(&pipeline_storage, &paper_workers)?;
    let mut stream = BinanceSpotMarketStream::connect_with_endpoint(
        instrument.clone(),
        host,
        port,
        Duration::from_secs(10),
    )?;
    context.mark(
        qx_runtime::ServiceStatus::Ready,
        "market stream connected",
        Some(runtime_timestamp_ms()),
    )?;
    let mut quotes = 0_u64;
    while !context.should_stop() {
        match stream.recv_quote(runtime_timestamp_ms())? {
            Some(quote) => {
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
                bridge_market_quote_to_paper(
                    &mut paper_bridges,
                    PaperMarketQuote {
                        source_worker_id: &worker.id,
                        instrument: &instrument,
                        bid: quote.bid,
                        bid_qty: quote.bid_qty,
                        ask: quote.ask,
                        ask_qty: quote.ask_qty,
                        event_ts: quote.ts,
                        receive_ts: received_ts,
                        source_seq: quote.source_seq,
                    },
                )?;
                quotes = quotes.saturating_add(1);
                context.heartbeat(received_ts)?;
            }
            None => break,
        }
    }
    let _ = stream.close();
    context.mark(
        qx_runtime::ServiceStatus::Stopped,
        format!("market stream stopped quotes={quotes}"),
        Some(runtime_timestamp_ms()),
    )?;
    Ok(())
}

pub(crate) fn run_binance_user_worker(
    context: qx_runtime::WorkerContext,
    worker: WorkerConfig,
    pipeline_storage: PipelineStorage,
    runtime_config_path: PathBuf,
) -> Result<(), String> {
    let policy =
        BinanceStreamRetryPolicy::new(10, Duration::from_secs(1), Duration::from_secs(30))?;
    let initial_auth = load_binance_worker_auth(&worker)?;
    let mut venue = new_binance_venue(&worker, initial_auth)?;
    let mut pipeline = pipeline_storage
        .open(binance_event_log_name(&worker), "USDT")
        .map_err(|error| format!("创建用户流事件管线失败: {error}"))?;
    venue
        .restore_orders(pipeline.orders())
        .map_err(|error| format!("恢复用户流订单状态失败: {error:?}"))?;
    let stop_context = context.clone();
    let sleep_context = context.clone();
    let event_context = context.clone();
    let request_prefix = worker.id.clone();
    let worker_id = worker.id.clone();
    let spec_worker = worker.clone();
    let spec_config_path = runtime_config_path.clone();
    let (host, port) = configured_ws_endpoint(&worker)?;
    let mut source_seq = 0_u64;
    let mut should_stop = move || stop_context.should_stop();
    let mut sleep = move |delay| {
        thread::sleep(delay);
        let _ = sleep_context.heartbeat(runtime_timestamp_ms());
    };
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
                // listenKey 过期后必须关闭当前会话并重新订阅；重连后的
                // reconciler 会先用 REST 快照确认状态，再允许新的提交。
                if payload.contains("listenKeyExpired") {
                    venue.reconnect();
                }
                return Err(format!("Binance 用户事件处理失败: {error:?}"));
            }
        };
        let received_ts = runtime_timestamp_ms();
        let spec = worker_report_spec(&spec_worker, &events, &pipeline, Some(&spec_config_path))?;
        let event_count = match &spec {
            Some(spec) => ingest_venue_events_with_spec(
                &mut pipeline,
                events,
                &worker_id,
                received_ts,
                &mut source_seq,
                spec,
            )?,
            None => ingest_venue_events(
                &mut pipeline,
                events,
                &worker_id,
                received_ts,
                &mut source_seq,
            )?,
        };
        event_context.heartbeat(received_ts)?;
        if event_count != 0 {
            event_context.mark(
                qx_runtime::ServiceStatus::Ready,
                format!("user events={event_count}"),
                Some(runtime_timestamp_ms()),
            )?;
        }
        Ok(())
    };
    let run_config = BinanceUserStreamRunConfig::with_port(
        host,
        port,
        request_prefix,
        Duration::from_secs(10),
        policy,
    )?;
    let report = qx_adapter::run_binance_user_stream_with_config_loader(
        || load_binance_worker_auth(&worker),
        run_config,
        &mut should_stop,
        &mut sleep,
        &mut on_event,
    )?;
    context.mark(
        qx_runtime::ServiceStatus::Stopped,
        format!("user stream stopped events={}", report.events),
        Some(runtime_timestamp_ms()),
    )?;
    Ok(())
}
