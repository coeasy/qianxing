use qx_core::ExactOhlcv;
use serde::{Deserialize, Serialize};

pub const DATA_SCHEMA_VERSION: u32 = 2;

pub type Timestamp = u64;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Bar {
    pub instrument: String,
    pub timestamp: Timestamp,
    pub open_raw: i128,
    pub high_raw: i128,
    pub low_raw: i128,
    pub close_raw: i128,
    pub volume_raw: i128,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exact_ohlcv: Option<ExactOhlcv>,
}

impl Bar {
    pub fn validate(&self) -> Result<(), String> {
        if self.instrument.trim().is_empty() {
            return Err("missing instrument".into());
        }
        if self.timestamp == 0 {
            return Err("invalid timestamp".into());
        }
        if let Some(exact) = self.exact_ohlcv {
            exact.validate()?;
            let raw = [
                self.open_raw,
                self.high_raw,
                self.low_raw,
                self.close_raw,
                self.volume_raw,
            ];
            for (raw, value) in raw.into_iter().zip(exact.raw_projection()?) {
                if value != raw {
                    return Err("exact_ohlcv raw projection mismatch".into());
                }
            }
        }
        Ok(())
    }
}
