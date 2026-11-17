//! Canonical dataset and provenance identity for BarFrame and depth-frame backtests.
//!
//! The CLI owns path discovery and its deploy-specific diagnostics. Once a frame has
//! been decoded, identity construction and provider parity are application decisions
//! shared by every facade.

use crate::artifact_store::BacktestInputProvenance;
use qx_data::JsonBarFrameProvider;
use qx_datastruct::BarFrame;
use qx_guanxing::Bar;
use qx_xingban::DepthFrame;
use std::path::Path;

pub const BARFRAME_DATASET_VERSION: &str = qx_data::BARFRAME_DATASET_VERSION;
pub const DEPTH_FRAME_DATASET_VERSION: &str = qx_data::DEPTHFRAME_DATASET_VERSION;

/// Bind the columnar BarFrame to the canonical ingestion provider and verify every
/// timestamp/OHLCV cell before exposing the bars and manifest to a backtest.
pub fn barframe_dataset_identity(
    frame_path: &Path,
    frame: &BarFrame,
) -> Result<(Vec<Bar>, qx_data::DatasetManifest), String> {
    barframe_dataset_identity_with_version(
        frame_path,
        frame,
        qx_data::barframe_registry_version(frame),
    )
}

/// Reconstruct the pre-content-addressed identity for already-published summaries. New
/// backtests always use `barframe_dataset_identity`; this compatibility reader is only used
/// when a verified artifact explicitly declares the former provider-wide version.
pub fn legacy_barframe_dataset_identity(
    frame_path: &Path,
    frame: &BarFrame,
) -> Result<(Vec<Bar>, qx_data::DatasetManifest), String> {
    barframe_dataset_identity_with_version(frame_path, frame, BARFRAME_DATASET_VERSION.to_owned())
}

fn barframe_dataset_identity_with_version(
    frame_path: &Path,
    frame: &BarFrame,
    dataset_version: String,
) -> Result<(Vec<Bar>, qx_data::DatasetManifest), String> {
    frame
        .validate()
        .map_err(|error| format!("BarFrame 列式输入无效: {error:?}"))?;
    let start = *frame
        .ts
        .first()
        .ok_or_else(|| "BarFrame 列式输入不能为空".to_string())?;
    let end = *frame
        .ts
        .last()
        .ok_or_else(|| "BarFrame 列式输入不能为空".to_string())?;
    let instrument = frame.instrument.to_string();
    let provider = JsonBarFrameProvider::new(frame.source.0.clone(), dataset_version, frame_path);
    let (provider_bars, manifest) = provider.load_bars_with_manifest(
        &format!("strategy-bars:{instrument}"),
        &instrument,
        start,
        end,
    )?;
    if provider_bars.len() != frame.ts.len()
        || !provider_bars
            .iter()
            .enumerate()
            .all(|(index, ingest)| ingest_bar_matches_frame(ingest, frame, &instrument, index))
    {
        return Err("qx-data Provider 与 BarFrame 列式输入不一致，拒绝开始回测".into());
    }
    // Reuse the provider's already validated rows as the engine input instead of
    // building a second full Bar vector from the same columnar frame.
    let bars = provider_bars
        .into_iter()
        .map(|ingest| Bar {
            ts: ingest.timestamp,
            open: ingest.open_raw,
            high: ingest.high_raw,
            low: ingest.low_raw,
            close: ingest.close_raw,
            volume: ingest.volume_raw,
            exact_ohlcv: ingest.exact_ohlcv,
        })
        .collect();
    Ok((bars, manifest))
}

fn ingest_bar_matches_frame(
    ingest: &qx_data::Bar,
    frame: &BarFrame,
    instrument: &str,
    index: usize,
) -> bool {
    let exact_matches = frame
        .exact_ohlcv
        .as_ref()
        .map_or(ingest.exact_ohlcv.is_none(), |rows| {
            rows.get(index)
                .is_some_and(|value| ingest.exact_ohlcv == Some(*value))
        });
    ingest.instrument == instrument
        && frame
            .ts
            .get(index)
            .is_some_and(|timestamp| ingest.timestamp == *timestamp)
        && frame
            .open_raw
            .get(index)
            .is_some_and(|value| ingest.open_raw == *value)
        && frame
            .high_raw
            .get(index)
            .is_some_and(|value| ingest.high_raw == *value)
        && frame
            .low_raw
            .get(index)
            .is_some_and(|value| ingest.low_raw == *value)
        && frame
            .close_raw
            .get(index)
            .is_some_and(|value| ingest.close_raw == *value)
        && frame
            .volume_raw
            .get(index)
            .is_some_and(|value| ingest.volume_raw == *value)
        && exact_matches
}

