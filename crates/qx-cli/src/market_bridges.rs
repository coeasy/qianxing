//! 行情桥：账户事件日志命名、API 投影桥与 Paper 行情桥。

use super::*;

pub(crate) fn account_event_log_name(account_id: &str, venue_id: &str) -> Option<String> {
    let venue = venue_id.trim().to_ascii_lowercase();
    if venue.is_empty() {
        return None;
    }
    let prefix = if venue == "paper" {
        "paper"
    } else if venue.contains("binance") {
        "binance"
    } else {
        "ccxt"
    };
    Some(format!("{prefix}-{account_id}-{venue_id}-events"))
}

/// 返回所有启用的账户级运行时 EventLog。相同 account/venue 可能同时由
/// user-stream、execution、reconciler 等 worker 使用，但 API 只能建立一个
/// 隔离投影，避免多个 worker 重复写同一游标。
pub(crate) fn configured_account_event_logs(
    config: &RuntimeConfig,
) -> Vec<(String, String, String, String)> {
    let keys = config
        .workers
        .iter()
        .filter(|worker| {
            worker.enabled
                && matches!(
                    worker.role,
                    WorkerRole::UserStream
                        | WorkerRole::Execution
                        | WorkerRole::SpreadRecovery
                        | WorkerRole::Reconciler
                )
                && worker.account_id.is_some()
                && worker.venue_id.is_some()
        })
        .filter_map(|worker| {
            Some((
                worker.account_id.as_deref()?.to_string(),
                worker.venue_id.as_deref()?.to_string(),
            ))
        })
        .collect::<BTreeSet<_>>();
    keys.into_iter()
        .filter_map(|(account_id, venue_id)| {
            Some((
                account_id.clone(),
                venue_id.clone(),
                account_event_log_name(&account_id, &venue_id)?,
                "USDT".into(),
            ))
        })
        .collect()
}

/// 在 API 服务旁启动只读投影桥。它只读取 Runtime EventLog，调用 API 的
/// `project_event_log` 更新查询/订阅读模型，不拥有订单、账本或外部副作用。
pub(crate) fn spawn_api_projection_bridge(
    config: &RuntimeConfig,
    service: ApiService,
    stop: Arc<AtomicBool>,
) -> Option<thread::JoinHandle<()>> {
    let sources = configured_account_event_logs(config);
    if sources.is_empty() {
        return None;
    }
    let storage = match PipelineStorage::from_config(config) {
        Ok(storage) => storage,
        Err(error) => {
            eprintln!("[运行时 · API] 初始化 EventLog 投影桥失败: {error}");
            return None;
        }
    };
    let poll_interval = Duration::from_millis(250);
    Some(thread::spawn(move || {
        let mut pipelines = BTreeMap::<(String, String), LiveEventPipeline>::new();
        while !stop.load(Ordering::Acquire) {
            for (account_id, venue_id, log_name, currency) in &sources {
                let pipeline_key = (account_id.clone(), venue_id.clone());
                if let std::collections::btree_map::Entry::Vacant(entry) =
                    pipelines.entry(pipeline_key.clone())
                {
                    match storage.open(log_name.clone(), currency.clone()) {
                        Ok(opened) => {
                            entry.insert(opened);
                        }
                        Err(error) => {
                            eprintln!(
                                "[运行时 · API] 打开账户 EventLog 投影源失败 account={} venue={}: {error}",
                                account_id, venue_id
                            );
                            continue;
                        }
                    }
                }
                let Some(current) = pipelines.get_mut(&pipeline_key) else {
                    continue;
                };
                if let Err(error) = current.refresh() {
                    eprintln!(
                        "[运行时 · API] 刷新账户 EventLog 投影源失败 account={} venue={}: {error:?}",
                        account_id, venue_id
                    );
                    pipelines.remove(&pipeline_key);
                    continue;
                }
                if let Err(error) =
                    service.project_account_event_log(account_id, venue_id, current.log())
                {
                    eprintln!(
                        "[运行时 · API] 写入账户查询投影失败 account={} venue={}: {error}",
                        account_id, venue_id
                    );
                    pipelines.remove(&(account_id.clone(), venue_id.clone()));
                }
            }
            thread::sleep(poll_interval);
        }
    }))
}

pub(crate) fn ccxt_event_log_name(worker: &WorkerConfig) -> String {
    let venue = worker.venue_id.as_deref().unwrap_or("unknown");
    let prefix = if venue.to_ascii_lowercase().contains("binance") {
        "binance"
    } else {
        "ccxt"
    };
    format!(
        "{prefix}-{}-{venue}-events",
        worker.account_id.as_deref().unwrap_or("unknown")
    )
}

