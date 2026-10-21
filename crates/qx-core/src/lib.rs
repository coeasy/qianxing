//! # qx-core — 牵星内核
//!
//! 确定性内核：时间戳口径（无虚拟时钟对象）、身份、定点数值、订单状态机、事件溯源与重放校验。
//!
//! 设计底线（改动前请先读）：
//! 1. **热路径不用浮点**：所有金额/价格/数量走 128-bit 定点 [`Fixed`]。
//! 2. **不用系统时间**：回测只认输入数据自带的时间戳，时间轴由 `qx-data` 的排序与
//!    「严格递增」闸门决定（见 [`clock`] 模块说明），内核不提供虚拟时钟对象。
//! 3. **不用无序容器做顺序敏感迭代**：`HashMap` 只用于查找，遍历一律排序。
//! 4. **事件即事实**：状态由事件日志重建，任何旁路写入都是 bug。
//!    因果序（`(ts, prio, seq)` 全序）由 [`EventLog::validate`] 单点裁决；
//!    内核不提供第二套事件循环——回测（`qx-xingban`）与 paper/live（`qx-runtime` 的
//!    `pipeline.rs`）的循环各在自己的 crate 里，但都只能通过 `EventLog` 落地事实。

pub mod benchmark;
pub mod clock;
pub mod contract;
pub mod error;
pub mod event;
pub mod fee;
pub mod file_lock;
pub mod fill_apply;
pub mod identity;
pub mod ledger;
pub mod numeric;
pub mod order;
pub mod performance;
pub mod retry;
pub mod sourcing;
pub mod target;
pub mod trading;
pub mod trading_policy;
pub mod valuation;
pub mod venue;

pub use self::benchmark::{
    benchmark_relative_metrics, benchmark_relative_metrics_from_columns, BenchmarkRelativeMetrics,
};
pub use self::clock::Ts;
pub use self::contract::{ContractConcept, CONTRACT_MATRIX};
pub use self::error::{
    classify_ccxt_category, classify_ccxt_exception, CcxtErrorClassification, ErrorCode,
    ErrorContract, QxError, QxResult, Retryability,
};
pub use self::event::{
    AccountBalance, AccountCashflow, AccountPositionSnapshot, CashflowKind, Event, EventContext,
    EventKind, EventMetadata, FundingRateSnapshot, Priority, EVENT_METADATA_SCHEMA_VERSION,
};
pub use self::fee::{
    bp_amount, bp_amount_checked, notional, AShareFeeModel, FeeModel, MakerTakerFeeModel,
    MakerTakerFeeSchedule, ZeroFeeModel, DEFAULT_MAKER_BP, DEFAULT_TAKER_BP,
};
pub use self::file_lock::{
    decide_lock, lock_age, lock_contention_message, FileLock, LockDecision, LockError, LockPolicy,
    DEFAULT_LOCK_STALE_AFTER,
};
pub use self::fill_apply::{apply_fill_to_books, apply_ledger_fill, FillTerms, OrderFillBook};
pub use self::identity::{
    CanonicalProduct, InstrumentId, MarketId, VenueId, DEFAULT_SETTLEMENT_CURRENCY,
};
pub use self::ledger::{
    ConvertibleBondConversion, CorporateAction, FundingRateEvent, InstrumentFunding, Ledger,
    LedgerEntry, LedgerEntryKind, PositionState, RightsIssueEvent, FUNDING_INTERVAL_MS,
};
pub use self::numeric::{
    parse_decimal_exact, parse_decimal_half_even, parse_decimal_integer_exact,
    parse_decimal_scaled_integer, Fixed, Money, Price, Quantity, SCALE,
};
pub use self::order::{Fill, Order, OrderStatus, OrderTrace, Side};
pub use self::performance::{backtest_risk_metrics, BacktestRiskMetrics};
pub use self::retry::{Backoff, RetryPolicy};
pub use self::sourcing::{EventLog, Fnv1a, ReplayFacts, ReplayVerifier, RunManifest};
pub use self::target::TargetPosition;
pub use self::trading::{
    resolve_settlement_currency, MarginMode, MarginState, OrderPolicy, PositionMode, PositionSide,
    TradingInstrumentSpec, TradingProduct,
};
pub use self::trading_policy::{margin_mode_for_product, short_selling_enabled};
pub use self::valuation::{PaperValuationContext, ValuationContext, ValuationResult};
pub use self::venue::VenueFamily;
