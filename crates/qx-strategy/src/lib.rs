//! 牵星统一策略 SDK：Rust/C ABI/Python 策略共享同一不可变输入与 `StrategyDecision` 契约，执行权仍归 Rust Risk/OMS。

use qx_core::{
    ExactDecimal, ExactOhlcv, ExactOrderAmounts, Fill, InstrumentId, OrderPolicy, Price, Quantity,
    Side,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};
pub mod builtin;
pub mod builtin_signal;
pub mod c_api;
pub mod contract;
mod decision;
pub mod diagnostics;
pub mod file_digest;
pub mod frame;
pub mod ring;
pub mod transport;
pub use builtin::{BuiltinStrategy, BuiltinStrategyConfig, BuiltinStrategyKind};
pub use c_api::{
    sha256_hex, verify_file_sha256, CAbiStrategy, DynamicCAbiLoadPolicy, DynamicCAbiStrategy,
    QxMarketEventV2, QxStrategyAccountUpdateCallbackV1, QxStrategyContextCallbackV2,
    QxStrategyContextV2, QxStrategyDiagnosticState, QxStrategyErrorCallbackV1, QxStrategyExactKvV1,
    QxStrategyExactOhlcvV1, QxStrategyFillUpdateCallbackV2,
    QxStrategyHistoryRequirementsCallbackV1, QxStrategyJsonUpdateCallbackV1,
    QxStrategyMarketEventCallbackV2, QxStrategyOrderPolicyCallbackV1,
    QxStrategyOrderStatusCallbackV1, QxStrategyStartCallbackV1, QxStrategyStopCallbackV1,
    QxStrategyStopCallbackV2, QxStrategyVTable, QxStrategyWarmupCompleteCallbackV1,
    QxStrategyWarmupCompleteV1, QX_C_STRATEGY_ABI_VERSION, QX_C_STRATEGY_API_VERSION,
    QX_MARKET_EVENT_SCHEMA_VERSION_V2, QX_STRATEGY_CONTEXT_SCHEMA_VERSION_V2,
    QX_STRATEGY_WARMUP_COMPLETE_SCHEMA_VERSION_V1,
};
pub use contract::{
    STRATEGY_ACCOUNT_UPDATE_SCHEMA_VERSION, STRATEGY_CHECKPOINT_SCHEMA_VERSION,
    STRATEGY_CONTRACT_INPUT_V2_SCHEMA_VERSION, STRATEGY_CONTRACT_INPUT_V3_SCHEMA_VERSION,
    STRATEGY_CONTRACT_INPUT_V4_SCHEMA_VERSION, STRATEGY_CONTRACT_INPUT_V5_SCHEMA_VERSION,
    STRATEGY_CONTRACT_INPUT_V6_SCHEMA_VERSION, STRATEGY_CONTRACT_INPUT_V7_SCHEMA_VERSION,
    STRATEGY_CONTRACT_INPUT_V8_SCHEMA_VERSION,
};
pub use frame::{
    StrategyFrame, StrategyFrameKind, DEFAULT_MAX_FRAME_BYTES, STRATEGY_FRAME_HEADER_LEN,
    STRATEGY_FRAME_MAGIC, STRATEGY_FRAME_VERSION,
};
pub use ring::{
    SharedRingConfig, SharedRingError, SharedRingReader, SharedRingWriter, DEFAULT_RING_CAPACITY,
    DEFAULT_RING_SLOT_BYTES,
};
pub const STRATEGY_API_VERSION: u32 = 1;
/// JSON wire version for lifecycle facts delivered to strategy update callbacks.
pub const ORDER_UPDATE_SCHEMA_VERSION: u32 = 1;
/// JSON wire version for fill updates bound to their authoritative EventLog sequence.
pub const ORDER_FILL_UPDATE_EVENT_SEQ_SCHEMA_VERSION: u32 = 2;
/// JSON wire version for order-state facts delivered to strategy update callbacks.
pub const ORDER_STATUS_UPDATE_SCHEMA_VERSION: u32 = 1;

/// Declares how much closed-bar history a strategy needs before it can trade.
///
/// This is deliberately a small core contract. Data selection and PIT-safe history
/// loading remain host responsibilities; the strategy cannot fetch arbitrary future
/// data through this declaration.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StrategyHistoryRequirements {
    #[serde(default)]
    pub warmup_bars: u64,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct StrategyContext {
    pub strategy_id: String,
    pub strategy_version: String,
    pub account_id: String,
    pub venue_id: String,
    pub data_fingerprint: String,
    pub as_of: u64,
    pub positions: BTreeMap<String, i128>,
    pub cash: BTreeMap<String, i128>,
    pub available_margin_raw: Option<i128>,
    pub risk_state: String,
    /// Exact quantity snapshots paired with the legacy raw projections above.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub positions_exact: BTreeMap<String, ExactDecimal>,
    /// Exact currency balances paired with the legacy cash projection.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub cash_exact: BTreeMap<String, ExactDecimal>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub available_margin_exact: Option<ExactDecimal>,
}

pub const STRATEGY_STOP_CONTEXT_SCHEMA_VERSION_V1: u32 = 1;
pub const STRATEGY_ERROR_SCHEMA_VERSION_V1: u32 = 1;

/// The strategy callback boundary that produced an observable failure.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StrategyErrorStage {
    Initialization,
    HistoryRequirements,
    WarmupComplete,
    Start,
    MarketEvent,
    OrderUpdate,
    FillUpdate,
    AccountUpdate,
    Stop,
}

/// Bounded, versioned error context shared by native Rust and C ABI strategies.
/// It deliberately omits account balances and positions; hosts retain those facts.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StrategyErrorV1 {
    pub schema_version: u32,
    pub stage: StrategyErrorStage,
    pub strategy_id: String,
    pub strategy_version: String,
    pub timestamp: Option<u64>,
    pub instrument: Option<String>,
    pub message: String,
}

impl StrategyErrorV1 {
    pub fn new(
        stage: StrategyErrorStage,
        context: &StrategyContext,
        timestamp: Option<u64>,
        instrument: Option<InstrumentId>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            schema_version: STRATEGY_ERROR_SCHEMA_VERSION_V1,
            stage,
            strategy_id: context.strategy_id.clone(),
            strategy_version: context.strategy_version.clone(),
            timestamp,
            instrument: instrument.map(|instrument| instrument.to_string()),
            message: message.into(),
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != STRATEGY_ERROR_SCHEMA_VERSION_V1
            || self.strategy_id.trim().is_empty()
            || self.strategy_version.trim().is_empty()
            || self.timestamp.is_some_and(|timestamp| timestamp == 0)
            || self.message.trim().is_empty()
            || self.message.len() > 16 * 1024
            || self
                .instrument
                .as_ref()
                .is_some_and(|instrument| InstrumentId::parse(instrument).is_none())
        {
            return Err("StrategyErrorV1 schema、身份、时间或消息字段非法".into());
        }
        Ok(())
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StrategyStopReason {
    Completed,
    Cancelled,
    Failed,
    Shutdown,
    HostStop,
}

/// Versioned read-only lifecycle payload. The context is optional for hosts that
/// cannot provide a final committed snapshot, while the reason is always explicit.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StrategyStopContextV1 {
    pub schema_version: u32,
    pub reason: StrategyStopReason,
    pub context: Option<StrategyContext>,
}

impl StrategyStopContextV1 {
    pub fn new(reason: StrategyStopReason, context: Option<StrategyContext>) -> Self {
        Self {
            schema_version: STRATEGY_STOP_CONTEXT_SCHEMA_VERSION_V1,
            reason,
            context,
        }
    }

