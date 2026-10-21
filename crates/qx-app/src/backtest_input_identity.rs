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
    let bars: Vec<Bar> = frame.into();
    let provider =
        JsonBarFrameProvider::new(frame.source.0.clone(), BARFRAME_DATASET_VERSION, frame_path);
    let (provider_bars, manifest) = provider.load_bars_with_manifest(
        &format!("strategy-bars:{}", frame.instrument),
        &frame.instrument.to_string(),
        bars.first().map(|bar| bar.ts).unwrap_or(1),
        bars.last().map(|bar| bar.ts).unwrap_or(1),
    )?;
    if provider_bars.len() != bars.len()
        || !provider_bars
            .iter()
            .zip(&bars)
            .all(|(ingest, market)| ingest_bar_matches_market_bar(ingest, market))
    {
        return Err("qx-data Provider 与 BarFrame 列式输入不一致，拒绝开始回测".into());
    }
    Ok((bars, manifest))
}

/// Compare the ingestion and market representations cell by cell.
pub fn ingest_bar_matches_market_bar(ingest: &qx_data::Bar, market: &Bar) -> bool {
    ingest.timestamp == market.ts
        && ingest.open_raw == market.open
        && ingest.high_raw == market.high
        && ingest.low_raw == market.low
        && ingest.close_raw == market.close
        && ingest.volume_raw == market.volume
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
    use super::ingest_bar_matches_market_bar;
    use qx_data::Bar as IngestBar;
    use qx_guanxing::Bar as MarketBar;

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
}
