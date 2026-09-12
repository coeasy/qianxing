//! Deterministic in-memory cache for canonical datasets.

use crate::schema::Bar;
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct CacheKey {
    pub dataset_id: String,
    pub fingerprint: String,
}

impl CacheKey {
    pub fn new(
        dataset_id: impl Into<String>,
        fingerprint: impl Into<String>,
    ) -> Result<Self, String> {
        let key = Self {
            dataset_id: dataset_id.into(),
            fingerprint: fingerprint.into(),
        };
        if key.dataset_id.trim().is_empty() || key.fingerprint.trim().is_empty() {
            return Err("cache key requires dataset_id and fingerprint".into());
        }
        Ok(key)
    }
}

#[derive(Default)]
pub struct DataCache {
    bars: BTreeMap<CacheKey, Vec<Bar>>,
}

impl DataCache {
    pub fn put_bars(&mut self, key: CacheKey, bars: Vec<Bar>) {
        self.bars.insert(key, bars);
    }

    pub fn get_bars(&self, key: &CacheKey) -> Option<&[Bar]> {
        self.bars.get(key).map(Vec::as_slice)
    }

    pub fn invalidate_dataset(&mut self, dataset_id: &str) -> usize {
        let before = self.bars.len();
        self.bars.retain(|key, _| key.dataset_id != dataset_id);
        before - self.bars.len()
    }

    pub fn len(&self) -> usize {
        self.bars.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bars.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(ts: u64) -> Bar {
        Bar {
            instrument: "XSHG:600000".into(),
            timestamp: ts,
            open_raw: 10,
            high_raw: 12,
            low_raw: 9,
            close_raw: 11,
            volume_raw: 100,
        }
    }

    #[test]
    fn cache_is_versioned_by_fingerprint() {
        let mut cache = DataCache::default();
        let v1 = CacheKey::new("bars.daily", "fp-v1").unwrap();
        let v2 = CacheKey::new("bars.daily", "fp-v2").unwrap();
        cache.put_bars(v1.clone(), vec![bar(1)]);
        cache.put_bars(v2.clone(), vec![bar(2)]);

        assert_eq!(cache.get_bars(&v1).unwrap()[0].timestamp, 1);
        assert_eq!(cache.get_bars(&v2).unwrap()[0].timestamp, 2);
        assert_eq!(cache.invalidate_dataset("bars.daily"), 2);
        assert!(cache.is_empty());
    }
}
