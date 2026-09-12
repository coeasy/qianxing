//! # qx-xingban — 星板
//!
//! 撮合与仿真。**撮合模型首先要回答"我不知道什么"**，而不是追求更复杂的滑点公式。
//!
//! 核心设计：**规则包 × 数据档位** 两维插件。
//! - 数据档位决定可见深度（L2/L3、L1、Bar）
//! - 规则包描述订单簿与业务约束
//! - 两者匹配才启用精细撮合，否则**自动降级为保守假设并写入审计**

pub mod backtest;
pub mod cost;
pub mod fill;
pub mod orderbook;
pub mod orderbook_backtest;
pub mod rng;
pub mod tick_backtest;
pub mod venue;

pub use self::backtest::{
    BacktestConfig, BacktestEngine, BacktestReport, BarStrategy, DeliveryEvent, FundingEvent,
    InterestEvent, NativeBarStrategy, VirtualTradingConfig,
};
pub use self::cost::{
    FeeContext, FeeModel, FixedRateMargin, LatencyModel, LeverageMargin, LiquidityRole,
    MakerTakerFeeModel, MarginRule, MarginTier, NoMargin, StaticLatency, TieredMargin,
    ZeroFeeModel, ZeroLatency,
};
pub use self::fill::{
    BestPriceFillModel, DataTier, FillContext, FillModel, NextBarOpenFillModel,
    OneTickSlippageFillModel, ProbabilisticFillModel, VolumeSensitiveFillModel,
};
pub use self::orderbook::{
    BookLevel, OrderBookExecutionModel, OrderBookMatchingEngine, OrderBookSnapshot,
};
pub use self::orderbook_backtest::{
    NativeOrderBookStrategy, OrderBookBacktestConfig, OrderBookBacktestEngine,
    OrderBookBacktestReport, OrderBookStrategy,
};
pub use self::rng::DeterministicRng;
pub use self::tick_backtest::{
    NativeTickStrategy, TickBacktestConfig, TickBacktestEngine, TickBacktestReport, TickStrategy,
};
pub use self::venue::{BarMatchingEngine, MatchingConfig};
