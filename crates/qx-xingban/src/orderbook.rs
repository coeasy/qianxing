//! L1/L2 订单簿逐档撮合内核。
//!
//! 该模块不依赖网络或存储，可被历史 Tick 回放、Paper 模拟和性能基准共同使用。
//! 盘口快照先做顺序/价格/数量校验，再按价格优先、同价位数量优先逐档消费。

use qx_core::{Fill, Money, Order, OrderStatus, Price, Quantity, Side, SCALE};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct BookLevel {
    pub price: Price,
    pub qty: Quantity,
}

impl BookLevel {
    pub fn validate(&self) -> Result<(), String> {
        if self.price.raw() <= 0 || self.qty.raw() <= 0 {
            return Err("盘口档位价格和数量必须为正".into());
        }
        Ok(())
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct OrderBookSnapshot {
    pub instrument: qx_core::InstrumentId,
    pub ts: u64,
    pub sequence: u64,
    /// 买盘按价格从高到低排列。
    pub bids: Vec<BookLevel>,
    /// 卖盘按价格从低到高排列。
    pub asks: Vec<BookLevel>,
}

impl OrderBookSnapshot {
    pub fn validate(&self) -> Result<(), String> {
        if self.ts == 0 || self.sequence == 0 {
            return Err("OrderBookSnapshot ts/sequence 必须为正".into());
        }
        for level in self.bids.iter().chain(self.asks.iter()) {
            level.validate()?;
        }
        if self
            .bids
            .windows(2)
            .any(|window| window[0].price.raw() <= window[1].price.raw())
        {
            return Err("买盘必须严格按价格降序排列".into());
        }
        if self
            .asks
            .windows(2)
            .any(|window| window[0].price.raw() >= window[1].price.raw())
        {
            return Err("卖盘必须严格按价格升序排列".into());
        }
        if let (Some(bid), Some(ask)) = (self.bids.first(), self.asks.first()) {
            if bid.price.raw() > ask.price.raw() {
                return Err("盘口买一不能高于卖一".into());
            }
        }
        Ok(())
    }
}

/// 一份可交付的 L1/L2 深度数据帧。`source` 必填，用于把回测输入绑定到
/// 数据来源；快照顺序、价格和数量由 `OrderBookSnapshot::validate` 约束。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DepthFrame {
    pub schema_version: u32,
    pub source: String,
    pub instrument: qx_core::InstrumentId,
    pub snapshots: Vec<OrderBookSnapshot>,
}

impl DepthFrame {
    pub const SUPPORTED_SCHEMA_VERSION: u32 = 1;

    pub fn from_json(input: &str) -> Result<Self, String> {
        let frame: Self = serde_json::from_str(input)
            .map_err(|error| format!("深度数据帧 JSON 无效: {error}"))?;
        frame.validate()?;
        Ok(frame)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != Self::SUPPORTED_SCHEMA_VERSION {
            return Err(format!(
                "深度数据帧 schema_version 仅支持 {}，实际 {}",
                Self::SUPPORTED_SCHEMA_VERSION,
                self.schema_version
            ));
        }
        if self.source.trim().is_empty() {
            return Err("深度数据帧 source 不能为空".into());
        }
        if self.snapshots.is_empty() {
            return Err("深度数据帧至少需要一份盘口快照".into());
        }
        for snapshot in &self.snapshots {
            snapshot.validate()?;
            if snapshot.instrument != self.instrument {
                return Err(format!(
                    "深度数据帧快照标的与声明不一致: {} != {}",
                    snapshot.instrument, self.instrument
                ));
            }
        }
        Ok(())
    }

