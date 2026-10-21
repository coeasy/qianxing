//! 行情桥：账户事件日志身份与 Paper 行情桥。

use crate::pipeline::PipelineStorage;
use crate::pipeline_metrics_report::PipelineMetricsReporter;
use qx_core::{InstrumentId, Price, Quantity, VenueFamily};
use qx_guanxing::QuoteTick;
use qx_runtime::account_worker_settlement_currency;

pub use qx_runtime::{
    account_event_log_name, required_account_event_log, worker_account_event_log,
};
use qx_runtime::{
    LiveEventPipeline, RuntimeConfig, RuntimeEventEnvelope, WorkerConfig, WorkerRole,
};
use std::collections::BTreeSet;

/// 账户级事实身份的唯一构造点。
///
/// 一个账户在某个 Venue 上只有一本 EventLog，身份键是 `(account_id, venue_id)`。
/// 写入端（user-stream / execution / spread recovery / reconciler）与只读端（API
/// 投影、策略上下文、行情桥）都必须从这里取名，否则同一账户会出现互不相认的账；
/// 规范化（trim + Venue 大小写）也只在这里做一次——`Paper` 与 `paper` 是同一个
/// 账户域，拼出两个日志名就等于把一份账拆成两半。
/// 本轮 venue 余额快照的事实身份：`source_seq` 从这本 EventLog 的尾端接上，
/// `correlation_id` 带本轮时间戳。两条对账链（Binance / CCXT）共用这一个构造点。
///
/// 之前 Binance 侧每进程从 0 起算 seq、correlation 又由 seq 拼出，于是对账进程重启后的
/// 第一条余额事实与重启前那条同 seq 同 id，被去重判成"已经应用过"而静默丢弃——账本从此
/// 不再跟进，worker 却照常报 Ready。seq 单独不足以做身份，因为进程重启会把它清零。
pub fn venue_balance_fact_identity(
    pipeline: &LiveEventPipeline,
    worker_id: &str,
    received_ts: u64,
) -> (u64, String) {
    let source_seq = pipeline
        .log()
        .events()
        .last()
        .map(|event| event.source_seq)
        .unwrap_or(0)
        .saturating_add(1);
    (source_seq, format!("{worker_id}:balances:{received_ts}"))
}

/// 账户之外、按 worker 各自成册的供应商事实（行情源、BarFrame 快照）。
/// `source` 是来源前缀（如 `ccxt-market`），空串表示直接用 worker id 成册。
pub fn worker_scoped_event_log_name(source: &str, worker_id: &str) -> String {
    if source.is_empty() {
        format!("{worker_id}-events")
    } else {
        format!("{source}-{worker_id}-events")
    }
}

pub fn ccxt_market_event_log_name(worker: &WorkerConfig) -> String {
    worker_scoped_event_log_name("ccxt-market", &worker.id)
}

/// 一份配置可能落盘的全部 EventLog 名，doctor 用它判定"没人认领的账本"。
///
/// 故意按最宽口径收：未启用的 worker 也算，否则临时下线一个 worker 就会把它的
/// 账本报成孤儿；行情册的两种拼写（裸 worker id 与 `ccxt-market-` 前缀）都算，
/// 因为同一个 MarketData worker 由哪个入口启动决定它用哪一种。宁可漏报也不误报
/// ——误报会让运维把在用的账本当垃圾归档。
pub fn configured_event_log_names(config: &RuntimeConfig) -> BTreeSet<String> {
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
pub struct PaperMarketBridge {
    pub workers: Vec<WorkerConfig>,
    pub log_name: String,
    pub pipeline: LiveEventPipeline,
}

pub struct PaperMarketQuote<'a> {
    pub source_worker_id: &'a str,
    pub instrument: &'a InstrumentId,
    pub bid: Price,
    pub bid_qty: Quantity,
    pub ask: Price,
    pub ask_qty: Quantity,
    pub event_ts: u64,
    pub receive_ts: u64,
    pub source_seq: u64,
}

pub fn paper_market_worker_matches_instrument(
    worker: &WorkerConfig,
    instrument: &InstrumentId,
) -> bool {
    worker.enabled
        && worker.role == WorkerRole::Execution
        && VenueFamily::parse_option(worker.venue_id.as_deref()) == Some(VenueFamily::Paper)
        && (worker.symbols.is_empty()
            || worker
                .symbols
                .iter()
                .any(|symbol| InstrumentId::parse(symbol).as_ref() == Some(instrument)))
}

/// 打开 Paper 行情桥。`workers` 必须是配置里的**全部** worker：桥自身按角色和 Venue
/// 过滤，而记账币种要按账户身份找出这本账的所有写入方，传过滤后的子集会漏掉冲突方。
pub fn open_paper_market_bridges(
    storage: &PipelineStorage,
    config: &RuntimeConfig,
) -> Result<Vec<PaperMarketBridge>, String> {
    let workers = &config.workers;
    let mut bridges: Vec<PaperMarketBridge> = Vec::new();
    for worker in workers.iter().filter(|worker| {
        worker.enabled
            && worker.role == WorkerRole::Execution
            && VenueFamily::parse_option(worker.venue_id.as_deref()) == Some(VenueFamily::Paper)
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
        let pipeline = storage.open(
            log_name.clone(),
            account_worker_settlement_currency(config, worker)?,
        )?;
        bridges.push(PaperMarketBridge {
            workers: vec![worker.clone()],
            log_name,
            pipeline,
        });
    }
    Ok(bridges)
}

pub fn bridge_market_quote_to_paper(
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

/// Include the worker-owned Paper mirror pipelines in the same worker metrics total.
pub fn observe_paper_market_bridges(
    bridges: &[PaperMarketBridge],
    metrics: &mut PipelineMetricsReporter,
) {
    for bridge in bridges {
        metrics.observe_named(
            &format!("paper-bridge:{}", bridge.log_name),
            &bridge.pipeline,
        );
    }
}
