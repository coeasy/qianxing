use super::super::funding::SpotCashReservations;
use crate::orderbook_strategies::OrderBookStrategy;
use crate::{OrderBookExecutionModel, OrderBookMatchingEngine, OrderBookSnapshot};
use qx_core::{EventLog, Fill, Ledger, Money, Price, TradingInstrumentSpec};
use std::collections::BTreeMap;

pub struct OrderBookPortfolioMarket<'a> {
    pub instrument_spec: TradingInstrumentSpec,
    pub priority: u32,
    pub snapshots: &'a [OrderBookSnapshot],
    pub execution_model: OrderBookExecutionModel,
    pub strategy: &'a mut dyn OrderBookStrategy,
}

pub struct OrderBookPortfolioBacktestEngine<'a> {
    pub(super) account_id: String,
    pub(super) currency: String,
    pub(super) initial_cash: Money,
    pub(super) risk: qx_zhenlu::RiskGate,
    pub(super) markets: Vec<OrderBookPortfolioMarket<'a>>,
}

pub struct OrderBookPortfolioBacktestReport {
    pub fills: Vec<Fill>,
    pub fills_by_instrument: BTreeMap<qx_core::InstrumentId, Vec<Fill>>,
    pub ledger: Ledger,
    pub event_log: EventLog,
    pub equity: Vec<(u64, i128)>,
    pub pending_orders: usize,
    pub fees_raw: i128,
    pub turnover_raw: i128,
    pub return_bps: i32,
    pub max_drawdown_bps: u32,
    pub input_data_hash: u64,
    pub clock_start: u64,
    pub clock_end: u64,
}

impl OrderBookPortfolioBacktestReport {
    pub fn result_hash(&self) -> u64 {
        self.event_log.digest()
    }

    pub fn verify(&self) -> Result<(), qx_core::QxError> {
        self.event_log.validate()?;
        qx_core::ReplayVerifier::verify(self.event_log.events(), &self.ledger)?;
        Ok(())
    }
}

pub(super) struct MarketRuntime<'a> {
    pub(super) instrument: qx_core::InstrumentId,
    pub(super) spec: TradingInstrumentSpec,
    pub(super) fee_bps: i128,
    pub(super) strategy: &'a mut dyn OrderBookStrategy,
    pub(super) matcher: OrderBookMatchingEngine,
    pub(super) reservations: SpotCashReservations,
    pub(super) cancellations_for_next_snapshot: Vec<u64>,
}

pub(super) fn total_reserved_cash(markets: &[MarketRuntime<'_>]) -> Result<i128, qx_core::QxError> {
    markets.iter().try_fold(0_i128, |total, market| {
        total
            .checked_add(market.reservations.total()?)
            .ok_or_else(|| qx_core::QxError::Invariant("共享盘口现金预留求和溢出".into()))
    })
}

pub(super) fn reference_price(snapshot: &OrderBookSnapshot) -> Option<Price> {
    match (
        snapshot.bids.first().map(|level| level.price),
        snapshot.asks.first().map(|level| level.price),
    ) {
        (Some(bid), Some(ask)) => Some(Price::from_raw(bid.raw().saturating_add(ask.raw()) / 2)),
        (Some(bid), None) => Some(bid),
        (None, Some(ask)) => Some(ask),
        (None, None) => None,
    }
}
