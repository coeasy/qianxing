//! 牵星统一策略 SDK。
//!
//! 该 crate 是 Rust 策略、C ABI 策略和 Python 策略共同遵守的领域边界：
//! 策略只消费不可变上下文和市场事件，只产生 `StrategyDecision`，不得直接
//! 访问 Venue、EventLog、Ledger 或凭证。运行时仍会对每个 intent 执行 Risk/OMS。

use qx_core::{InstrumentId, Order, OrderPolicy, OrderStatus, OrderTrace, Price, Quantity, Side};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::collections::VecDeque;

pub mod c_api;
pub mod frame;
pub mod ring;

pub use c_api::{
    sha256_hex, verify_file_sha256, CAbiStrategy, DynamicCAbiLoadPolicy, DynamicCAbiStrategy,
    QxStrategyVTable, QX_C_STRATEGY_API_VERSION,
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
        Ok(())
    }
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
                if bids
                    .windows(2)
                    .any(|window| window[0].price_raw <= window[1].price_raw)
                    || asks
                        .windows(2)
                        .any(|window| window[0].price_raw >= window[1].price_raw)
                    || matches!((bids.first(), asks.first()), (Some(bid), Some(ask)) if bid.price_raw > ask.price_raw)
                {
                    return Err("OrderBook MarketEvent 价格档位非法".into());
                }
            }
            Self::Timer { name, .. } if name.trim().is_empty() => {
                return Err("Timer MarketEvent name 不能为空".into())
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
}

impl OrderBookLevel {
    pub fn validate(&self) -> Result<(), String> {
        if self.price_raw <= 0 || self.qty_raw <= 0 {
            return Err("OrderBookLevel 价格和数量必须为正".into());
        }
        Ok(())
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct StrategyOrderIntent {
    pub intent_id: u64,
    pub instrument: InstrumentId,
    pub side: Side,
    pub qty: Quantity,
    pub limit: Option<Price>,
    pub policy: Option<OrderPolicy>,
    pub reduce_only: bool,
    pub post_only: bool,
}

impl StrategyOrderIntent {
    pub fn validate(&self) -> Result<(), String> {
        if self.intent_id == 0 || self.qty.raw() <= 0 {
            return Err("StrategyOrderIntent id 或数量非法".into());
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

impl StrategyDecision {
    pub fn validate_for(&self, context: &StrategyContext, event_ts: u64) -> Result<(), String> {
        context.validate()?;
        if self.schema_version != STRATEGY_API_VERSION
            || self.request_id.trim().is_empty()
            || self.strategy_id != context.strategy_id
            || self.signal_id == 0
            || self.intents.iter().any(|intent| intent.validate().is_err())
            || (self.expires_at != 0 && self.expires_at < event_ts)
        {
            return Err("StrategyDecision 身份、版本、有效期或订单意图非法".into());
        }
        let mut ids = std::collections::BTreeSet::new();
        for intent in &self.intents {
            intent.validate()?;
            if !ids.insert(intent.intent_id) {
                return Err("StrategyDecision 存在重复 intent_id".into());
            }
        }
        Ok(())
    }

    /// 将决定转换为核心订单；该函数只做协议转换，调用方仍必须执行
    /// RiskGate/OMS 校验，不能把返回值视为已提交订单。
    pub fn to_orders(&self, context: &StrategyContext) -> Result<Vec<Order>, String> {
        self.validate_for(context, context.as_of)?;
        self.intents
            .iter()
            .map(|intent| {
                let mut policy = intent.policy.unwrap_or_default();
                policy.reduce_only = intent.reduce_only;
                policy.post_only = intent.post_only;
                let order = Order {
                    client_id: intent.intent_id,
                    instrument: intent.instrument.clone(),
                    side: intent.side,
                    qty: intent.qty,
                    limit: intent.limit,
                    status: OrderStatus::PendingSubmit,
                    filled: Quantity::ZERO,
                    account_id: context.account_id.clone(),
                    trace: Some(OrderTrace {
                        strategy_id: Some(context.strategy_id.clone()),
                        signal_id: Some(self.signal_id),
                        intent_id: Some(intent.intent_id),
                        rule_version: Some(context.strategy_version.clone()),
                    }),
                    policy: Some(policy),
                };
                order
                    .validate()
                    .map_err(|error| format!("StrategyDecision 转核心订单失败: {error}"))?;
                Ok(order)
            })
            .collect()
    }
}

/// 原生 Rust 策略接口。策略生命周期与交易执行解耦，所有订单意图必须经过
/// Runtime 的 RiskGate、OMS 和 ExecutionPort。
pub trait Strategy: Send {
    fn on_init(&mut self, _context: &StrategyContext) -> Result<(), String> {
        Ok(())
    }

    fn on_event(
        &mut self,
        context: &StrategyContext,
        event: &MarketEvent,
    ) -> Result<StrategyDecision, String>;

    fn on_order_update(
        &mut self,
        _context: &StrategyContext,
        _update: &str,
    ) -> Result<Option<StrategyDecision>, String> {
        Ok(None)
    }

    fn on_stop(&mut self) -> Result<(), String> {
        Ok(())
    }
}

/// 一个不依赖浮点的内置 Rust 策略示例，证明原生策略可以直接输出统一
/// `StrategyDecision`，并可被回测/Paper/实盘适配器复用。
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
                },
            )
            .unwrap();
        assert_eq!(decision.intents.len(), 1);
        assert_eq!(decision.intents[0].side, Side::Buy);
    }
}