    pub fn host_stop(context: Option<StrategyContext>) -> Self {
        Self::new(StrategyStopReason::HostStop, context)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != STRATEGY_STOP_CONTEXT_SCHEMA_VERSION_V1 {
            return Err(format!(
                "StrategyStopContextV1 schema_version 不支持: {}",
                self.schema_version
            ));
        }
        if let Some(context) = &self.context {
            context.validate()?;
        }
        Ok(())
    }
}

impl StrategyContext {
    pub fn validate(&self) -> Result<(), String> {
        if self.strategy_id.trim().is_empty()
            || self.strategy_version.trim().is_empty()
            || self.account_id.trim().is_empty()
            || self.venue_id.trim().is_empty()
            || self.data_fingerprint.trim().is_empty()
            || self.risk_state.trim().is_empty()
            || self.as_of == 0
            || self.available_margin_raw.is_some_and(|value| value < 0)
        {
            return Err("StrategyContext 身份、时间或风险字段非法".into());
        }
        for instrument in self.positions.keys() {
            InstrumentId::parse(instrument)
                .ok_or_else(|| format!("StrategyContext instrument 非法: {instrument}"))?;
        }
        for (instrument, exact) in &self.positions_exact {
            InstrumentId::parse(instrument)
                .ok_or_else(|| format!("StrategyContext exact instrument 非法: {instrument}"))?;
            let projected = project_exact_to_raw(*exact)?;
            if self.positions.get(instrument).copied().unwrap_or_default() != projected {
                return Err(format!(
                    "StrategyContext positions_exact[{instrument}] 与 raw 投影不一致"
                ));
            }
        }
        for (currency, exact) in &self.cash_exact {
            let raw = self.cash.get(currency).copied().unwrap_or_default();
            let projected = project_exact_to_raw(*exact)?;
            if currency.trim().is_empty() || raw != projected {
                return Err(format!(
                    "StrategyContext cash_exact[{currency}] 与 raw 投影不一致: raw={raw}, exact={exact}, projected={projected}"
                ));
            }
        }
        if let (Some(raw), Some(exact)) = (self.available_margin_raw, self.available_margin_exact) {
            if raw != project_exact_to_raw(exact)? {
                return Err("StrategyContext available_margin_exact 与 raw 投影不一致".into());
            }
        }
        Ok(())
    }
}

fn project_exact_to_raw(value: ExactDecimal) -> Result<i128, String> {
    value
        .checked_mul_at_scale(ExactDecimal::ONE, 9, qx_core::DecimalRounding::TowardZero)
        .and_then(|projected| projected.to_raw(9).ok())
        .ok_or_else(|| "StrategyContext exact 数值无法投影到旧 raw 字段".into())
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum MarketEvent {
    Bar {
        instrument: InstrumentId,
        ts: u64,
        open_raw: i128,
        high_raw: i128,
        low_raw: i128,
        close_raw: i128,
        volume_raw: i128,
        /// Source OHLCV precision, when carried by a version-2 BarFrame.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exact_ohlcv: Option<ExactOhlcv>,
    },
    Tick {
        instrument: InstrumentId,
        ts: u64,
        bid_raw: i128,
        ask_raw: i128,
        last_raw: Option<i128>,
        volume_raw: Option<i128>,
    },
    OrderBook {
        instrument: InstrumentId,
        ts: u64,
        sequence: u64,
        bids: Vec<OrderBookLevel>,
        asks: Vec<OrderBookLevel>,
    },
    Timer {
        name: String,
        ts: u64,
    },
}

impl MarketEvent {
    pub fn ts(&self) -> u64 {
        match self {
            Self::Bar { ts, .. }
            | Self::Tick { ts, .. }
            | Self::OrderBook { ts, .. }
            | Self::Timer { ts, .. } => *ts,
        }
    }

    pub fn instrument(&self) -> Option<&InstrumentId> {
        match self {
            Self::Bar { instrument, .. }
            | Self::Tick { instrument, .. }
            | Self::OrderBook { instrument, .. } => Some(instrument),
            Self::Timer { .. } => None,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.ts() == 0 {
            return Err("MarketEvent ts 必须为正".into());
        }
        match self {
            Self::Bar {
                instrument,
                open_raw,
                high_raw,
                low_raw,
                close_raw,
                volume_raw,
                exact_ohlcv,
                ..
            } => {
                if *high_raw < *low_raw || *volume_raw < 0 || instrument.to_string().is_empty() {
                    return Err("Bar MarketEvent 数值非法".into());
                }
                if *open_raw < *low_raw
                    || *open_raw > *high_raw
                    || *close_raw < *low_raw
                    || *close_raw > *high_raw
                {
                    return Err("Bar OHLC 不满足 high/low 边界".into());
                }
                if let Some(exact) = exact_ohlcv {
                    exact.validate()?;
                    let raw = [*open_raw, *high_raw, *low_raw, *close_raw, *volume_raw];
                    for (raw, precise) in raw.into_iter().zip(exact.raw_projection()?) {
                        if precise != raw {
                            return Err("Bar exact_ohlcv raw projection mismatch".into());
                        }
                    }
                }
            }
            Self::Tick {
                instrument,
                bid_raw,
                ask_raw,
                last_raw,
                volume_raw,
                ..
            } => {
                if bid_raw <= &0
                    || ask_raw <= &0
                    || bid_raw > ask_raw
                    || last_raw.is_some_and(|value| value <= 0)
                    || volume_raw.is_some_and(|value| value < 0)
                    || instrument.to_string().is_empty()
                {
                    return Err("Tick MarketEvent 数值非法".into());
                }
            }
            Self::OrderBook {
                instrument,
                sequence,
                bids,
                asks,
                ..
            } => {
                if *sequence == 0 || instrument.to_string().is_empty() {
                    return Err("OrderBook MarketEvent 身份非法".into());
                }
                for level in bids.iter().chain(asks.iter()) {
                    level.validate()?;
                }
                if bids.windows(2).any(|window| {
                    window[0]
                        .price_decimal()
                        .checked_cmp(window[1].price_decimal())
                        .is_none_or(|ordering| !ordering.is_gt())
                }) || asks.windows(2).any(|window| {
                    window[0]
                        .price_decimal()
                        .checked_cmp(window[1].price_decimal())
                        .is_none_or(|ordering| !ordering.is_lt())
                }) || matches!((bids.first(), asks.first()), (Some(bid), Some(ask)) if bid.price_decimal().checked_cmp(ask.price_decimal()).is_none_or(|ordering| ordering.is_gt()))
                {
                    return Err("OrderBook MarketEvent 价格档位非法".into());
                }
            }
            Self::Timer { name, .. } if name.trim().is_empty() => {
                return Err("Timer MarketEvent name 不能为空".into());
            }
            Self::Timer { .. } => {}
        }
        Ok(())
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct OrderBookLevel {
    pub price_raw: i128,
    pub qty_raw: i128,
    /// Source price from an exact DepthFrame; raw stays its toward-zero 1e-9 projection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price_exact: Option<ExactDecimal>,
    /// Source quantity from an exact DepthFrame; raw stays its toward-zero 1e-9 projection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qty_exact: Option<ExactDecimal>,
}

impl OrderBookLevel {
    pub fn validate(&self) -> Result<(), String> {
        match (self.price_exact, self.qty_exact) {
            (Some(price), Some(qty)) => {
                if !price.is_positive() || !qty.is_positive() {
                    return Err("OrderBookLevel 精确价格和数量必须为正".into());
                }
                if Self::legacy_projection(price)? != self.price_raw
                    || Self::legacy_projection(qty)? != self.qty_raw
                {
                    return Err("OrderBookLevel 精确值与 raw 投影不一致".into());
                }
            }
            (None, None) if self.price_raw > 0 && self.qty_raw > 0 => {}
            (None, None) => return Err("OrderBookLevel 价格和数量必须为正".into()),
            _ => return Err("OrderBookLevel 精确价格和数量必须同时提供".into()),
        }
        Ok(())
    }

    pub fn price_decimal(&self) -> ExactDecimal {
        self.price_exact.unwrap_or_else(|| {
            ExactDecimal::from_parts(self.price_raw, 9).expect("legacy price scale is supported")
        })
    }

    fn legacy_projection(value: ExactDecimal) -> Result<i128, String> {
        value
            .checked_mul_at_scale(ExactDecimal::ONE, 9, qx_core::DecimalRounding::TowardZero)
            .and_then(|projected| projected.to_raw(9).ok())
            .ok_or_else(|| "OrderBookLevel 精确值无法投影到 raw 字段".to_string())
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StrategyOrderIntent {
    pub intent_id: u64,
    pub instrument: InstrumentId,
    pub side: Side,
    pub qty: Quantity,
    /// Optional scale-aware base quantity for live crypto markets.
    #[serde(default)]
    pub qty_exact: Option<ExactDecimal>,
    pub limit: Option<Price>,
    /// Optional scale-aware limit price, paired with `qty_exact`.
    #[serde(default)]
    pub limit_price_exact: Option<ExactDecimal>,
    /// Optional quote-currency budget for Bitget Spot market buys.
    #[serde(default)]
    pub quote_budget: Option<ExactDecimal>,
    pub policy: Option<OrderPolicy>,
    pub reduce_only: bool,
    pub post_only: bool,
}

impl StrategyOrderIntent {
    pub fn validate(&self) -> Result<(), String> {
        if self.intent_id == 0 {
            return Err("StrategyOrderIntent id 非法".into());
        }
        if let Some(budget) = self.quote_budget {
            if self.side != Side::Buy
                || self.qty.raw() != 0
                || self.qty_exact.is_some()
                || self.limit.is_some()
                || self.limit_price_exact.is_some()
                || !budget.is_positive()
            {
                return Err("StrategyOrderIntent 计价币预算必须是正数、只用于市价买入，且不能与数量/限价混用".into());
            }
        } else if let Some(quantity) = self.qty_exact {
            if self.qty.raw() != 0 || !quantity.is_positive() || self.limit.is_some() {
                return Err(
                    "StrategyOrderIntent 精确数量必须为正、qty 旧字段为零，且不能混用旧限价".into(),
                );
            }
        } else if self.qty.raw() <= 0 {
            return Err("StrategyOrderIntent 数量非法".into());
        }
        if let Some(price) = self.limit_price_exact {
            if self.qty_exact.is_none() || !price.is_positive() || self.limit.is_some() {
                return Err("StrategyOrderIntent 精确限价必须与精确数量一起使用".into());
            }
        }
        if self.limit.is_some_and(|price| price.raw() <= 0) {
            return Err("StrategyOrderIntent 限价必须为正".into());
        }
        if self.policy.is_some_and(|policy| policy.leverage == 0) {
            return Err("StrategyOrderIntent 杠杆必须为正".into());
        }
        Ok(())
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StrategyDecision {
    pub schema_version: u32,
    pub request_id: String,
    pub strategy_id: String,
    pub signal_id: u64,
    pub confidence: i128,
    pub priority: i32,
    pub expires_at: u64,
    pub intents: Vec<StrategyOrderIntent>,
}

/// The stable event kind for a versioned strategy execution update.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StrategyUpdateEvent {
    Fill,
}

/// Order facts attached to an execution update. Exact amounts are present only for
/// scale-aware orders, keeping legacy callback payloads unchanged.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StrategyOrderUpdate {
    pub order_id: u64,
    pub account_id: String,
    pub instrument: String,
    pub side: Side,
    pub qty_raw: i128,
    pub filled_raw: i128,
    pub strategy_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exact_amounts: Option<ExactOrderAmounts>,
}

impl StrategyOrderUpdate {
    pub fn from_order(order: &qx_core::Order, strategy_id: &str) -> Result<Self, String> {
        order.validate()?;
        let update = Self::from_order_snapshot(order, strategy_id);
        update.validate()?;
        Ok(update)
    }

    fn from_order_snapshot(order: &qx_core::Order, strategy_id: &str) -> Self {
        Self {
            order_id: order.client_id,
            account_id: order.account_id.clone(),
            instrument: order.instrument.to_string(),
            side: order.side,
            qty_raw: order.qty.raw(),
            filled_raw: order.filled.raw(),
            strategy_id: strategy_id.to_string(),
            exact_amounts: order.exact_amounts,
        }
    }

    fn validate_identity(&self) -> Result<(), String> {
        if self.order_id == 0
            || self.account_id.trim().is_empty()
            || self.strategy_id.trim().is_empty()
            || self.instrument.trim().is_empty()
        {
            return Err("StrategyUpdate 订单身份非法".into());
        }
        Ok(())
    }

    pub fn validate(&self) -> Result<(), String> {
        self.validate_identity()?;
        if InstrumentId::parse(&self.instrument).is_none() {
            return Err("StrategyUpdate 订单标的非法".into());
        }
        if let Some(amounts) = self.exact_amounts {
            if self.qty_raw != 0 || self.filled_raw != 0 {
                return Err("StrategyUpdate 精确订单混用了旧精度字段".into());
            }
            if let Some(budget) = amounts.quote_budget {
                if self.side != Side::Buy
                    || amounts.limit_price.is_some()
                    || !amounts.quantity.is_zero()
                    || !budget.is_positive()
                    || amounts.filled.is_negative()
                    || amounts.quote_filled.is_negative()
                    || amounts
                        .quote_filled
                        .checked_cmp(budget)
                        .is_none_or(|ordering| ordering.is_gt())
                {
                    return Err("StrategyUpdate 计价币预算订单金额非法".into());
                }
            } else if !amounts.quantity.is_positive()
                || amounts.filled.is_negative()
                || amounts
                    .filled
                    .checked_cmp(amounts.quantity)
                    .is_none_or(|ordering| ordering.is_gt())
                || !amounts.quote_filled.is_zero()
            {
                return Err("StrategyUpdate 精确订单数量或成交量非法".into());
            }
            if amounts
                .limit_price
                .is_some_and(|price| !price.is_positive())
                || amounts
                    .market_reference_price
                    .is_some_and(|price| !price.is_positive())
                || (amounts.limit_price.is_some() && amounts.market_reference_price.is_some())
            {
                return Err("StrategyUpdate 精确订单价格字段非法".into());
            }
        } else if self.qty_raw <= 0 || self.filled_raw < 0 || self.filled_raw > self.qty_raw {
            return Err("StrategyUpdate 旧精度订单数量或成交量非法".into());
        }
        Ok(())
    }
}

/// Canonical strategy-facing order lifecycle state. The wire spelling is stable
/// snake_case even though the Rust core uses idiomatic enum variant names.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StrategyOrderStatus {
    PendingSubmit,
    Submitted,
    Accepted,
    Working,
    PartiallyFilled,
    Filled,
    CancelPending,
    Cancelled,
    Rejected,
    Expired,
    Unknown,
}

impl From<qx_core::OrderStatus> for StrategyOrderStatus {
    fn from(status: qx_core::OrderStatus) -> Self {
        match status {
            qx_core::OrderStatus::PendingSubmit => Self::PendingSubmit,
            qx_core::OrderStatus::Submitted => Self::Submitted,
            qx_core::OrderStatus::Accepted => Self::Accepted,
            qx_core::OrderStatus::Working => Self::Working,
            qx_core::OrderStatus::PartiallyFilled => Self::PartiallyFilled,
            qx_core::OrderStatus::Filled => Self::Filled,
            qx_core::OrderStatus::CancelPending => Self::CancelPending,
            qx_core::OrderStatus::Cancelled => Self::Cancelled,
            qx_core::OrderStatus::Rejected => Self::Rejected,
            qx_core::OrderStatus::Expired => Self::Expired,
            qx_core::OrderStatus::Unknown => Self::Unknown,
        }
    }
}

/// A committed order-state transition delivered after the host's authoritative
/// OMS/EventLog reduction. `event_seq` is the EventLog sequence used for
/// idempotent delivery; `ts` is the business event time.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StrategyOrderStatusUpdate {
    pub schema_version: u32,
    pub event: StrategyOrderStatusEvent,
    pub event_seq: u64,
    pub ts: u64,
    pub status: StrategyOrderStatus,
    pub order: StrategyOrderUpdate,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StrategyOrderStatusEvent {
    OrderStatus,
}

impl StrategyOrderStatusUpdate {
    pub fn from_order(
        order: &qx_core::Order,
        strategy_id: &str,
        event_seq: u64,
        ts: u64,
        status: qx_core::OrderStatus,
        reason: Option<&str>,
    ) -> Result<Self, String> {
        let update = Self {
            schema_version: ORDER_STATUS_UPDATE_SCHEMA_VERSION,
            event: StrategyOrderStatusEvent::OrderStatus,
            event_seq,
            ts,
            status: status.into(),
            order: StrategyOrderUpdate::from_order_snapshot(order, strategy_id),
            reason: reason.map(str::to_owned),
        };
        update.validate()?;
        Ok(update)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != ORDER_STATUS_UPDATE_SCHEMA_VERSION {
            return Err("StrategyOrderStatusUpdate schema_version 不支持".into());
        }
        // EventLog sequences are zero-based; ts is an epoch timestamp and must stay positive.
        if self.ts == 0 {
            return Err("StrategyOrderStatusUpdate ts 必须为正数".into());
        }
        if self
            .reason
            .as_ref()
            .is_some_and(|reason| reason.len() > 2048)
        {
            return Err("StrategyOrderStatusUpdate reason 超过 2048 字节".into());
        }
        if self.status == StrategyOrderStatus::Rejected {
            self.order.validate_identity()
        } else {
            self.order.validate()
        }
    }

    pub fn validate_for(&self, context: &StrategyContext) -> Result<(), String> {
        self.validate()?;
        if self.order.strategy_id != context.strategy_id {
            return Err("StrategyOrderStatusUpdate 策略身份与 StrategyContext 不匹配".into());
        }
        Ok(())
    }
}

/// Versioned, typed execution fact delivered after the host commits the fill.
/// `order` is the order snapshot captured before applying this individual fill;
/// cumulative fill validation belongs to the host's authoritative OMS reducer.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StrategyUpdate {
    pub schema_version: u32,
    pub event: StrategyUpdateEvent,
    pub order: StrategyOrderUpdate,
    pub fill: Fill,
}

impl StrategyUpdate {
    pub fn from_fill(
        order: &qx_core::Order,
        fill: &Fill,
        strategy_id: &str,
    ) -> Result<Self, String> {
        order.validate()?;
        fill.validate_exact_amounts()?;
        let update = Self {
            schema_version: ORDER_UPDATE_SCHEMA_VERSION,
            event: StrategyUpdateEvent::Fill,
            order: StrategyOrderUpdate {
                order_id: order.client_id,
                account_id: order.account_id.clone(),
                instrument: order.instrument.to_string(),
                side: order.side,
                qty_raw: order.qty.raw(),
                filled_raw: order.filled.raw(),
                strategy_id: strategy_id.to_string(),
                exact_amounts: order.exact_amounts,
            },
            fill: fill.clone(),
        };
        update.validate()?;
        Ok(update)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != ORDER_UPDATE_SCHEMA_VERSION {
            return Err("StrategyUpdate schema_version 不支持".into());
        }
        self.order.validate()?;
        self.fill.validate_exact_amounts()?;
        if self.fill.order_id != self.order.order_id
            || self.fill.account_id != self.order.account_id
            || self.fill.ts == 0
            || self
                .fill
                .strategy_id
                .as_deref()
                .is_some_and(|id| id != self.order.strategy_id)
            || (self.fill.qty.raw() <= 0
                && !self
                    .fill
                    .exact_amounts
                    .is_some_and(|amounts| amounts.quantity.is_positive()))
            || (self.fill.exact_amounts.is_none() && self.fill.price.raw() <= 0)
        {
            return Err("StrategyUpdate 成交与订单身份或数量不匹配".into());
        }
        Ok(())
    }

    pub fn validate_for(&self, context: &StrategyContext) -> Result<(), String> {
        self.validate()?;
        if self.order.strategy_id != context.strategy_id {
            return Err("StrategyUpdate 策略身份与 StrategyContext 不匹配".into());
        }
        Ok(())
    }
}

/// Fill update with the EventLog identity that committed the fill.
///
/// Version 1 remains available as a compatibility projection for existing Rust,
/// Python and C ABI strategies. New hosts should deliver this version so the
/// callback can be correlated with replay and deduplicated by `event_seq`.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StrategyFillUpdateV2 {
    pub schema_version: u32,
    pub event: StrategyUpdateEvent,
    pub event_seq: u64,
    pub ts: u64,
    pub order: StrategyOrderUpdate,
    pub fill: Fill,
}

impl StrategyFillUpdateV2 {
    pub fn from_fill(
        order: &qx_core::Order,
        fill: &Fill,
        strategy_id: &str,
        event_seq: u64,
    ) -> Result<Self, String> {
        let legacy = StrategyUpdate::from_fill(order, fill, strategy_id)?;
        let update = Self {
            schema_version: ORDER_FILL_UPDATE_EVENT_SEQ_SCHEMA_VERSION,
            event: StrategyUpdateEvent::Fill,
            event_seq,
            ts: fill.ts,
            order: legacy.order,
            fill: legacy.fill,
        };
        update.validate()?;
        Ok(update)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != ORDER_FILL_UPDATE_EVENT_SEQ_SCHEMA_VERSION {
            return Err("StrategyFillUpdateV2 schema_version 不支持".into());
        }
        self.order.validate()?;
        self.fill.validate_exact_amounts()?;
        if self.ts == 0
            || self.ts != self.fill.ts
            || self.fill.order_id != self.order.order_id
            || self.fill.account_id != self.order.account_id
            || self
                .fill
                .strategy_id
                .as_deref()
                .is_some_and(|id| id != self.order.strategy_id)
            || (self.fill.qty.raw() <= 0
                && !self
                    .fill
                    .exact_amounts
                    .is_some_and(|amounts| amounts.quantity.is_positive()))
            || (self.fill.exact_amounts.is_none() && self.fill.price.raw() <= 0)
        {
            return Err("StrategyFillUpdateV2 成交时间、身份或数量与订单不匹配".into());
        }
        Ok(())
    }

    pub fn validate_for(&self, context: &StrategyContext) -> Result<(), String> {
        self.validate()?;
        if self.order.strategy_id != context.strategy_id {
            return Err("StrategyFillUpdateV2 策略身份与 StrategyContext 不匹配".into());
        }
        Ok(())
    }

    /// Preserve the v1 callback payload for strategies that have not opted into
    /// EventLog-aware fill updates yet.
    pub fn legacy_projection(&self) -> StrategyUpdate {
        StrategyUpdate {
            schema_version: ORDER_UPDATE_SCHEMA_VERSION,
            event: self.event,
            order: self.order.clone(),
            fill: self.fill.clone(),
        }
    }
}

/// 原生 Rust 策略接口；所有订单意图必须经过 Runtime 的 RiskGate、OMS 和 ExecutionPort。
pub trait Strategy: Send {
    /// Observe a callback failure without replacing the original error.
    fn on_error(&mut self, _error: &StrategyErrorV1) -> Result<(), String> {
        Ok(())
    }

    fn on_init(&mut self, _context: &StrategyContext) -> Result<(), String> {
        Ok(())
    }

    /// Declare closed Bar history needed before the first tradeable decision.
    /// Hosts that do not consume Bars must reject non-zero `warmup_bars` rather
    /// than silently treating an event count as a bar count.
    fn history_requirements(&self) -> Result<StrategyHistoryRequirements, String> {
        Ok(StrategyHistoryRequirements::default())
    }

    /// Called once when the requested Bar warmup has become visible for an
    /// instrument. A zero-warmup strategy receives one call after `on_start` with
    /// `instrument = None` and `observed_bars = 0`.
    fn on_warmup_complete(
        &mut self,
        _context: &StrategyContext,
        _instrument: Option<&InstrumentId>,
        _observed_bars: u64,
    ) -> Result<(), String> {
        Ok(())
    }

    /// Called once after the host validates the run and immediately before the first
    /// market event. Warmup bars, when requested by the strategy manifest, are still
    /// market events; this hook does not imply that warmup has completed.
    fn on_start(&mut self, _context: &StrategyContext) -> Result<(), String> {
        Ok(())
    }

    fn on_event(
        &mut self,
        context: &StrategyContext,
        event: &MarketEvent,
    ) -> Result<StrategyDecision, String>;

    /// Receive a JSON fact after execution accepts it; Bar emits a versioned fill fact after OMS/ledger/replay commit. Returned decisions queue for the next boundary and still pass risk/OMS.
    fn on_order_update(
        &mut self,
        _context: &StrategyContext,
        _update: &str,
    ) -> Result<Option<StrategyDecision>, String> {
        Ok(None)
    }

    /// Typed update entry point. The default preserves existing implementations by
    /// serializing the stable DTO into the legacy JSON callback.
    fn on_strategy_update(
        &mut self,
        context: &StrategyContext,
        update: &StrategyUpdate,
    ) -> Result<Option<StrategyDecision>, String> {
        update.validate_for(context)?;
        let payload = serde_json::to_string(update).map_err(|error| error.to_string())?;
        self.on_order_update(context, &payload)
    }

    /// Receive a committed fill with its EventLog sequence. Existing strategies
    /// keep receiving the v1 projection unless they override this method.
    fn on_strategy_fill_update_v2(
        &mut self,
        context: &StrategyContext,
        update: &StrategyFillUpdateV2,
    ) -> Result<Option<StrategyDecision>, String> {
        update.validate_for(context)?;
        let legacy = update.legacy_projection();
        self.on_strategy_update(context, &legacy)
    }

    /// Typed order-state entry point. It is deliberately separate from the
    /// legacy fill-only `on_order_update` callback so existing strategies that
    /// parse only fill facts do not receive a new event kind unexpectedly.
    fn on_strategy_order_status(
        &mut self,
        context: &StrategyContext,
        update: &StrategyOrderStatusUpdate,
    ) -> Result<Option<StrategyDecision>, String> {
        update.validate_for(context)?;
        Ok(None)
    }

    /// Receive a committed account balance or position snapshot from the account EventLog.
    /// The snapshot is an observation only; it does not mutate the Rust ledger. Strategies
    /// that need these facts can override this typed hook and return intents for the next
    /// decision boundary. Older strategies remain compatible and ignore the update.
    fn on_strategy_account_update(
        &mut self,
        context: &StrategyContext,
        update: &contract::StrategyAccountUpdateV1,
    ) -> Result<Option<StrategyDecision>, String> {
        update.validate_for(context)?;
        Ok(None)
    }

    fn on_stop(&mut self) -> Result<(), String> {
        Ok(())
    }

    /// Versioned stop hook. Existing strategies keep their no-argument callback;
    /// newer hosts may provide the final committed context and termination reason.
    fn on_stop_with_context(&mut self, stop: &StrategyStopContextV1) -> Result<(), String> {
        stop.validate()?;
        self.on_stop()
    }

    /// Optional, read-only state snapshot for explicit strategy-bias diagnostics.
    /// Implementations should return deterministic JSON and omit secrets or large buffers.
    fn diagnostic_state(&self) -> Option<serde_json::Value> {
        None
    }
}

/// Invoke the optional error observer while preserving the original failure.
/// Observer errors and Rust panics are diagnostic failures, not replacements.
pub fn notify_strategy_error(strategy: &mut dyn Strategy, error: &StrategyErrorV1) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| strategy.on_error(error)));
}

/// Preserve a failed strategy callback result while notifying the strategy's optional observer.
/// Hosts should use this at every callback boundary so equivalent event paths report the same
/// versioned error context without allowing observer failures to replace the original error.
pub fn notify_strategy_callback_result<T>(
    strategy: &mut dyn Strategy,
    context: &StrategyContext,
    stage: StrategyErrorStage,
    timestamp: Option<u64>,
    instrument: Option<&InstrumentId>,
    result: Result<T, String>,
) -> Result<T, String> {
    match result {
        Ok(value) => Ok(value),
        Err(message) => {
            let error = StrategyErrorV1::new(
                stage,
                context,
                timestamp,
                instrument.cloned(),
                message.clone(),
            );
            notify_strategy_error(strategy, &error);
            Err(message)
        }
    }
}

/// 一个不依赖浮点的内置 Rust 策略示例，可被回测/Paper/实盘适配器复用。
pub struct SmaCrossStrategy {
    pub strategy_id: String,
    pub instrument: InstrumentId,
    pub fast_window: usize,
    pub slow_window: usize,
    pub quantity: Quantity,
    closes: VecDeque<i128>,
    next_signal_id: u64,
    next_intent_id: u64,
}

impl SmaCrossStrategy {
    pub fn new(
        strategy_id: impl Into<String>,
        instrument: InstrumentId,
        fast_window: usize,
        slow_window: usize,
        quantity: Quantity,
    ) -> Result<Self, String> {
        if fast_window == 0 || slow_window == 0 || fast_window > slow_window || quantity.raw() <= 0
        {
            return Err("SmaCrossStrategy 参数非法".into());
        }
        Ok(Self {
            strategy_id: strategy_id.into(),
            instrument,
            fast_window,
            slow_window,
            quantity,
            closes: VecDeque::with_capacity(slow_window),
            next_signal_id: 1,
            next_intent_id: 1,
        })
    }
}

impl Strategy for SmaCrossStrategy {
    fn history_requirements(&self) -> Result<StrategyHistoryRequirements, String> {
        let warmup_bars = u64::try_from(self.slow_window.saturating_sub(1))
            .map_err(|_| "SmaCrossStrategy slow_window 超出 warmup 范围".to_string())?;
        Ok(StrategyHistoryRequirements { warmup_bars })
    }

