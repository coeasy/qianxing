//! 实盘事件归约边界。
//!
//! 连接器只负责把供应商协议转换成标准事实；本模块负责把事实按统一的
//! Kernel 语义写入 EventLog，并在同一事务边界内更新 OMS/Ledger。事件日志
//! 采用文件后端时，每次成功归约都会原子保存，因此进程重启可以先恢复账簿
//! 和订单状态，再继续接收新的行情或用户流事实。
//!
//! 两个子模块按职责切出去，本文件只留归约本身：
//! [`backend_switch`] 是打开时的 EventLog 后端切换闸门，[`fact_context`] 是
//! 事实进入日志前的元数据与归因上下文补全，[`commit`] 是事实提交与失败回滚。
//!
//! # EventLog 的保留 / 归档策略（P0-2；与 Scheduler 共见 `maturity/retention_policy.yaml`）
//!
//! 这本日志是账簿与全部内存索引的**重放源**，所以保留策略定成"只增长 + 按段归档"：
//! 1. **册内保留被禁止**：`EventLog` 不提供、也不得新增任何截断 / 淘汰 / 压缩入口——把一条活
//!    日志变短，会让账簿重放与 `refresh_latest` 的"本地这份是不是 store 的前缀"校验同时失去
//!    参照物。日志只增长。
//! 2. **轮转是运维杠杆**：换一本账靠换 `log_name`（新运行写新名，旧日志原样留着供审计与重放），
//!    而不是靠删旧事实。
//! 3. **归档粒度是封段**：分段后端把日志切成 `storage.event_log_segment_events` 大小的段，
//!    **只有尾段可增长、满段永远不可变**（`crates/qx-storage/src/lib.rs` 的
//!    `active_segment_can_extend` 判据），段清单带摘要校验，所以封段就是可离线归档、可事后校验
//!    的单元。
//!
//! 内存投影（`Ledger.entries` / `Oms.orders` / `ApiState.events`）的上界跟着这本日志，同一策略。

mod backend_switch;
mod commit;
mod event_identity;
mod fact_context;

use event_identity::RuntimeEventIdentityIndex;
use fact_context::{enrich_runtime_event_context, runtime_event_metadata};

pub use qx_control::order_from_submit_command;
use qx_core::{
    AccountBalance, AccountCashflow, AccountPositionSnapshot, CashflowKind, DecimalRounding, Event,
    EventContext, EventKind, EventLog, EventMetadata, ExactDecimal,
    ExecutionCapabilityDecisionFact, ExecutionCapabilityDecisionKind, Fill, FundingRateSnapshot,
    InstrumentId, Ledger, MarketDepthLevel, Order, OrderStatus, Price, Priority, Quantity, QxError,
    QxResult, ReplayVerifier, TradingInstrumentSpec, EXECUTION_CAPABILITY_DECISION_SCHEMA_VERSION,
};
use qx_guanxing::QuoteTick;
#[cfg(feature = "postgres")]
use qx_storage::PostgresEventLogStore;
#[cfg(feature = "sqlite")]
use qx_storage::SqliteEventLogStore;
use qx_storage::{
    project_event_log_to_outbox, EventLogFileStore, FileOutboxStore, SegmentedEventLogStore,
    StorageError,
};
use qx_zhenlu::Oms;
use serde::{Deserialize, Serialize};
#[path = "pipeline_metrics.rs"]
mod pipeline_metrics;
use pipeline_metrics::PipelineMetrics;
pub use pipeline_metrics::{PipelineMetricsSnapshot, PIPELINE_INGEST_WARNING_BUDGET_NS};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;

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
        bid_qty_exact: Option<ExactDecimal>,
        ask_qty_exact: Option<ExactDecimal>,
        bid_exact: Option<ExactDecimal>,
        ask_exact: Option<ExactDecimal>,
    },
    MarketOrderBook {
        instrument: InstrumentId,
        sequence: u64,
        bids: Vec<MarketDepthLevel>,
        asks: Vec<MarketDepthLevel>,
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
    FeeAdjustment {
        account_id: String,
        venue_id: String,
        order_id: u64,
        instrument: InstrumentId,
        fees: Vec<qx_core::FillFee>,
    },
    Cancelled {
        client_order_id: u64,
    },
    CancelPending {
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
                bid_qty_exact: quote.bid_qty_exact,
                ask_qty_exact: quote.ask_qty_exact,
                bid_exact: quote.bid_exact,
                ask_exact: quote.ask_exact,
            },
            event_ts: quote.ts,
            receive_ts,
            source_seq,
            metadata: runtime_event_metadata(&correlation_id, source_seq, "market_data"),
            correlation_id,
        }
    }

    pub fn market_order_book(
        instrument: InstrumentId,
        sequence: u64,
        bids: Vec<MarketDepthLevel>,
        asks: Vec<MarketDepthLevel>,
        event_ts: u64,
        receive_ts: u64,
        source_seq: u64,
        correlation_id: impl Into<String>,
    ) -> Self {
        let correlation_id = correlation_id.into();
        Self {
            event: RuntimeExternalEvent::MarketOrderBook {
                instrument,
                sequence,
                bids,
                asks,
            },
            event_ts,
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

    pub fn order_cancel_pending(
        client_order_id: u64,
        event_ts: u64,
        receive_ts: u64,
        source_seq: u64,
        correlation_id: impl Into<String>,
    ) -> Self {
        let correlation_id = correlation_id.into();
        Self {
            event: RuntimeExternalEvent::CancelPending { client_order_id },
            event_ts,
            receive_ts,
            source_seq,
            metadata: runtime_event_metadata(&correlation_id, source_seq, "control"),
            correlation_id,
        }
    }

    pub fn with_metadata(mut self, metadata: EventMetadata) -> Self {
        self.metadata = metadata;
        self
    }
}

/// `ingest` 的回执：这次归约是否幂等命中、落了哪几条事件、引擎时间推进到哪。
///
/// 不带主事件序号与日志摘要：事实流是唯一事实源，要按序号或摘要读请走 `log()`；
/// 这两格此前在全仓（含用例）零读者，随 V13 §9.12 #170 删除。
///
/// 剩下三格的读者也不同层：只有 `deduplicated` 有生产读者（`ccxt_facts.rs` 用它决定
/// 这条现金流水要不要计入"本次新入账几条"），`derived_seqs` 与 `engine_ts` 目前只有本
/// crate 的用例读者 —— 那两类断言（"派生事件落了几条""引擎时间推进到哪"）是这条归约链的
/// 回归证明面，按仓库口径保留，不算 #170 的漏删。
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RuntimeIngestReceipt {
    pub derived_seqs: Vec<u64>,
    pub engine_ts: u64,
    pub deduplicated: bool,
}

struct EventFact {
    metadata: EventMetadata,
    kind: EventKind,
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
    /// `None` = 柜台这一轮没报该币种（缺席）；把缺席折算成 0 参与比较，等于替交易所宣称该币种为 0。
    pub venue_raw: Option<i128>,
}

#[derive(Clone)]
enum RuntimeEventStore {
    Flat(EventLogFileStore, FileOutboxStore),
    Segmented(SegmentedEventLogStore, FileOutboxStore),
    #[cfg(feature = "sqlite")]
    Sqlite(SqliteEventLogStore),
    #[cfg(feature = "postgres")]
    Postgres(PostgresEventLogStore),
}

impl RuntimeEventStore {
    fn read_if_exists(&self, name: &str) -> Result<Option<EventLog>, StorageError> {
        match self {
            Self::Flat(store, _) => store.read_if_exists(name),
            Self::Segmented(store, _) => store.read_if_exists(name),
            #[cfg(feature = "sqlite")]
            Self::Sqlite(store) => store
                .read_if_exists(name)
                .map_err(|error| StorageError::Io(format!("SQLite EventLog: {error:?}"))),
            #[cfg(feature = "postgres")]
            Self::Postgres(store) => store
                .read_if_exists(name)
                .map_err(|error| StorageError::Io(format!("PostgreSQL EventLog: {error:?}"))),
        }
    }

    fn write(
        &self,
        name: &str,
        log: &EventLog,
        projection_cursor: u64,
    ) -> Result<std::path::PathBuf, StorageError> {
        // 只投影游标之后的新事件。文件后端在 ack 时会删除事件文件，整段重投影会把
        // 已投递并确认的事实重新放回 Outbox，relay/consumer 于是永远重复消费同一批
        // 事件；写放大也随日志长度变成平方级。过滤发生在序列化之前（见
        // `project_event_log_to_outbox`），所以游标之后的序号不再为已投递的事实付 serde。
        let outbox_events = project_event_log_to_outbox(name, log, projection_cursor)?;
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
            #[cfg(feature = "sqlite")]
            Self::Sqlite(store) => store.append_batch(name, log, &outbox_events),
            #[cfg(feature = "postgres")]
            Self::Postgres(store) => store.write_with_outbox(name, log, &outbox_events),
        }
    }

    /// 游标增量读（P1-2）：只取 `after_seq` 之后追加的事实。`Ok(None)` 有两种含义，调用方都按
    /// 同一件事处理（退回整份重建）：本后端没提供行级尾部读（文件后端按 DD-3 定位降级、
    /// PostgreSQL 行式读排在 §7 M2 的下一步），或本地这份已经不是 store 的前缀（被改写/分叉）。
    /// 这是**显式**回落不是静默降级；默认特征（`default = []`）下三个入参都不被读，`let _` 压住它。
    fn read_since(
        &self,
        name: &str,
        after_seq: Option<u64>,
        expected_prefix: Option<(&Event, usize)>,
    ) -> Result<Option<Vec<Event>>, StorageError> {
        #[cfg(not(feature = "sqlite"))]
        let _ = (&name, &after_seq, &expected_prefix);
        match self {
            #[cfg(feature = "sqlite")]
            Self::Sqlite(store) => store
                .read_since(name, after_seq, expected_prefix)
                .map_err(|error| StorageError::Io(format!("SQLite EventLog: {error:?}"))),
            _ => Ok(None),
        }
    }
}

