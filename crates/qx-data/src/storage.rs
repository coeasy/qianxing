//! qx-data storage abstraction.

use crate::schema::Bar;

pub trait DataStorage {
    fn save_bars(&mut self, dataset: &str, bars: &[Bar]) -> Result<(), String>;

    fn load_bars(&self, dataset: &str) -> Result<Vec<Bar>, String>;
}

#[derive(Default)]
pub struct MemoryDataStorage {
    bars: std::collections::BTreeMap<String, Vec<Bar>>,
}

impl DataStorage for MemoryDataStorage {
    fn save_bars(&mut self, dataset: &str, bars: &[Bar]) -> Result<(), String> {
        self.bars.insert(dataset.to_string(), bars.to_vec());
        Ok(())
    }

    fn load_bars(&self, dataset: &str) -> Result<Vec<Bar>, String> {
        self.bars
            .get(dataset)
            .cloned()
            .ok_or_else(|| format!("dataset not found: {dataset}"))
    }
}