    fn diagnostic_state(&self) -> Option<serde_json::Value> {
        Some(serde_json::json!({
            "closes": self.closes.iter().copied().collect::<Vec<_>>(),
            "next_signal_id": self.next_signal_id,
            "next_intent_id": self.next_intent_id,
        }))
    }

    fn on_event(
        &mut self,
        context: &StrategyContext,
        event: &MarketEvent,
    ) -> Result<StrategyDecision, String> {
        context.validate()?;
        event.validate()?;
        let event_ts = event.ts();
        let MarketEvent::Bar {
            instrument,
            ts,
            close_raw,
            ..
        } = event
        else {
            return Ok(StrategyDecision {
                schema_version: STRATEGY_API_VERSION,
                request_id: format!("{}:{event_ts}", self.strategy_id),
                strategy_id: context.strategy_id.clone(),
                signal_id: self.next_signal_id,
                confidence: 0,
                priority: 0,
                expires_at: event_ts,
                intents: Vec::new(),
            });
        };
        self.next_signal_id = self.next_signal_id.saturating_add(1);
        if instrument != &self.instrument {
            return Err("SmaCrossStrategy 收到未绑定 instrument".into());
        }
        self.closes.push_back(*close_raw);
        if self.closes.len() > self.slow_window {
            self.closes.pop_front();
        }
        let request_id = format!("{}:{ts}", self.strategy_id);
        if self.closes.len() < self.slow_window {
            return Ok(StrategyDecision {
                schema_version: STRATEGY_API_VERSION,
                request_id,
                strategy_id: context.strategy_id.clone(),
                signal_id: self.next_signal_id.saturating_sub(1),
                confidence: 0,
                priority: 0,
                expires_at: *ts,
                intents: Vec::new(),
            });
        }
        let fast_start = self.closes.len() - self.fast_window;
        let fast_sum: i128 = self.closes.iter().skip(fast_start).sum();
        let slow_sum: i128 = self.closes.iter().sum();
        let fast_avg = fast_sum / self.fast_window as i128;
        let slow_avg = slow_sum / self.slow_window as i128;
        let side = if fast_avg >= slow_avg {
            Side::Buy
        } else {
            Side::Sell
        };
        let decision = StrategyDecision {
            schema_version: STRATEGY_API_VERSION,
            request_id,
            strategy_id: context.strategy_id.clone(),
            signal_id: self.next_signal_id.saturating_sub(1),
            confidence: (fast_avg - slow_avg).abs(),
            priority: 0,
            expires_at: *ts,
            intents: vec![StrategyOrderIntent {
                intent_id: self.next_intent_id,
                instrument: self.instrument.clone(),
                side,
                qty: self.quantity,
                limit: None,
                policy: None,
                reduce_only: false,
                post_only: false,

                qty_exact: None,
                limit_price_exact: None,
                quote_budget: None,
            }],
        };
        self.next_intent_id = self.next_intent_id.saturating_add(1);
        decision.validate_for(context, *ts)?;
        Ok(decision)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qx_core::{ExactFillAmounts, Order, OrderStatus, OrderTrace};

    fn update_fixture() -> (StrategyContext, Order, Fill) {
        let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
        let context = StrategyContext {
            strategy_id: "update-test".into(),
            strategy_version: "v1".into(),
            account_id: "paper".into(),
            venue_id: "BINANCE".into(),
            data_fingerprint: "bars:update-test".into(),
            as_of: 10,
            positions: BTreeMap::new(),
            cash: BTreeMap::new(),
            available_margin_raw: None,
            risk_state: "backtest".into(),
            positions_exact: BTreeMap::new(),
            cash_exact: BTreeMap::new(),
            available_margin_exact: None,
        };
        let order = Order {
            client_id: 7,
            instrument,
            side: Side::Buy,
            qty: Quantity::from_i64(2),
            limit: None,
            status: OrderStatus::PartiallyFilled,
            filled: Quantity::from_i64(1),
            account_id: "paper".into(),
            trace: Some(OrderTrace {
                strategy_id: Some("update-test".into()),
                signal_id: Some(3),
                intent_id: Some(4),
                rule_version: None,
            }),
            policy: None,
            exact_amounts: None,
        };
        let fill = Fill {
            order_id: 7,
            qty: Quantity::from_i64(1),
            price: Price::from_i64(100),
            fee: qx_core::Money::ZERO,
            ts: 10,
            account_id: "paper".into(),
            strategy_id: Some("update-test".into()),
            signal_id: Some(3),
            intent_id: Some(4),
            venue_id: Some("BINANCE".into()),
            venue_order_id: None,
            rule_version: None,
            fee_currency: None,
            fees: Vec::new(),
            exact_fees: Vec::new(),
            exact_amounts: None,
        };
        (context, order, fill)
    }

    #[test]
    fn strategy_update_is_versioned_and_keeps_the_legacy_json_shape() {
        let (context, order, fill) = update_fixture();
        let update = StrategyUpdate::from_fill(&order, &fill, &context.strategy_id).unwrap();
        update.validate_for(&context).unwrap();
        let wire = serde_json::to_value(&update).unwrap();
        let golden: serde_json::Value = serde_json::from_str(include_str!(
            "../tests/fixtures/strategy-update-fill-v1.json"
        ))
        .unwrap();
        assert_eq!(wire, golden);
        assert_eq!(wire["schema_version"], ORDER_UPDATE_SCHEMA_VERSION);
        assert_eq!(wire["event"], "fill");
        assert_eq!(
            wire["order"]["qty_raw"].as_i64(),
            Some(i64::try_from(order.qty.raw()).unwrap())
        );
        assert!(wire["order"].get("exact_amounts").is_none());
        let decoded: StrategyUpdate = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(decoded, update);

        let mut wrong_order = update;
        wrong_order.fill.order_id += 1;
        assert!(wrong_order.validate_for(&context).is_err());
    }

    #[test]
    fn strategy_fill_update_v2_binds_fill_to_eventlog_sequence_and_keeps_v1_projection() {
        let (context, order, fill) = update_fixture();
        let update =
            StrategyFillUpdateV2::from_fill(&order, &fill, &context.strategy_id, 9).unwrap();
        update.validate_for(&context).unwrap();
        let wire = serde_json::to_value(&update).unwrap();
        let golden: serde_json::Value = serde_json::from_str(include_str!(
            "../tests/fixtures/strategy-fill-update-v2.json"
        ))
        .unwrap();
        assert_eq!(wire, golden);
        assert_eq!(wire["event_seq"], 9);
        assert_eq!(wire["ts"], fill.ts);
        assert_eq!(
            update.legacy_projection().schema_version,
            ORDER_UPDATE_SCHEMA_VERSION
        );

        let mut mismatched_time = update.clone();
        mismatched_time.ts += 1;
        assert!(mismatched_time.validate_for(&context).is_err());
        let mut wrong_owner = update;
        wrong_owner.order.strategy_id = "other-strategy".into();
        assert!(wrong_owner.validate_for(&context).is_err());
    }

    #[test]
    fn strategy_update_preserves_scale_aware_small_coin_amounts() {
        let (context, mut order, mut fill) = update_fixture();
        let tiny = ExactDecimal::parse("0.000000000000000001").unwrap();
        order.qty = Quantity::from_raw(0);
        order.filled = Quantity::from_raw(0);
        order.exact_amounts = Some(ExactOrderAmounts {
            quantity: tiny,
            limit_price: None,
            filled: tiny,
            quote_budget: None,
            quote_filled: ExactDecimal::ZERO,
            market_reference_price: None,
        });
        fill.qty = Quantity::from_raw(0);
        fill.price = Price::from_raw(0);
        fill.exact_amounts = Some(ExactFillAmounts {
            quantity: tiny,
            price: ExactDecimal::parse("0.00012").unwrap(),
            quote_cost: None,
            quote_budget_complete: false,
        });

        let update = StrategyUpdate::from_fill(&order, &fill, &context.strategy_id).unwrap();
        let wire = serde_json::to_value(&update).unwrap();
        assert_eq!(
            wire["order"]["exact_amounts"]["quantity"]["coefficient"],
            "1"
        );
        assert_eq!(wire["order"]["exact_amounts"]["quantity"]["scale"], 18);
        assert_eq!(
            wire["fill"]["exact_amounts"]["quantity"]["coefficient"],
            "1"
        );
        update.validate_for(&context).unwrap();
    }

    #[test]
    fn typed_strategy_update_delegates_to_legacy_order_update_implementations() {
        struct LegacyReceiver(Option<String>);

        impl Strategy for LegacyReceiver {
            fn on_event(
                &mut self,
                _context: &StrategyContext,
                _event: &MarketEvent,
            ) -> Result<StrategyDecision, String> {
                unreachable!("the lifecycle update test does not dispatch market events")
            }

            fn on_order_update(
                &mut self,
                _context: &StrategyContext,
                update: &str,
            ) -> Result<Option<StrategyDecision>, String> {
                self.0 = Some(update.to_string());
                Ok(None)
            }
        }

        let (context, order, fill) = update_fixture();
        let update = StrategyUpdate::from_fill(&order, &fill, &context.strategy_id).unwrap();
        let expected = serde_json::to_value(&update).unwrap();
        let mut strategy = LegacyReceiver(None);
        assert!(strategy
            .on_strategy_update(&context, &update)
            .unwrap()
            .is_none());
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(strategy.0.as_deref().unwrap()).unwrap(),
            expected
        );
    }