/// 打开管线时对「EventLog 已落盘、Outbox 未写完」崩溃缺口的处理方式。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OutboxRecovery {
    /// 写面（worker、提交路径）：日志非空时整段补投影一次，填掉崩溃留下的缺口。
    /// 已 ack 的事实可能因此重放——Outbox 本来就是至少一次投递，消费端按 event_id 幂等。
    ReprojectOnOpen,
    /// 读模型（账户快照、Ledger 读模型、StrategyContext）：打开不写任何东西。
    /// 读请求不能产生写副作用；缺口留给写面下一次打开补齐。
    ReadOnly,
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
    event_identities: RuntimeEventIdentityIndex,
    // 下面这一格**只增不减的成交幂等台账**共用同一个上界：进程一生 ingest 过的成交数。
    // user stream、reconcile 与 Outbox 补投影三条路径都会把同一笔 fill 重投，`fill_key` 挡
    // 住第二次 `oms.apply_fill`。淘汰任何一条已见键就等于允许同一笔成交被记两次账，这是本
    // 仓库唯一不接受的失败方式；`rebuild_runtime_indexes` 重启时把整本 EventLog 的
    // `Filled` 全插回来，重启也不缩小（V13 R6-A，与 Binance `seen_fill_keys`、CCXT
    // `seen_trade_ids` 同族，共三本）。
    seen_fills: BTreeSet<(u64, u64, i128, i128, i128, String, String)>,
    store: RuntimeEventStore,
    log_name: String,
    currency: String,
    last_engine_ts: u64,
    /// Outbox 投影游标：小于该序号的事实已投影过，等于/大于的仍需投影。序号从 0
    /// 开始，因此这里存"下一条待投影"而不是"最后一条已投影"。游标只在本进程内推进：
    /// 写面 `open` 时仍会整段补投影一次（[`OutboxRecovery::ReprojectOnOpen`]），以便
    /// 填掉"EventLog 已落盘、Outbox 未写完"的崩溃缺口；读面打开一次都不补。
    outbox_projection_cursor: u64,
    metrics: Arc<PipelineMetrics>,
}

impl LiveEventPipeline {
    /// 打开一个可恢复的运行日志。已有文件损坏或违反事件不变量时直接失败，
    /// 不会用空状态掩盖生产数据问题。
    ///
    /// 这条入口在日志非空时会做一次启动补投影，因此它是**写面**入口。只读方
    /// （账户快照、Ledger 读模型、StrategyContext）必须走 [`Self::open_read_only`]：
    /// 否则每个 GET 请求都会把整本日志重新投影进 Outbox，一次读变成一次写。
    pub fn open(
        root: impl Into<std::path::PathBuf>,
        log_name: impl Into<String>,
        currency: impl Into<String>,
    ) -> QxResult<Self> {
        Self::open_stored(
            root,
            log_name,
            currency,
            None,
            OutboxRecovery::ReprojectOnOpen,
        )
    }

    /// 按运行时存储配置打开 EventLog。`None` 保持兼容的单文件后端；`Some`
    /// 使用完整段不可变、尾段追加并由 manifest 摘要校验的分段后端，
    /// `max_events_per_segment` 决定恢复和归档粒度。
    ///
    /// 分段换来的是**不可变段与按段归档/摘要校验**，不是更快的稳态运行：本轮实测
    /// 8000 条日志下每次追加的稳态代价是分段 0.417s vs 单文件 0.343s（约 +21%），
    /// 因为两种后端每次写都要按 chunk 重新序列化整本日志（见 #169）。
    /// 另外：两套文件后端写的是互不相交的文件，换这个参数等于换一本账。三条打开入口
    /// 都会在同一目录下发现另一本历史时当场拒绝（口径同
    /// [`Self::assert_no_abandoned_file_log`]），不会静默从空账本起步。
    pub fn open_configured(
        root: impl Into<std::path::PathBuf>,
        log_name: impl Into<String>,
        currency: impl Into<String>,
        max_events_per_segment: Option<usize>,
    ) -> QxResult<Self> {
        Self::open_stored(
            root,
            log_name,
            currency,
            max_events_per_segment,
            OutboxRecovery::ReprojectOnOpen,
        )
    }

    /// 读模型入口：后端选择与 [`Self::open_configured`] 完全一致，但打开过程不写
    /// 任何东西。补投影要填的崩溃缺口留给写面（worker、提交路径）的下一次打开处理
    /// ——Outbox 本来就是至少一次投递，读请求不是补齐它的时机。
    pub fn open_read_only(
        root: impl Into<std::path::PathBuf>,
        log_name: impl Into<String>,
        currency: impl Into<String>,
        max_events_per_segment: Option<usize>,
    ) -> QxResult<Self> {
        Self::open_stored(
            root,
            log_name,
            currency,
            max_events_per_segment,
            OutboxRecovery::ReadOnly,
        )
    }

    fn open_stored(
        root: impl Into<std::path::PathBuf>,
        log_name: impl Into<String>,
        currency: impl Into<String>,
        max_events_per_segment: Option<usize>,
        recovery: OutboxRecovery,
    ) -> QxResult<Self> {
        let root = root.into();
        let store = match max_events_per_segment {
            Some(max_events_per_segment) => RuntimeEventStore::Segmented(
                SegmentedEventLogStore::new(root.clone(), max_events_per_segment)
                    .map_err(storage_error)?,
                FileOutboxStore::new(root),
            ),
            None => RuntimeEventStore::Flat(
                EventLogFileStore::new(root.clone()),
                FileOutboxStore::new(root),
            ),
        };
        Self::open_with_store(store, log_name, currency, recovery)
    }

    /// 选定**数据库**后端（SQLite / PostgreSQL）之前要过的闸门：`storage.root` 下还留着
    /// 这个账户的文件后端历史就当场拒绝。为什么要拦、拦到哪几本历史，口径都在 [`backend_switch`]。
    pub fn assert_no_abandoned_file_log(
        root: impl Into<std::path::PathBuf>,
        log_name: impl Into<String>,
    ) -> QxResult<()> {
        backend_switch::guard_database_backend(&root.into(), &log_name.into())
    }

    /// 使用 SQLite 事务 EventLog 打开单机持久运行管线。事件事实与 Outbox
    /// 投影在同一事务内提交，与队列/控制面共享同一个数据库文件。
    #[cfg(feature = "sqlite")]
    pub fn open_sqlite(
        path: impl Into<std::path::PathBuf>,
        log_name: impl Into<String>,
        currency: impl Into<String>,
    ) -> QxResult<Self> {
        Self::open_sqlite_stored(path, log_name, currency, OutboxRecovery::ReprojectOnOpen)
    }

    /// SQLite 后端的读模型入口，语义同 [`Self::open_read_only`]：打开事务里只读
    /// EventLog，不再把整本日志重新投影进 Outbox 表。
    #[cfg(feature = "sqlite")]
    pub fn open_sqlite_read_only(
        path: impl Into<std::path::PathBuf>,
        log_name: impl Into<String>,
        currency: impl Into<String>,
    ) -> QxResult<Self> {
        Self::open_sqlite_stored(path, log_name, currency, OutboxRecovery::ReadOnly)
    }

    #[cfg(feature = "sqlite")]
    fn open_sqlite_stored(
        path: impl Into<std::path::PathBuf>,
        log_name: impl Into<String>,
        currency: impl Into<String>,
        recovery: OutboxRecovery,
    ) -> QxResult<Self> {
        let store = SqliteEventLogStore::new(path)
            .map_err(|error| QxError::Permanent(format!("打开 SQLite EventLog 失败: {error:?}")))?;
        Self::open_with_store(
            RuntimeEventStore::Sqlite(store),
            log_name,
            currency,
            recovery,
        )
    }

    /// 使用 PostgreSQL 事务 EventLog 打开运行管线。队列、控制面和 EventLog
    /// 可以共享同一个 `PostgresStorage`，当前方法通过 DSN 建立独立安全连接，
    /// 由部署层连接池和数据库 HA 提供跨节点可用性。
    #[cfg(feature = "postgres")]
    pub fn open_postgres_with_pool_size(
        dsn: &str,
        pool_size: usize,
        log_name: impl Into<String>,
        currency: impl Into<String>,
    ) -> QxResult<Self> {
        Self::open_postgres_stored(
            dsn,
            pool_size,
            log_name,
            currency,
            OutboxRecovery::ReprojectOnOpen,
        )
    }

    /// PostgreSQL 后端的读模型入口，语义同 [`Self::open_read_only`]。
    #[cfg(feature = "postgres")]
    pub fn open_postgres_read_only(
        dsn: &str,
        pool_size: usize,
        log_name: impl Into<String>,
        currency: impl Into<String>,
    ) -> QxResult<Self> {
        Self::open_postgres_stored(dsn, pool_size, log_name, currency, OutboxRecovery::ReadOnly)
    }

    #[cfg(feature = "postgres")]
    fn open_postgres_stored(
        dsn: &str,
        pool_size: usize,
        log_name: impl Into<String>,
        currency: impl Into<String>,
        recovery: OutboxRecovery,
    ) -> QxResult<Self> {
        let store =
            PostgresEventLogStore::connect_with_pool_size(dsn, pool_size).map_err(|error| {
                QxError::Permanent(format!("打开 PostgreSQL EventLog 失败: {error:?}"))
            })?;
        Self::open_with_store(
            RuntimeEventStore::Postgres(store),
            log_name,
            currency,
            recovery,
        )
    }

