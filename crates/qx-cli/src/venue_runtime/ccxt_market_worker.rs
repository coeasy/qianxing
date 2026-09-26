use crate::*;

/// 一轮轮询的健康结论：本轮任何一次 ticker / OHLCV 调用失败都不能被"healthy"覆盖。
///
/// 判定此前直接写在 worker 循环末尾的 `context.mark(Ready, ...)` 里：全部标的都失败的
/// worker 每轮仍然报 Ready + `ccxt live market healthy`，运维侧看到的永远是健康。
pub(crate) fn ccxt_market_cycle_health(
    failures: u32,
    attempted: u32,
    quotes: u64,
    bars_written: u64,
) -> (qx_runtime::ServiceStatus, String) {
    if failures == 0 {
        return (
            qx_runtime::ServiceStatus::Ready,
            format!("ccxt live market healthy quotes={quotes} bar_snapshots={bars_written}"),
        );
    }
    (
        qx_runtime::ServiceStatus::Degraded,
        format!(
            "ccxt live market 本轮 {failures}/{attempted} 次行情调用失败 quotes={quotes} bar_snapshots={bars_written}"
        ),
    )
}

/// 复活公共 CCXT Worker 的终止性口径（V11 K1，与 R3/R4 同族）。
///
/// 修前的形状：轮内计数 `cycle_failures` 每轮归零、复活后的等待恒为 500 ms，于是 Python
/// 子进程持续不可用时 worker 以每秒两次的频率复活它、状态永远停在 `Degraded`，既不退避
/// 也永不 `Failed`——进程活着但链路已经断了，`/ready` 那侧看到的是"还在重试"。
/// 这里把两件事收敛成纯函数，判据可确定性复现：
/// - [`ccxt_respawn_delay`]：连续失败次数决定等待，指数退避封顶 10 秒；成功一次调用即复位。
/// - [`ccxt_dead_cycle_ledger`]：只有"整轮全部调用失败"才算一个死轮，任何一次成功都清零；
///   连续死轮到达 [`CCXT_DEAD_CYCLE_BUDGET`] 即返回终止原因，由调用方标 `Failed` 并退出。
pub(crate) const CCXT_DEAD_CYCLE_BUDGET: u32 = 5;

/// 连续第 `failures` 次失败之后的复活等待：500 ms 起指数翻倍，封顶 10 秒。
pub(crate) fn ccxt_respawn_delay(failures: u32) -> Duration {
    qx_core::retry::Backoff::exponential(Duration::from_millis(500), Duration::from_secs(10))
        .delay_before_attempt(failures)
}

/// 同一颗标的连续失败到第几次之后，不再为它的失败复活公共 Worker（V11 O5）。
pub(crate) const CCXT_RESPAWN_STRIKE_BUDGET: u32 = 3;

/// 该不该为这颗标的的本次失败去复活子进程（`strikes` 是**计入本次之后**的连续失败次数）。
///
/// K1 的两半在**混合失败**下同时失效：`respawn_streak` 按"任一次成功调用"复位、死轮账按
/// "整轮全部失败"推进，于是两份标的里固定坏一份时，坏的那份每轮失败、好的那份每轮成功——
/// streak 恒被复位回 1（退避永远停在 500 ms）、死轮永远攒不到预算，worker 以约 1 Hz 永久
/// kill+wait+spawn Python 子进程，状态永久停在 `Degraded`。这里换一个问题：上一次复活
/// 之后这颗标的好了没有？连续 [`CCXT_RESPAWN_STRIKE_BUDGET`] 次都没好，就不再为它复活——
/// 一次都救不活的复活，第 N+1 次也不会。真的整进程不可用时每一颗标的的计数一起增长，
/// 仍然由 [`ccxt_dead_cycle_ledger`] 在预算处收口，两种形状都有出口。
pub(crate) fn ccxt_respawn_allowed(strikes: u32, budget: u32) -> bool {
    strikes <= budget
}