    #[test]
    fn sequenced_fill_update_defaults_to_the_legacy_strategy_callback() {
        struct LegacyReceiver(Option<String>);

        impl Strategy for LegacyReceiver {
            fn on_event(
                &mut self,
                _context: &StrategyContext,
                _event: &MarketEvent,
            ) -> Result<StrategyDecision, String> {
                unreachable!("the lifecycle update test does not dispatch market events")
            }

            fn on_order_update(
                &mut self,
                _context: &StrategyContext,
                update: &str,
            ) -> Result<Option<StrategyDecision>, String> {
                self.0 = Some(update.to_string());
                Ok(None)
            }
        }

        let (context, order, fill) = update_fixture();
        let update =
            StrategyFillUpdateV2::from_fill(&order, &fill, &context.strategy_id, 9).unwrap();
        let expected = serde_json::to_value(&update.legacy_projection()).unwrap();
        let mut strategy = LegacyReceiver(None);
        assert!(strategy
            .on_strategy_fill_update_v2(&context, &update)
            .unwrap()
            .is_none());
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(strategy.0.as_deref().unwrap()).unwrap(),
            expected
        );
    }

    #[test]
    fn strategy_order_status_update_is_versioned_ordered_and_keeps_exact_identity() {
        let (context, mut order, _) = update_fixture();
        order.status = OrderStatus::Accepted;
        order.filled = Quantity::ZERO;
        let update = StrategyOrderStatusUpdate::from_order(
            &order,
            &context.strategy_id,
            23,
            10,
            OrderStatus::Accepted,
            None,
        )
        .unwrap();
        update.validate_for(&context).unwrap();

        let wire = serde_json::to_value(&update).unwrap();
        let golden: serde_json::Value = serde_json::from_str(include_str!(
            "../tests/fixtures/strategy-order-status-accepted-v1.json"
        ))
        .unwrap();
        assert_eq!(wire, golden);
        assert_eq!(wire["event_seq"], 23);
        assert_eq!(wire["status"], "accepted");
        assert_eq!(wire["order"]["filled_raw"], 0);
        let decoded: StrategyOrderStatusUpdate = serde_json::from_value(wire).unwrap();
        assert_eq!(decoded, update);

        let mut invalid = update;
        invalid.event_seq = 0;
        assert!(invalid.validate_for(&context).is_ok());
        invalid.ts = 0;
        assert!(invalid.validate_for(&context).is_err());
    }

