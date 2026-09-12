use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TradingSession {
    pub market: String,
    pub open: u64,
    pub close: u64,
}

pub trait TradingCalendar {
    fn market(&self) -> &str;

    fn is_open(&self, timestamp: u64) -> bool;

    fn sessions(&self, start: u64, end: u64) -> Vec<TradingSession>;
}
