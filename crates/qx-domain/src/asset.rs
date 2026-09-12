use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum AssetClass {
    Equity,
    Future,
    Option,
    Etf,
    Crypto,
    Currency,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Instrument {
    pub id: String,
    pub symbol: String,
    pub asset_class: AssetClass,
    pub venue: String,
}