    #[test]
    fn order_status_update_does_not_invoke_legacy_fill_callback() {
        struct LegacyReceiver(Option<String>);

        impl Strategy for LegacyReceiver {
            fn on_event(
                &mut self,
                _context: &StrategyContext,
                _event: &MarketEvent,
            ) -> Result<StrategyDecision, String> {
                unreachable!("the lifecycle update test does not dispatch market events")
            }

            fn on_order_update(
                &mut self,
                _context: &StrategyContext,
                update: &str,
            ) -> Result<Option<StrategyDecision>, String> {
                self.0 = Some(update.to_string());
                Ok(None)
            }
        }

        let (context, mut order, _) = update_fixture();
        order.status = OrderStatus::Accepted;
        order.filled = Quantity::ZERO;
        let update = StrategyOrderStatusUpdate::from_order(
            &order,
            &context.strategy_id,
            23,
            10,
            OrderStatus::Accepted,
            None,
        )
        .unwrap();
        let mut strategy = LegacyReceiver(None);
        assert!(strategy
            .on_strategy_order_status(&context, &update)
            .unwrap()
            .is_none());
        assert!(strategy.0.is_none());
    }

    #[test]
    fn decision_validates_multi_intent_and_context_identity() {
        let context = StrategyContext {
            strategy_id: "rust-demo".into(),
            strategy_version: "v1".into(),
            account_id: "main".into(),
            venue_id: "paper".into(),
            data_fingerprint: "bars-1".into(),
            as_of: 10,
            positions: BTreeMap::from([("BTCUSDT.BINANCE".into(), 0)]),
            cash: BTreeMap::from([("USDT".into(), 1_000)]),
            available_margin_raw: Some(1_000),
            risk_state: "ready".into(),
            positions_exact: BTreeMap::new(),
            cash_exact: BTreeMap::new(),
            available_margin_exact: None,
        };
        let decision = StrategyDecision {
            schema_version: STRATEGY_API_VERSION,
            request_id: "req-1".into(),
            strategy_id: "rust-demo".into(),
            signal_id: 1,
            confidence: 800,
            priority: 1,
            expires_at: 10,
            intents: vec![StrategyOrderIntent {
                intent_id: 1001,
                instrument: InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
                side: Side::Buy,
                qty: Quantity::from_i64(1),
                limit: Some(Price::from_i64(100)),
                policy: None,
                reduce_only: false,
                post_only: true,

                qty_exact: None,
                limit_price_exact: None,
                quote_budget: None,
            }],
        };
        assert!(decision.validate_for(&context, 10).is_ok());
        let orders = decision.to_orders(&context).unwrap();
        assert_eq!(orders.len(), 1);
        assert_eq!(orders[0].trace.as_ref().unwrap().intent_id, Some(1001));
    }

