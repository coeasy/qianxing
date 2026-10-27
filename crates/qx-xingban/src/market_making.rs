//! Deterministic L2 two-sided market-making quotes for shared backtests.

use crate::{OrderBookActions, OrderBookSnapshot, OrderBookStrategy};
use qx_core::{InstrumentId, Order, OrderPolicy, OrderStatus, OrderTrace, Price, Quantity, Side};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarketMakingConfig {
    /// Total quoted spread in basis points around the inventory-adjusted center.
    pub spread_bps: u32,
    pub quote_qty_raw: i128,
    pub max_inventory_raw: i128,
    pub tick_size_raw: i128,
    /// Maximum center shift at the configured inventory bound.
    pub inventory_skew_bps: u32,
}

impl MarketMakingConfig {
    pub fn validate(&self) -> Result<(), String> {
        if !(2..=10_000).contains(&self.spread_bps) {
            return Err("market_making.spread_bps 必须在 2..=10000 内".into());
        }
        if self.quote_qty_raw <= 0 || self.max_inventory_raw <= 0 || self.tick_size_raw <= 0 {
            return Err("market_making 数量、库存上限和 tick_size 必须为正".into());
        }
        if self.inventory_skew_bps > 10_000 {
            return Err("market_making.inventory_skew_bps 必须在 0..=10000 内".into());
        }
        Ok(())
    }
}

pub struct MarketMakingStrategy {
    config: MarketMakingConfig,
    instrument: InstrumentId,
    account_id: String,
    strategy_id: String,
    policy: Option<OrderPolicy>,
}

impl MarketMakingStrategy {
    pub fn new(
        config: MarketMakingConfig,
        instrument: InstrumentId,
        account_id: impl Into<String>,
        strategy_id: impl Into<String>,
        policy: Option<OrderPolicy>,
    ) -> Result<Self, String> {
        config.validate()?;
        let account_id = account_id.into();
        let strategy_id = strategy_id.into();
        if account_id.trim().is_empty() || strategy_id.trim().is_empty() {
            return Err("market_making account_id/strategy_id 不能为空".into());
        }
        Ok(Self {
            config,
            instrument,
            account_id,
            strategy_id,
            policy,
        })
    }

    fn align_down(&self, raw: i128) -> Result<i128, String> {
        raw.checked_div(self.config.tick_size_raw)
            .and_then(|ticks| ticks.checked_mul(self.config.tick_size_raw))
            .ok_or_else(|| "market-making bid tick 对齐溢出".into())
    }

    fn align_up(&self, raw: i128) -> Result<i128, String> {
        let tick = self.config.tick_size_raw;
        let rounded = raw
            .checked_add(tick - 1)
            .and_then(|value| value.checked_div(tick))
            .and_then(|ticks| ticks.checked_mul(tick));
        rounded.ok_or_else(|| "market-making ask tick 对齐溢出".into())
    }
}

impl OrderBookStrategy for MarketMakingStrategy {
    fn on_order_book(
        &mut self,
        snapshot: &OrderBookSnapshot,
        position: i128,
    ) -> Result<Vec<Order>, String> {
        Ok(self.on_order_book_actions(snapshot, position, &[])?.orders)
    }

