//! 实盘事件归约边界。
//!
//! 连接器只负责把供应商协议转换成标准事实；本模块负责把事实按统一的
//! Kernel 语义写入 EventLog，并在同一事务边界内更新 OMS/Ledger。事件日志
//! 采用文件后端时，每次成功归约都会原子保存，因此进程重启可以先恢复账簿
//! 和订单状态，再继续接收新的行情或用户流事实。

pub use qx_control::order_from_submit_command;
use qx_control::ControlCommand;
use qx_core::{
    AccountBalance, AccountCashflow, AccountPositionSnapshot, CashflowKind, Event, EventContext,
    EventKind, EventLog, EventMetadata, Fill, FundingRateSnapshot, InstrumentId, Ledger, Order,
    OrderStatus, Price, Priority, Quantity, QxError, QxResult, ReplayVerifier,
    TradingInstrumentSpec,
};
use qx_guanxing::QuoteTick;
use qx_oms::Oms;
#[cfg(feature = "postgres")]
use qx_storage::PostgresEventLogStore;
use qx_storage::{
    project_event_log_to_outbox, EventLogFileStore, FileOutboxStore, SegmentedEventLogStore,
    StorageError,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// 供应商适配器输出的最小标准事实集合。
///
/// 这里不引用任何具体 Venue 类型，避免 Binance、CTP 或其他连接器把协议
/// 类型泄漏到 Kernel。适配器只需要把自己的事件映射到此枚举。
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum RuntimeExternalEvent {
    MarketQuote {
        instrument: InstrumentId,
        bid: Price,
        ask: Price,
        bid_qty: Quantity,
        ask_qty: Quantity,
    },
    AccountBalanceSnapshot {
        account_id: String,
        venue_id: String,
        balances: Vec<AccountBalance>,
    },
    AccountPositionSnapshot {
        account_id: String,
        venue_id: String,
        positions: Vec<AccountPositionSnapshot>,
    },
    FundingRateSnapshot {
        snapshot: FundingRateSnapshot,
    },
    AccountCashflow {
        cashflow: AccountCashflow,
    },
    Accepted {
        client_order_id: u64,
        venue_order_id: Option<String>,
    },
    Fill {
        fill: Fill,
    },
    /// 带冻结产品规格的成交事实。事件日志仍只保存标准 Filled/LedgerApplied，
    /// 规格只用于当前归约选择；LedgerApplied 已经包含完整的衍生品落账事实，
    /// 因而重启重放不依赖运行时内存。
    FillWithSpec {
        fill: Box<Fill>,
        spec: Box<TradingInstrumentSpec>,
    },
    Cancelled {
        client_order_id: u64,
    },
    ReconcileRequired {
        client_order_id: u64,
    },
}

/// 外部事实的时间与血缘元数据。
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RuntimeEventEnvelope {
    pub event: RuntimeExternalEvent,
    /// 供应商业务发生时间；乱序或迟到时会被映射到单调 engine time。
    pub event_ts: u64,
    /// 本地收到该事实的时间，永远不用于覆盖业务发生时间。
    pub receive_ts: u64,
    /// 供应商序号、trade id 或本地稳定序号。
    pub source_seq: u64,
    pub correlation_id: String,
    /// 外部事实的 schema/source/dedup/rule 元数据；业务时间仍由 event_ts 和
    /// receive_ts 分别表达 effective_at/observed_at。
    pub metadata: EventMetadata,
}

impl RuntimeEventEnvelope {
    pub fn market_quote(
        instrument: InstrumentId,
        quote: QuoteTick,
        receive_ts: u64,
        source_seq: u64,
        correlation_id: impl Into<String>,
    ) -> Self {
        let correlation_id = correlation_id.into();
        Self {
            event: RuntimeExternalEvent::MarketQuote {
                instrument,
                bid: quote.bid,
                ask: quote.ask,
                bid_qty: quote.bid_qty,
                ask_qty: quote.ask_qty,
            },
            event_ts: quote.ts,
            receive_ts,
            source_seq,
            metadata: runtime_event_metadata(&correlation_id, source_seq, "market_data"),
            correlation_id,
        }
    }

    pub fn venue(
        event: RuntimeExternalEvent,
        event_ts: u64,
        receive_ts: u64,
        source_seq: u64,
        correlation_id: impl Into<String>,
    ) -> Self {
        let correlation_id = correlation_id.into();
        Self {
            event,
            event_ts,
            receive_ts,
            source_seq,
            metadata: runtime_event_metadata(&correlation_id, source_seq, "venue"),
            correlation_id,
        }
    }

    pub fn with_metadata(mut self, metadata: EventMetadata) -> Self {
        self.metadata = metadata;
        self
    }

    pub fn with_context(mut self, context: EventContext) -> Self {
        self.metadata.context = context;
        self
    }
}

fn runtime_event_metadata(
    correlation_id: &str,
    source_seq: u64,
    source_kind: &str,
) -> EventMetadata {
    let source_id = correlation_id
        .split(':')
        .next()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("runtime")
        .to_string();
    let dedup_key = if correlation_id.trim().is_empty() {
        format!("runtime:source:{source_seq}")
    } else {
        format!("{correlation_id}:source:{source_seq}")
    };
    EventMetadata {
        schema_version: qx_core::EVENT_METADATA_SCHEMA_VERSION,
        source_id,
        source_kind: source_kind.into(),
        dedup_key,
        rule_version: "runtime-v1".into(),
        context: EventContext::default(),
    }
}

fn enrich_runtime_event_context(metadata: &mut EventMetadata, event: &RuntimeExternalEvent) {
    let context = &mut metadata.context;
    match event {
        RuntimeExternalEvent::AccountBalanceSnapshot {
            account_id,
            venue_id,
            ..
        }
        | RuntimeExternalEvent::AccountPositionSnapshot {
            account_id,
            venue_id,
            ..
        } => {
            if context.account_id.trim().is_empty() {
                context.account_id = account_id.clone();
            }
            if context.portfolio_id.trim().is_empty() {
                context.portfolio_id = account_id.clone();
            }
            if context.tenant_id.trim().is_empty() {
                context.tenant_id = account_id.clone();
            }
            if context.run_id.trim().is_empty() {
                context.run_id = format!("account:{account_id}");
            }
            if context.strategy_id.trim().is_empty() {
                context.strategy_id = "external-account-state".into();
            }
            if context.signal_id.trim().is_empty() {
                context.signal_id = venue_id.clone();
            }
        }
        RuntimeExternalEvent::AccountCashflow { cashflow } => {
            if context.account_id.trim().is_empty() {
                context.account_id = cashflow.account_id.clone();
            }
            if context.portfolio_id.trim().is_empty() {
                context.portfolio_id = cashflow.account_id.clone();
            }
            if context.tenant_id.trim().is_empty() {
                context.tenant_id = cashflow.account_id.clone();
            }
            if context.run_id.trim().is_empty() {
                context.run_id = format!("account:{}", cashflow.account_id);
            }
            if context.strategy_id.trim().is_empty() {
                context.strategy_id = "external-cashflow".into();
            }
            if context.signal_id.trim().is_empty() {
                context.signal_id = cashflow.venue_id.clone();
            }
        }
        RuntimeExternalEvent::Fill { fill } => enrich_fill_context(context, fill),
        RuntimeExternalEvent::FillWithSpec { fill, .. } => enrich_fill_context(context, fill),
        RuntimeExternalEvent::MarketQuote { .. }
        | RuntimeExternalEvent::FundingRateSnapshot { .. }
        | RuntimeExternalEvent::Accepted { .. }
        | RuntimeExternalEvent::Cancelled { .. }
        | RuntimeExternalEvent::ReconcileRequired { .. } => {}
    }
}

fn enrich_fill_context(context: &mut EventContext, fill: &Fill) {
    if context.account_id.trim().is_empty() {
        context.account_id = fill.account_id.clone();
    }
    if context.strategy_id.trim().is_empty() {
        context.strategy_id = fill
            .strategy_id
            .clone()
            .unwrap_or_else(|| "external-execution".into());
    }
    if context.signal_id.trim().is_empty() {
        context.signal_id = fill
            .signal_id
            .map(|value| value.to_string())
            .unwrap_or_default();
    }
    if context.intent_id.trim().is_empty() {
        context.intent_id = fill
            .intent_id
            .map(|value| value.to_string())
            .unwrap_or_default();
    }
    if context.run_id.trim().is_empty() && !context.account_id.trim().is_empty() {
        context.run_id = format!("account:{}", context.account_id);
    }
    if context.portfolio_id.trim().is_empty() && !context.account_id.trim().is_empty() {
        context.portfolio_id = context.account_id.clone();
    }
    if context.tenant_id.trim().is_empty() && !context.account_id.trim().is_empty() {
        context.tenant_id = context.account_id.clone();
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RuntimeIngestReceipt {
    pub primary_seq: u64,
    pub derived_seqs: Vec<u64>,
    pub engine_ts: u64,
    pub log_digest: u64,
    pub deduplicated: bool,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct PipelineMetricsSnapshot {
    pub ingest_attempts: u64,
    pub ingested_events: u64,
    pub deduplicated_events: u64,
    pub transient_retries: u64,
    pub refreshes: u64,
    pub failures: u64,
}

struct EventFact {
    metadata: EventMetadata,
    kind: EventKind,
}

impl PipelineMetricsSnapshot {
    pub fn to_prometheus(self) -> String {
        format!(
            "# HELP qx_pipeline_ingest_attempts_total Events presented to the live pipeline.\\n\
# TYPE qx_pipeline_ingest_attempts_total counter\\n\
qx_pipeline_ingest_attempts_total {}\\n\
# HELP qx_pipeline_ingested_events_total Events appended to the live pipeline.\\n\
# TYPE qx_pipeline_ingested_events_total counter\\n\
qx_pipeline_ingested_events_total {}\\n\
# HELP qx_pipeline_deduplicated_events_total Duplicate facts accepted without reapplying effects.\\n\
# TYPE qx_pipeline_deduplicated_events_total counter\\n\
qx_pipeline_deduplicated_events_total {}\\n\
# HELP qx_pipeline_transient_retries_total Transient storage retries.\\n\
# TYPE qx_pipeline_transient_retries_total counter\\n\
qx_pipeline_transient_retries_total {}\\n\
# HELP qx_pipeline_refreshes_total Shared EventLog refreshes.\\n\
# TYPE qx_pipeline_refreshes_total counter\\n\
qx_pipeline_refreshes_total {}\\n\
# HELP qx_pipeline_failures_total Pipeline ingestion or refresh failures.\\n\
# TYPE qx_pipeline_failures_total counter\\n\
qx_pipeline_failures_total {}\\n",
            self.ingest_attempts,
            self.ingested_events,
            self.deduplicated_events,
            self.transient_retries,
            self.refreshes,
            self.failures,
        )
    }
}

#[derive(Default)]
struct PipelineMetrics {
    ingest_attempts: AtomicU64,
    ingested_events: AtomicU64,
    deduplicated_events: AtomicU64,
    transient_retries: AtomicU64,
    refreshes: AtomicU64,
    failures: AtomicU64,
}

impl PipelineMetrics {
    fn snapshot(&self) -> PipelineMetricsSnapshot {
        PipelineMetricsSnapshot {
            ingest_attempts: self.ingest_attempts.load(Ordering::Relaxed),
            ingested_events: self.ingested_events.load(Ordering::Relaxed),
            deduplicated_events: self.deduplicated_events.load(Ordering::Relaxed),
            transient_retries: self.transient_retries.load(Ordering::Relaxed),
            refreshes: self.refreshes.load(Ordering::Relaxed),
            failures: self.failures.load(Ordering::Relaxed),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct LivePipelineSnapshot {
    pub log_name: String,
    pub log_len: usize,
    pub log_digest: u64,
    pub ledger_entries: usize,
    pub orders: Vec<Order>,
    pub marks: Vec<(InstrumentId, Price)>,
    pub account_balances: BTreeMap<(String, String), Vec<AccountBalance>>,
    pub account_positions: BTreeMap<(String, String), Vec<AccountPositionSnapshot>>,
    pub funding_rates: BTreeMap<InstrumentId, FundingRateSnapshot>,
    pub last_engine_ts: u64,
}

/// 结算币种的账簿/柜台余额差异。
///
/// Spot 账户的非结算资产还需要 InstrumentSpec 才能把 symbol 拆成 base/quote；
/// 因此这里先严格比较运行时配置声明的结算币种，避免把持仓数量误当成现金。
/// 差异只用于对账和健康状态，不会自动写入 Ledger。
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct RuntimeBalanceDiscrepancy {
    pub account_id: String,
    pub venue_id: String,
    pub asset: String,
    pub ledger_raw: i128,
    pub venue_raw: i128,
}

#[derive(Clone)]
enum RuntimeEventStore {
    Flat(EventLogFileStore, FileOutboxStore),
    Segmented(SegmentedEventLogStore, FileOutboxStore),
    #[cfg(feature = "postgres")]
    Postgres(PostgresEventLogStore),
}

impl RuntimeEventStore {
    fn read_if_exists(&self, name: &str) -> Result<Option<EventLog>, StorageError> {
        match self {
            Self::Flat(store, _) => store.read_if_exists(name),
            Self::Segmented(store, _) => store.read_if_exists(name),
            #[cfg(feature = "postgres")]
            Self::Postgres(store) => store
                .read_if_exists(name)
                .map_err(|error| StorageError::Io(format!("PostgreSQL EventLog: {error:?}"))),
        }
    }

    fn write(&self, name: &str, log: &EventLog) -> Result<std::path::PathBuf, StorageError> {
        let outbox_events = project_event_log_to_outbox(name, log)?;
        match self {
            Self::Flat(store, outbox) => {
                let path = store.write(name, log)?;
                for event in outbox_events {
                    outbox.append(event)?;
                }
                Ok(path)
            }
            Self::Segmented(store, outbox) => {
                let path = store.write(name, log)?;
                for event in outbox_events {
                    outbox.append(event)?;
                }
                Ok(path)
            }
            #[cfg(feature = "postgres")]
            Self::Postgres(store) => store.write_with_outbox(name, log, &outbox_events),
        }
    }
}

/// Kernel EventLog、OMS 订单状态、Ledger 和可恢复存储的单进程归约器。
#[derive(Clone)]
pub struct LiveEventPipeline {
    log: EventLog,
    ledger: Ledger,
    oms: Oms,
    marks: BTreeMap<InstrumentId, Price>,
    account_balances: BTreeMap<(String, String), Vec<AccountBalance>>,
    account_positions: BTreeMap<(String, String), Vec<AccountPositionSnapshot>>,
    funding_rates: BTreeMap<InstrumentId, FundingRateSnapshot>,
    seen_fills: BTreeSet<(u64, u64, i128, i128, i128, String)>,
    store: RuntimeEventStore,
    log_name: String,
    currency: String,
    last_engine_ts: u64,
    metrics: Arc<PipelineMetrics>,
}

impl LiveEventPipeline {
    /// 打开一个可恢复的运行日志。已有文件损坏或违反事件不变量时直接失败，
    /// 不会用空状态掩盖生产数据问题。
    pub fn open(
        root: impl Into<std::path::PathBuf>,
        log_name: impl Into<String>,
        currency: impl Into<String>,
    ) -> QxResult<Self> {
        Self::open_configured(root, log_name, currency, None)
    }

    /// 按运行时存储配置打开 EventLog。`None` 保持兼容的单文件后端；`Some`
    /// 使用完整段不可变、尾段追加和 manifest 校验的分段后端。
    pub fn open_configured(
        root: impl Into<std::path::PathBuf>,
        log_name: impl Into<String>,
        currency: impl Into<String>,
        max_events_per_segment: Option<usize>,
    ) -> QxResult<Self> {
        let root = root.into();
        match max_events_per_segment {
            Some(max_events_per_segment) => {
                Self::open_segmented(root, log_name, currency, max_events_per_segment)
            }
            None => Self::open_with_store(
                RuntimeEventStore::Flat(
                    EventLogFileStore::new(root.clone()),
                    FileOutboxStore::new(root),
                ),
                log_name,
                currency,
            ),
        }
    }

    /// 使用追加分段日志打开运行时。完整 segment 不会被覆盖，适合长时间
    /// 高频运行；`max_events_per_segment` 决定恢复和归档粒度。
    pub fn open_segmented(
        root: impl Into<std::path::PathBuf>,
        log_name: impl Into<String>,
        currency: impl Into<String>,
        max_events_per_segment: usize,
    ) -> QxResult<Self> {
        let root = root.into();
        let store = SegmentedEventLogStore::new(root.clone(), max_events_per_segment)
            .map_err(storage_error)?;
        Self::open_with_store(
            RuntimeEventStore::Segmented(store, FileOutboxStore::new(root)),
            log_name,
            currency,
        )
    }

    /// 使用 PostgreSQL 事务 EventLog 打开运行管线。队列、控制面和 EventLog
    /// 可以共享同一个 `PostgresStorage`，当前方法通过 DSN 建立独立安全连接，
    /// 由部署层连接池和数据库 HA 提供跨节点可用性。
    #[cfg(feature = "postgres")]
    pub fn open_postgres(
        dsn: &str,
        log_name: impl Into<String>,
        currency: impl Into<String>,
    ) -> QxResult<Self> {
        Self::open_postgres_with_pool_size(dsn, 1, log_name, currency)
    }

    #[cfg(feature = "postgres")]
    pub fn open_postgres_with_pool_size(
        dsn: &str,
        pool_size: usize,
        log_name: impl Into<String>,
        currency: impl Into<String>,
    ) -> QxResult<Self> {
        let store =
            PostgresEventLogStore::connect_with_pool_size(dsn, pool_size).map_err(|error| {
                QxError::Permanent(format!("打开 PostgreSQL EventLog 失败: {error:?}"))
            })?;
        Self::open_with_store(RuntimeEventStore::Postgres(store), log_name, currency)
    }

    fn open_with_store(
        store: RuntimeEventStore,
        log_name: impl Into<String>,
        currency: impl Into<String>,
    ) -> QxResult<Self> {
        let log_name = log_name.into();
        if log_name.trim().is_empty()
            || !log_name
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
        {
            return Err(QxError::BusinessViolation(
                "live event log name 非法".into(),
            ));
        }
        let currency = currency.into();
        if currency.trim().is_empty() {
            return Err(QxError::BusinessViolation("Ledger 结算币种不能为空".into()));
        }
        let log = store
            .read_if_exists(&log_name)
            .map_err(storage_error)?
            .unwrap_or_default();
        if !log.is_empty() {
            // 启动恢复时重新投影一次，补齐进程在 EventLog 写成功、Outbox
            // 尚未写完时崩溃留下的文件后端缺口；幂等后端不会重复事实。
            store.write(&log_name, &log).map_err(storage_error)?;
        }
        let ledger = ReplayVerifier::rebuild_ledger(log.events())?;
        let mut pipeline = Self {
            last_engine_ts: log.events().last().map(|event| event.ts).unwrap_or(0),
            log,
            ledger,
            oms: Oms::new(),
            marks: BTreeMap::new(),
            account_balances: BTreeMap::new(),
            account_positions: BTreeMap::new(),
            funding_rates: BTreeMap::new(),
            seen_fills: BTreeSet::new(),
            store,
            log_name,
            currency,
            metrics: Arc::new(PipelineMetrics::default()),
        };
        pipeline.rebuild_runtime_indexes()?;
        Ok(pipeline)
    }

    pub fn log(&self) -> &EventLog {
        &self.log
    }

    pub fn ledger(&self) -> &Ledger {
        &self.ledger
    }

    pub fn orders(&self) -> Vec<Order> {
        self.oms.all_orders()
    }

    pub fn venue_order_id(&self, client_order_id: u64) -> Option<String> {
        self.log
            .events()
            .iter()
            .rev()
            .find_map(|event| match &event.kind {
                EventKind::Accepted {
                    client_order_id: id,
                    venue_order_id,
                } if *id == client_order_id => venue_order_id.clone(),
                _ => None,
            })
    }

    pub fn marks(&self) -> &BTreeMap<InstrumentId, Price> {
        &self.marks
    }

    /// 返回 EventLog 中最近一次 L1 价格，兼容只需要价格的调用方。
    ///
    /// 返回 EventLog 中最近一次完整 L1 报价及数量，供 Paper/模拟执行使用。
    /// Paper 执行不得把固定价格当作默认市场；没有真实行情事实时应停在
    /// 等待/失败状态，由行情 worker 先写入报价后再重试订单。
    pub fn latest_quote(&self, instrument: &InstrumentId) -> Option<(Price, Price, u64)> {
        self.latest_quote_with_depth(instrument)
            .map(|quote| (quote.bid, quote.ask, quote.ts))
    }

    /// 返回 EventLog 中最近一次完整 L1 报价及买卖盘数量。
    pub fn latest_quote_with_depth(&self, instrument: &InstrumentId) -> Option<QuoteTick> {
        self.log.events().iter().rev().find_map(|event| {
            if let EventKind::MarketQuote {
                instrument: event_instrument,
                bid,
                ask,
                bid_qty,
                ask_qty,
            } = &event.kind
            {
                (event_instrument == instrument).then_some(QuoteTick::new(
                    event.ts,
                    *bid,
                    *bid_qty,
                    *ask,
                    *ask_qty,
                    event.source_seq,
                ))
            } else {
                None
            }
        })
    }

    pub fn metrics(&self) -> PipelineMetricsSnapshot {
        self.metrics.snapshot()
    }

    pub fn snapshot(&self) -> LivePipelineSnapshot {
        LivePipelineSnapshot {
            log_name: self.log_name.clone(),
            log_len: self.log.len(),
            log_digest: self.log.digest(),
            ledger_entries: self.ledger.entries().len(),
            orders: self.orders(),
            marks: self
                .marks
                .iter()
                .map(|(instrument, price)| (instrument.clone(), *price))
                .collect(),
            account_balances: self.account_balances.clone(),
            account_positions: self.account_positions.clone(),
            funding_rates: self.funding_rates.clone(),
            last_engine_ts: self.last_engine_ts,
        }
    }

    /// 比较指定账户在结算币种上的本地 Ledger 与柜台余额快照。
    ///
    /// `free + locked` 才是柜台可对账余额；输入会经过同一套非负/重复资产
    /// 校验。该方法是纯查询，不追加任何调整 entry，也不改变 EventLog。
    pub fn settlement_balance_discrepancies(
        &self,
        account_id: &str,
        venue_id: &str,
        balances: &[AccountBalance],
    ) -> QxResult<Vec<RuntimeBalanceDiscrepancy>> {
        if account_id.trim().is_empty() || venue_id.trim().is_empty() {
            return Err(QxError::ReconcileRequired(
                "余额对账缺少 account_id 或 venue_id".into(),
            ));
        }
        let balances = normalize_balances(balances.to_vec())?;
        let venue_raw = balances
            .iter()
            .find(|balance| balance.asset == self.currency)
            .map(|balance| {
                balance
                    .free
                    .raw()
                    .checked_add(balance.locked.raw())
                    .and_then(|value| value.checked_sub(balance.borrowed.raw()))
                    .ok_or_else(|| QxError::ReconcileRequired("柜台余额相加溢出".into()))
            })
            .transpose()?
            .unwrap_or(0);
        let ledger_raw = self.ledger.cash_for(account_id, &self.currency);
        if ledger_raw == venue_raw {
            return Ok(Vec::new());
        }
        Ok(vec![RuntimeBalanceDiscrepancy {
            account_id: account_id.into(),
            venue_id: venue_id.into(),
            asset: self.currency.clone(),
            ledger_raw,
            venue_raw,
        }])
    }

    /// 重新载入共享 EventLog 的最新前缀并恢复订单、账簿和去重索引。
    /// 账户级 UserStream/Reconciler/Execution worker 在处理下一条外部事实前
    /// 应调用它，以便看到其他进程刚刚追加的订单或回报。
    pub fn refresh(&mut self) -> QxResult<()> {
        self.metrics.refreshes.fetch_add(1, Ordering::Relaxed);
        let result = self.refresh_latest();
        if result.is_err() {
            self.metrics.failures.fetch_add(1, Ordering::Relaxed);
        }
        result
    }

    /// 把一个本地提交订单作为可恢复事实写入日志。
    ///
    /// `PendingSubmit` 会在提交事实边界被规范化为 `Submitted`；之后 Accepted、
    /// Fill、Cancelled 都只能通过事件驱动状态迁移。
    pub fn register_order(&mut self, order: Order, ts: u64) -> QxResult<u64> {
        self.register_order_with_correlation(order, ts, None)
    }

    /// 从已通过控制面权限校验的 `SubmitOrder` 命令注册订单。
    pub fn register_control_order(&mut self, command: &ControlCommand, ts: u64) -> QxResult<u64> {
        let order = order_from_submit_command(command)?;
        self.register_order_with_correlation(
            order,
            ts,
            Some(format!("control:{}", command.command_id)),
        )
    }

    pub fn register_order_with_correlation(
        &mut self,
        order: Order,
        ts: u64,
        correlation: Option<String>,
    ) -> QxResult<u64> {
        for _ in 0..3 {
            self.refresh_latest()?;
            match self.register_order_with_correlation_once(order.clone(), ts, correlation.clone())
            {
                Err(QxError::Transient(_)) => continue,
                result => return result,
            }
        }
        Err(QxError::Transient(
            "共享 EventLog 并发写入超过重试上限".into(),
        ))
    }

    fn register_order_with_correlation_once(
        &mut self,
        mut order: Order,
        ts: u64,
        correlation: Option<String>,
    ) -> QxResult<u64> {
        order.validate().map_err(QxError::BusinessViolation)?;
        if self.oms.get(order.client_id).is_some() {
            return Err(QxError::Invariant(format!(
                "重复的 live client_order_id: {}",
                order.client_id
            )));
        }
        if order.status == OrderStatus::PendingSubmit {
            order
                .status
                .transition(OrderStatus::Submitted)
                .map_err(QxError::Invariant)?;
        }
        if order.status != OrderStatus::Submitted {
            return Err(QxError::BusinessViolation(
                "register_order 只接受 PendingSubmit 或 Submitted 订单".into(),
            ));
        }
        let mut staged = self.clone();
        let context = order_event_context(&order);
        context
            .validate_for_trading()
            .map_err(QxError::BusinessViolation)?;
        let event = staged.append_fact(
            ts,
            ts,
            Priority::COMMAND,
            order.client_id,
            correlation.unwrap_or_else(|| format!("{}:order:{}", staged.log_name, order.client_id)),
            EventFact {
                metadata: EventMetadata {
                    source_id: "control".into(),
                    source_kind: "control".into(),
                    dedup_key: format!("control:order:{}", order.client_id),
                    context,
                    ..EventMetadata::default()
                },
                kind: EventKind::OrderSubmitted {
                    order: order.clone(),
                },
            },
        )?;
        staged.oms.insert_replayed(order)?;
        staged.persist()?;
        *self = staged;
        Ok(event.seq)
    }

    /// 归约一条外部事实。所有内存变更和事件日志持久化成功后才提交给调用方。
    /// 多个账户 worker 共享同一 EventLog 时，写冲突会重新载入最新事实并重放
    /// 当前输入；同一 correlation/source 只会落一次，避免重复成交或重复余额快照。
    pub fn ingest(&mut self, envelope: RuntimeEventEnvelope) -> QxResult<RuntimeIngestReceipt> {
        self.metrics.ingest_attempts.fetch_add(1, Ordering::Relaxed);
        for _ in 0..3 {
            self.metrics.refreshes.fetch_add(1, Ordering::Relaxed);
            if let Err(error) = self.refresh_latest() {
                self.metrics.failures.fetch_add(1, Ordering::Relaxed);
                return Err(error);
            }
            match self.ingest_once(envelope.clone()) {
                Err(QxError::Transient(_)) => {
                    self.metrics
                        .transient_retries
                        .fetch_add(1, Ordering::Relaxed);
                    continue;
                }
                Ok(receipt) => {
                    if receipt.deduplicated {
                        self.metrics
                            .deduplicated_events
                            .fetch_add(1, Ordering::Relaxed);
                    } else {
                        self.metrics.ingested_events.fetch_add(1, Ordering::Relaxed);
                    }
                    return Ok(receipt);
                }
                Err(error) => {
                    self.metrics.failures.fetch_add(1, Ordering::Relaxed);
                    return Err(error);
                }
            }
        }
        self.metrics.failures.fetch_add(1, Ordering::Relaxed);
        Err(QxError::Transient(
            "共享 EventLog 并发写入超过重试上限".into(),
        ))
    }

    fn ingest_once(&mut self, envelope: RuntimeEventEnvelope) -> QxResult<RuntimeIngestReceipt> {
        let mut staged = self.clone();
        let RuntimeEventEnvelope {
            event,
            event_ts,
            receive_ts,
            source_seq,
            correlation_id,
            mut metadata,
        } = envelope;
        let correlation_id = if correlation_id.trim().is_empty() {
            format!("{}:source:{}", staged.log_name, source_seq)
        } else {
            correlation_id
        };
        if metadata.source_id.trim().is_empty() {
            metadata.source_id =
                runtime_event_metadata(&correlation_id, source_seq, "internal").source_id;
        }
        if metadata.dedup_key.trim().is_empty() {
            metadata.dedup_key =
                runtime_event_metadata(&correlation_id, source_seq, "internal").dedup_key;
        }
        if metadata.source_kind.trim().is_empty() {
            metadata.source_kind =
                runtime_event_metadata(&correlation_id, source_seq, "internal").source_kind;
        }
        if metadata.rule_version.trim().is_empty() {
            metadata.rule_version = "runtime-v1".into();
        }
        enrich_runtime_event_context(&mut metadata, &event);
        metadata.validate().map_err(QxError::BusinessViolation)?;

        let semantic_replay = matches!(
            &event,
            RuntimeExternalEvent::Accepted { .. }
                | RuntimeExternalEvent::Cancelled { .. }
                | RuntimeExternalEvent::ReconcileRequired { .. }
                | RuntimeExternalEvent::AccountCashflow { .. }
                | RuntimeExternalEvent::FillWithSpec { .. }
        );
        if let Some(existing) = staged.log.events().iter().find(|event| {
            (!metadata.dedup_key.is_empty() && event.metadata.dedup_key == metadata.dedup_key)
                || (event.correlation_id == correlation_id
                    && (event.source_seq == source_seq || semantic_replay))
        }) {
            return Ok(RuntimeIngestReceipt {
                primary_seq: existing.seq,
                derived_seqs: Vec::new(),
                engine_ts: existing.ts,
                log_digest: staged.log.digest(),
                deduplicated: true,
            });
        }

        let fill_for_dedup = match &event {
            RuntimeExternalEvent::Fill { fill } => Some(fill),
            RuntimeExternalEvent::FillWithSpec { fill, .. } => Some(fill.as_ref()),
            _ => None,
        };
        if let Some(fill) = fill_for_dedup {
            let key = fill_key(fill);
            if staged.seen_fills.contains(&key) {
                return Ok(RuntimeIngestReceipt {
                    primary_seq: staged
                        .log
                        .events()
                        .last()
                        .map(|event| event.seq)
                        .unwrap_or(0),
                    derived_seqs: Vec::new(),
                    engine_ts: staged.last_engine_ts,
                    log_digest: staged.log.digest(),
                    deduplicated: true,
                });
            }
        }

        let (kind, priority) = match &event {
            RuntimeExternalEvent::MarketQuote {
                instrument,
                bid,
                ask,
                bid_qty,
                ask_qty,
            } => {
                if bid.raw() <= 0
                    || ask.raw() <= 0
                    || bid.raw() > ask.raw()
                    || bid_qty.raw() <= 0
                    || ask_qty.raw() <= 0
                {
                    return Err(QxError::BusinessViolation(
                        "L1 行情 bid/ask/数量非法或买卖盘交叉".into(),
                    ));
                }
                (
                    EventKind::MarketQuote {
                        instrument: instrument.clone(),
                        bid: *bid,
                        ask: *ask,
                        bid_qty: *bid_qty,
                        ask_qty: *ask_qty,
                    },
                    Priority::MARKET,
                )
            }
            RuntimeExternalEvent::AccountBalanceSnapshot {
                account_id,
                venue_id,
                balances,
            } => {
                if account_id.trim().is_empty() || venue_id.trim().is_empty() {
                    return Err(QxError::ReconcileRequired(
                        "账户余额快照缺少 account_id 或 venue_id".into(),
                    ));
                }
                let balances = normalize_balances(balances.clone())?;
                (
                    EventKind::AccountBalanceSnapshot {
                        account_id: account_id.clone(),
                        venue_id: venue_id.clone(),
                        balances,
                    },
                    Priority::FEEDBACK,
                )
            }
            RuntimeExternalEvent::AccountPositionSnapshot {
                account_id,
                venue_id,
                positions,
            } => {
                if account_id.trim().is_empty() || venue_id.trim().is_empty() {
                    return Err(QxError::ReconcileRequired(
                        "账户持仓快照缺少 account_id 或 venue_id".into(),
                    ));
                }
                let positions = normalize_positions(positions.clone())?;
                (
                    EventKind::AccountPositionSnapshot {
                        account_id: account_id.clone(),
                        venue_id: venue_id.clone(),
                        positions,
                    },
                    Priority::FEEDBACK,
                )
            }
            RuntimeExternalEvent::FundingRateSnapshot { snapshot } => {
                if snapshot.instrument.to_string().trim().is_empty()
                    || snapshot.next_funding_timestamp_ms == Some(0)
                {
                    return Err(QxError::ReconcileRequired(
                        "资金费率快照 instrument 或下一结算时间非法".into(),
                    ));
                }
                (
                    EventKind::FundingRateSnapshot {
                        instrument: snapshot.instrument.clone(),
                        funding_rate_bps: snapshot.funding_rate_bps,
                        next_funding_timestamp_ms: snapshot.next_funding_timestamp_ms,
                    },
                    Priority::FEEDBACK,
                )
            }
            RuntimeExternalEvent::AccountCashflow { cashflow } => {
                validate_cashflow(cashflow)?;
                (
                    EventKind::AccountCashflow {
                        cashflow: cashflow.clone(),
                    },
                    Priority::FEEDBACK,
                )
            }
            RuntimeExternalEvent::Accepted {
                client_order_id,
                venue_order_id,
            } => (
                EventKind::Accepted {
                    client_order_id: *client_order_id,
                    venue_order_id: venue_order_id.clone(),
                },
                Priority::FEEDBACK,
            ),
            RuntimeExternalEvent::Fill { fill } => {
                let mut fill = fill.clone();
                staged.normalize_fill_account(&mut fill)?;
                (EventKind::Filled { fill }, Priority::APPLY)
            }
            RuntimeExternalEvent::FillWithSpec { fill, .. } => {
                let mut fill = (**fill).clone();
                staged.normalize_fill_account(&mut fill)?;
                (EventKind::Filled { fill }, Priority::APPLY)
            }
            RuntimeExternalEvent::Cancelled { client_order_id } => (
                EventKind::Cancelled {
                    client_order_id: *client_order_id,
                },
                Priority::FEEDBACK,
            ),
            RuntimeExternalEvent::ReconcileRequired { client_order_id } => (
                EventKind::ReconcileRequired {
                    client_order_id: *client_order_id,
                },
                Priority::FEEDBACK,
            ),
        };
        let primary = staged.append_fact(
            event_ts,
            receive_ts,
            priority,
            source_seq,
            correlation_id.clone(),
            EventFact {
                metadata: metadata.clone(),
                kind,
            },
        )?;
        let engine_ts = primary.ts;
        let mut derived_seqs = Vec::new();

        match event {
            RuntimeExternalEvent::MarketQuote {
                instrument,
                bid: _bid,
                ask,
                bid_qty: _bid_qty,
                ask_qty: _ask_qty,
            } => {
                staged.marks.insert(instrument, ask);
            }
            RuntimeExternalEvent::AccountBalanceSnapshot {
                account_id,
                venue_id,
                balances,
            } => {
                staged
                    .account_balances
                    .insert((account_id, venue_id), normalize_balances(balances)?);
            }
            RuntimeExternalEvent::AccountPositionSnapshot {
                account_id,
                venue_id,
                positions,
            } => {
                staged
                    .account_positions
                    .insert((account_id, venue_id), normalize_positions(positions)?);
            }
            RuntimeExternalEvent::FundingRateSnapshot { snapshot } => {
                staged
                    .funding_rates
                    .insert(snapshot.instrument.clone(), snapshot);
            }
            RuntimeExternalEvent::AccountCashflow { cashflow } => {
                let entry_id = staged.apply_cashflow(&cashflow, engine_ts)?;
                let entry = staged
                    .ledger
                    .entries()
                    .iter()
                    .find(|entry| entry.id == entry_id)
                    .cloned()
                    .ok_or_else(|| QxError::Invariant("Cashflow Ledger entry 丢失".into()))?;
                let derived = staged.append_at_engine(
                    engine_ts,
                    receive_ts,
                    Priority::APPLY,
                    source_seq,
                    correlation_id.clone(),
                    EventFact {
                        metadata: metadata.derived(format!("ledger:{entry_id}")),
                        kind: EventKind::LedgerApplied { entry },
                    },
                )?;
                derived_seqs.push(derived.seq);
            }
            RuntimeExternalEvent::Accepted {
                client_order_id, ..
            } => {
                staged.apply_accepted(client_order_id)?;
            }
            RuntimeExternalEvent::Fill { fill } => {
                let mut fill = fill;
                staged.normalize_fill_account(&mut fill)?;
                staged.seen_fills.insert(fill_key(&fill));
                let ids = staged.apply_fill(&fill)?;
                for id in ids {
                    let entry = staged
                        .ledger
                        .entries()
                        .iter()
                        .find(|entry| entry.id == id)
                        .cloned()
                        .ok_or_else(|| QxError::Invariant("Ledger entry 丢失".into()))?;
                    let derived = staged.append_at_engine(
                        engine_ts,
                        receive_ts,
                        Priority::APPLY,
                        source_seq,
                        correlation_id.clone(),
                        EventFact {
                            metadata: metadata.derived(format!("ledger:{id}")),
                            kind: EventKind::LedgerApplied { entry },
                        },
                    )?;
                    derived_seqs.push(derived.seq);
                }
            }
            RuntimeExternalEvent::FillWithSpec { fill, spec } => {
                let mut fill = *fill;
                let spec = *spec;
                staged.normalize_fill_account(&mut fill)?;
                staged.seen_fills.insert(fill_key(&fill));
                let ids = staged.apply_fill_with_spec(&fill, &spec)?;
                for id in ids {
                    let entry = staged
                        .ledger
                        .entries()
                        .iter()
                        .find(|entry| entry.id == id)
                        .cloned()
                        .ok_or_else(|| QxError::Invariant("Ledger entry 丢失".into()))?;
                    let derived = staged.append_at_engine(
                        engine_ts,
                        receive_ts,
                        Priority::APPLY,
                        source_seq,
                        correlation_id.clone(),
                        EventFact {
                            metadata: metadata.derived(format!("ledger:{id}")),
                            kind: EventKind::LedgerApplied { entry },
                        },
                    )?;
                    derived_seqs.push(derived.seq);
                }
            }
            RuntimeExternalEvent::Cancelled { client_order_id } => {
                staged.apply_cancelled(client_order_id)?;
            }
            RuntimeExternalEvent::ReconcileRequired { client_order_id } => {
                staged.apply_reconcile_required(client_order_id)?;
            }
        }
        staged.persist()?;
        let receipt = RuntimeIngestReceipt {
            primary_seq: primary.seq,
            derived_seqs,
            engine_ts,
            log_digest: staged.log.digest(),
            deduplicated: false,
        };
        *self = staged;
        Ok(receipt)
    }

    fn rebuild_runtime_indexes(&mut self) -> QxResult<()> {
        let events = self.log.events().to_vec();
        for event in events {
            match &event.kind {
                EventKind::OrderSubmitted { order } => {
                    self.oms.insert_replayed(order.clone()).map_err(|error| {
                        QxError::Invariant(format!(
                            "EventLog OrderSubmitted 无法进入 OMS: {error:?}"
                        ))
                    })?;
                }
                EventKind::Accepted {
                    client_order_id, ..
                } => {
                    self.apply_accepted(*client_order_id)?;
                }
                EventKind::Filled { fill } => {
                    self.seen_fills.insert(fill_key(fill));
                    self.apply_order_fill(fill)?;
                }
                EventKind::Cancelled { client_order_id } => {
                    self.apply_cancelled(*client_order_id)?;
                }
                EventKind::ReconcileRequired { client_order_id } => {
                    self.apply_reconcile_required(*client_order_id)?;
                }
                EventKind::MarketQuote {
                    instrument, ask, ..
                } => {
                    self.marks.insert(instrument.clone(), *ask);
                }
                EventKind::AccountBalanceSnapshot {
                    account_id,
                    venue_id,
                    balances,
                } => {
                    self.account_balances.insert(
                        (account_id.clone(), venue_id.clone()),
                        normalize_balances(balances.clone())?,
                    );
                }
                EventKind::AccountPositionSnapshot {
                    account_id,
                    venue_id,
                    positions,
                } => {
                    self.account_positions.insert(
                        (account_id.clone(), venue_id.clone()),
                        normalize_positions(positions.clone())?,
                    );
                }
                EventKind::FundingRateSnapshot {
                    instrument,
                    funding_rate_bps,
                    next_funding_timestamp_ms,
                } => {
                    self.funding_rates.insert(
                        instrument.clone(),
                        FundingRateSnapshot {
                            instrument: instrument.clone(),
                            funding_rate_bps: *funding_rate_bps,
                            next_funding_timestamp_ms: *next_funding_timestamp_ms,
                        },
                    );
                }
                EventKind::AccountCashflow { .. }
                | EventKind::LedgerApplied { .. }
                | EventKind::MarketBar { .. }
                | EventKind::Timer { .. }
                | EventKind::Submit { .. }
                | EventKind::Rejected { .. }
                | EventKind::Settle => {}
            }
        }
        Ok(())
    }

    fn apply_cashflow(&mut self, cashflow: &AccountCashflow, ts: u64) -> QxResult<u64> {
        match cashflow.kind {
            CashflowKind::Funding => self.ledger.apply_funding(
                &cashflow.account_id,
                &cashflow.currency,
                cashflow.amount,
                ts,
            ),
            CashflowKind::Interest => self.ledger.apply_interest(
                &cashflow.account_id,
                &cashflow.currency,
                cashflow.amount,
                ts,
            ),
            CashflowKind::Settlement => self.ledger.apply_settlement(
                &cashflow.account_id,
                &cashflow.currency,
                cashflow.amount,
                ts,
            ),
            CashflowKind::Transfer => self.ledger.apply_adjustment(
                &cashflow.account_id,
                &cashflow.currency,
                cashflow.amount,
                ts,
            ),
        }
    }

    fn append_fact(
        &mut self,
        event_ts: u64,
        receive_ts: u64,
        priority: u8,
        source_seq: u64,
        correlation_id: String,
        fact: EventFact,
    ) -> QxResult<Event> {
        let engine_ts = self.last_engine_ts.max(event_ts);
        self.append_at_engine(
            engine_ts,
            if receive_ts == 0 {
                event_ts
            } else {
                receive_ts
            },
            priority,
            source_seq,
            correlation_id,
            fact,
        )
    }

    fn append_at_engine(
        &mut self,
        engine_ts: u64,
        receive_ts: u64,
        priority: u8,
        source_seq: u64,
        correlation_id: String,
        fact: EventFact,
    ) -> QxResult<Event> {
        let mut effective_ts = engine_ts.max(self.last_engine_ts);
        if let Some(previous) = self.log.events().last() {
            if (effective_ts, priority) <= (previous.ts, previous.prio) {
                effective_ts = previous
                    .ts
                    .checked_add(1)
                    .ok_or_else(|| QxError::Invariant("live engine time 溢出".into()))?;
            }
        }
        let seq = self.log.alloc_seq();
        let event = Event::new(seq, effective_ts, priority, fact.kind)
            .received_at(receive_ts)
            .engine_at(effective_ts)
            .sourced_by(source_seq)
            .correlated(correlation_id)
            .with_metadata(fact.metadata);
        self.log.append_checked(event.clone())?;
        self.last_engine_ts = self.last_engine_ts.max(effective_ts);
        Ok(event)
    }

    fn normalize_fill_account(&self, fill: &mut Fill) -> QxResult<()> {
        let order = self
            .oms
            .get(fill.order_id)
            .ok_or_else(|| QxError::ReconcileRequired("成交对应的本地订单不存在".into()))?;
        if fill.account_id.is_empty() {
            fill.account_id = order.account_id.clone();
        }
        if fill.account_id != order.account_id {
            return Err(QxError::Invariant("成交账户与本地订单账户不一致".into()));
        }
        Ok(())
    }

    fn apply_accepted(&mut self, client_order_id: u64) -> QxResult<()> {
        let order = self
            .oms
            .get_mut(client_order_id)
            .ok_or_else(|| QxError::ReconcileRequired("Accepted 对应未知订单".into()))?;
        match order.status {
            OrderStatus::Accepted
            | OrderStatus::Working
            | OrderStatus::PartiallyFilled
            | OrderStatus::Filled => Ok(()),
            OrderStatus::PendingSubmit => {
                order
                    .status
                    .transition(OrderStatus::Submitted)
                    .map_err(QxError::Invariant)?;
                order
                    .status
                    .transition(OrderStatus::Accepted)
                    .map_err(QxError::Invariant)
            }
            OrderStatus::Submitted | OrderStatus::Unknown => order
                .status
                .transition(OrderStatus::Accepted)
                .map_err(QxError::Invariant),
            _ => Err(QxError::ReconcileRequired(format!(
                "订单 {} 已处于 {:?}，不能接受 Accepted 事实",
                client_order_id, order.status
            ))),
        }
    }

    fn apply_fill(&mut self, fill: &Fill) -> QxResult<Vec<u64>> {
        let order = self
            .oms
            .get(fill.order_id)
            .cloned()
            .ok_or_else(|| QxError::ReconcileRequired("成交对应的本地订单不存在".into()))?;
        let ids = self.ledger.apply_fill(&order, fill, &self.currency)?;
        self.apply_order_fill(fill)?;
        Ok(ids)
    }

    fn apply_fill_with_spec(
        &mut self,
        fill: &Fill,
        spec: &TradingInstrumentSpec,
    ) -> QxResult<Vec<u64>> {
        let order = self
            .oms
            .get(fill.order_id)
            .cloned()
            .ok_or_else(|| QxError::ReconcileRequired("成交对应的本地订单不存在".into()))?;
        let ids = self
            .ledger
            .apply_fill_with_spec(&order, fill, &self.currency, spec)?;
        self.apply_order_fill(fill)?;
        Ok(ids)
    }

    fn apply_order_fill(&mut self, fill: &Fill) -> QxResult<()> {
        self.oms.apply_fill(fill)
    }

    fn apply_cancelled(&mut self, client_order_id: u64) -> QxResult<()> {
        let order = self
            .oms
            .get_mut(client_order_id)
            .ok_or_else(|| QxError::ReconcileRequired("Cancelled 对应未知订单".into()))?;
        if order.status == OrderStatus::Cancelled {
            return Ok(());
        }
        if order.status.is_terminal() {
            return Err(QxError::ReconcileRequired(format!(
                "订单 {} 已处于 {:?}，不能接受取消事实",
                client_order_id, order.status
            )));
        }
        if order.status == OrderStatus::PendingSubmit {
            order
                .status
                .transition(OrderStatus::Submitted)
                .map_err(QxError::Invariant)?;
        }
        if !matches!(order.status, OrderStatus::CancelPending) {
            order
                .status
                .transition(OrderStatus::CancelPending)
                .map_err(QxError::Invariant)?;
        }
        order
            .status
            .transition(OrderStatus::Cancelled)
            .map_err(QxError::Invariant)
    }

    fn apply_reconcile_required(&mut self, client_order_id: u64) -> QxResult<()> {
        if let Some(order) = self.oms.get_mut(client_order_id) {
            if !order.status.is_terminal() && order.status != OrderStatus::Unknown {
                order
                    .status
                    .transition(OrderStatus::Unknown)
                    .map_err(QxError::Invariant)?;
            }
        }
        Ok(())
    }

    fn persist(&self) -> QxResult<()> {
        match self.store.write(&self.log_name, &self.log) {
            Ok(_) => Ok(()),
            Err(StorageError::NonAppendOnly(_)) => Err(QxError::Transient(
                "共享 EventLog 已被其他 worker 追加，需重新载入".into(),
            )),
            Err(error) => Err(storage_error(error)),
        }
    }

    fn refresh_latest(&mut self) -> QxResult<()> {
        let latest = self
            .store
            .read_if_exists(&self.log_name)
            .map_err(storage_error)?;
        let Some(log) = latest else {
            if !self.log.is_empty() {
                return Err(QxError::ReconcileRequired(
                    "运行时 EventLog 在进程运行期间消失".into(),
                ));
            }
            return Ok(());
        };
        if log.events() == self.log.events() {
            return Ok(());
        }
        let ledger = ReplayVerifier::rebuild_ledger(log.events())?;
        let mut refreshed = Self {
            last_engine_ts: log.events().last().map(|event| event.ts).unwrap_or(0),
            log,
            ledger,
            oms: Oms::new(),
            marks: BTreeMap::new(),
            account_balances: BTreeMap::new(),
            account_positions: BTreeMap::new(),
            funding_rates: BTreeMap::new(),
            seen_fills: BTreeSet::new(),
            store: self.store.clone(),
            log_name: self.log_name.clone(),
            currency: self.currency.clone(),
            metrics: Arc::clone(&self.metrics),
        };
        refreshed.rebuild_runtime_indexes()?;
        *self = refreshed;
        Ok(())
    }
}

fn order_event_context(order: &Order) -> EventContext {
    let trace = order.trace.as_ref();
    EventContext {
        tenant_id: order.account_id.clone(),
        run_id: format!("account:{}", order.account_id),
        account_id: order.account_id.clone(),
        portfolio_id: order.account_id.clone(),
        strategy_id: trace
            .and_then(|trace| trace.strategy_id.clone())
            .unwrap_or_else(|| "unattributed-order".into()),
        signal_id: trace
            .and_then(|trace| trace.signal_id)
            .map(|value| value.to_string())
            .unwrap_or_default(),
        intent_id: trace
            .and_then(|trace| trace.intent_id)
            .map(|value| value.to_string())
            .unwrap_or_default(),
    }
}

fn fill_key(fill: &Fill) -> (u64, u64, i128, i128, i128, String) {
    (
        fill.order_id,
        fill.ts,
        fill.qty.raw(),
        fill.price.raw(),
        fill.fee.raw(),
        fill.venue_order_id.clone().unwrap_or_default(),
    )
}

fn normalize_balances(mut balances: Vec<AccountBalance>) -> QxResult<Vec<AccountBalance>> {
    for balance in &balances {
        if balance.asset.trim().is_empty()
            || balance.free.raw() < 0
            || balance.locked.raw() < 0
            || balance.borrowed.raw() < 0
        {
            return Err(QxError::ReconcileRequired(
                "账户余额快照包含非法资产或负余额".into(),
            ));
        }
    }
    balances.sort_by(|left, right| left.asset.cmp(&right.asset));
    if balances
        .windows(2)
        .any(|pair| pair[0].asset == pair[1].asset)
    {
        return Err(QxError::ReconcileRequired(
            "账户余额快照包含重复资产".into(),
        ));
    }
    Ok(balances)
}

fn validate_cashflow(cashflow: &AccountCashflow) -> QxResult<()> {
    if cashflow.account_id.trim().is_empty()
        || cashflow.venue_id.trim().is_empty()
        || cashflow.currency.trim().is_empty()
        || cashflow.external_id.trim().is_empty()
    {
        return Err(QxError::ReconcileRequired(
            "账户现金流水缺少 account/venue/currency/external_id".into(),
        ));
    }
    Ok(())
}

fn normalize_positions(
    mut positions: Vec<AccountPositionSnapshot>,
) -> QxResult<Vec<AccountPositionSnapshot>> {
    for position in &positions {
        if position.instrument.to_string().trim().is_empty()
            || position.quantity.raw() == i128::MIN
            || position.unrealized_pnl.raw() == i128::MIN
            || position.initial_margin.raw() < 0
            || position.maintenance_margin.raw() < 0
            || position.leverage == Some(0)
        {
            return Err(QxError::ReconcileRequired(
                "账户持仓快照包含非法数量、保证金或杠杆".into(),
            ));
        }
        if position.average_price.is_some_and(|price| price.raw() <= 0)
            || position.mark_price.is_some_and(|price| price.raw() <= 0)
            || position
                .liquidation_price
                .is_some_and(|price| price.raw() <= 0)
        {
            return Err(QxError::ReconcileRequired(
                "账户持仓快照价格必须为正".into(),
            ));
        }
    }
    positions.sort_by(|left, right| {
        left.instrument
            .cmp(&right.instrument)
            .then_with(|| left.position_side.cmp(&right.position_side))
    });
    if positions.windows(2).any(|pair| {
        pair[0].instrument == pair[1].instrument && pair[0].position_side == pair[1].position_side
    }) {
        return Err(QxError::ReconcileRequired(
            "账户持仓快照包含重复 instrument".into(),
        ));
    }
    Ok(positions)
}

fn storage_error(error: StorageError) -> QxError {
    QxError::Permanent(format!("运行时事件存储失败: {error:?}"))
}

/// 仅用于文档/测试校验路径是否落在工作区内；实际存储仍由
/// `EventLogFileStore` 做文件名和原子写入校验。
pub fn pipeline_path(root: impl AsRef<Path>, log_name: &str) -> std::path::PathBuf {
    root.as_ref().join(format!("{log_name}.json"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use qx_control::{CommandKind, ControlCommand, Permission};
    use qx_core::{Money, OrderTrace, Quantity, Side, VenueId};
    use std::collections::BTreeMap;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_root(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "qianxing-runtime-pipeline-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn order() -> Order {
        Order {
            client_id: 7,
            instrument: InstrumentId::new("BTCUSDT", VenueId::new("BINANCE")),
            side: Side::Buy,
            qty: Quantity::from_i64(2),
            limit: Some(Price::from_i64(100)),
            status: OrderStatus::PendingSubmit,
            filled: Quantity::ZERO,
            account_id: "main".into(),
            trace: Some(OrderTrace {
                strategy_id: Some("demo".into()),
                signal_id: Some(1),
                intent_id: Some(7),
                rule_version: Some("v1".into()),
            }),
            policy: None,
        }
    }

    #[test]
    fn market_and_fill_facts_reach_eventlog_ledger_and_recover() {
        let root = temp_root("recover");
        let mut pipeline = LiveEventPipeline::open(&root, "binance-main", "USDT").unwrap();
        pipeline.register_order(order(), 100).unwrap();
        let submitted = pipeline
            .log()
            .events()
            .iter()
            .find(|event| matches!(event.kind, EventKind::OrderSubmitted { .. }))
            .expect("order submission fact");
        assert_eq!(submitted.metadata.context.account_id, "main");
        assert_eq!(submitted.metadata.context.strategy_id, "demo");
        pipeline
            .ingest(RuntimeEventEnvelope::venue(
                RuntimeExternalEvent::Accepted {
                    client_order_id: 7,
                    venue_order_id: None,
                },
                110,
                111,
                1,
                "ack-7",
            ))
            .unwrap();
        let quote = RuntimeEventEnvelope::market_quote(
            InstrumentId::new("BTCUSDT", VenueId::new("BINANCE")),
            QuoteTick::new(
                90,
                Price::from_i64(99),
                qx_core::Quantity::from_i64(2),
                Price::from_i64(100),
                qx_core::Quantity::from_i64(3),
                2,
            ),
            120,
            2,
            "quote-2",
        );
        let quote_receipt = pipeline.ingest(quote).unwrap();
        assert!(quote_receipt.engine_ts >= 110);
        pipeline
            .ingest(RuntimeEventEnvelope::venue(
                RuntimeExternalEvent::AccountBalanceSnapshot {
                    account_id: "main".into(),
                    venue_id: "BINANCE".into(),
                    balances: vec![AccountBalance {
                        asset: "USDT".into(),
                        free: qx_core::Money::from_i64(1000),
                        locked: qx_core::Money::ZERO,
                        borrowed: qx_core::Money::ZERO,
                    }],
                },
                121,
                121,
                2,
                "balances-2",
            ))
            .unwrap();
        assert_eq!(pipeline.snapshot().account_balances.len(), 1);
        let fill = Fill {
            order_id: 7,
            qty: Quantity::from_i64(2),
            price: Price::from_i64(100),
            fee: Money::from_i64(1),
            ts: 115,
            account_id: "main".into(),
            venue_id: Some("BINANCE".into()),
            venue_order_id: Some("9001".into()),
            ..Fill::default()
        };
        let receipt = pipeline
            .ingest(RuntimeEventEnvelope::venue(
                RuntimeExternalEvent::Fill { fill: fill.clone() },
                fill.ts,
                121,
                3,
                "fill-3",
            ))
            .unwrap();
        assert_eq!(receipt.derived_seqs.len(), 3);
        assert_eq!(pipeline.ledger().entries().len(), 3);
        assert_eq!(pipeline.orders()[0].status, OrderStatus::Filled);
        assert_eq!(pipeline.snapshot().log_len, 8);
        let late_quote = pipeline
            .ingest(RuntimeEventEnvelope::market_quote(
                InstrumentId::new("BTCUSDT", VenueId::new("BINANCE")),
                QuoteTick::new(
                    100,
                    Price::from_i64(98),
                    qx_core::Quantity::from_i64(2),
                    Price::from_i64(99),
                    qx_core::Quantity::from_i64(3),
                    5,
                ),
                123,
                5,
                "late-quote",
            ))
            .unwrap();
        assert!(late_quote.engine_ts > receipt.engine_ts);

        let duplicate = pipeline
            .ingest(RuntimeEventEnvelope::venue(
                RuntimeExternalEvent::Fill { fill },
                115,
                122,
                4,
                "fill-duplicate",
            ))
            .unwrap();
        assert!(duplicate.deduplicated);
        assert_eq!(pipeline.snapshot().log_len, 9);
        let metrics = pipeline.metrics();
        assert_eq!(metrics.ingested_events, 5);
        assert_eq!(metrics.deduplicated_events, 1);
        assert!(metrics.refreshes >= metrics.ingest_attempts);
        assert!(metrics
            .to_prometheus()
            .contains("qx_pipeline_ingested_events_total 5"));
        let outbox_count = std::fs::read_dir(root.join("outbox/events"))
            .unwrap()
            .filter_map(Result::ok)
            .count();
        assert_eq!(outbox_count, pipeline.log().len());

        let restored = LiveEventPipeline::open(&root, "binance-main", "USDT").unwrap();
        assert_eq!(restored.snapshot(), pipeline.snapshot());
        assert_eq!(restored.ledger().entries().len(), 3);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn external_fact_metadata_is_persisted_and_explicit_dedup_is_authoritative() {
        let root = temp_root("fact-metadata");
        let mut pipeline = LiveEventPipeline::open(&root, "ccxt-main", "USDT").unwrap();
        let instrument = InstrumentId::new("BTCUSDT", VenueId::new("OKX"));
        let envelope = RuntimeEventEnvelope::venue(
            RuntimeExternalEvent::MarketQuote {
                instrument: instrument.clone(),
                bid: Price::from_i64(100),
                ask: Price::from_i64(101),
                bid_qty: Quantity::from_i64(2),
                ask_qty: Quantity::from_i64(3),
            },
            100,
            110,
            7,
            "okx:ticker",
        )
        .with_metadata(EventMetadata {
            source_id: "okx-rest".into(),
            source_kind: "market_data".into(),
            dedup_key: "okx:BTCUSDT:quote:closed-1".into(),
            rule_version: "ccxt-market-v1".into(),
            ..EventMetadata::default()
        });
        pipeline.ingest(envelope.clone()).unwrap();
        let event = pipeline.log().events().last().unwrap();
        assert_eq!(event.metadata.source_id, "okx-rest");
        assert_eq!(event.metadata.source_kind, "market_data");
        assert_eq!(event.metadata.dedup_key, "okx:BTCUSDT:quote:closed-1");
        assert_eq!(event.metadata.rule_version, "ccxt-market-v1");
        assert_eq!(event.effective_at(), 100);
        assert_eq!(event.observed_at(), 110);

        let duplicate = RuntimeEventEnvelope::venue(
            RuntimeExternalEvent::MarketQuote {
                instrument,
                bid: Price::from_i64(100),
                ask: Price::from_i64(101),
                bid_qty: Quantity::from_i64(2),
                ask_qty: Quantity::from_i64(3),
            },
            101,
            111,
            8,
            "okx:ticker:retry",
        )
        .with_metadata(envelope.metadata.clone());
        assert!(pipeline.ingest(duplicate).unwrap().deduplicated);
        let restored = LiveEventPipeline::open(&root, "ccxt-main", "USDT").unwrap();
        assert_eq!(
            restored.log().events().last().unwrap().metadata,
            envelope.metadata
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn segmented_pipeline_reopens_with_identical_state() {
        let root = temp_root("segmented-recover");
        let mut pipeline =
            LiveEventPipeline::open_segmented(&root, "paper-segmented", "USDT", 2).unwrap();
        pipeline.register_order(order(), 100).unwrap();
        pipeline
            .ingest(RuntimeEventEnvelope::venue(
                RuntimeExternalEvent::Accepted {
                    client_order_id: 7,
                    venue_order_id: Some("paper-7".into()),
                },
                101,
                101,
                1,
                "accepted-7",
            ))
            .unwrap();
        let restored =
            LiveEventPipeline::open_segmented(&root, "paper-segmented", "USDT", 2).unwrap();
        assert_eq!(restored.snapshot(), pipeline.snapshot());
        assert!(root.join("paper-segmented.manifest.json").exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn settlement_balance_reconciliation_reports_without_mutating_ledger() {
        let root = temp_root("balance-discrepancy");
        let pipeline = LiveEventPipeline::open(&root, "binance-main", "USDT").unwrap();
        let balances = vec![AccountBalance {
            asset: "USDT".into(),
            free: Money::from_i64(12),
            locked: Money::from_i64(3),
            borrowed: Money::ZERO,
        }];
        let discrepancies = pipeline
            .settlement_balance_discrepancies("main", "BINANCE", &balances)
            .unwrap();
        assert_eq!(discrepancies.len(), 1);
        assert_eq!(discrepancies[0].ledger_raw, 0);
        assert_eq!(discrepancies[0].venue_raw, Money::from_i64(15).raw());
        let net_debt_discrepancy = pipeline
            .settlement_balance_discrepancies(
                "main",
                "BINANCE",
                &[AccountBalance {
                    asset: "USDT".into(),
                    free: Money::from_i64(12),
                    locked: Money::from_i64(3),
                    borrowed: Money::from_i64(5),
                }],
            )
            .unwrap();
        assert_eq!(net_debt_discrepancy[0].venue_raw, Money::from_i64(10).raw());
        assert!(pipeline.log().is_empty());
        assert!(pipeline
            .settlement_balance_discrepancies(
                "main",
                "BINANCE",
                &[AccountBalance {
                    asset: "USDT".into(),
                    free: Money::ZERO,
                    locked: Money::ZERO,
                    borrowed: Money::ZERO,
                }]
            )
            .unwrap()
            .is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn derivative_position_and_funding_snapshots_are_recoverable_facts() {
        let root = temp_root("derivative-snapshots");
        let mut pipeline = LiveEventPipeline::open(&root, "ccxt-main", "USDT").unwrap();
        let instrument = InstrumentId::parse("BTC/USDT:USDT.OKX").unwrap();
        pipeline
            .ingest(RuntimeEventEnvelope::venue(
                RuntimeExternalEvent::AccountPositionSnapshot {
                    account_id: "main".into(),
                    venue_id: "OKX".into(),
                    positions: vec![qx_core::AccountPositionSnapshot {
                        instrument: instrument.clone(),
                        quantity: Quantity::from_i64(2),
                        average_price: Some(Price::from_i64(100)),
                        mark_price: Some(Price::from_i64(101)),
                        liquidation_price: Some(Price::from_i64(50)),
                        unrealized_pnl: Money::from_i64(2),
                        initial_margin: Money::from_i64(20),
                        maintenance_margin: Money::from_i64(5),
                        leverage: Some(10),
                        margin_mode: Some("isolated".into()),
                        position_side: Some("long".into()),
                    }],
                },
                100,
                101,
                1,
                "position-1",
            ))
            .unwrap();
        pipeline
            .ingest(RuntimeEventEnvelope::venue(
                RuntimeExternalEvent::FundingRateSnapshot {
                    snapshot: qx_core::FundingRateSnapshot {
                        instrument: instrument.clone(),
                        funding_rate_bps: 3,
                        next_funding_timestamp_ms: Some(200),
                    },
                },
                110,
                111,
                2,
                "funding-1",
            ))
            .unwrap();
        let cashflow_receipt = pipeline
            .ingest(RuntimeEventEnvelope::venue(
                RuntimeExternalEvent::AccountCashflow {
                    cashflow: qx_core::AccountCashflow {
                        account_id: "main".into(),
                        venue_id: "OKX".into(),
                        currency: "USDT".into(),
                        kind: qx_core::CashflowKind::Funding,
                        amount: Money::from_i64(-2),
                        external_id: "funding-bill-1".into(),
                    },
                },
                120,
                121,
                3,
                "cashflow:funding-bill-1",
            ))
            .unwrap();
        assert_eq!(cashflow_receipt.derived_seqs.len(), 1);
        let derived = pipeline
            .log()
            .events()
            .iter()
            .find(|event| event.seq == cashflow_receipt.derived_seqs[0])
            .unwrap();
        assert!(derived.metadata.dedup_key.contains(":ledger:"));
        assert_eq!(derived.metadata.source_id, "cashflow");
        let duplicate = pipeline
            .ingest(RuntimeEventEnvelope::venue(
                RuntimeExternalEvent::AccountCashflow {
                    cashflow: qx_core::AccountCashflow {
                        account_id: "main".into(),
                        venue_id: "OKX".into(),
                        currency: "USDT".into(),
                        kind: qx_core::CashflowKind::Funding,
                        amount: Money::from_i64(-2),
                        external_id: "funding-bill-1".into(),
                    },
                },
                120,
                122,
                99,
                "cashflow:funding-bill-1",
            ))
            .unwrap();
        assert!(duplicate.deduplicated);
        assert_eq!(
            pipeline.ledger().cash_for("main", "USDT"),
            Money::from_i64(-2).raw()
        );
        assert_eq!(pipeline.snapshot().account_positions.len(), 1);
        assert_eq!(pipeline.snapshot().funding_rates.len(), 1);
        let restored = LiveEventPipeline::open(&root, "ccxt-main", "USDT").unwrap();
        assert_eq!(restored.snapshot(), pipeline.snapshot());
        assert_eq!(
            restored.ledger().cash_for("main", "USDT"),
            Money::from_i64(-2).raw()
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn unknown_fill_is_not_persisted_and_reconcile_marks_unknown() {
        let root = temp_root("reconcile");
        let mut pipeline = LiveEventPipeline::open(&root, "user", "USDT").unwrap();
        let fill = Fill {
            order_id: 99,
            qty: Quantity::from_i64(1),
            price: Price::from_i64(1),
            fee: Money::ZERO,
            ts: 1,
            ..Fill::default()
        };
        assert!(matches!(
            pipeline.ingest(RuntimeEventEnvelope::venue(
                RuntimeExternalEvent::Fill { fill },
                1,
                1,
                1,
                "unknown",
            )),
            Err(QxError::ReconcileRequired(_))
        ));
        assert!(pipeline.log().is_empty());
        let mut o = order();
        o.client_id = 99;
        pipeline.register_order(o, 1).unwrap();
        pipeline
            .ingest(RuntimeEventEnvelope::venue(
                RuntimeExternalEvent::ReconcileRequired {
                    client_order_id: 99,
                },
                2,
                2,
                2,
                "reconcile",
            ))
            .unwrap();
        assert_eq!(pipeline.orders()[0].status, OrderStatus::Unknown);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn submit_order_command_is_bound_to_audited_order_payload() {
        let order = order();
        let mut payload = BTreeMap::new();
        payload.insert("order_json".into(), serde_json::to_string(&order).unwrap());
        let command = ControlCommand {
            command_id: 100,
            request_id: "submit-100".into(),
            operator_id: "ops".into(),
            reason: "integration test".into(),
            kind: CommandKind::SubmitOrder,
            target: "7".into(),
            payload,
            permission: Permission::Trading,
            dry_run: false,
        };
        assert_eq!(order_from_submit_command(&command).unwrap(), order);
        let mut mismatched = command.clone();
        mismatched.target = "8".into();
        assert!(order_from_submit_command(&mismatched).is_err());
    }

    #[test]
    fn account_workers_refresh_shared_eventlog_before_appending() {
        let root = temp_root("shared-workers");
        let mut execution = LiveEventPipeline::open(&root, "account-events", "USDT").unwrap();
        let mut user = LiveEventPipeline::open(&root, "account-events", "USDT").unwrap();
        execution.register_order(order(), 10).unwrap();
        user.ingest(RuntimeEventEnvelope::venue(
            RuntimeExternalEvent::Accepted {
                client_order_id: 7,
                venue_order_id: None,
            },
            11,
            11,
            1,
            "user-accepted-7",
        ))
        .unwrap();
        assert_eq!(user.orders()[0].status, OrderStatus::Accepted);
        execution.refresh().unwrap();
        assert_eq!(execution.orders()[0].status, OrderStatus::Accepted);
        let duplicate = execution
            .ingest(RuntimeEventEnvelope::venue(
                RuntimeExternalEvent::Accepted {
                    client_order_id: 7,
                    venue_order_id: None,
                },
                11,
                12,
                99,
                "user-accepted-7",
            ))
            .unwrap();
        assert!(duplicate.deduplicated);
        let _ = std::fs::remove_dir_all(root);
    }
}
