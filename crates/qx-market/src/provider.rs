use qx_domain::{Instrument, Tick};

/// Canonical market provider boundary.
///
/// Implementations may connect exchanges, brokers or historical datasets.
pub trait MarketProvider {
    fn name(&self) -> &str;

    fn next_tick(&mut self) -> Option<MarketTick>;
}

#[derive(Debug, Clone)]
pub struct MarketTick {
    pub instrument: Instrument,
    pub tick: Tick,
}
