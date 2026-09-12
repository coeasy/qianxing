//! qx-data storage abstraction.

use crate::incremental::merge_bars;
use crate::pipeline::process_bars;
use crate::schema::Bar;

pub trait DataStorage {
    fn save_bars(&mut self, dataset: &str, bars: &[Bar]) -> Result<(), String>;

    fn load_bars(&self, dataset: &str) -> Result<Vec<Bar>, String>;

    fn load_range(
        &self,
        dataset: &str,
        instrument: &str,
        start: u64,
        end: u64,
    ) -> Result<Vec<Bar>, String> {
        if instrument.trim().is_empty() || start == 0 || start > end {
            return Err("invalid storage range request".into());
        }
        Ok(self
            .load_bars(dataset)?
            .into_iter()
            .filter(|bar| {
                bar.instrument == instrument && bar.timestamp >= start && bar.timestamp <= end
            })
            .collect())
    }

    fn upsert_bars(&mut self, dataset: &str, incoming: &[Bar]) -> Result<(), String> {
        let existing = self.load_bars(dataset).unwrap_or_default();
        let (merged, _) = merge_bars(&existing, incoming)?;
        self.save_bars(dataset, &merged)
    }
}

#[derive(Default)]
pub struct MemoryDataStorage {
    bars: std::collections::BTreeMap<String, Vec<Bar>>,
}

impl DataStorage for MemoryDataStorage {
    fn save_bars(&mut self, dataset: &str, bars: &[Bar]) -> Result<(), String> {
        if dataset.trim().is_empty() {
            return Err("dataset id is required".into());
        }
        let (canonical, _) = process_bars(bars.to_vec())?;
        self.bars.insert(dataset.to_string(), canonical);
        Ok(())
    }

    fn load_bars(&self, dataset: &str) -> Result<Vec<Bar>, String> {
        self.bars
            .get(dataset)
            .cloned()
            .ok_or_else(|| format!("dataset not found: {dataset}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(ts: u64, close_raw: i128) -> Bar {
        Bar {
            instrument: "XSHG:600000".into(),
            timestamp: ts,
            open_raw: close_raw,
            high_raw: close_raw,
            low_raw: close_raw,
            close_raw,
            volume_raw: 1,
        }
    }

    #[test]
    fn save_is_canonical_and_range_reads_are_bounded() {
        let mut storage = MemoryDataStorage::default();
        storage
            .save_bars("daily", &[bar(3, 30), bar(1, 10), bar(2, 20)])
            .unwrap();
        let all = storage.load_bars("daily").unwrap();
        assert_eq!(all[0].timestamp, 1);
        let range = storage.load_range("daily", "XSHG:600000", 2, 3).unwrap();
        assert_eq!(range.len(), 2);
        assert_eq!(range[0].timestamp, 2);
    }

    #[test]
    fn upsert_replaces_existing_identity() {
        let mut storage = MemoryDataStorage::default();
        storage.save_bars("daily", &[bar(1, 10)]).unwrap();
        storage
            .upsert_bars("daily", &[bar(1, 11), bar(2, 20)])
            .unwrap();
        let bars = storage.load_bars("daily").unwrap();
        assert_eq!(bars.len(), 2);
        assert_eq!(bars[0].close_raw, 11);
    }
}
