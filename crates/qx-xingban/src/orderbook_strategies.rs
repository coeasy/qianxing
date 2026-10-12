//! Strategy adapters and cancel/replace actions for the shared L2 backtest.

use crate::OrderBookSnapshot;
use qx_core::{Order, Price};

pub trait OrderBookStrategy {
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
}

impl<S: qx_strategy::Strategy> NativeOrderBookStrategy<S> {
    pub fn new(strategy: S, context: qx_strategy::StrategyContext) -> Self {
        Self { strategy, context }
    }
}

impl<S: qx_strategy::Strategy> OrderBookStrategy for NativeOrderBookStrategy<S> {
    fn on_order_book(
        &mut self,
        snapshot: &OrderBookSnapshot,
        position: i128,
    ) -> Result<Vec<Order>, String> {
        self.context.as_of = snapshot.ts;
        self.context
            .positions
            .insert(snapshot.instrument.to_string(), position);
        let event = qx_strategy::MarketEvent::OrderBook {
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
        };
        let decision = self.strategy.on_event(&self.context, &event)?;
        decision.to_orders(&self.context)
    }
}

/// Folds L1/L2 snapshots into deterministic Bar events for bar-only strategies.
pub struct DepthBarStrategy<S: qx_strategy::Strategy> {
    pub strategy: S,
    pub context: qx_strategy::StrategyContext,
    pub instrument: qx_core::InstrumentId,
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
    ) -> Result<Vec<Order>, String> {
        self.context.as_of = ts;
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
        self.on_mid(&snapshot.instrument, snapshot.ts, position, mid, volume_raw)
    }
}