    /// 输入数据指纹；与撮合结果无关，只描述深度序列本身。
    pub fn input_hash(&self) -> u64 {
        let mut hash = qx_core::Fnv1a::new();
        hash.write_text(&self.source);
        hash.write_text(&self.instrument.to_string());
        hash.write_u64(self.snapshots.len() as u64);
        for snapshot in &self.snapshots {
            hash.write_u64(snapshot.ts);
            hash.write_u64(snapshot.sequence);
            for level in snapshot.bids.iter().chain(snapshot.asks.iter()) {
                hash.write_i128(level.price.raw());
                hash.write_i128(level.qty.raw());
            }
        }
        hash.finish()
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
struct PendingBookOrder {
    order: Order,
    ready_at_snapshot: u64,
}

/// 订单簿回测的确定性执行假设。
///
/// `latency_snapshots` 表示订单提交后需要等待的后续快照数量；
/// `queue_position_bps` 将限价订单所在档位的一部分流动性视为前置队列；
/// `market_impact_bps` 对实际成交价施加固定比例冲击。默认模型保持原有
/// 逐档吃单行为，所有参数都应写入上层回测配置和 RunManifest。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct OrderBookExecutionModel {
    pub fee_bps: i128,
    pub latency_snapshots: u64,
    pub queue_position_bps: i128,
    pub market_impact_bps: i128,
}

impl OrderBookExecutionModel {
    pub fn new(fee_bps: i128) -> Result<Self, String> {
        let model = Self {
            fee_bps,
            latency_snapshots: 0,
            queue_position_bps: 0,
            market_impact_bps: 0,
        };
        model.validate()?;
        Ok(model)
    }

    pub fn validate(&self) -> Result<(), String> {
        if !(0..=10_000).contains(&self.fee_bps)
            || !(0..=10_000).contains(&self.queue_position_bps)
            || !(0..=10_000).contains(&self.market_impact_bps)
        {
            return Err("订单簿执行模型 bps 必须在 0..=10000 内".into());
        }
        Ok(())
    }
}

/// 逐档吃单撮合器。它只负责产生 Fill，不直接修改 Ledger。
pub struct OrderBookMatchingEngine {
    pending: Vec<PendingBookOrder>,
    model: OrderBookExecutionModel,
    snapshot_index: u64,
    last_snapshot_sequence: Option<u64>,
    last_snapshot_ts: Option<u64>,
}

impl OrderBookMatchingEngine {
    pub fn new(fee_bps: i128) -> Result<Self, String> {
        Self::with_model(OrderBookExecutionModel::new(fee_bps)?)
    }

    pub fn with_model(model: OrderBookExecutionModel) -> Result<Self, String> {
        model.validate()?;
        Ok(Self {
            pending: Vec::new(),
            model,
            snapshot_index: 0,
            last_snapshot_sequence: None,
            last_snapshot_ts: None,
        })
    }

    pub fn submit(&mut self, order: Order) -> Result<(), String> {
        order
            .validate()
            .map_err(|error| format!("订单簿订单非法: {error}"))?;
        if self
            .pending
            .iter()
            .any(|pending| pending.order.client_id == order.client_id)
        {
            return Err(format!("订单簿重复 client_id: {}", order.client_id));
        }
        let ready_at_snapshot = self
            .snapshot_index
            .saturating_add(self.model.latency_snapshots)
            .saturating_add(1);
        self.pending.push(PendingBookOrder {
            order,
            ready_at_snapshot,
        });
        Ok(())
    }

    /// 在下一次快照撮合前撤销一个尚未终态的订单。
    ///
    /// 撤单是否先于盘口事件生效由调用方的事件顺序决定；回测可使用
    /// `on_snapshot_with_cancellations` 将同一时间点的撤单明确排在撮合前。
    pub fn cancel(&mut self, client_id: u64) -> bool {
        let before = self.pending.len();
        self.pending
            .retain(|pending| pending.order.client_id != client_id);
        before != self.pending.len()
    }

    pub fn cancel_many(&mut self, client_ids: &[u64]) -> usize {
        client_ids
            .iter()
            .filter(|client_id| self.cancel(**client_id))
            .count()
    }

    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    pub fn on_snapshot(&mut self, snapshot: &OrderBookSnapshot) -> Result<Vec<Fill>, String> {
        self.on_snapshot_with_cancellations(snapshot, &[])
    }

