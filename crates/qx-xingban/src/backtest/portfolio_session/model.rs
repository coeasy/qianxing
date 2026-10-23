use crate::cost::{LatencyModel, MarginRule};
use crate::fill::FillModel;
use qx_core::{
    EventLog, FeeModel, Fill, FundingRateEvent, InstrumentId, Ledger, Money, TradingInstrumentSpec,
};
use qx_guanxing::Bar;
use qx_zhenlu::RiskGate;
use std::collections::BTreeMap;

pub struct PortfolioBarMarket<'a> {
    pub instrument: InstrumentId,
    pub priority: u32,
    pub bars: &'a [Bar],
    pub spec: TradingInstrumentSpec,
    pub fill: Box<dyn FillModel>,
    pub fee: Box<dyn FeeModel>,
    pub latency: Box<dyn LatencyModel>,
    pub margin: Box<dyn MarginRule>,
    pub seed: u64,
}

pub struct PortfolioBarSessionConfig {
    pub account_id: String,
    pub currency: String,
    pub initial_cash: Money,
    pub risk: RiskGate,
    /// Constant funding rate in basis points per UTC 8-hour settlement, keyed by derivative.
    pub funding_bps: BTreeMap<InstrumentId, i64>,
    /// Optional variable historical funding rates at explicit UTC 8-hour settlements.
    pub funding_events: Vec<FundingRateEvent>,
}

pub struct PortfolioBarSessionReport {
    pub fills: Vec<Fill>,
    pub fills_by_instrument: BTreeMap<InstrumentId, Vec<Fill>>,
    pub rejections_by_instrument: BTreeMap<InstrumentId, EventLog>,
    pub equity: Vec<(u64, i128)>,
    /// Bounded samples of each instrument's marked P&L contribution above initial cash.
    /// Each point is `(equity_index, timestamp, contribution_raw)`; all curves share one axis.
    pub instrument_equity_contributions: BTreeMap<InstrumentId, Vec<(u64, u64, i128)>>,
    pub initial_cash_raw: i128,
    pub ledger: Ledger,
    pub event_log: EventLog,
    pub market_specs: BTreeMap<InstrumentId, TradingInstrumentSpec>,
    pub funding_by_instrument_raw: BTreeMap<InstrumentId, i128>,
    pub funding_events_by_instrument: BTreeMap<InstrumentId, Vec<(u64, i128)>>,
    pub clock_start: u64,
    pub clock_end: u64,
}

pub struct PortfolioBarEngine<'a> {
    pub(super) config: PortfolioBarSessionConfig,
    pub(super) markets: Vec<PortfolioBarMarket<'a>>,
}
