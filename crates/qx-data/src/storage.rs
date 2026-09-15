//! qx-data storage abstraction.

use crate::incremental::merge_bars;
use crate::pipeline::process_bars;
use crate::schema::Bar;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

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

/// 单机数据集 JSON 文件存储。
///
/// 它只负责研究/回测数据集，不冒充运行时 EventLog；每次保存先写临时文件，
/// 再原子替换目标文件，避免 Provider 摄取过程中留下半份数据。
#[derive(Clone, Debug)]
pub struct JsonFileDataStorage {
    root: PathBuf,
}

impl JsonFileDataStorage {
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, String> {
        let root = root.into();
        std::fs::create_dir_all(&root)
            .map_err(|error| format!("create data storage root failed: {error}"))?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn path_for(&self, dataset: &str) -> Result<PathBuf, String> {
        if dataset.trim().is_empty()
            || dataset.contains('/')
            || dataset.contains('\\')
            || dataset.contains("..")
        {
            return Err("dataset id contains an unsafe path component".into());
        }
        Ok(self.root.join(format!("{dataset}.bars.json")))
    }
}

#[derive(Serialize, Deserialize)]
struct StoredBars {
    schema_version: u32,
    bars: Vec<Bar>,
}

impl DataStorage for JsonFileDataStorage {
    fn save_bars(&mut self, dataset: &str, bars: &[Bar]) -> Result<(), String> {
        let path = self.path_for(dataset)?;
        let (canonical, _) = process_bars(bars.to_vec())?;
        let payload = serde_json::to_vec_pretty(&StoredBars {
            schema_version: 1,
            bars: canonical,
        })
        .map_err(|error| format!("encode data storage {} failed: {error}", path.display()))?;
        let temporary = path.with_extension(format!("bars.json.tmp.{}", std::process::id()));
        std::fs::write(&temporary, payload).map_err(|error| {
            format!("write data storage {} failed: {error}", temporary.display())
        })?;
        if let Err(error) = std::fs::rename(&temporary, &path) {
            let _ = std::fs::remove_file(&path);
            std::fs::rename(&temporary, &path).map_err(|replacement| {
                format!(
                    "commit data storage {} failed: initial={error}; replacement={replacement}",
                    path.display()
                )
            })?;
        }
        Ok(())
    }

    fn load_bars(&self, dataset: &str) -> Result<Vec<Bar>, String> {
        let path = self.path_for(dataset)?;
        let payload = std::fs::read_to_string(&path)
            .map_err(|error| format!("read data storage {} failed: {error}", path.display()))?;
        let stored: StoredBars = serde_json::from_str(&payload)
            .map_err(|error| format!("decode data storage {} failed: {error}", path.display()))?;
        if stored.schema_version != 1 {
            return Err(format!(
                "unsupported data storage schema {}: {}",
                stored.schema_version,
                path.display()
            ));
        }
        process_bars(stored.bars).map(|(bars, _)| bars)
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

    #[test]
    fn json_file_storage_is_atomic_and_rejects_unsafe_dataset_ids() {
        let root = std::env::temp_dir().join(format!(
            "qianxing-data-file-storage-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut storage = JsonFileDataStorage::new(&root).unwrap();
        storage
            .save_bars("daily", &[bar(2, 20), bar(1, 10)])
            .unwrap();
        assert_eq!(storage.load_bars("daily").unwrap()[0].timestamp, 1);
        assert!(storage.save_bars("../escape", &[]).is_err());
        assert!(!root.join("daily.bars.json.tmp").exists());
        let _ = std::fs::remove_dir_all(root);
    }
}