    #[test]
    fn sma_cross_emits_a_native_order_intent_after_warmup() {
        let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
        let context = StrategyContext {
            strategy_id: "sma".into(),
            strategy_version: "v1".into(),
            account_id: "main".into(),
            venue_id: "paper".into(),
            data_fingerprint: "bars-1".into(),
            as_of: 3,
            positions_exact: std::collections::BTreeMap::new(),
            cash_exact: std::collections::BTreeMap::new(),
            available_margin_exact: None,
            positions: BTreeMap::new(),
            cash: BTreeMap::new(),
            available_margin_raw: Some(1_000),
            risk_state: "ready".into(),
        };
        let mut strategy =
            SmaCrossStrategy::new("sma", instrument.clone(), 2, 3, Quantity::from_i64(1)).unwrap();
        for (ts, close_raw) in [(1, 100), (2, 101)] {
            let decision = strategy
                .on_event(
                    &context,
                    &MarketEvent::Bar {
                        instrument: instrument.clone(),
                        ts,
                        open_raw: close_raw,
                        high_raw: close_raw,
                        low_raw: close_raw,
                        close_raw,
                        volume_raw: 1,
                        exact_ohlcv: None,
                    },
                )
                .unwrap();
            assert!(decision.intents.is_empty());
        }
        let decision = strategy
            .on_event(
                &context,
                &MarketEvent::Bar {
                    instrument,
                    ts: 3,
                    open_raw: 103,
                    high_raw: 103,
                    low_raw: 103,
                    close_raw: 103,
                    volume_raw: 1,
                    exact_ohlcv: None,
                },
            )
            .unwrap();
        assert_eq!(decision.intents.len(), 1);
        assert_eq!(decision.intents[0].side, Side::Buy);
    }

