//! # qx-core — 牵星内核
//!
//! 确定性内核：时钟、因果事件队列、身份、定点数值、订单状态机、事件溯源与重放校验。
//!
//! 设计底线（改动前请先读）：
//! 1. **热路径不用浮点**：所有金额/价格/数量走 128-bit 定点 [`Fixed`]。
//! 2. **不用系统时间**：回测只认 [`TestClock`]，实盘才接入真实时钟。
//! 3. **不用无序容器做顺序敏感迭代**：`HashMap` 只用于查找，遍历一律排序。
//! 4. **事件即事实**：状态由事件日志重建，任何旁路写入都是 bug。

pub mod clock;
pub mod engine;
pub mod error;
pub mod event;
pub mod fee;
pub mod fenye;
pub mod fill_apply;
pub mod identity;
pub mod ledger;
pub mod numeric;
pub mod order;
pub mod queue;
pub mod retry;
pub mod sourcing;
pub mod target;
pub mod trading;

pub use self::clock::{ClockError, TestClock, Ts};
pub use self::engine::{Engine, EngineCtx, EngineRunOptions, EngineRunReport, Handler};
pub use self::error::{QxError, QxResult};
pub use self::event::{
    AccountBalance, AccountCashflow, AccountPositionSnapshot, CashflowKind, Event, EventContext,
    EventKind, EventMetadata, FundingRateSnapshot, Priority, EVENT_METADATA_SCHEMA_VERSION,
};
pub use self::fee::{
    bp_amount, notional, AShareFeeModel, FeeModel, MakerTakerFeeModel, ZeroFeeModel,
    DEFAULT_MAKER_BP, DEFAULT_TAKER_BP,
};
pub use self::fill_apply::{apply_fill_to_books, apply_ledger_fill, FillTerms, OrderFillBook};
pub use self::identity::{CanonicalProduct, InstrumentId, MarketId, VenueId};
pub use self::ledger::{
    ConvertibleBondConversion, CorporateAction, Ledger, LedgerEntry, LedgerEntryKind,
    PositionState, RightsIssueEvent, ShareSubscription,
};
pub use self::numeric::{Fixed, Money, Price, Quantity, SCALE};
pub use self::order::{Fill, Order, OrderStatus, OrderTrace, Side};
pub use self::queue::CausalQueue;
pub use self::retry::{Backoff, RetryPolicy};
pub use self::sourcing::{EventLog, Fnv1a, ReplayVerifier, RunManifest};
pub use self::target::TargetPosition;
pub use self::trading::{
    MarginMode, MarginState, OrderPolicy, PositionMode, PositionSide, TradingInstrumentSpec,
    TradingProduct,
};