    fn on_order_book_actions(
        &mut self,
        snapshot: &OrderBookSnapshot,
        position: i128,
        open_order_ids: &[u64],
    ) -> Result<OrderBookActions, String> {
        if snapshot.instrument != self.instrument {
            return Err("market-making 快照标的与策略绑定标的不一致".into());
        }
        let (Some(bid), Some(ask)) = (snapshot.bids.first(), snapshot.asks.first()) else {
            return Ok(OrderBookActions {
                orders: Vec::new(),
                cancel_client_ids: open_order_ids.to_vec(),
            });
        };
        if bid.price.raw() >= ask.price.raw() {
            return Err("market-making 要求有效的非交叉 L2 最优报价".into());
        }
        let mid = bid
            .price
            .raw()
            .checked_add(ask.price.raw())
            .and_then(|sum| sum.checked_div(2))
            .ok_or_else(|| "market-making 中间价计算溢出".to_string())?;
        let bounded_position = position.clamp(
            -self.config.max_inventory_raw,
            self.config.max_inventory_raw,
        );
        let shift = mid
            .checked_mul(i128::from(self.config.inventory_skew_bps))
            .and_then(|value| value.checked_div(10_000))
            .and_then(|value| value.checked_mul(bounded_position))
            .and_then(|value| value.checked_div(self.config.max_inventory_raw))
            .ok_or_else(|| "market-making 库存偏移计算溢出".to_string())?;
        let center = mid
            .checked_sub(shift)
            .ok_or_else(|| "market-making 中心价计算溢出".to_string())?;
        let half_spread = mid
            .checked_mul(i128::from(self.config.spread_bps))
            .and_then(|value| value.checked_div(20_000))
            .ok_or_else(|| "market-making 点差计算溢出".to_string())?;
        let mut quote_bid = self.align_down(
            center
                .checked_sub(half_spread)
                .ok_or_else(|| "market-making bid 价格计算溢出".to_string())?,
        )?;
        let mut quote_ask = self.align_up(
            center
                .checked_add(half_spread)
                .ok_or_else(|| "market-making ask 价格计算溢出".to_string())?,
        )?;
        // Never cross the visible opposite best; preserve at least one tick of separation.
        let max_bid = ask
            .price
            .raw()
            .checked_sub(self.config.tick_size_raw)
            .ok_or_else(|| "market-making bid 被盘口限制为非正".to_string())?;
        let min_ask = bid
            .price
            .raw()
            .checked_add(self.config.tick_size_raw)
            .ok_or_else(|| "market-making ask 盘口限制溢出".to_string())?;
        quote_bid = quote_bid.min(self.align_down(max_bid)?);
        quote_ask = quote_ask.max(self.align_up(min_ask)?);
        if quote_bid <= 0 || quote_ask <= quote_bid {
            return Ok(OrderBookActions {
                orders: Vec::new(),
                cancel_client_ids: open_order_ids.to_vec(),
            });
        }

        let mut orders = Vec::with_capacity(2);
        let buy_capacity = self
            .config
            .max_inventory_raw
            .saturating_sub(position)
            .max(0);
        let sell_capacity = self
            .config
            .max_inventory_raw
            .saturating_add(position)
            .max(0);
        if buy_capacity > 0 {
            orders.push(self.order(
                snapshot.sequence,
                Side::Buy,
                quote_bid,
                buy_capacity.min(self.config.quote_qty_raw),
            )?);
        }
        if sell_capacity > 0 {
            orders.push(self.order(
                snapshot.sequence,
                Side::Sell,
                quote_ask,
                sell_capacity.min(self.config.quote_qty_raw),
            )?);
        }
        Ok(OrderBookActions {
            orders,
            cancel_client_ids: open_order_ids.to_vec(),
        })
    }
}

impl MarketMakingStrategy {
    fn order(&self, sequence: u64, side: Side, price: i128, qty: i128) -> Result<Order, String> {
        let base = sequence
            .checked_mul(2)
            .ok_or_else(|| "market-making client id 溢出".to_string())?;
        let client_id = base
            .checked_add(u64::from(side == Side::Sell))
            .ok_or_else(|| "market-making client id 溢出".to_string())?;
        Ok(Order {
            client_id,
            instrument: self.instrument.clone(),
            side,
            qty: Quantity::from_raw(qty),
            limit: Some(Price::from_raw(price)),
            status: OrderStatus::Submitted,
            filled: Quantity::ZERO,
            account_id: self.account_id.clone(),
            trace: Some(OrderTrace {
                strategy_id: Some(self.strategy_id.clone()),
                signal_id: Some(sequence),
                intent_id: Some(client_id),
                rule_version: Some("market-making-v1".into()),
            }),
            policy: self.policy,
            exact_amounts: None,
        })
    }
}

#[cfg(test)]
#[path = "market_making/tests.rs"]
mod tests;