/// 死轮账：返回（新的连续死轮数，到达预算时的终止原因）。
///
/// `attempted == 0` 不构成死轮——没有标的可试是配置形状，不是对端故障的证据。
pub(crate) fn ccxt_dead_cycle_ledger(
    failures: u32,
    attempted: u32,
    previous: u32,
    budget: u32,
) -> (u32, Option<String>) {
    let dead = if attempted > 0 && failures >= attempted {
        previous.saturating_add(1)
    } else {
        0
    };
    if dead < budget {
        return (dead, None);
    }
    (
        dead,
        Some(format!(
            "ccxt live market 连续 {dead} 轮 {attempted}/{attempted} 次行情调用全部失败，复活预算已用尽"
        )),
    )
}

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
    let attempted_calls = (instruments.len() + live_specs.len()) as u32;
    let mut respawn_streak = 0_u32;
    let mut dead_cycles = 0_u32;
    let mut respawn_strikes: BTreeMap<String, u32> = BTreeMap::new();
    while !context.should_stop() {
        let cycle_now = runtime_timestamp_ms();
        let mut cycle_failures = 0_u32;
        for instrument in &instruments {
            let strike_key = format!("ticker:{instrument}");
            let result = match client.call(serde_json::json!({
                "op": "fetch_ticker",
                "instrument": instrument.to_string(),
            })) {
                Ok(result) => {
                    respawn_streak = 0;
                    respawn_strikes.insert(strike_key, 0);
                    result
                }
                Err(error) => {
                    cycle_failures = cycle_failures.saturating_add(1);
                    respawn_streak = respawn_streak.saturating_add(1);
                    let strikes = respawn_strikes
                        .get(&strike_key)
                        .copied()
                        .unwrap_or(0)
                        .saturating_add(1);
                    respawn_strikes.insert(strike_key, strikes);
                    if ccxt_respawn_allowed(strikes, CCXT_RESPAWN_STRIKE_BUDGET) {
                        context.mark(
                            qx_runtime::ServiceStatus::Degraded,
                            format!("CCXT ticker 暂时失败 {}: {error}; reconnecting", instrument),
                            Some(cycle_now),
                        )?;
                        client = CcxtProcessClient::spawn(&python, &ccxt_config_path, None)
                            .map_err(|spawn_error| {
                                format!("重启公共 CCXT Worker 失败: {spawn_error}")
                            })?;
                        thread::sleep(ccxt_respawn_delay(respawn_streak));
                    } else {
                        context.mark(
                            qx_runtime::ServiceStatus::Degraded,
                            format!(
                                "CCXT ticker 连续 {strikes} 次失败 {}: {error}; 复活对该标的无效，已停止为它重启公共 Worker",
                                instrument
                            ),
                            Some(cycle_now),
                        )?;
                    }
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
            let strike_key = format!("ohlcv:{}", spec.instrument);
            let result = match client.call(serde_json::json!({
                "op": "fetch_ohlcv",
                "instrument": spec.instrument.to_string(),
                "timeframe": spec.timeframe,
                "start_ms": start_ms,
                "end_ms": cycle_now,
                "limit": spec.history_limit.saturating_add(2),
            })) {
                Ok(result) => {
                    respawn_streak = 0;
                    respawn_strikes.insert(strike_key, 0);
                    result
                }
                Err(error) => {
                    cycle_failures = cycle_failures.saturating_add(1);
                    respawn_streak = respawn_streak.saturating_add(1);
                    let strikes = respawn_strikes
                        .get(&strike_key)
                        .copied()
                        .unwrap_or(0)
                        .saturating_add(1);
                    respawn_strikes.insert(strike_key, strikes);
                    if ccxt_respawn_allowed(strikes, CCXT_RESPAWN_STRIKE_BUDGET) {
                        context.mark(
                            qx_runtime::ServiceStatus::Degraded,
                            format!(
                                "CCXT OHLCV 暂时失败 {}: {error}; reconnecting",
                                spec.instrument
                            ),
                            Some(cycle_now),
                        )?;
                        client = CcxtProcessClient::spawn(&python, &ccxt_config_path, None)
                            .map_err(|spawn_error| {
                                format!("重启公共 CCXT Worker 失败: {spawn_error}")
                            })?;
                        thread::sleep(ccxt_respawn_delay(respawn_streak));
                    } else {
                        context.mark(
                            qx_runtime::ServiceStatus::Degraded,
                            format!(
                                "CCXT OHLCV 连续 {strikes} 次失败 {}: {error}; 复活对该标的无效，已停止为它重启公共 Worker",
                                spec.instrument
                            ),
                            Some(cycle_now),
                        )?;
                    }
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
        context.mark(status, detail, Some(cycle_now))?;
        let (dead, exhausted) = ccxt_dead_cycle_ledger(
            cycle_failures,
            attempted_calls,
            dead_cycles,
            CCXT_DEAD_CYCLE_BUDGET,
        );
        dead_cycles = dead;
        if let Some(reason) = exhausted {
            context.mark(qx_runtime::ServiceStatus::Failed, reason, Some(cycle_now))?;
            return Err(format!(
                "worker {} 连续 {dead} 轮全部行情调用失败，已放弃复活公共 CCXT Worker",
                worker.id
            ));
        }
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