    #[test]
    fn orderbook_events_keep_sub_nano_source_precision_and_compare_exact_prices() {
        let level = |price: &str| OrderBookLevel {
            price_raw: 100_000_000_000,
            qty_raw: 0,
            price_exact: Some(ExactDecimal::parse(price).unwrap()),
            qty_exact: Some(ExactDecimal::parse("0.0000000001").unwrap()),
        };
        let bid = level("100.0000000002");
        bid.validate().unwrap();
        let wire = serde_json::to_value(&bid).unwrap();
        assert_eq!(wire["qty_raw"], 0);
        assert_eq!(wire["qty_exact"]["scale"], 10);

        let event = MarketEvent::OrderBook {
            instrument: InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
            ts: 1,
            sequence: 1,
            bids: vec![bid],
            asks: vec![level("100.0000000001")],
        };
        assert!(event.validate().unwrap_err().contains("价格档位非法"));
    }

    #[test]
    fn orderbook_level_rejects_inconsistent_exact_projection() {
        let mut level = OrderBookLevel {
            price_raw: 100_000_000_000,
            qty_raw: 0,
            price_exact: Some(ExactDecimal::parse("100.0000000001").unwrap()),
            qty_exact: Some(ExactDecimal::parse("0.0000000001").unwrap()),
        };
        level.validate().unwrap();
        level.price_raw += 1;
        assert!(level.validate().unwrap_err().contains("raw 投影不一致"));
    }
}