    /// 应用一组在该盘口快照撮合前生效的撤单，再按快照撮合。
    pub fn on_snapshot_with_cancellations(
        &mut self,
        snapshot: &OrderBookSnapshot,
        cancelled_client_ids: &[u64],
    ) -> Result<Vec<Fill>, String> {
        snapshot.validate()?;
        if self
            .last_snapshot_sequence
            .is_some_and(|sequence| snapshot.sequence <= sequence)
            || self.last_snapshot_ts.is_some_and(|ts| snapshot.ts <= ts)
        {
            return Err("订单簿快照 sequence/ts 必须严格递增".into());
        }
        self.last_snapshot_sequence = Some(snapshot.sequence);
        self.last_snapshot_ts = Some(snapshot.ts);
        self.cancel_many(cancelled_client_ids);
        self.snapshot_index = self.snapshot_index.saturating_add(1);
        let pending = std::mem::take(&mut self.pending);
        let mut not_ready = Vec::new();
        let mut ready = Vec::new();
        for pending_order in pending {
            if pending_order.ready_at_snapshot <= self.snapshot_index {
                ready.push(pending_order);
            } else {
                not_ready.push(pending_order);
            }
        }
        let mut remaining_orders = Vec::with_capacity(ready.len());
        let mut fills = Vec::new();
        let mut remaining_bids: Vec<i128> =
            snapshot.bids.iter().map(|level| level.qty.raw()).collect();
        let mut remaining_asks: Vec<i128> =
            snapshot.asks.iter().map(|level| level.qty.raw()).collect();
        let mut queue_bids: Vec<i128> = snapshot
            .bids
            .iter()
            .map(|level| self.queue_ahead(level.qty.raw()))
            .collect();
        let mut queue_asks: Vec<i128> = snapshot
            .asks
            .iter()
            .map(|level| self.queue_ahead(level.qty.raw()))
            .collect();
        for pending_order in ready {
            let mut order = pending_order.order;
            if order.instrument != snapshot.instrument {
                return Err("订单簿订单 instrument 与快照不一致".into());
            }
            let levels = if order.side == Side::Buy {
                &snapshot.asks
            } else {
                &snapshot.bids
            };
            let mut remaining_qty = order.remaining().raw();
            for (level_index, level) in levels.iter().enumerate() {
                if remaining_qty <= 0 || !Self::limit_ok(&order, level.price) {
                    break;
                }
                let raw_available = if order.side == Side::Buy {
                    &mut remaining_asks[level_index]
                } else {
                    &mut remaining_bids[level_index]
                };
                let queue_ahead = if order.limit.is_some() {
                    if order.side == Side::Buy {
                        queue_asks[level_index]
                    } else {
                        queue_bids[level_index]
                    }
                } else {
                    0
                };
                let available_qty = (*raw_available).saturating_sub(queue_ahead);
                let fill_qty = remaining_qty.min(available_qty);
                if fill_qty <= 0 {
                    continue;
                }
                let execution_price_raw = self
                    .execution_price(order.side, level.price.raw())
                    .ok_or_else(|| "订单簿冲击价格计算溢出或非正".to_string())?;
                if !Self::limit_ok(&order, Price::from_raw(execution_price_raw)) {
                    break;
                }
                let notional_raw = fill_qty
                    .checked_mul(execution_price_raw)
                    .and_then(|value| value.checked_div(SCALE))
                    .ok_or_else(|| "订单簿成交名义额溢出".to_string())?;
                let fee_raw = notional_raw
                    .checked_mul(self.model.fee_bps)
                    .and_then(|value| value.checked_div(10_000))
                    .ok_or_else(|| "订单簿成交手续费溢出".to_string())?;
                fills.push(Fill {
                    order_id: order.client_id,
                    qty: Quantity::from_raw(fill_qty),
                    price: Price::from_raw(execution_price_raw),
                    fee: Money::from_raw(fee_raw),
                    ts: snapshot.ts,
                    account_id: order.account_id.clone(),
                    strategy_id: order
                        .trace
                        .as_ref()
                        .and_then(|trace| trace.strategy_id.clone()),
                    signal_id: order.trace.as_ref().and_then(|trace| trace.signal_id),
                    intent_id: order.trace.as_ref().and_then(|trace| trace.intent_id),
                    venue_id: None,
                    venue_order_id: None,
                    rule_version: order
                        .trace
                        .as_ref()
                        .and_then(|trace| trace.rule_version.clone()),
                    fee_currency: None,
                });
                remaining_qty -= fill_qty;
                *raw_available -= fill_qty;
                if order.limit.is_some() {
                    if order.side == Side::Buy {
                        queue_asks[level_index] = queue_asks[level_index].saturating_sub(fill_qty);
                    } else {
                        queue_bids[level_index] = queue_bids[level_index].saturating_sub(fill_qty);
                    }
                }
                order.filled = Quantity::from_raw(
                    order
                        .filled
                        .raw()
                        .checked_add(fill_qty)
                        .ok_or_else(|| "订单簿订单成交数量溢出".to_string())?,
                );
            }
            order.status = if order.filled.raw() >= order.qty.raw() {
                OrderStatus::Filled
            } else if order.filled.raw() > 0 {
                OrderStatus::PartiallyFilled
            } else {
                order.status
            };
            if order.status != OrderStatus::Filled {
                remaining_orders.push(PendingBookOrder {
                    order,
                    ready_at_snapshot: pending_order.ready_at_snapshot,
                });
            }
        }
        not_ready.extend(remaining_orders);
        self.pending = not_ready;
        Ok(fills)
    }

