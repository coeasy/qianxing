use serde::{Deserialize, Serialize};

use crate::schema::Bar;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProviderMetadata {
    pub name: String,
    pub version: String,
}

pub trait DataProvider {
    fn metadata(&self) -> ProviderMetadata;

    fn load_bars(&self, instrument: &str, start: u64, end: u64) -> Result<Vec<Bar>, String>;
}
