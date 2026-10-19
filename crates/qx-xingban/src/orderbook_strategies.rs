//! Strategy adapters and cancel/replace actions for the shared L2 backtest.

use crate::OrderBookSnapshot;
use qx_core::{Order, Price};

pub trait OrderBookStrategy {
    fn update_account_state(
        &mut self,
        _cash: &std::collections::BTreeMap<String, i128>,
        _available_margin_raw: Option<i128>,
    ) {
    }
    fn on_order_book(
        &mut self,
        snapshot: &OrderBookSnapshot,
        position: i128,
    ) -> Result<Vec<Order>, String>;

    /// Stateful L2 strategies may cancel resting orders before the next snapshot is matched.
    /// Existing order-only strategies inherit this compatibility adapter.
    fn on_order_book_actions(
        &mut self,
        snapshot: &OrderBookSnapshot,
        position: i128,
        _open_order_ids: &[u64],
    ) -> Result<OrderBookActions, String> {
        Ok(OrderBookActions {
            orders: self.on_order_book(snapshot, position)?,
            cancel_client_ids: Vec::new(),
        })
    }

    /// Decision-time buying power for spot sizing. The engine supplies a value
    /// only when it can derive a supported spot cash snapshot; derivative
    /// margin remains unavailable until a certified free-margin model exists.
    fn on_order_book_actions_with_margin(
        &mut self,
        snapshot: &OrderBookSnapshot,
        position: i128,
        open_order_ids: &[u64],
        _available_margin_raw: Option<i128>,
    ) -> Result<OrderBookActions, String> {
        self.on_order_book_actions(snapshot, position, open_order_ids)
    }

    /// Strategy portfolios may enforce per-member capital before an order reaches OMS.
    fn check_order_budget(
        &self,
        _order: &Order,
        _reference_price: i128,
        _instrument_spec: Option<&qx_core::TradingInstrumentSpec>,
    ) -> Result<(), qx_core::QxError> {
        Ok(())
    }

    /// Validate the actual fill price before the authoritative ledger is updated.
    fn validate_fill_budget(
        &self,
        _order: &Order,
        _fill: &qx_core::Fill,
        _instrument_spec: Option<&qx_core::TradingInstrumentSpec>,
    ) -> Result<(), qx_core::QxError> {
        Ok(())
    }

    fn on_order_accepted(
        &mut self,
        _order: &Order,
        _reference_price: Option<i128>,
    ) -> Result<(), qx_core::QxError> {
        Ok(())
    }

    fn on_order_cancelled(&mut self, _order: &Order) -> Result<(), qx_core::QxError> {
        Ok(())
    }