    fn queue_ahead(&self, level_qty: i128) -> i128 {
        level_qty
            .checked_mul(self.model.queue_position_bps)
            .and_then(|value| value.checked_div(10_000))
            .unwrap_or(level_qty)
    }

    fn execution_price(&self, side: Side, raw: i128) -> Option<i128> {
        let impact = raw
            .checked_mul(self.model.market_impact_bps)
            .and_then(|value| value.checked_div(10_000))?;
        let price = match side {
            Side::Buy => raw.checked_add(impact)?,
            Side::Sell => raw.checked_sub(impact)?,
        };
        (price > 0).then_some(price)
    }

    fn limit_ok(order: &Order, market_price: Price) -> bool {
        match order.limit {
            None => true,
            Some(limit) => match order.side {
                Side::Buy => market_price.raw() <= limit.raw(),
                Side::Sell => market_price.raw() >= limit.raw(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qx_core::{InstrumentId, OrderTrace};

    fn order(side: Side, qty: i128, limit: Option<i128>) -> Order {
        Order {
            client_id: 1,
            instrument: InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
            side,
            qty: Quantity::from_raw(qty),
            limit: limit.map(Price::from_raw),
            status: OrderStatus::PendingSubmit,
            filled: Quantity::ZERO,
            account_id: "main".into(),
            trace: Some(OrderTrace {
                strategy_id: Some("book-test".into()),
                signal_id: Some(1),
                intent_id: Some(1),
                rule_version: Some("v1".into()),
            }),
            policy: None,
        }
    }

    fn book() -> OrderBookSnapshot {
        OrderBookSnapshot {
            instrument: InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
            ts: 10,
            sequence: 1,
            bids: vec![BookLevel {
                price: Price::from_i64(99),
                qty: Quantity::from_i64(2),
            }],
            asks: vec![
                BookLevel {
                    price: Price::from_i64(100),
                    qty: Quantity::from_i64(1),
                },
                BookLevel {
                    price: Price::from_i64(101),
                    qty: Quantity::from_i64(2),
                },
            ],
        }
    }

    #[test]
    fn market_buy_consumes_multiple_ask_levels_and_keeps_partial_order() {
        let mut engine = OrderBookMatchingEngine::new(10).unwrap();
        engine
            .submit(order(Side::Buy, Quantity::from_i64(2).raw(), None))
            .unwrap();
        let fills = engine.on_snapshot(&book()).unwrap();
        assert_eq!(fills.len(), 2);
        assert_eq!(fills[0].price.raw(), Price::from_i64(100).raw());
        assert_eq!(fills[1].price.raw(), Price::from_i64(101).raw());
        assert_eq!(engine.pending_count(), 0);
    }

    #[test]
    fn limit_order_waits_when_best_price_is_not_executable() {
        let mut engine = OrderBookMatchingEngine::new(0).unwrap();
        engine
            .submit(order(Side::Buy, Quantity::from_i64(1).raw(), Some(99)))
            .unwrap();
        let fills = engine.on_snapshot(&book()).unwrap();
        assert!(fills.is_empty());
        assert_eq!(engine.pending_count(), 1);
    }

    #[test]
    fn one_snapshot_liquidity_is_not_reused_by_multiple_orders() {
        let mut engine = OrderBookMatchingEngine::new(0).unwrap();
        let mut first = order(Side::Buy, Quantity::from_i64(2).raw(), None);
        first.client_id = 1;
        let mut second = order(Side::Buy, Quantity::from_i64(2).raw(), None);
        second.client_id = 2;
        engine.submit(first).unwrap();
        engine.submit(second).unwrap();
        let fills = engine.on_snapshot(&book()).unwrap();
        assert_eq!(
            fills.iter().map(|fill| fill.qty.raw()).sum::<i128>(),
            Quantity::from_i64(3).raw()
        );
        assert_eq!(engine.pending_count(), 1);
    }

    #[test]
    fn execution_model_applies_latency_queue_and_market_impact() {
        let model = OrderBookExecutionModel {
            fee_bps: 0,
            latency_snapshots: 1,
            queue_position_bps: 5_000,
            market_impact_bps: 100,
        };
        let mut engine = OrderBookMatchingEngine::with_model(model).unwrap();
        engine
            .submit(order(Side::Buy, Quantity::from_i64(1).raw(), None))
            .unwrap();
        let first = book();
        assert!(engine.on_snapshot(&first).unwrap().is_empty());
        let second = OrderBookSnapshot {
            ts: 11,
            sequence: 2,
            ..first
        };
        let fills = engine.on_snapshot(&second).unwrap();
        assert_eq!(fills.len(), 1);
        assert_eq!(fills[0].price, Price::from_i64(101));
        assert_eq!(fills[0].qty, Quantity::from_i64(1));
    }

    #[test]
    fn cancellation_before_snapshot_prevents_fill_and_preserves_event_order() {
        let mut engine = OrderBookMatchingEngine::new(0).unwrap();
        engine
            .submit(order(Side::Buy, Quantity::from_i64(1).raw(), None))
            .unwrap();
        let fills = engine
            .on_snapshot_with_cancellations(&book(), &[1])
            .unwrap();
        assert!(fills.is_empty());
        assert_eq!(engine.pending_count(), 0);
        assert!(engine
            .on_snapshot(&OrderBookSnapshot {
                ts: 9,
                sequence: 2,
                ..book()
            })
            .is_err());
    }

    #[test]
    fn duplicate_order_and_non_monotonic_snapshot_are_rejected() {
        let mut engine = OrderBookMatchingEngine::new(0).unwrap();
        engine
            .submit(order(Side::Buy, Quantity::from_i64(1).raw(), None))
            .unwrap();
        assert!(engine
            .submit(order(Side::Buy, Quantity::from_i64(1).raw(), None))
            .is_err());
        engine.on_snapshot(&book()).unwrap();
        assert!(engine.on_snapshot(&book()).is_err());
    }
}
