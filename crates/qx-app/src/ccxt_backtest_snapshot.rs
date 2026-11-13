//! Immutable, content-addressed persistence for fetched CCXT backtest bars.

use qx_datastruct::BarFrame;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CcxtBacktestSnapshot {
    pub bars_path: PathBuf,
    pub manifest_path: PathBuf,
}

/// Persist a validated provider frame and its lineage as one immutable dataset snapshot.
pub fn persist_ccxt_backtest_snapshot(
    frame: &BarFrame,
    interval_ms: u64,
    query_source: &str,
    storage_data_dir: &Path,
) -> Result<CcxtBacktestSnapshot, String> {
    let manifest = qx_data::manifest_from_bar_frame(
        frame,
        &format!("strategy-bars:{}", frame.instrument),
        &qx_data::barframe_snapshot_version(frame),
        interval_ms,
        "UTC",
        qx_data::BARFRAME_DATASET_VERSION,
        query_source,
    )?;
    let manifest_payload = manifest.to_json()?;
    let cache_key = qx_data::sha256_hex_bytes(manifest_payload.as_bytes());
    let data_root = if storage_data_dir.is_absolute() {
        storage_data_dir.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| format!("读取当前目录失败: {error}"))?
            .join(storage_data_dir)
    };
    let dataset_root = data_root.join("datasets").join("ccxt").join(cache_key);
    std::fs::create_dir_all(&dataset_root)
        .map_err(|error| format!("创建 CCXT 不可变数据快照目录失败: {error}"))?;
    let snapshot = CcxtBacktestSnapshot {
        bars_path: dataset_root.join("bars.json"),
        manifest_path: dataset_root.join("manifest.v2.json"),
    };
    let bars_payload = frame.to_json();
    super::artifact_store::write_backtest_artifact(
        &snapshot.bars_path,
        &bars_payload,
        "CCXT BarFrame 快照",
    )?;
    super::artifact_store::write_backtest_artifact(
        &snapshot.manifest_path,
        &manifest_payload,
        "CCXT DatasetManifestV2",
    )?;
    Ok(snapshot)
}

#[cfg(test)]
mod tests {
    use super::*;
    use qx_core::InstrumentId;
    use qx_guanxing::DataSourceId;

    fn frame() -> BarFrame {
        BarFrame {
            instrument: InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
            source: DataSourceId::new("ccxt:binance"),
            ts: vec![60_000, 120_000],
            open_raw: vec![10, 11],
            high_raw: vec![12, 13],
            low_raw: vec![9, 10],
            close_raw: vec![11, 12],
            volume_raw: vec![100, 110],
            exact_ohlcv: None,
        }
    }

    #[test]
    fn ccxt_snapshot_is_content_addressed_and_idempotently_published() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("target")
            .join("qx-app-test-tmp")
            .join(format!(
                "qx-ccxt-snapshot-{}-{}",
                std::process::id(),
                SNAPSHOT_TEST_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
        let first = persist_ccxt_backtest_snapshot(
            &frame(),
            60_000,
            "ccxt:binance;requested_range=60000..120000",
            &root,
        )
        .unwrap();
        let second = persist_ccxt_backtest_snapshot(
            &frame(),
            60_000,
            "ccxt:binance;requested_range=60000..120000",
            &root,
        )
        .unwrap();
        let changed_lineage = persist_ccxt_backtest_snapshot(
            &frame(),
            60_000,
            "ccxt:binance;requested_range=60000..180000",
            &root,
        )
        .unwrap();

        assert_eq!(first, second);
        assert_ne!(first.bars_path, changed_lineage.bars_path);
        assert!(first.bars_path.is_file());
        assert!(first.manifest_path.is_file());
        let manifest: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&first.manifest_path).unwrap()).unwrap();
        assert_eq!(
            manifest["dataset"]["dataset_id"],
            "strategy-bars:BTCUSDT.BINANCE"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    static SNAPSHOT_TEST_SEQUENCE: std::sync::atomic::AtomicUsize =
        std::sync::atomic::AtomicUsize::new(0);
}