    fn open_with_store(
        store: RuntimeEventStore,
        log_name: impl Into<String>,
        currency: impl Into<String>,
        recovery: OutboxRecovery,
    ) -> QxResult<Self> {
        let log_name = log_name.into();
        if !is_valid_log_name(&log_name) {
            return Err(QxError::BusinessViolation(
                "live event log name 非法".into(),
            ));
        }
        let currency = currency.into();
        if currency.trim().is_empty() {
            return Err(QxError::BusinessViolation("Ledger 结算币种不能为空".into()));
        }
        backend_switch::guard_file_backends(&store, &log_name)?;
        let log = store
            .read_if_exists(&log_name)
            .map_err(storage_error)?
            .unwrap_or_default();
        if recovery == OutboxRecovery::ReprojectOnOpen && !log.is_empty() {
            // 启动恢复时重新投影一次，补齐进程在 EventLog 写成功、Outbox
            // 尚未写完时崩溃留下的文件后端缺口。这里的补投影是整段的，因此运行期
            // 已 ack 的事件可能在重启后重新出现：Outbox 只保证至少一次投递，
            // 消费端必须按 event_id 幂等。
            // 读面（[`OutboxRecovery::ReadOnly`]）不做这一步：账户快照/读模型每来一个
            // 请求就打开一次管线，整段补投影会把一次 GET 变成一次写（文件后端是每事件
            // 一把全局锁 + 一个文件），并把写面已经 ack 的事实重新放回待投递队列。
            store.write(&log_name, &log, 0).map_err(storage_error)?;
        }
        let outbox_projection_cursor = log
            .events()
            .last()
            .map(|event| event.seq.saturating_add(1))
            .unwrap_or(0);
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
            event_identities: RuntimeEventIdentityIndex::default(),
            seen_fills: BTreeSet::new(),
            store,
            log_name,
            currency,
            outbox_projection_cursor,
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

    /// 本管线记账使用的结算币种。现金腿按它落账并随 `LedgerApplied` 事实持久化，
    /// 因此读取同一账户的现金与权益必须回读这个值，而不是另猜一本账。
    pub fn settlement_currency(&self) -> &str {
        &self.currency
    }

    /// Rebuild the signed cumulative venue-fee baseline for a restored order from durable
    /// primary facts. This prevents the first post-restart CCXT order snapshot from rebooking
    /// commissions already applied to the ledger.
    pub fn fee_baseline_for_order(&self, order_id: u64) -> Result<BTreeMap<String, i128>, String> {
        let mut totals = BTreeMap::<String, i128>::new();
        let mut add = |currency: Option<&str>, amount: i128| -> Result<(), String> {
            if amount == 0 {
                return Ok(());
            }
            let currency = currency
                .map(str::trim)
                .filter(|currency| !currency.is_empty())
                .unwrap_or(self.settlement_currency())
                .to_ascii_uppercase();
            let total = totals.entry(currency).or_default();
            *total = total
                .checked_add(amount)
                .ok_or_else(|| format!("订单 {order_id} 的手续费基线累计溢出"))?;
            Ok(())
        };
        for event in self.log.events() {
            match &event.kind {
                EventKind::Filled { fill } if fill.order_id == order_id => {
                    // Exact fee rows have their own scale-bearing recovery baseline. Do not
                    // project them onto this legacy map or make exact fills unreadable here.
                    if fill.exact_fees.is_empty() {
                        for charge in fill.fee_charges()? {
                            add(charge.currency.as_deref(), charge.amount.raw())?;
                        }
                    }
                }
                EventKind::FeeAdjustment {
                    order_id: adjusted_order_id,
                    fees,
                    ..
                } if *adjusted_order_id == order_id => {
                    for fee in fees {
                        add(fee.currency.as_deref(), fee.amount.raw())?;
                    }
                }
                _ => {}
            }
        }
        Ok(totals)
    }

    /// Rebuild exact cumulative venue fees from durable fills without projecting them onto
    /// the legacy 1e-9 `Money` grid. Legacy fee rows are lifted exactly at scale 9.
    pub fn exact_fee_baseline_for_order(
        &self,
        order_id: u64,
    ) -> Result<BTreeMap<String, ExactDecimal>, String> {
        let mut totals = BTreeMap::<String, ExactDecimal>::new();
        let mut add = |currency: Option<&str>, amount: ExactDecimal| -> Result<(), String> {
            if amount.is_zero() {
                return Ok(());
            }
            let currency = currency
                .map(str::trim)
                .filter(|currency| !currency.is_empty())
                .unwrap_or(self.settlement_currency())
                .to_ascii_uppercase();
            let total = totals.entry(currency).or_insert(ExactDecimal::ZERO);
            *total = total
                .checked_add(amount)
                .ok_or_else(|| format!("订单 {order_id} 的精确手续费基线累计溢出"))?;
            Ok(())
        };
        for event in self.log.events() {
            match &event.kind {
                EventKind::Filled { fill } if fill.order_id == order_id => {
                    if !fill.exact_fees.is_empty() {
                        if !fill.fees.is_empty() || fill.fee.raw() != 0 {
                            return Err(format!(
                                "订单 {order_id} 的精确成交混用了 raw 与 exact 手续费"
                            ));
                        }
                        for charge in &fill.exact_fees {
                            add(charge.currency.as_deref(), charge.amount)?;
                        }
                    } else {
                        for charge in fill.fee_charges()? {
                            let amount = ExactDecimal::from_parts(charge.amount.raw(), 9).map_err(
                                |error| format!("订单 {order_id} 旧手续费转换失败: {error}"),
                            )?;
                            add(charge.currency.as_deref(), amount)?;
                        }
                    }
                }
                EventKind::FeeAdjustment {
                    order_id: adjusted_order_id,
                    fees,
                    ..
                } if *adjusted_order_id == order_id => {
                    for fee in fees {
                        let amount =
                            ExactDecimal::from_parts(fee.amount.raw(), 9).map_err(|error| {
                                format!("订单 {order_id} 旧手续费转换失败: {error}")
                            })?;
                        add(fee.currency.as_deref(), amount)?;
                    }
                }
                _ => {}
            }
        }
        Ok(totals)
    }

    /// Rebuild the trade IDs already represented by durable per-trade fee rows. Cumulative
    /// order-fee snapshots do not carry trade IDs and therefore contribute nothing here.
    pub fn fee_trade_ids_for_order(&self, order_id: u64) -> Result<BTreeSet<String>, String> {
        let mut trade_ids = BTreeSet::new();
        for event in self.log.events() {
            match &event.kind {
                EventKind::Filled { fill } if fill.order_id == order_id => {
                    if !fill.exact_fees.is_empty() {
                        for fee in &fill.exact_fees {
                            trade_ids.extend(
                                fee.source_trade_ids
                                    .iter()
                                    .map(|trade_id| trade_id.trim().to_string())
                                    .filter(|trade_id| !trade_id.is_empty()),
                            );
                        }
                    } else {
                        for fee in fill.fee_charges()? {
                            trade_ids.extend(
                                fee.source_trade_ids
                                    .into_iter()
                                    .map(|trade_id| trade_id.trim().to_string())
                                    .filter(|trade_id| !trade_id.is_empty()),
                            );
                        }
                    }
                }
                EventKind::FeeAdjustment {
                    order_id: adjusted_order_id,
                    fees,
                    ..
                } if *adjusted_order_id == order_id => {
                    for fee in fees {
                        trade_ids.extend(
                            fee.source_trade_ids
                                .iter()
                                .map(|trade_id| trade_id.trim().to_string())
                                .filter(|trade_id| !trade_id.is_empty()),
                        );
                    }
                }
                _ => {}
            }
        }
        Ok(trade_ids)
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

    /// 返回 EventLog 中最近一次完整 L1 报价及买卖盘数量。
    pub fn latest_quote_with_depth(&self, instrument: &InstrumentId) -> Option<QuoteTick> {
        self.log.events().iter().rev().find_map(|event| {
            if let EventKind::MarketQuote {
                instrument: event_instrument,
                bid,
                ask,
                bid_qty,
                ask_qty,
                bid_qty_exact,
                ask_qty_exact,
                bid_exact,
                ask_exact,
            } = &event.kind
            {
                (event_instrument == instrument).then_some(
                    QuoteTick::new(event.ts, *bid, *bid_qty, *ask, *ask_qty, event.source_seq)
                        .with_optional_exact_prices(*bid_exact, *ask_exact)
                        .with_optional_exact_sizes(*bid_qty_exact, *ask_qty_exact),
                )
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
    /// `free + locked` 才是柜台可对账余额；输入经同一套非负/重复资产校验。纯查询：不追加
    /// entry、不改 EventLog。柜台没报该币种时产出 `venue_raw: None` 的缺席差异，不参与"相等"判定。
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
                    .net_cash_raw()
                    .ok_or_else(|| QxError::ReconcileRequired("柜台余额相加溢出".into()))
            })
            .transpose()?;
        let ledger_raw = self.ledger.cash_for(account_id, &self.currency);
        if venue_raw == Some(ledger_raw) {
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
                // 重试资格取错误码五元契约（P1-11）：能立即重试的只有 `Retryability::Allowed`。
                Err(error) if error.contract().retryability.allows_retry() => continue,
                result => return result,
            }
        }
        Err(QxError::Transient(
            "共享 EventLog 并发写入超过重试上限".into(),
        ))
    }

    /// Persist an allowed execution preflight before creating a venue connector.
    /// A client order ID has one immutable capability decision; a retry with different order,
    /// rules, catalog or evidence is rejected instead of silently replacing its audit record.
    pub fn record_execution_capability_decision(
        &mut self,
        decision: ExecutionCapabilityDecisionFact,
        ts: u64,
    ) -> QxResult<u64> {
        for _ in 0..3 {
            self.refresh_latest()?;
            match self.record_execution_capability_decision_once(decision.clone(), ts) {
                Err(error) if error.contract().retryability.allows_retry() => continue,
                result => return result,
            }
        }
        Err(QxError::Transient(
            "共享 EventLog 并发写入 execution capability decision 超过重试上限".into(),
        ))
    }

    /// Read the immutable decision bound to one client order ID.
    pub fn execution_capability_decision(
        &self,
        client_order_id: u64,
    ) -> Option<&ExecutionCapabilityDecisionFact> {
        self.log
            .events()
            .iter()
            .find_map(|event| match &event.kind {
                EventKind::ExecutionCapabilityDecision { decision }
                    if decision.client_order_id == client_order_id =>
                {
                    Some(decision)
                }
                _ => None,
            })
    }

    /// 归约一条外部事实。所有内存变更和事件日志持久化成功后才提交给调用方。
    /// 多个账户 worker 共享同一 EventLog 时，写冲突会重新载入最新事实并重放
    /// 当前输入；同一 correlation/source 只会落一次，避免重复成交或重复余额快照。
    pub fn ingest(&mut self, envelope: RuntimeEventEnvelope) -> QxResult<RuntimeIngestReceipt> {
        let started = Instant::now();
        let result = self.ingest_inner(envelope);
        let elapsed_ns = started.elapsed().as_nanos().min(u64::MAX as u128) as u64;
        self.metrics.record_ingest_duration(elapsed_ns);
        result
    }

    fn ingest_inner(&mut self, envelope: RuntimeEventEnvelope) -> QxResult<RuntimeIngestReceipt> {
        self.metrics.ingest_attempts.fetch_add(1, Ordering::Relaxed);
        for _ in 0..3 {
            self.metrics.refreshes.fetch_add(1, Ordering::Relaxed);
            if let Err(error) = self.refresh_latest() {
                self.metrics.failures.fetch_add(1, Ordering::Relaxed);
                return Err(error);
            }
            match self.ingest_once(envelope.clone()) {
                // 同 `register_order_with_correlation`：重试资格取五元契约（P1-11）。
                Err(error) if error.contract().retryability.allows_retry() => {
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

    fn rebuild_runtime_indexes(&mut self) -> QxResult<()> {
        let events = self.log.events().to_vec();
        for event in &events {
            self.event_identities.remember(event);
            self.apply_index_event(event)?;
        }
        Ok(())
    }

    /// 单条事实对索引（OMS / marks / 账户快照 / 资金费率）的作用。
    ///
    /// 整份重建与游标增量（[`Self::apply_tail`]）共用它：重建逐条喂整条日志，增量只喂新追加
    /// 的尾部——两条路径必须落在同一份归约上，否则"重启后状态"与"跑着的时候状态"会分叉。
    /// 账簿不在这里动：现金与持仓由 `LedgerApplied` 事实单独驱动（见调用点）。
    fn apply_index_event(&mut self, event: &Event) -> QxResult<()> {
        match &event.kind {
            EventKind::OrderSubmitted { order } => {
                self.oms.insert_replayed(order.clone()).map_err(|error| {
                    QxError::Invariant(format!("EventLog OrderSubmitted 无法进入 OMS: {error:?}"))
                })?;
            }
            EventKind::Accepted {
                client_order_id, ..
            } => {
                self.apply_accepted(*client_order_id)?;
            }
            EventKind::Filled { fill } => {
                self.seen_fills.insert(fill_key(fill));
                // 索引重建只回放订单状态；现金与持仓由 Ledger 自身的回放恢复，
                // 在这里再记一次会重复入账。
                self.oms.apply_fill(fill)?;
            }
            EventKind::Cancelled { client_order_id } => {
                self.apply_cancelled(*client_order_id)?;
            }
            EventKind::CancelPending { client_order_id } => {
                self.apply_cancel_pending(*client_order_id)?;
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
            EventKind::MarketOrderBook { .. }
            | EventKind::AccountCashflow { .. }
            | EventKind::FeeAdjustment { .. }
            | EventKind::LedgerApplied { .. }
            | EventKind::StrategyDecision { .. }
            | EventKind::OrderRiskDecision { .. }
            | EventKind::ExecutionCapabilityDecision { .. }
            | EventKind::Submit { .. }
            | EventKind::Rejected { .. }
            | EventKind::Settle => {}
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
        self.event_identities.remember(&event);
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
        qx_core::apply_fill_to_books(
            &mut self.ledger,
            &mut self.oms,
            &self.currency,
            fill,
            qx_core::FillTerms::LegacyMultiplier(1),
        )
    }

    fn apply_fill_with_spec(
        &mut self,
        fill: &Fill,
        spec: &TradingInstrumentSpec,
    ) -> QxResult<Vec<u64>> {
        qx_core::apply_fill_to_books(
            &mut self.ledger,
            &mut self.oms,
            &self.currency,
            fill,
            qx_core::FillTerms::Instrument(spec),
        )
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
        if !matches!(
            order.status,
            OrderStatus::CancelPending | OrderStatus::Unknown
        ) {
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

    fn apply_cancel_pending(&mut self, client_order_id: u64) -> QxResult<()> {
        let order = self
            .oms
            .get_mut(client_order_id)
            .ok_or_else(|| QxError::ReconcileRequired("CancelPending 对应未知订单".into()))?;
        if order.status == OrderStatus::CancelPending {
            return Err(QxError::ReconcileRequired(format!(
                "订单 {} 已有撤单操作在途，拒绝重复发起",
                client_order_id
            )));
        }
        if order.status.is_terminal()
            || matches!(
                order.status,
                OrderStatus::Unknown | OrderStatus::PendingSubmit
            )
        {
            return Err(QxError::ReconcileRequired(format!(
                "订单 {} 处于 {:?}，不能直接进入撤单在途状态",
                client_order_id, order.status
            )));
        }
        order
            .status
            .transition(OrderStatus::CancelPending)
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

    fn persist(&mut self) -> QxResult<()> {
        let cursor = self.outbox_projection_cursor;
        match self.store.write(&self.log_name, &self.log, cursor) {
            Ok(_) => {
                // 游标只在整段写成功后推进；中途失败时下一次 persist 会重放缺口，
                // 已存在的 Outbox 事件由 same_fact 幂等吸收。
                if let Some(last_seq) = self.log.events().last().map(|event| event.seq) {
                    self.outbox_projection_cursor = self
                        .outbox_projection_cursor
                        .max(last_seq.saturating_add(1));
                }
                Ok(())
            }
            Err(StorageError::NonAppendOnly(_)) => Err(QxError::Transient(
                "共享 EventLog 已被其他 worker 追加，需重新载入".into(),
            )),
            Err(error) => Err(storage_error(error)),
        }
    }

    /// 把共享 EventLog 的新事实接进本地状态（P1-2 游标增量）。
    ///
    /// 旧实现每次都把整条日志读回来 + 整份重放重建（`rebuild_ledger` +
    /// `rebuild_runtime_indexes` + 整份替换），而本方法在每条外部事实之前都会被调一次——
    /// 没有别的写者时这些工作全是白做。现在分三支：没有新事实直接返回；有新增且本地这份仍是
    /// store 的前缀时只把尾部接上去；前缀对不上或后端没有行级尾部读时退回整份重建。
    fn refresh_latest(&mut self) -> QxResult<()> {
        let after_seq = self.log.events().last().map(|event| event.seq);
        let prefix = self
            .log
            .events()
            .last()
            .map(|event| (event, self.log.len()));
        match self
            .store
            .read_since(&self.log_name, after_seq, prefix)
            .map_err(storage_error)?
        {
            Some(tail) if tail.is_empty() => Ok(()),
            Some(tail) => {
                self.metrics.tail_appends.fetch_add(1, Ordering::Relaxed);
                self.apply_tail(&tail)
            }
            None => {
                self.metrics.rebuilds.fetch_add(1, Ordering::Relaxed);
                self.rebuild_from_store(false)
            }
        }
    }

    /// 整份重建：读回整条日志并按事实重放全部索引。
    ///
    /// 三个调用场景：① 本地这份不是 store 的前缀（被改写/分叉）；② 后端没有行级尾部读——
    /// 这两条都是增量路径的兜底（`force=false`，日志逐条相等可短路）；③ 提交失败回滚
    /// （`force=true`，见 `pipeline::commit` 的薄包装，必须强制重放才能把 `next_seq` 一起收回）。
    /// 它也是"日志在运行期间消失"这类异常的报错点。
    fn rebuild_from_store(&mut self, force: bool) -> QxResult<()> {
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
        // `force=false`（常规刷新）：日志逐条相等说明本地这份就是 store 的最新前缀，无需重放。
        // `force=true`（失败回滚）：**不许短路**——失败路径上 `append_at_engine` 可能在
        // `append_checked` 之前就 `alloc_seq` 推过 `next_seq`，而 `events` 一条没变，
        // 短路会让这一格漏回滚。重放会把 `next_seq` 按 max(seq)+1 重算，因此只有强制重放才精确。
        if !force && log.events() == self.log.events() {
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
            event_identities: RuntimeEventIdentityIndex::default(),
            seen_fills: BTreeSet::new(),
            store: self.store.clone(),
            log_name: self.log_name.clone(),
            currency: self.currency.clone(),
            outbox_projection_cursor: self.outbox_projection_cursor,
            metrics: Arc::clone(&self.metrics),
        };
        refreshed.rebuild_runtime_indexes()?;
        *self = refreshed;
        Ok(())
    }

    /// 把 store 里新追加的尾部接进本地状态：日志按单事务批量追加（P1-1 的 `append_batch`），索引
    /// 与账簿逐条增量应用。索引那一步与整份重建共用 [`Self::apply_index_event`]，归约单源。
    fn apply_tail(&mut self, tail: &[Event]) -> QxResult<()> {
        self.log.append_batch(tail)?;
        for event in tail {
            self.event_identities.remember(event);
            self.apply_index_event(event)?;
            if let EventKind::LedgerApplied { entry } = &event.kind {
                self.ledger.apply_entry(entry.clone())?;
            }
        }
        self.last_engine_ts = self.log.events().last().map(|event| event.ts).unwrap_or(0);
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

fn fill_key(fill: &Fill) -> (u64, u64, i128, i128, i128, String, String) {
    (
        fill.order_id,
        fill.ts,
        fill.qty.raw(),
        fill.price.raw(),
        fill.fee.raw(),
        fill.venue_order_id.clone().unwrap_or_default(),
        fill.exact_identity_suffix(),
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
            || position
                .unrealized_pnl
                .is_some_and(|pnl| pnl.raw() == i128::MIN)
            || position
                .initial_margin
                .is_some_and(|margin| margin.raw() < 0)
            || position
                .maintenance_margin
                .is_some_and(|margin| margin.raw() < 0)
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

/// EventLog 名字同时充当文件名，所以它必须能安全落盘。
fn is_valid_log_name(name: &str) -> bool {
    !name.trim().is_empty()
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
}

#[cfg(test)]
mod tests {
    use super::*;
    use qx_control::{CommandKind, ControlCommand, Permission};
    use qx_core::{
        ExactFillAmounts, ExactFillFee, ExactOrderAmounts, ExactTradingRules, Money, OrderTrace,
        Quantity, Side, VenueId,
    };
    use std::collections::BTreeMap;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_root(label: &str) -> std::path::PathBuf {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-temp")
            .join(format!(
                "qianxing-runtime-pipeline-{label}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
        std::fs::create_dir_all(&root).expect("create runtime pipeline test directory");
        root
    }

    fn writable_eventlog_root(label: &str) -> std::path::PathBuf {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-temp")
            .join(format!(
                "qianxing-runtime-capability-{label}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
        std::fs::create_dir_all(&root).expect("create capability EventLog test directory");
        root
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
            exact_amounts: None,
        }
    }

    fn capability_decision(
        kind: ExecutionCapabilityDecisionKind,
    ) -> ExecutionCapabilityDecisionFact {
        ExecutionCapabilityDecisionFact {
            schema_version: EXECUTION_CAPABILITY_DECISION_SCHEMA_VERSION,
            kind,
            decision_id: "a".repeat(64),
            command_id: (kind == ExecutionCapabilityDecisionKind::Submit).then_some(99),
            order_group_id: None,
            client_order_id: 7,
            account_id: "main".into(),
            venue_id: "BINANCE".into(),
            order_fingerprint: "b".repeat(64),
            catalog_fingerprint: "c".repeat(64),
            market_rule_fingerprint: "d".repeat(64),
            preflight_json: r#"{"schema_version":1}"#.into(),
        }
    }

    #[test]
    fn execution_capability_decision_is_durable_idempotent_and_immutable() {
        let root = writable_eventlog_root("decision");
        let mut pipeline = LiveEventPipeline::open(&root, "binance-main", "USDT").unwrap();
        let decision = capability_decision(ExecutionCapabilityDecisionKind::Submit);
        let seq = pipeline
            .record_execution_capability_decision(decision.clone(), 100)
            .unwrap();
        assert_eq!(
            pipeline
                .record_execution_capability_decision(decision.clone(), 100)
                .unwrap(),
            seq,
            "retrying the same gate record must be idempotent"
        );
        assert_eq!(pipeline.log().events().len(), 1);

        let mut conflicting = decision.clone();
        conflicting.catalog_fingerprint = "e".repeat(64);
        conflicting.decision_id = "f".repeat(64);
        assert!(pipeline
            .record_execution_capability_decision(conflicting, 101)
            .is_err());
        assert_eq!(pipeline.log().events().len(), 1);

        let reopened = LiveEventPipeline::open(&root, "binance-main", "USDT").unwrap();
        assert_eq!(reopened.execution_capability_decision(7), Some(&decision));
        assert_eq!(reopened.log().events()[0].seq, seq);
    }

    #[test]
    fn recovery_adoption_requires_an_existing_nonterminal_order() {
        let root = writable_eventlog_root("recovery-adoption");
        let mut pipeline = LiveEventPipeline::open(&root, "binance-main", "USDT").unwrap();
        let fact = capability_decision(ExecutionCapabilityDecisionKind::RecoveryAdoption);
        assert!(pipeline
            .record_execution_capability_decision(fact.clone(), 100)
            .is_err());
        pipeline.register_order(order(), 110).unwrap();
        pipeline
            .record_execution_capability_decision(fact, 120)
            .unwrap();
        assert!(matches!(
            pipeline.log().events().last().map(|event| &event.kind),
            Some(EventKind::ExecutionCapabilityDecision { .. })
        ));
    }

    #[test]
    fn concurrent_capability_decision_retries_converge_to_one_fact() {
        let root = writable_eventlog_root("concurrent");
        let left = LiveEventPipeline::open(&root, "binance-main", "USDT").unwrap();
        let right = LiveEventPipeline::open(&root, "binance-main", "USDT").unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let fact = capability_decision(ExecutionCapabilityDecisionKind::Submit);
        let (left_seq, right_seq) = std::thread::scope(|scope| {
            let left_barrier = barrier.clone();
            let left_fact = fact.clone();
            let left_thread = scope.spawn(move || {
                let mut pipeline = left;
                left_barrier.wait();
                pipeline
                    .record_execution_capability_decision(left_fact, 100)
                    .unwrap()
            });
            let right_thread = scope.spawn(move || {
                let mut pipeline = right;
                barrier.wait();
                pipeline
                    .record_execution_capability_decision(fact, 100)
                    .unwrap()
            });
            (left_thread.join().unwrap(), right_thread.join().unwrap())
        });
        assert_eq!(left_seq, right_seq);
        let reopened = LiveEventPipeline::open(&root, "binance-main", "USDT").unwrap();
        assert_eq!(
            reopened
                .log()
                .events()
                .iter()
                .filter(|event| matches!(event.kind, EventKind::ExecutionCapabilityDecision { .. }))
                .count(),
            1
        );
    }

    /// P0-2(a)：失败的 ingest 不留半截事实，也不漏掉一格 seq。
    ///
    /// `ingest_commit` 就地改 `self`（旧实现改的是 `self.clone()` 的副本，代价是每次追加
    /// O(日志长度)），失败由 `pipeline::commit` 的薄包装强制重放回滚。这条用例钉住回滚真的发生：
    /// 未知订单的 `Accepted` 在**追加事实之后**才失败（`apply_accepted` 找不到订单），若回滚没做，
    /// EventLog 里会留下一条 Accepted 而 OMS 里没有对应订单——下次恢复重放就会撞上这条不一致。
    #[test]
    fn failed_ingest_rolls_back_the_appended_fact_and_the_seq_cursor() {
        let root = temp_root("rollback");
        let mut pipeline = LiveEventPipeline::open(&root, "binance-main", "USDT").unwrap();
        pipeline.register_order(order(), 100).unwrap();
        let events_before = pipeline.log().events().len();
        let next_seq_before = pipeline.log().next_seq();

        let failed = pipeline.ingest(RuntimeEventEnvelope::venue(
            RuntimeExternalEvent::Accepted {
                client_order_id: 999, // 本地没有这张订单
                venue_order_id: None,
            },
            110,
            110,
            1,
            "ack-unknown",
        ));
        assert!(failed.is_err(), "未知订单的 Accepted 必须被拒");

        assert_eq!(
            pipeline.log().events().len(),
            events_before,
            "失败的 ingest 不得在 EventLog 里留下半截事实"
        );
        assert_eq!(
            pipeline.log().next_seq(),
            next_seq_before,
            "失败前 append_at_engine 已 alloc_seq 推过游标，回滚必须把它一起收回"
        );

        // 回滚之后系统仍可继续用：下一条事实照常落地，且重开能重放出一致的日志。
        let receipt = pipeline
            .ingest(RuntimeEventEnvelope::venue(
                RuntimeExternalEvent::Accepted {
                    client_order_id: 7,
                    venue_order_id: None,
                },
                120,
                120,
                2,
                "ack-7",
            ))
            .unwrap();
        assert_eq!(receipt.engine_ts, 120);
        let reopened = LiveEventPipeline::open(&root, "binance-main", "USDT").unwrap();
        assert_eq!(reopened.log().events().len(), pipeline.log().events().len());
    }

    #[cfg(feature = "sqlite")]
    #[test]
    fn concurrent_stale_workers_cancel_one_order_with_one_venue_side_effect() {
        use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
        use std::sync::Barrier;
        use std::time::Duration;

        struct CountingCancelVenue {
            calls: Arc<AtomicUsize>,
        }

        impl qx_execution::VenuePort for CountingCancelVenue {
            fn venue_id(&self) -> &str {
                "BINANCE"
            }

            fn capabilities(&self) -> qx_execution::VenueCapabilities {
                qx_execution::PAPER_VENUE_CAPABILITIES
            }

            fn execution_readiness(&self) -> Result<(), String> {
                Ok(())
            }

            fn submit_order(
                &mut self,
                _order: Order,
                _ts: u64,
            ) -> Result<Vec<qx_execution::ExecutionEvent>, String> {
                panic!("concurrent cancel fixture must not submit")
            }

            fn cancel_order(
                &mut self,
                client_order_id: u64,
                _ts: u64,
            ) -> Result<Vec<qx_execution::ExecutionEvent>, String> {
                self.calls.fetch_add(1, AtomicOrdering::SeqCst);
                // Keep the winning Venue call open while the competing worker
                // refreshes and tries to commit its own CancelPending fact.
                std::thread::sleep(Duration::from_millis(80));
                Ok(vec![qx_execution::ExecutionEvent::Cancelled {
                    client_order_id,
                }])
            }
        }

        let root = temp_root("concurrent-cancel-one-side-effect");
        std::fs::create_dir_all(&root).unwrap();
        let database = root.join("eventlog.sqlite");
        {
            let mut setup =
                LiveEventPipeline::open_sqlite(&database, "binance-main", "USDT").unwrap();
            setup.register_order(order(), 100).unwrap();
            setup
                .ingest(RuntimeEventEnvelope::venue(
                    RuntimeExternalEvent::Accepted {
                        client_order_id: 7,
                        venue_order_id: Some("remote-7".into()),
                    },
                    110,
                    110,
                    1,
                    "ack-7",
                ))
                .unwrap();
        }

        // Both workers retain an Accepted snapshot while they race to cancel.
        let left = LiveEventPipeline::open_sqlite(&database, "binance-main", "USDT").unwrap();
        let right = LiveEventPipeline::open_sqlite(&database, "binance-main", "USDT").unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(Barrier::new(2));
        let (left_result, right_result) = std::thread::scope(|scope| {
            let left_barrier = barrier.clone();
            let left_calls = calls.clone();
            let left_thread = scope.spawn(move || {
                let mut pipeline = left;
                let mut venue = CountingCancelVenue { calls: left_calls };
                let mut source_seq = 1;
                left_barrier.wait();
                qx_execution::PortExecutionService::new(
                    &mut venue,
                    &mut pipeline,
                    "cancel-worker-left",
                    120,
                    &mut source_seq,
                )
                .cancel(7)
            });

            let right_calls = calls.clone();
            let right_thread = scope.spawn(move || {
                let mut pipeline = right;
                let mut venue = CountingCancelVenue { calls: right_calls };
                let mut source_seq = 1;
                barrier.wait();
                qx_execution::PortExecutionService::new(
                    &mut venue,
                    &mut pipeline,
                    "cancel-worker-right",
                    121,
                    &mut source_seq,
                )
                .cancel(7)
            });
            (left_thread.join().unwrap(), right_thread.join().unwrap())
        });

        assert_eq!(calls.load(AtomicOrdering::SeqCst), 1);
        assert_eq!(
            left_result.is_ok() as usize + right_result.is_ok() as usize,
            1
        );
        assert!(left_result.is_err() || right_result.is_err());

        let recovered = LiveEventPipeline::open_sqlite(&database, "binance-main", "USDT").unwrap();
        assert_eq!(recovered.orders()[0].status, OrderStatus::Cancelled);
        assert_eq!(
            recovered
                .log()
                .events()
                .iter()
                .filter(|event| matches!(
                    event.kind,
                    EventKind::CancelPending { client_order_id: 7 }
                ))
                .count(),
            1,
        );
        assert_eq!(
            recovered
                .log()
                .events()
                .iter()
                .filter(|event| matches!(event.kind, EventKind::Cancelled { client_order_id: 7 }))
                .count(),
            1,
        );
        drop(recovered);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn cancel_pending_is_durable_and_replayable_across_process_restart() {
        let root = writable_eventlog_root("cancel-pending-recovery");
        let mut pipeline = LiveEventPipeline::open(&root, "binance-main", "USDT").unwrap();
        pipeline.register_order(order(), 100).unwrap();
        pipeline
            .ingest(RuntimeEventEnvelope::venue(
                RuntimeExternalEvent::Accepted {
                    client_order_id: 7,
                    venue_order_id: Some("remote-7".into()),
                },
                110,
                110,
                1,
                "ack-7",
            ))
            .unwrap();
        let mut stale_worker = LiveEventPipeline::open(&root, "binance-main", "USDT").unwrap();
        pipeline
            .ingest(RuntimeEventEnvelope::order_cancel_pending(
                7,
                120,
                120,
                2,
                "cancel-request-7",
            ))
            .unwrap();
        let duplicate = qx_execution::EventAppender::append_execution_event_with_outcome(
            &mut stale_worker,
            qx_execution::ExecutionEventEnvelope {
                event: qx_execution::ExecutionEvent::CancelPending { client_order_id: 7 },
                event_ts: 121,
                receive_ts: 121,
                source_seq: 3,
                correlation_id: "cancel-request-7".into(),
            },
        )
        .unwrap();
        assert_eq!(duplicate, qx_execution::EventAppendOutcome::Duplicate);
        let competing_operation = qx_execution::EventAppender::append_execution_event_with_outcome(
            &mut stale_worker,
            qx_execution::ExecutionEventEnvelope {
                event: qx_execution::ExecutionEvent::CancelPending { client_order_id: 7 },
                event_ts: 122,
                receive_ts: 122,
                source_seq: 4,
                correlation_id: "different-cancel-command-7".into(),
            },
        );
        assert!(competing_operation.is_err());
        assert_eq!(stale_worker.orders()[0].status, OrderStatus::CancelPending);
        assert_eq!(stale_worker.log().len(), pipeline.log().len());
        assert_eq!(pipeline.orders()[0].status, OrderStatus::CancelPending);
        assert!(pipeline
            .log()
            .events()
            .iter()
            .any(|event| matches!(event.kind, EventKind::CancelPending { client_order_id: 7 })));

        let mut recovered = LiveEventPipeline::open(&root, "binance-main", "USDT").unwrap();
        assert_eq!(recovered.orders()[0].status, OrderStatus::CancelPending);
        let pending_log_len = recovered.log().len();
        assert!(recovered
            .ingest(RuntimeEventEnvelope::order_cancel_pending(
                7,
                125,
                125,
                3,
                "different-cancel-operation-7",
            ))
            .is_err());
        assert_eq!(recovered.log().len(), pending_log_len);
        recovered
            .ingest(RuntimeEventEnvelope::venue(
                RuntimeExternalEvent::ReconcileRequired { client_order_id: 7 },
                130,
                130,
                4,
                "cancel-reconcile-7",
            ))
            .unwrap();
        assert_eq!(recovered.orders()[0].status, OrderStatus::Unknown);
        let log_len = recovered.log().len();
        assert!(recovered
            .ingest(RuntimeEventEnvelope::order_cancel_pending(
                7,
                135,
                135,
                5,
                "cancel-request-unknown-7",
            ))
            .is_err());
        assert_eq!(recovered.log().len(), log_len);
        recovered
            .ingest(RuntimeEventEnvelope::venue(
                RuntimeExternalEvent::Cancelled { client_order_id: 7 },
                140,
                140,
                6,
                "cancel-confirmed-7",
            ))
            .unwrap();
        assert_eq!(recovered.orders()[0].status, OrderStatus::Cancelled);
        let final_recovery = LiveEventPipeline::open(&root, "binance-main", "USDT").unwrap();
        assert_eq!(final_recovery.orders()[0].status, OrderStatus::Cancelled);
    }

    #[test]
    fn partial_fill_during_cancel_pending_replays_before_cancel_confirmation() {
        let root = writable_eventlog_root("cancel-pending-partial-fill");
        let mut pipeline = LiveEventPipeline::open(&root, "binance-main", "USDT").unwrap();
        pipeline.register_order(order(), 100).unwrap();
        pipeline
            .ingest(RuntimeEventEnvelope::venue(
                RuntimeExternalEvent::Accepted {
                    client_order_id: 7,
                    venue_order_id: Some("remote-7".into()),
                },
                110,
                110,
                1,
                "ack-7",
            ))
            .unwrap();
        pipeline
            .ingest(RuntimeEventEnvelope::order_cancel_pending(
                7,
                120,
                120,
                2,
                "cancel-request-7",
            ))
            .unwrap();

        let fill = Fill {
            order_id: 7,
            qty: Quantity::from_i64(1),
            price: Price::from_i64(100),
            ts: 125,
            account_id: "main".into(),
            venue_id: Some("BINANCE".into()),
            venue_order_id: Some("remote-7".into()),
            ..Fill::default()
        };
        pipeline
            .ingest(RuntimeEventEnvelope::venue(
                RuntimeExternalEvent::Fill { fill: fill.clone() },
                125,
                126,
                3,
                "fill-during-cancel-7",
            ))
            .unwrap();

        assert_eq!(pipeline.orders()[0].status, OrderStatus::CancelPending);
        assert_eq!(pipeline.orders()[0].filled, Quantity::from_i64(1));
        let ledger_entries_after_fill = pipeline.ledger().entries().len();
        assert!(ledger_entries_after_fill > 0);

        let mut recovered = LiveEventPipeline::open(&root, "binance-main", "USDT").unwrap();
        assert_eq!(recovered.orders()[0].status, OrderStatus::CancelPending);
        assert_eq!(recovered.orders()[0].filled, Quantity::from_i64(1));
        assert_eq!(
            recovered.ledger().entries().len(),
            ledger_entries_after_fill
        );

        let duplicate_fill = recovered
            .ingest(RuntimeEventEnvelope::venue(
                RuntimeExternalEvent::Fill { fill: fill.clone() },
                125,
                127,
                4,
                "duplicate-fill-during-cancel-7",
            ))
            .unwrap();
        assert!(duplicate_fill.deduplicated);
        assert_eq!(recovered.orders()[0].status, OrderStatus::CancelPending);
        assert_eq!(recovered.orders()[0].filled, Quantity::from_i64(1));
        assert_eq!(
            recovered.ledger().entries().len(),
            ledger_entries_after_fill
        );

        recovered
            .ingest(RuntimeEventEnvelope::venue(
                RuntimeExternalEvent::Cancelled { client_order_id: 7 },
                130,
                131,
                5,
                "cancel-confirmed-7",
            ))
            .unwrap();
        assert_eq!(recovered.orders()[0].status, OrderStatus::Cancelled);
        assert_eq!(recovered.orders()[0].filled, Quantity::from_i64(1));

        let final_recovery = LiveEventPipeline::open(&root, "binance-main", "USDT").unwrap();
        assert_eq!(final_recovery.snapshot(), recovered.snapshot());
        assert_eq!(final_recovery.orders()[0].status, OrderStatus::Cancelled);
        assert_eq!(final_recovery.orders()[0].filled, Quantity::from_i64(1));
        assert_eq!(
            final_recovery.ledger().entries().len(),
            ledger_entries_after_fill
        );
    }

    struct ExitAfterRemoteCancelVenue {
        remote_order_path: std::path::PathBuf,
    }

    impl qx_execution::VenuePort for ExitAfterRemoteCancelVenue {
        fn venue_id(&self) -> &str {
            "BINANCE"
        }

        fn capabilities(&self) -> qx_execution::VenueCapabilities {
            qx_execution::PAPER_VENUE_CAPABILITIES
        }

        fn execution_readiness(&self) -> Result<(), String> {
            Ok(())
        }

        fn submit_order(
            &mut self,
            _order: Order,
            _ts: u64,
        ) -> Result<Vec<qx_execution::ExecutionEvent>, String> {
            panic!("cancel crash fixture must not submit")
        }

        fn cancel_order(
            &mut self,
            client_order_id: u64,
            _ts: u64,
        ) -> Result<Vec<qx_execution::ExecutionEvent>, String> {
            // Model the exchange committing the cancellation, then losing the
            // response when the worker exits before it can append the Venue fact.
            std::fs::write(
                &self.remote_order_path,
                format!("order_id={client_order_id}\nstatus=cancelled\ncancel_calls=1\n"),
            )
            .expect("persist simulated remote cancellation before dropping response");
            std::process::exit(86)
        }
    }

    #[derive(Default)]
    struct CountingCancelVenue {
        cancel_calls: usize,
    }

    impl qx_execution::VenuePort for CountingCancelVenue {
        fn venue_id(&self) -> &str {
            "BINANCE"
        }

        fn capabilities(&self) -> qx_execution::VenueCapabilities {
            qx_execution::PAPER_VENUE_CAPABILITIES
        }

        fn execution_readiness(&self) -> Result<(), String> {
            Ok(())
        }

        fn submit_order(
            &mut self,
            _order: Order,
            _ts: u64,
        ) -> Result<Vec<qx_execution::ExecutionEvent>, String> {
            panic!("cancel recovery fixture must not submit")
        }

        fn cancel_order(
            &mut self,
            client_order_id: u64,
            _ts: u64,
        ) -> Result<Vec<qx_execution::ExecutionEvent>, String> {
            self.cancel_calls += 1;
            Ok(vec![qx_execution::ExecutionEvent::Cancelled {
                client_order_id,
            }])
        }
    }

    #[test]
    fn cancellation_worker_process_exit_after_venue_call_replays_pending_without_retry() {
        const CHILD_ENV: &str = "QX_TEST_CANCEL_PROCESS_EXIT_CHILD";
        const ROOT_ENV: &str = "QX_TEST_CANCEL_PROCESS_EXIT_ROOT";
        const REMOTE_ENV: &str = "QX_TEST_CANCEL_PROCESS_EXIT_REMOTE";
        if std::env::var(CHILD_ENV).as_deref() == Ok("1") {
            let root = std::env::var_os(ROOT_ENV).expect("crash child needs its EventLog root");
            let remote_order_path = std::env::var_os(REMOTE_ENV)
                .map(std::path::PathBuf::from)
                .expect("crash child needs the simulated venue state path");
            let mut pipeline =
                LiveEventPipeline::open(std::path::PathBuf::from(root), "binance-main", "USDT")
                    .unwrap();
            let mut venue = ExitAfterRemoteCancelVenue { remote_order_path };
            let mut source_seq = 1;
            let mut gateway = qx_execution::PortExecutionService::new(
                &mut venue,
                &mut pipeline,
                "crash-child",
                120,
                &mut source_seq,
            );
            let _ = gateway.cancel(7);
            panic!("Venue exit fixture did not terminate the child process");
        }

        let root = writable_eventlog_root("cancel-process-exit");
        let remote_order_path = root.join("simulated-remote-order.txt");
        std::fs::write(
            &remote_order_path,
            "order_id=7\nstatus=open\ncancel_calls=0\n",
        )
        .unwrap();
        let mut pipeline = LiveEventPipeline::open(&root, "binance-main", "USDT").unwrap();
        pipeline.register_order(order(), 100).unwrap();
        pipeline
            .ingest(RuntimeEventEnvelope::venue(
                RuntimeExternalEvent::Accepted {
                    client_order_id: 7,
                    venue_order_id: Some("remote-7".into()),
                },
                110,
                110,
                1,
                "ack-7",
            ))
            .unwrap();
        drop(pipeline);

        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "pipeline::tests::cancellation_worker_process_exit_after_venue_call_replays_pending_without_retry",
            ])
            .env(CHILD_ENV, "1")
            .env(ROOT_ENV, &root)
            .env(REMOTE_ENV, &remote_order_path)
            .output()
            .expect("spawn cancellation crash-injection child");
        assert_eq!(
            output.status.code(),
            Some(86),
            "child must exit from the Venue call after writing CancelPending; stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );

        let remote_state = std::fs::read_to_string(&remote_order_path).unwrap();
        assert!(remote_state.contains("status=cancelled"), "{remote_state}");
        assert!(remote_state.contains("cancel_calls=1"), "{remote_state}");

        let mut recovered = LiveEventPipeline::open(&root, "binance-main", "USDT").unwrap();
        assert_eq!(recovered.orders()[0].status, OrderStatus::CancelPending);
        assert_eq!(
            recovered
                .log()
                .events()
                .iter()
                .filter(|event| matches!(
                    event.kind,
                    EventKind::CancelPending { client_order_id: 7 }
                ))
                .count(),
            1,
        );
        let log_len = recovered.log().len();
        let mut venue = CountingCancelVenue::default();
        let mut source_seq = 2;
        let error = qx_execution::PortExecutionService::new(
            &mut venue,
            &mut recovered,
            "recovery-worker",
            130,
            &mut source_seq,
        )
        .cancel(7)
        .expect_err("recovered CancelPending must require reconciliation");
        assert!(error.contains("必须先对账"), "{error}");
        assert_eq!(venue.cancel_calls, 0);
        assert_eq!(recovered.log().len(), log_len);

        // The provider's post-restart order query observes the side effect that
        // happened before the lost response. Feed that authoritative fact through
        // the normal runtime ingress and prove the final state itself is durable.
        let remote_status = remote_state
            .lines()
            .find_map(|line| line.strip_prefix("status="))
            .expect("remote order query must return a status");
        assert_eq!(remote_status, "cancelled");
        let reconciled_fact = match remote_status {
            "cancelled" => RuntimeExternalEvent::Cancelled { client_order_id: 7 },
            other => panic!("unexpected simulated remote order status: {other}"),
        };
        recovered
            .ingest(RuntimeEventEnvelope::venue(
                reconciled_fact,
                140,
                140,
                3,
                "cancel-reconciled-from-remote-7",
            ))
            .unwrap();
        assert_eq!(recovered.orders()[0].status, OrderStatus::Cancelled);
        let final_recovery = LiveEventPipeline::open(&root, "binance-main", "USDT").unwrap();
        assert_eq!(final_recovery.orders()[0].status, OrderStatus::Cancelled);
        assert_eq!(
            std::fs::read_to_string(&remote_order_path).unwrap(),
            "order_id=7\nstatus=cancelled\ncancel_calls=1\n"
        );
        let _ = std::fs::remove_dir_all(root);
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
                        net_cash_balance: None,
                        exact_amounts: None,
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
    fn market_event_preserves_sub_nano_bid_ask_through_recovery() {
        let root = temp_root("sub-nano-market-quote");
        let mut pipeline = LiveEventPipeline::open(&root, "ccxt-market", "USDT").unwrap();
        let instrument = InstrumentId::new("TINYUSDT", VenueId::new("OKX"));
        pipeline
            .ingest(RuntimeEventEnvelope::market_quote(
                instrument.clone(),
                QuoteTick::new(
                    100,
                    Price::ZERO,
                    Quantity::ZERO,
                    Price::ZERO,
                    Quantity::ZERO,
                    1,
                )
                .with_exact_prices(
                    ExactDecimal::parse("0.0000000007").unwrap(),
                    ExactDecimal::parse("0.0000000008").unwrap(),
                )
                .with_exact_sizes(
                    ExactDecimal::parse("0.0000000001").unwrap(),
                    ExactDecimal::parse("0.0000000002").unwrap(),
                ),
                101,
                1,
                "okx:tiny-quote",
            ))
            .expect("exact positive quote may have a zero 1e9 projection");

        let quote = pipeline
            .log()
            .events()
            .iter()
            .find_map(|event| match &event.kind {
                EventKind::MarketQuote {
                    bid_exact,
                    ask_exact,
                    bid_qty_exact,
                    ask_qty_exact,
                    ..
                } => Some((*bid_exact, *ask_exact, *bid_qty_exact, *ask_qty_exact)),
                _ => None,
            })
            .expect("market quote fact is persisted");
        assert_eq!(quote.0, Some(ExactDecimal::parse("0.0000000007").unwrap()));
        assert_eq!(quote.1, Some(ExactDecimal::parse("0.0000000008").unwrap()));
        assert_eq!(quote.2, Some(ExactDecimal::parse("0.0000000001").unwrap()));
        assert_eq!(quote.3, Some(ExactDecimal::parse("0.0000000002").unwrap()));

        let recovered = LiveEventPipeline::open(&root, "ccxt-market", "USDT").unwrap();
        let recovered_quote = recovered.latest_quote_with_depth(&instrument).unwrap();
        assert_eq!(
            recovered_quote.bid_exact,
            Some(ExactDecimal::parse("0.0000000007").unwrap())
        );
        assert_eq!(
            recovered_quote.ask_exact,
            Some(ExactDecimal::parse("0.0000000008").unwrap())
        );
        assert_eq!(
            recovered_quote.bid_qty_exact,
            Some(ExactDecimal::parse("0.0000000001").unwrap())
        );
        assert_eq!(
            recovered_quote.ask_qty_exact,
            Some(ExactDecimal::parse("0.0000000002").unwrap())
        );
    }

    #[test]
    fn exact_ccxt_fee_baselines_and_trade_ids_survive_pipeline_recovery() {
        let root = temp_root("exact-fee-recovery");
        let mut pipeline = LiveEventPipeline::open(&root, "ccxt-main", "USDT").unwrap();
        let instrument = InstrumentId::parse("PEPE/USDT.BINANCE").unwrap();
        let mut exact_order = order();
        exact_order.instrument = instrument.clone();
        exact_order.qty = Quantity::ZERO;
        exact_order.limit = None;
        exact_order.exact_amounts = Some(ExactOrderAmounts {
            quantity: ExactDecimal::parse("0.0000000002").unwrap(),
            limit_price: Some(ExactDecimal::parse("0.00000001").unwrap()),
            filled: ExactDecimal::ZERO,
            market_reference_price: None,
            quote_budget: None,
            quote_filled: Default::default(),
        });
        pipeline.register_order(exact_order, 100).unwrap();
        pipeline
            .ingest(RuntimeEventEnvelope::venue(
                RuntimeExternalEvent::Accepted {
                    client_order_id: 7,
                    venue_order_id: Some("remote-exact-7".into()),
                },
                105,
                106,
                1,
                "exact-accepted-7",
            ))
            .unwrap();

        let mut spec = TradingInstrumentSpec::spot_default(&instrument, "USDT");
        spec.base_currency = "PEPE".into();
        spec.exact_rules = Some(ExactTradingRules {
            contract_size: ExactDecimal::parse("1").unwrap(),
            price_tick: ExactDecimal::parse("0.00000001").unwrap(),
            quantity_step: ExactDecimal::parse("0.0000000001").unwrap(),
            minimum_quantity: ExactDecimal::parse("0.0000000001").unwrap(),
            minimum_notional: ExactDecimal::ZERO,
        });

        let fill = Fill {
            order_id: 7,
            ts: 110,
            account_id: "main".into(),
            exact_amounts: Some(ExactFillAmounts {
                quantity: ExactDecimal::parse("0.0000000001").unwrap(),
                price: ExactDecimal::parse("0.00000001").unwrap(),
                quote_cost: None,
                quote_budget_complete: false,
            }),
            exact_fees: vec![ExactFillFee {
                amount: ExactDecimal::parse("0.000000000000000001").unwrap(),
                currency: Some("PEPE".into()),
                source_trade_ids: vec!["trade-pepe-1".into()],
            }],
            ..Fill::default()
        };
        pipeline
            .ingest(RuntimeEventEnvelope::venue(
                RuntimeExternalEvent::FillWithSpec {
                    fill: Box::new(fill),
                    spec: Box::new(spec.clone()),
                },
                110,
                111,
                2,
                "exact-fill-pepe-1",
            ))
            .unwrap();

        let second_fill = Fill {
            order_id: 7,
            ts: 110,
            account_id: "main".into(),
            exact_amounts: Some(ExactFillAmounts {
                quantity: ExactDecimal::parse("0.0000000001").unwrap(),
                price: ExactDecimal::parse("0.00000001").unwrap(),
                quote_cost: None,
                quote_budget_complete: false,
            }),
            exact_fees: vec![ExactFillFee {
                amount: ExactDecimal::parse("0.000000000000000001").unwrap(),
                currency: Some("PEPE".into()),
                source_trade_ids: vec!["trade-pepe-2".into()],
            }],
            ..Fill::default()
        };
        pipeline
            .ingest(RuntimeEventEnvelope::venue(
                RuntimeExternalEvent::FillWithSpec {
                    fill: Box::new(second_fill),
                    spec: Box::new(spec.clone()),
                },
                110,
                112,
                3,
                "exact-fill-pepe-2",
            ))
            .unwrap();

        let exact_baseline = pipeline.exact_fee_baseline_for_order(7).unwrap();
        assert_eq!(
            exact_baseline.get("PEPE"),
            Some(&ExactDecimal::parse("0.000000000000000002").unwrap())
        );
        assert!(pipeline.fee_baseline_for_order(7).unwrap().is_empty());
        assert_eq!(
            pipeline.fee_trade_ids_for_order(7).unwrap(),
            ["trade-pepe-1".to_string(), "trade-pepe-2".to_string()]
                .into_iter()
                .collect()
        );

        let restored = LiveEventPipeline::open(&root, "ccxt-main", "USDT").unwrap();
        assert_eq!(
            restored.exact_fee_baseline_for_order(7).unwrap(),
            exact_baseline
        );
        assert!(restored.fee_baseline_for_order(7).unwrap().is_empty());
        assert_eq!(
            restored.fee_trade_ids_for_order(7).unwrap(),
            ["trade-pepe-1".to_string(), "trade-pepe-2".to_string()]
                .into_iter()
                .collect()
        );
        assert_eq!(
            restored.orders()[0].filled_decimal(),
            ExactDecimal::parse("0.0000000002").unwrap()
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// SQLite 事务 EventLog 的单机持久化门禁：事实、Ledger 与去重语义必须
    /// 与文件后端一致，并能在进程重启（重新打开数据库）后完整回放。
    #[cfg(feature = "sqlite")]
    #[test]
    fn sqlite_pipeline_persists_replays_and_dedups_across_reopen() {
        let root = temp_root("sqlite");
        std::fs::create_dir_all(&root).unwrap();
        let db = root.join("qx-runtime.sqlite");
        let mut pipeline = LiveEventPipeline::open_sqlite(&db, "binance-main", "USDT").unwrap();
        pipeline.register_order(order(), 100).unwrap();
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
        pipeline
            .ingest(RuntimeEventEnvelope::venue(
                RuntimeExternalEvent::Fill { fill: fill.clone() },
                fill.ts,
                121,
                2,
                "sqlite-fill-1",
            ))
            .unwrap();
        assert_eq!(pipeline.orders()[0].status, OrderStatus::Filled);
        assert!(pipeline.ledger().entries().len() >= 2);
        let duplicate = pipeline
            .ingest(RuntimeEventEnvelope::venue(
                RuntimeExternalEvent::Fill { fill: fill.clone() },
                115,
                122,
                3,
                "sqlite-fill-duplicate",
            ))
            .unwrap();
        assert!(duplicate.deduplicated);

        drop(pipeline);
        let mut restored = LiveEventPipeline::open_sqlite(&db, "binance-main", "USDT").unwrap();
        assert_eq!(restored.orders()[0].status, OrderStatus::Filled);
        assert_eq!(restored.ledger().entries().len(), 3);
        let replayed_duplicate = restored
            .ingest(RuntimeEventEnvelope::venue(
                RuntimeExternalEvent::Fill { fill },
                115,
                123,
                4,
                "sqlite-fill-duplicate-again",
            ))
            .unwrap();
        assert!(replayed_duplicate.deduplicated);
        let _ = std::fs::remove_dir_all(root);
    }

    /// P1-2：第二个写者往共享 SQLite EventLog 追加的事实，必须被 `refresh()` 以**游标增量**
    /// 接进来（只读尾部 + 只追加尾部），而不是每次把整条日志读回来整份重放重建。
    ///
    /// 用两个 `LiveEventPipeline` 实例模拟两个进程共享一份日志。关键是：增量路径与整份重建路径的
    /// **终态完全一致**，纯行为断言抓不到退化；退化信号落在 `tail_appends`/`rebuilds` 两个计数器上。
    #[cfg(feature = "sqlite")]
    #[test]
    fn sqlite_pipeline_refresh_takes_the_cursor_incremental_path() {
        let root = temp_root("sqlite-cursor");
        std::fs::create_dir_all(&root).unwrap();
        let db = root.join("qx-runtime.sqlite");
        let mut writer = LiveEventPipeline::open_sqlite(&db, "binance-main", "USDT").unwrap();
        writer.register_order(order(), 100).unwrap();
        // 读者在写者已落一条事实之后才打开：open 走的是整份载入（不经过 refresh_latest），
        // 所以两个计数器都从 0 起。
        let mut reader = LiveEventPipeline::open_sqlite(&db, "binance-main", "USDT").unwrap();
        assert_eq!(reader.orders().len(), 1);
        assert_eq!(reader.metrics.tail_appends.load(Ordering::Relaxed), 0);
        assert_eq!(reader.metrics.rebuilds.load(Ordering::Relaxed), 0);

        // 写者再追加接受与成交两条事实；读者 refresh 必须只接尾部。
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
        writer
            .ingest(RuntimeEventEnvelope::venue(
                RuntimeExternalEvent::Accepted {
                    client_order_id: 7,
                    venue_order_id: None,
                },
                110,
                111,
                1,
                "cursor-ack-7",
            ))
            .unwrap();
        writer
            .ingest(RuntimeEventEnvelope::venue(
                RuntimeExternalEvent::Fill { fill },
                115,
                121,
                2,
                "cursor-fill-1",
            ))
            .unwrap();

        reader.refresh().unwrap();
        assert_eq!(reader.orders()[0].status, OrderStatus::Filled);
        assert_eq!(reader.metrics.tail_appends.load(Ordering::Relaxed), 1);
        assert_eq!(reader.metrics.rebuilds.load(Ordering::Relaxed), 0);

        // 没有新事实时 refresh 是 no-op：既不重建也不追加。
        reader.refresh().unwrap();
        assert_eq!(reader.metrics.tail_appends.load(Ordering::Relaxed), 1);
        assert_eq!(reader.metrics.rebuilds.load(Ordering::Relaxed), 0);
        let _ = std::fs::remove_dir_all(root);
    }

    /// Outbox 必须只向前投影：已 ack 的事实不能被后续写入复活，否则 relay/consumer
    /// 会对同一批事实无限重复投递。重新 open 时仍整段补投影一次，用于填崩溃缺口，
    /// 所以 Outbox 是至少一次投递，消费端按 event_id 幂等。
    #[test]
    fn outbox_projection_does_not_resurrect_acked_events() {
        let root = temp_root("outbox-watermark");
        let mut pipeline = LiveEventPipeline::open(&root, "binance-main", "USDT").unwrap();
        let outbox = FileOutboxStore::new(&root);
        pipeline.register_order(order(), 100).unwrap();
        let acked_ids: Vec<String> = outbox
            .available(1_000, usize::MAX)
            .unwrap()
            .iter()
            .map(|event| {
                let lease = outbox
                    .claim(&event.event_id, "relay", 1_000, 30)
                    .expect("claim outbox event");
                outbox
                    .ack(&event.event_id, "relay", lease.fencing_token, 1_000)
                    .expect("ack outbox event");
                event.event_id.clone()
            })
            .collect();
        assert_eq!(acked_ids.len(), pipeline.log().len());
        assert!(outbox.available(1_000, usize::MAX).unwrap().is_empty());

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
        pipeline
            .ingest(RuntimeEventEnvelope::venue(
                RuntimeExternalEvent::MarketQuote {
                    instrument: InstrumentId::new("BTCUSDT", VenueId::new("BINANCE")),
                    bid: Price::from_i64(99),
                    ask: Price::from_i64(100),
                    bid_qty: Quantity::from_i64(2),
                    ask_qty: Quantity::from_i64(3),
                    bid_qty_exact: None,
                    ask_qty_exact: None,
                    bid_exact: None,
                    ask_exact: None,
                },
                120,
                120,
                2,
                "quote-99",
            ))
            .unwrap();
        let pending: Vec<String> = {
            let mut events = outbox.available(1_000, usize::MAX).unwrap();
            events.sort_by_key(|event| event.sequence);
            events.into_iter().map(|event| event.event_id).collect()
        };
        let expected: Vec<String> = pipeline
            .log()
            .events()
            .iter()
            .map(|event| format!("binance-main:{}", event.seq))
            .filter(|id| !acked_ids.contains(id))
            .collect();
        assert_eq!(pending, expected);
        assert_eq!(pending.len(), 2);
        assert_eq!(pipeline.log().len(), 3);

        let restored = LiveEventPipeline::open(&root, "binance-main", "USDT").unwrap();
        assert_eq!(restored.snapshot(), pipeline.snapshot());
        let replayed = outbox.available(1_000, usize::MAX).unwrap();
        assert_eq!(replayed.len(), pipeline.log().len());
        let acked_came_back = acked_ids
            .iter()
            .all(|id| replayed.iter().any(|event| &event.event_id == id));
        assert!(acked_came_back);
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
                bid_qty_exact: None,
                ask_qty_exact: None,
                bid_exact: None,
                ask_exact: None,
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
        assert_eq!(event.receive_time, 110);

        let duplicate = RuntimeEventEnvelope::venue(
            RuntimeExternalEvent::MarketQuote {
                instrument,
                bid: Price::from_i64(100),
                ask: Price::from_i64(101),
                bid_qty: Quantity::from_i64(2),
                ask_qty: Quantity::from_i64(3),
                bid_qty_exact: None,
                ask_qty_exact: None,
                bid_exact: None,
                ask_exact: None,
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
    fn market_order_book_is_validated_persisted_and_replayed_with_exact_precision() {
        let root = writable_eventlog_root("market-order-book");
        let instrument = InstrumentId::new("ALTUSDT", VenueId::new("BINANCE"));
        let level = |price: &str, quantity: &str| MarketDepthLevel {
            price: Price::from_raw(0),
            quantity: Quantity::from_raw(0),
            price_exact: Some(ExactDecimal::parse(price).unwrap()),
            quantity_exact: Some(ExactDecimal::parse(quantity).unwrap()),
        };
        let bids = vec![level("0.0000000007", "0.0000000002")];
        let asks = vec![level("0.0000000009", "0.0000000004")];
        let mut pipeline = LiveEventPipeline::open(&root, "binance-depth", "USDT").unwrap();
        pipeline
            .ingest(RuntimeEventEnvelope::market_order_book(
                instrument.clone(),
                88,
                bids.clone(),
                asks.clone(),
                100,
                110,
                1,
                "binance:orderbook:88",
            ))
            .unwrap();
        let event = pipeline.log().events().last().unwrap();
        let EventKind::MarketOrderBook {
            sequence,
            bids: saved_bids,
            asks: saved_asks,
            ..
        } = &event.kind
        else {
            panic!("expected durable MarketOrderBook event")
        };
        assert_eq!(*sequence, 88);
        assert_eq!(saved_bids, &bids);
        assert_eq!(saved_asks, &asks);
        assert_eq!(saved_bids[0].price.raw(), 0);
        assert_eq!(
            saved_bids[0].price_exact.unwrap().to_string(),
            "0.0000000007"
        );

        let error = pipeline
            .ingest(RuntimeEventEnvelope::market_order_book(
                instrument,
                89,
                bids,
                vec![level("0.0000000006", "0.0000000004")],
                101,
                111,
                2,
                "binance:orderbook:89",
            ))
            .unwrap_err();
        assert!(format!("{error:?}").contains("盘口最优买价高于最优卖价"));
        assert_eq!(pipeline.log().len(), 1);

        let restored = LiveEventPipeline::open(&root, "binance-depth", "USDT").unwrap();
        assert_eq!(restored.snapshot(), pipeline.snapshot());
        drop(restored);
        drop(pipeline);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn segmented_pipeline_reopens_with_identical_state() {
        let root = temp_root("segmented-recover");
        let mut pipeline =
            LiveEventPipeline::open_configured(&root, "paper-segmented", "USDT", Some(2)).unwrap();
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
            LiveEventPipeline::open_configured(&root, "paper-segmented", "USDT", Some(2)).unwrap();
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
            net_cash_balance: None,
            exact_amounts: None,
        }];
        let discrepancies = pipeline
            .settlement_balance_discrepancies("main", "BINANCE", &balances)
            .unwrap();
        assert_eq!(discrepancies.len(), 1);
        assert_eq!(discrepancies[0].ledger_raw, 0);
        assert_eq!(discrepancies[0].venue_raw, Some(Money::from_i64(15).raw()));
        let net_debt_discrepancy = pipeline
            .settlement_balance_discrepancies(
                "main",
                "BINANCE",
                &[AccountBalance {
                    asset: "USDT".into(),
                    free: Money::from_i64(12),
                    locked: Money::from_i64(3),
                    borrowed: Money::from_i64(5),
                    net_cash_balance: None,
                    exact_amounts: None,
                }],
            )
            .unwrap();
        assert_eq!(
            net_debt_discrepancy[0].venue_raw,
            Some(Money::from_i64(10).raw())
        );
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
                    net_cash_balance: None,
                    exact_amounts: None,
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
                    positions: vec![
                        qx_core::AccountPositionSnapshot {
                            instrument: instrument.clone(),
                            settlement_currency: Some("USDT".into()),
                            quantity: Quantity::from_i64(2),
                            average_price: Some(Price::from_i64(100)),
                            mark_price: Some(Price::from_i64(101)),
                            liquidation_price: Some(Price::from_i64(50)),
                            unrealized_pnl: Some(Money::from_i64(2)),
                            initial_margin: Some(Money::from_i64(20)),
                            maintenance_margin: Some(Money::from_i64(5)),
                            leverage: Some(10),
                            margin_mode: Some("isolated".into()),
                            position_side: Some("long".into()),
                            exact_amounts: None,
                        },
                        // 第二行是"交易所报了零"和"交易所没报"同时出现在一份快照里：
                        // 落盘与回读必须把这两种状态分开保存（V11 Q68）。
                        qx_core::AccountPositionSnapshot {
                            instrument: InstrumentId::parse("ETH/USDT:USDT.OKX").unwrap(),
                            settlement_currency: Some("USDT".into()),
                            quantity: Quantity::from_i64(3),
                            average_price: None,
                            mark_price: None,
                            liquidation_price: None,
                            unrealized_pnl: Some(Money::ZERO),
                            initial_margin: None,
                            maintenance_margin: None,
                            leverage: None,
                            margin_mode: None,
                            position_side: None,
                            exact_amounts: None,
                        },
                    ],
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
        let reopened = restored.snapshot();
        let rows = reopened
            .account_positions
            .get(&("main".to_string(), "OKX".to_string()))
            .expect("重开日志后必须仍有一组持仓事实");
        assert_eq!(rows.len(), 2, "两份持仓观察都要活过落盘与回读");
        let reported_zero = rows
            .iter()
            .find(|row| row.instrument.to_string().starts_with("ETH"))
            .unwrap();
        assert_eq!(
            (
                reported_zero.unrealized_pnl,
                reported_zero.initial_margin,
                reported_zero.maintenance_margin
            ),
            (Some(Money::ZERO), None, None),
            "「报了零」与「没报」在回读后仍是两种状态，不能一起收敛成 0"
        );
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
