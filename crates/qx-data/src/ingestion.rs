//! Provider 到本地数据集的确定性摄取边界。
//!
//! 外部 Provider 只返回带来源的标准 Bar；本模块负责把它们幂等合并到
//! `DataStorage`，再用最终数据集计算并注册 `DatasetManifest`。数据库、文件和
//! 内存存储都复用同一套流程，避免 A 股、CCXT 和离线 BarFrame 各自实现缓存语义。

use crate::catalog::DatasetManifest;
use crate::fingerprint::fingerprint_bars;
use crate::provider::DataProvider;
use crate::registry::DatasetRegistrar;
use crate::storage::DataStorage;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IngestionReport {
    pub dataset_id: String,
    pub dataset_version: String,
    pub incoming_rows: usize,
    pub stored_rows: usize,
    pub manifest: DatasetManifest,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IngestionRequest {
    pub dataset_id: String,
    pub dataset_version: String,
    pub instrument: String,
    pub start: u64,
    pub end: u64,
}

/// 将一个 Provider 的指定标的/时间范围幂等摄取到本地数据集。
///
/// `dataset_version` 必须由调用方绑定到源快照或发布版本；同一版本产生不同
/// fingerprint 时，`DatasetRegistry` 会拒绝覆盖，防止回测输入被静默替换。
pub fn ingest_bars<P: DataProvider, S: DataStorage, R: DatasetRegistrar>(
    provider: &P,
    request: &IngestionRequest,
    storage: &mut S,
    registry: &mut R,
) -> Result<IngestionReport, String> {
    if request.dataset_id.trim().is_empty() || request.dataset_version.trim().is_empty() {
        return Err("dataset_id and dataset_version are required".into());
    }
    let incoming = provider.load_bars(&request.instrument, request.start, request.end)?;
    let incoming_rows = incoming.len();
    storage.upsert_bars(&request.dataset_id, &incoming)?;
    let mut stored = storage
        .load_bars(&request.dataset_id)?
        .into_iter()
        .filter(|bar| bar.instrument == request.instrument)
        .collect::<Vec<_>>();
    stored.sort_by_key(|bar| bar.timestamp);
    let first = stored
        .first()
        .ok_or_else(|| "ingested dataset contains no bars for instrument".to_string())?;
    let last = stored.last().expect("first implies last");
    let manifest = DatasetManifest {
        dataset_id: request.dataset_id.clone(),
        version: request.dataset_version.clone(),
        source: provider.metadata().name,
        fingerprint: fingerprint_bars(&stored)?,
        schema_version: 1,
        start_timestamp: first.timestamp,
        end_timestamp: last.timestamp,
    };
    registry.register_manifest(manifest.clone())?;
    Ok(IngestionReport {
        dataset_id: request.dataset_id.clone(),
        dataset_version: request.dataset_version.clone(),
        incoming_rows,
        stored_rows: stored.len(),
        manifest,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::JsonBarFrameProvider;
    use crate::registry::DatasetRegistry;
    use crate::storage::MemoryDataStorage;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn provider_ingestion_merges_and_registers_manifest() {
        let root = std::env::temp_dir().join(format!(
            "qianxing-data-ingestion-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("bars.json");
        std::fs::write(
            &path,
            r#"{"instrument":"000001.SZSE","source":"akshare","ts":[1000,2000,3000],"open_raw":[10,20,30],"high_raw":[11,21,31],"low_raw":[9,19,29],"close_raw":[10,20,30],"volume_raw":[1,2,3]}"#,
        )
        .unwrap();
        let provider = JsonBarFrameProvider::new("akshare", "source-v1", &path);
        let mut storage = MemoryDataStorage::default();
        let mut registry = DatasetRegistry::default();
        let report = ingest_bars(
            &provider,
            &IngestionRequest {
                dataset_id: "ashare.daily".into(),
                dataset_version: "snapshot-20260915".into(),
                instrument: "000001.SZSE".into(),
                start: 1_500,
                end: 3_000,
            },
            &mut storage,
            &mut registry,
        )
        .unwrap();
        assert_eq!(report.incoming_rows, 2);
        assert_eq!(report.stored_rows, 2);
        assert_eq!(registry.len(), 1);
        assert!(registry
            .verify(
                "ashare.daily",
                "snapshot-20260915",
                &report.manifest.fingerprint
            )
            .is_ok());
        assert_eq!(storage.load_bars("ashare.daily").unwrap().len(), 2);
        let _ = std::fs::remove_dir_all(root);
    }
}
