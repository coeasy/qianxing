//! 行情桥：账户事件日志命名、API 投影桥与 Paper 行情桥。

use super::*;

/// 账户级事实身份的唯一构造点。
///
/// 一个账户在某个 Venue 上只有一本 EventLog，身份键是 `(account_id, venue_id)`。
/// 写入端（user-stream / execution / spread recovery / reconciler）与只读端（API
/// 投影、策略上下文、行情桥）都必须从这里取名，否则同一账户会出现互不相认的账；
/// 规范化（trim + Venue 大小写）也只在这里做一次——`Paper` 与 `paper` 是同一个
/// 账户域，拼出两个日志名就等于把一份账拆成两半。
pub(crate) fn account_event_log_name(account_id: &str, venue_id: &str) -> Option<String> {
    let account = account_id.trim();
    let venue = venue_id.trim().to_ascii_lowercase();
    if account.is_empty() || venue.is_empty() {
        return None;
    }
    let prefix = if venue == "paper" {
        "paper"
    } else if venue.contains("binance") {
        "binance"
    } else {
        "ccxt"
    };
    Some(format!("{prefix}-{account}-{venue}-events"))
}

/// worker 声明的账户级日志身份；缺少 account/venue 时返回 `None`，由调用方决定
/// 是 fail closed 还是跳过。不给"unknown"兜底名：写进编造日志的事实，读取端按
/// 账户身份永远找不到，等于静默丢账。
pub(crate) fn worker_account_event_log(worker: &WorkerConfig) -> Option<String> {
    account_event_log_name(worker.account_id.as_deref()?, worker.venue_id.as_deref()?)
}

/// 需要账户日志却拿不到身份的 worker 是配置错误，必须点名报出来。
pub(crate) fn required_account_event_log(worker: &WorkerConfig) -> Result<String, String> {
    worker_account_event_log(worker).ok_or_else(|| {
        format!(
            "FAIL_CLOSED: worker {}（role={:?}）缺少 account_id/venue_id，无法确定账户级 EventLog",
            worker.id, worker.role
        )
    })
}

/// 账户之外、按 worker 各自成册的供应商事实（行情源、BarFrame 快照）。
/// `source` 是来源前缀（如 `ccxt-market`），空串表示直接用 worker id 成册。
pub(crate) fn worker_scoped_event_log_name(source: &str, worker_id: &str) -> String {
    if source.is_empty() {
        format!("{worker_id}-events")
    } else {
        format!("{source}-{worker_id}-events")
    }
}

/// 返回所有启用的账户级运行时 EventLog。相同 account/venue 可能同时由
/// user-stream、execution、reconciler 等 worker 使用，但 API 只能建立一个
/// 隔离投影，避免多个 worker 重复写同一游标。
///
/// 去重键是派生出来的日志名（也就是账户身份），不是配置里写死的
/// `(account_id, venue_id)` 原文：`paper` 与 `Paper` 是同一本账，按原文去重会
/// 给同一本日志建出两个投影、各推各的游标。
///
/// 记账币种按 [`settlement_currency_for_log`] 找回；同一本账的写入方声明不一致时
/// 直接返回配置错误——投影桥带着猜出来的口径启动，读到的是半本账。
pub(crate) fn configured_account_event_logs(
    config: &RuntimeConfig,
) -> Result<Vec<(String, String, String, String)>, String> {
    let mut identities = BTreeMap::<String, (String, String)>::new();
    for worker in config
        .workers
        .iter()
        .filter(|worker| owns_account_event_log(worker))
    {
        let (Some(account_id), Some(venue_id)) =
            (worker.account_id.as_deref(), worker.venue_id.as_deref())
        else {
            continue;
        };
        let Some(log_name) = account_event_log_name(account_id, venue_id) else {
            continue;
        };
        identities
            .entry(log_name)
            .or_insert_with(|| (account_id.to_string(), venue_id.to_string()));
    }
    identities
        .into_iter()
        .map(|(log_name, (account_id, venue_id))| {
            Ok((
                account_id,
                venue_id,
                log_name.clone(),
                settlement_currency_for_log(config, &log_name)?,
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
    let sources = match configured_account_event_logs(config) {
        Ok(sources) => sources,
        Err(error) => {
            eprintln!("[运行时 · API] 账户 EventLog 投影桥未启动: {error}");
            return None;
        }
    };
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

pub(crate) fn ccxt_market_event_log_name(worker: &WorkerConfig) -> String {
    worker_scoped_event_log_name("ccxt-market", &worker.id)
}

/// 一份配置可能落盘的全部 EventLog 名，doctor 用它判定"没人认领的账本"。
///
/// 故意按最宽口径收：未启用的 worker 也算，否则临时下线一个 worker 就会把它的
/// 账本报成孤儿；行情册的两种拼写（裸 worker id 与 `ccxt-market-` 前缀）都算，
/// 因为同一个 MarketData worker 由哪个入口启动决定它用哪一种。宁可漏报也不误报
/// ——误报会让运维把在用的账本当垃圾归档。
pub(crate) fn configured_event_log_names(config: &RuntimeConfig) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for worker in &config.workers {
        if let Some(name) = worker_account_event_log(worker) {
            names.insert(name);
        }
        names.insert(worker_scoped_event_log_name("", &worker.id));
        names.insert(ccxt_market_event_log_name(worker));
    }
    names
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
        let log_name = required_account_event_log(worker)?;
        // 同一账户/venue 允许配置多个标的 worker，但它们共享一个账户级日志；
        // 合并 worker 的过滤条件，避免第一个 worker 的 symbols 遮蔽其它标的。
        if let Some(existing) = bridges
            .iter_mut()
            .find(|bridge| bridge.log_name == log_name)
        {
            existing.workers.push(worker.clone());
            continue;
        }
        let pipeline = storage.open(log_name.clone(), worker_settlement_currency(worker))?;
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
