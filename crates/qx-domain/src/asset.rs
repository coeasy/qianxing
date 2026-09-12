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

impl Instrument {
    pub fn validate(&self) -> Result<(), String> {
        if self.id.trim().is_empty() {
            return Err("instrument id is required".into());
        }
        if self.symbol.trim().is_empty() {
            return Err("instrument symbol is required".into());
        }
        if self.venue.trim().is_empty() {
            return Err("instrument venue is required".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instrument_requires_stable_identity() {
        let instrument = Instrument {
            id: "".into(),
            symbol: "600000".into(),
            asset_class: AssetClass::Equity,
            venue: "XSHG".into(),
        };
        assert!(instrument.validate().is_err());
    }
}
