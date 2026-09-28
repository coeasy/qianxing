use crate::*;

pub(crate) fn run_binance_market_worker(
    context: qx_runtime::WorkerContext,
    worker: WorkerConfig,
    pipeline_storage: PipelineStorage,
    all_workers: Vec<WorkerConfig>,
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
        .open(
            binance_event_log_name(&worker)?,
            worker_settlement_currency(&worker),
        )
        .map_err(|error| format!("创建行情事件管线失败: {error}"))?;
    let mut paper_bridges = open_paper_market_bridges(&pipeline_storage, &all_workers)?;
    let policy =
        BinanceStreamRetryPolicy::new(10, Duration::from_secs(1), Duration::from_secs(30))?;
    // 静默与故障在行情流上同样要分开：一条薄行情 10 秒不 tick 曾经会让这颗 worker
    // 直接 return，而监督器按进程粒度收工——同机的其它 worker 一起没了。
    let connect_context = context.clone();
    let stop_context = context.clone();
    let sleep_context = context.clone();
    let connect_instrument = instrument.clone();
    let worker_id = worker.id.clone();
    let report = qx_adapter::run_binance_stream(
        move || {
            let stream = BinanceSpotMarketStream::connect_with_endpoint(
                connect_instrument.clone(),
                host.as_str(),
                port,
                Duration::from_secs(10),
            )?;
            connect_context.mark(
                qx_runtime::ServiceStatus::Ready,
                "market stream connected",
                Some(runtime_timestamp_ms()),
            )?;
            Ok(stream)
        },
        policy,
        move || stop_context.should_stop(),
        move |delay| {
            thread::sleep(delay);
            let _ = sleep_context.heartbeat(runtime_timestamp_ms());
        },
        runtime_timestamp_ms,
        |quote| {
            let received_ts = runtime_timestamp_ms();
            pipeline
                .ingest(RuntimeEventEnvelope::market_quote(
                    instrument.clone(),
                    quote,
                    received_ts,
                    quote.source_seq,
                    format!("{worker_id}:quote:{}", quote.source_seq),
                ))
                .map_err(|error| format!("行情事实归约失败: {error:?}"))?;
            bridge_market_quote_to_paper(
                &mut paper_bridges,
                PaperMarketQuote {
                    source_worker_id: &worker_id,
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
            context.heartbeat(received_ts)?;
            Ok(())
        },
    )?;
    context.mark(
        qx_runtime::ServiceStatus::Stopped,
        format!(
            "market stream stopped quotes={} idle_windows={} reconnects={}",
            report.events, report.idle_windows, report.reconnects
        ),
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
    let runtime_config = read_runtime_config(&runtime_config_path)?;
    let mut pipeline = pipeline_storage
        .open(
            binance_event_log_name(&worker)?,
            account_worker_settlement_currency(&runtime_config, &worker)?,
        )
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
        format!(
            "user stream stopped events={} idle_windows={} reconnects={}",
            report.events, report.idle_windows, report.reconnects
        ),
        Some(runtime_timestamp_ms()),
    )?;
    Ok(())
}