pub(crate) fn ccxt_market_event_log_name(worker: &WorkerConfig) -> String {
    format!("ccxt-market-{}-events", worker.id)
}

/// Paper execution 必须消费与市场数据相同的行情事实。
///
/// MarketData worker 自己的 EventLog 用于保留供应商来源与 BarFrame 快照，
/// Paper 账户则有独立的账户级 EventLog（订单、账簿、行情共同归约）。因此
/// 公共 CCXT/Binance 行情需要在这里显式镜像到匹配的 Paper 账户，而不是让
/// Paper execution worker 读取另一个 worker 的内存状态。镜像沿用来源 worker
/// 与 source sequence 组成幂等键；同一行情重试不会重复写入。
pub(crate) struct PaperMarketBridge {
    pub(crate) workers: Vec<WorkerConfig>,
    pub(crate) log_name: String,
    pub(crate) pipeline: LiveEventPipeline,
}

pub(crate) struct PaperMarketQuote<'a> {
    pub(crate) source_worker_id: &'a str,
    pub(crate) instrument: &'a InstrumentId,
    pub(crate) bid: Price,
    pub(crate) bid_qty: Quantity,
    pub(crate) ask: Price,
    pub(crate) ask_qty: Quantity,
    pub(crate) event_ts: u64,
    pub(crate) receive_ts: u64,
    pub(crate) source_seq: u64,
}

pub(crate) fn paper_market_worker_matches_instrument(
    worker: &WorkerConfig,
    instrument: &InstrumentId,
) -> bool {
    worker.enabled
        && worker.role == WorkerRole::Execution
        && worker
            .venue_id
            .as_deref()
            .is_some_and(|venue| venue.eq_ignore_ascii_case("paper"))
        && (worker.symbols.is_empty()
            || worker
                .symbols
                .iter()
                .any(|symbol| InstrumentId::parse(symbol).as_ref() == Some(instrument)))
}

pub(crate) fn open_paper_market_bridges(
    storage: &PipelineStorage,
    workers: &[WorkerConfig],
) -> Result<Vec<PaperMarketBridge>, String> {
    let mut bridges: Vec<PaperMarketBridge> = Vec::new();
    for worker in workers.iter().filter(|worker| {
        worker.enabled
            && worker.role == WorkerRole::Execution
            && worker
                .venue_id
                .as_deref()
                .is_some_and(|venue| venue.eq_ignore_ascii_case("paper"))
    }) {
        let log_name = format!(
            "paper-{}-{}-events",
            worker.account_id.as_deref().unwrap_or("unknown"),
            worker.venue_id.as_deref().unwrap_or("paper")
        );
        // 同一账户/venue 允许配置多个标的 worker，但它们共享一个账户级日志；
        // 合并 worker 的过滤条件，避免第一个 worker 的 symbols 遮蔽其它标的。
        if let Some(existing) = bridges
            .iter_mut()
            .find(|bridge| bridge.log_name == log_name)
        {
            existing.workers.push(worker.clone());
            continue;
        }
        let pipeline = storage.open(
            log_name.clone(),
            worker.settlement_currency.as_deref().unwrap_or("USDT"),
        )?;
        bridges.push(PaperMarketBridge {
            workers: vec![worker.clone()],
            log_name,
            pipeline,
        });
    }
    Ok(bridges)
}

pub(crate) fn bridge_market_quote_to_paper(
    bridges: &mut [PaperMarketBridge],
    quote: PaperMarketQuote<'_>,
) -> Result<usize, String> {
    let mut mirrored = 0;
    for bridge in bridges.iter_mut().filter(|bridge| {
        bridge
            .workers
            .iter()
            .any(|worker| paper_market_worker_matches_instrument(worker, quote.instrument))
    }) {
        bridge
            .pipeline
            .ingest(RuntimeEventEnvelope::market_quote(
                quote.instrument.clone(),
                QuoteTick::new(
                    quote.event_ts,
                    quote.bid,
                    quote.bid_qty,
                    quote.ask,
                    quote.ask_qty,
                    quote.source_seq,
                ),
                quote.receive_ts,
                quote.source_seq,
                format!(
                    "market-bridge:{}:{}:{}",
                    quote.source_worker_id, quote.instrument, quote.source_seq
                ),
            ))
            .map_err(|error| {
                format!(
                    "镜像公共行情到 Paper EventLog 失败 {} {}: {error:?}",
                    bridge.log_name, quote.instrument
                )
            })?;
        mirrored += 1;
    }
    Ok(mirrored)
}