/// Compare the ingestion and market representations cell by cell.
pub fn ingest_bar_matches_market_bar(ingest: &qx_data::Bar, market: &Bar) -> bool {
    ingest.timestamp == market.ts
        && ingest.open_raw == market.open
        && ingest.high_raw == market.high
        && ingest.low_raw == market.low
        && ingest.close_raw == market.close
        && ingest.volume_raw == market.volume
        && ingest.exact_ohlcv == market.exact_ohlcv
}

pub fn barframe_input_provenance(
    frame_path: &Path,
    manifest: &qx_data::DatasetManifest,
) -> BacktestInputProvenance {
    BacktestInputProvenance {
        kind: "barframe",
        path: frame_path.to_string_lossy().into_owned(),
        dataset_id: manifest.dataset_id.clone(),
        dataset_version: manifest.version.clone(),
        fingerprint: manifest.fingerprint.clone(),
    }
}

pub fn depth_frame_input_provenance(
    frame_path: &Path,
    frame: &DepthFrame,
) -> BacktestInputProvenance {
    BacktestInputProvenance {
        kind: "depth-frame",
        path: frame_path.to_string_lossy().into_owned(),
        dataset_id: format!("depth-bars:{}", frame.instrument),
        dataset_version: DEPTH_FRAME_DATASET_VERSION.to_string(),
        fingerprint: format!("{:016x}", frame.input_hash()),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        barframe_dataset_identity, ingest_bar_matches_market_bar, legacy_barframe_dataset_identity,
    };
    use qx_data::Bar as IngestBar;
    use qx_datastruct::BarFrame;
    use qx_guanxing::Bar as MarketBar;
    use std::path::Path;

    #[test]
    fn provider_parity_checks_timestamp_and_every_ohlcv_cell() {
        let market = MarketBar::new(10, 11, 12, 9, 11, 4);
        let ingest = |timestamp| IngestBar {
            instrument: "000001.SZSE".into(),
            timestamp,
            open_raw: 11,
            high_raw: 12,
            low_raw: 9,
            close_raw: 11,
            volume_raw: 4,
            exact_ohlcv: None,
        };
        assert!(ingest_bar_matches_market_bar(&ingest(10), &market));
        assert!(!ingest_bar_matches_market_bar(&ingest(9), &market));
        for mutate in 0..5 {
            let mut row = ingest(10);
            match mutate {
                0 => row.open_raw += 1,
                1 => row.high_raw += 1,
                2 => row.low_raw += 1,
                3 => row.close_raw += 1,
                _ => row.volume_raw += 1,
            }
            assert!(!ingest_bar_matches_market_bar(&row, &market));
        }
    }

    #[test]
    fn ccxt_refreshes_get_content_addressed_versions_and_legacy_frames_stay_compatible() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("target")
            .join(format!("qx-ccxt-registry-version-{}", std::process::id()));
        std::fs::create_dir_all(&root).expect("create isolated identity directory");
        let path = root.join("bars.json");
        let first = BarFrame::from_json(
            r#"{"schema_version":1,"instrument":"BTCUSDT.BINANCE","source":"ccxt:binance:BTC/USDT:1m","ts":[60000,120000],"open_raw":[10,11],"high_raw":[12,13],"low_raw":[9,10],"close_raw":[11,12],"volume_raw":[1,2]}"#,
        )
        .unwrap();
        std::fs::write(&path, first.to_json()).expect("write first provider frame");
        let (_, original) = barframe_dataset_identity(&path, &first).unwrap();
        let (_, identical) = barframe_dataset_identity(&path, &first).unwrap();
        assert_eq!(original, identical);

        let mut revised = first.clone();
        revised.close_raw[1] += 1;
        std::fs::write(&path, revised.to_json()).expect("write revised provider frame");
        let (_, refreshed) = barframe_dataset_identity(&path, &revised).unwrap();
        assert_eq!(refreshed.dataset_id, original.dataset_id);
        assert_ne!(refreshed.version, original.version);
        assert_ne!(refreshed.fingerprint, original.fingerprint);

        let (_, legacy) = legacy_barframe_dataset_identity(&path, &revised).unwrap();
        assert_eq!(legacy.version, super::BARFRAME_DATASET_VERSION);
        std::fs::remove_dir_all(root).expect("remove isolated identity directory");
    }
}