    /// Receives fills only after OMS and Ledger accepted the fill.
    fn on_fill(&mut self, _order: &Order, _fill: &qx_core::Fill) -> Result<(), qx_core::QxError> {
        Ok(())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OrderBookActions {
    pub orders: Vec<Order>,
    pub cancel_client_ids: Vec<u64>,
}

/// Adapts the shared Rust strategy protocol to full order-book events.
pub struct NativeOrderBookStrategy<S: qx_strategy::Strategy> {
    pub strategy: S,
    pub context: qx_strategy::StrategyContext,
    data_tier: crate::DataTier,
    pending_order_updates: Vec<Order>,
}

impl<S: qx_strategy::Strategy> NativeOrderBookStrategy<S> {
    pub fn new(strategy: S, context: qx_strategy::StrategyContext) -> Self {
        Self {
            strategy,
            context,
            data_tier: crate::DataTier::L2L3,
            pending_order_updates: Vec::new(),
        }
    }

    /// Emit the L1 quote contract while retaining the shared order-book matcher.
    /// L1 callers must supply exactly one bid and one ask on every snapshot.
    pub fn with_data_tier(mut self, data_tier: crate::DataTier) -> Self {
        self.data_tier = data_tier;
        self
    }
}

impl<S: qx_strategy::Strategy> OrderBookStrategy for NativeOrderBookStrategy<S> {
    fn update_account_state(
        &mut self,
        cash: &std::collections::BTreeMap<String, i128>,
        available_margin_raw: Option<i128>,
    ) {
        self.context.cash.clone_from(cash);
        self.context.available_margin_raw = available_margin_raw;
    }

    fn on_order_book(
        &mut self,
        snapshot: &OrderBookSnapshot,
        position: i128,
    ) -> Result<Vec<Order>, String> {
        self.context.as_of = snapshot.ts;
        self.context
            .positions
            .insert(snapshot.instrument.to_string(), position);
        let event = match self.data_tier {
            crate::DataTier::L1 => {
                if !crate::orderbook::has_l1_shape(snapshot) {
                    return Err(format!(
                        "L1 Tick 事件只接受一档 bid/ask，实际 bid={} ask={}",
                        snapshot.bids.len(),
                        snapshot.asks.len()
                    ));
                }
                let bid = &snapshot.bids[0];
                let ask = &snapshot.asks[0];
                qx_strategy::MarketEvent::Tick {
                    instrument: snapshot.instrument.clone(),
                    ts: snapshot.ts,
                    bid_raw: bid.price.raw(),
                    ask_raw: ask.price.raw(),
                    last_raw: Some(bid.price.raw().saturating_add(ask.price.raw()) / 2),
                    volume_raw: Some(bid.qty.raw().saturating_add(ask.qty.raw())),
                }
            }
            crate::DataTier::Bar => return Err("订单簿策略不能消费 Bar 数据档位".into()),
            crate::DataTier::L2L3 => qx_strategy::MarketEvent::OrderBook {
                instrument: snapshot.instrument.clone(),
                ts: snapshot.ts,
                sequence: snapshot.sequence,
                bids: snapshot
                    .bids
                    .iter()
                    .map(|level| qx_strategy::OrderBookLevel {
                        price_raw: level.price.raw(),
                        qty_raw: level.qty.raw(),
                    })
                    .collect(),
                asks: snapshot
                    .asks
                    .iter()
                    .map(|level| qx_strategy::OrderBookLevel {
                        price_raw: level.price.raw(),
                        qty_raw: level.qty.raw(),
                    })
                    .collect(),
            },
        };
        let decision = self.strategy.on_event(&self.context, &event)?;
        decision.to_orders(&self.context)
    }

    fn on_order_book_actions_with_margin(
        &mut self,
        snapshot: &OrderBookSnapshot,
        position: i128,
        open_order_ids: &[u64],
        available_margin_raw: Option<i128>,
    ) -> Result<OrderBookActions, String> {
        self.context.available_margin_raw = available_margin_raw;
        let _ = open_order_ids;
        let mut orders = std::mem::take(&mut self.pending_order_updates);
        orders.extend(self.on_order_book(snapshot, position)?);
        Ok(OrderBookActions {
            orders,
            cancel_client_ids: Vec::new(),
        })
    }

    fn on_fill(&mut self, order: &Order, fill: &qx_core::Fill) -> Result<(), qx_core::QxError> {
        deliver_order_update(
            &mut self.strategy,
            &mut self.context,
            &mut self.pending_order_updates,
            order,
            fill,
        )
    }
}

/// Folds L1/L2 snapshots into deterministic Bar events for bar-only strategies.
pub struct DepthBarStrategy<S: qx_strategy::Strategy> {
    pub strategy: S,
    pub context: qx_strategy::StrategyContext,
    pub instrument: qx_core::InstrumentId,
    pending_order_updates: Vec<Order>,
}

impl<S: qx_strategy::Strategy> DepthBarStrategy<S> {
    pub fn new(
        strategy: S,
        context: qx_strategy::StrategyContext,
        instrument: qx_core::InstrumentId,
    ) -> Self {
        Self {
            strategy,
            context,
            instrument,
            pending_order_updates: Vec::new(),
        }
    }

    pub fn initialize(&mut self) -> Result<(), String> {
        self.strategy.on_init(&self.context)
    }

    pub(crate) fn on_mid(
        &mut self,
        instrument: &qx_core::InstrumentId,
        ts: u64,
        position: i128,
        mid: Price,
        volume_raw: i128,
        available_margin_raw: Option<i128>,
    ) -> Result<Vec<Order>, String> {
        self.context.as_of = ts;
        self.context.available_margin_raw = available_margin_raw;
        self.context
            .positions
            .insert(instrument.to_string(), position);
        let mid_raw = mid.raw();
        let event = qx_strategy::MarketEvent::Bar {
            instrument: instrument.clone(),
            ts,
            open_raw: mid_raw,
            high_raw: mid_raw,
            low_raw: mid_raw,
            close_raw: mid_raw,
            volume_raw,
        };
        let decision = self.strategy.on_event(&self.context, &event)?;
        decision.to_orders(&self.context)
    }
}

impl<S: qx_strategy::Strategy> OrderBookStrategy for DepthBarStrategy<S> {
    fn update_account_state(
        &mut self,
        cash: &std::collections::BTreeMap<String, i128>,
        available_margin_raw: Option<i128>,
    ) {
        self.context.cash.clone_from(cash);
        self.context.available_margin_raw = available_margin_raw;
    }

    fn on_order_book(
        &mut self,
        snapshot: &OrderBookSnapshot,
        position: i128,
    ) -> Result<Vec<Order>, String> {
        let (Some(bid), Some(ask)) = (snapshot.bids.first(), snapshot.asks.first()) else {
            return Ok(Vec::new());
        };
        let mid = Price::from_raw(bid.price.raw().saturating_add(ask.price.raw()) / 2);
        let volume_raw = snapshot
            .bids
            .iter()
            .chain(snapshot.asks.iter())
            .fold(0_i128, |total, level| total.saturating_add(level.qty.raw()));
        let mut orders = std::mem::take(&mut self.pending_order_updates);
        orders.extend(self.on_mid(
            &snapshot.instrument,
            snapshot.ts,
            position,
            mid,
            volume_raw,
            None,
        )?);
        Ok(orders)
    }

    fn on_order_book_actions_with_margin(
        &mut self,
        snapshot: &OrderBookSnapshot,
        position: i128,
        _open_order_ids: &[u64],
        available_margin_raw: Option<i128>,
    ) -> Result<OrderBookActions, String> {
        let (Some(bid), Some(ask)) = (snapshot.bids.first(), snapshot.asks.first()) else {
            return Ok(OrderBookActions {
                orders: std::mem::take(&mut self.pending_order_updates),
                cancel_client_ids: Vec::new(),
            });
        };
        let mid = Price::from_raw(bid.price.raw().saturating_add(ask.price.raw()) / 2);
        let volume_raw = snapshot
            .bids
            .iter()
            .chain(snapshot.asks.iter())
            .fold(0_i128, |total, level| total.saturating_add(level.qty.raw()));
        let mut orders = std::mem::take(&mut self.pending_order_updates);
        orders.extend(self.on_mid(
            &snapshot.instrument,
            snapshot.ts,
            position,
            mid,
            volume_raw,
            available_margin_raw,
        )?);
        Ok(OrderBookActions {
            orders,
            cancel_client_ids: Vec::new(),
        })
    }

    fn on_fill(&mut self, order: &Order, fill: &qx_core::Fill) -> Result<(), qx_core::QxError> {
        deliver_order_update(
            &mut self.strategy,
            &mut self.context,
            &mut self.pending_order_updates,
            order,
            fill,
        )
    }
}

fn deliver_order_update<S: qx_strategy::Strategy>(
    strategy: &mut S,
    context: &mut qx_strategy::StrategyContext,
    pending: &mut Vec<Order>,
    order: &Order,
    fill: &qx_core::Fill,
) -> Result<(), qx_core::QxError> {
    let owned = order
        .trace
        .as_ref()
        .and_then(|trace| trace.strategy_id.as_deref())
        == Some(context.strategy_id.as_str());
    if !owned {
        return Ok(());
    }
    context.as_of = context.as_of.max(fill.ts);
    let key = order.instrument.to_string();
    let current = context.positions.get(&key).copied().unwrap_or_default();
    let delta = if order.side == qx_core::Side::Buy {
        fill.qty.raw()
    } else {
        -fill.qty.raw()
    };
    context.positions.insert(key, current.saturating_add(delta));
    let update = serde_json::json!({
        "schema_version": qx_strategy::ORDER_UPDATE_SCHEMA_VERSION,
        "event": "fill",
        "order": { "order_id": order.client_id, "account_id": order.account_id,
            "instrument": order.instrument.to_string(), "side": order.side,
            "qty_raw": order.qty.raw(), "filled_raw": order.filled.raw(),
            "strategy_id": context.strategy_id },
        "fill": fill,
    });
    if let Some(decision) = strategy
        .on_order_update(context, &update.to_string())
        .map_err(qx_core::QxError::BusinessViolation)?
    {
        pending.extend(
            decision
                .to_orders(context)
                .map_err(qx_core::QxError::BusinessViolation)?,
        );
    }
    Ok(())
}
