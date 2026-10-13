//! Build the canonical dataset manifest for an already normalized BarFrame.

use crate::{
    fingerprint_bars, Bar, DatasetManifest, DatasetManifestV2, DatasetQualityReport,
    DatasetSourceLineage, DatasetTier, BAR_FRAME_SCHEMA_VERSION,
    DATASET_MANIFEST_V2_SCHEMA_VERSION,
};
use qx_datastruct::BarFrame;
use ring::digest::{digest, SHA256};

/// Construct the shared v2 manifest used by Rust and Python Provider workflows.
/// `interval_ms` is explicit so gap counts are never guessed from a frequency label.
pub fn manifest_from_bar_frame(
    frame: &BarFrame,
    dataset_id: &str,
    dataset_version: &str,
    interval_ms: u64,
    timezone: &str,
    provider_version: &str,
    primary_source: &str,
) -> Result<DatasetManifestV2, String> {
    if dataset_id.trim().is_empty()
        || dataset_version.trim().is_empty()
        || interval_ms == 0
        || timezone.trim().is_empty()
        || provider_version.trim().is_empty()
        || primary_source.trim().is_empty()
    {
        return Err(
            "dataset identity, positive interval_ms, timezone, provider version and source are required"
                .into(),
        );
    }
    frame
        .validate()
        .map_err(|error| format!("BarFrame validation failed: {error:?}"))?;
    let bars = (0..frame.ts.len())
        .map(|index| Bar {
            instrument: frame.instrument.to_string(),
            timestamp: frame.ts[index],
            open_raw: frame.open_raw[index],
            high_raw: frame.high_raw[index],
            low_raw: frame.low_raw[index],
            close_raw: frame.close_raw[index],
            volume_raw: frame.volume_raw[index],
        })
        .collect::<Vec<_>>();
    let fingerprint = fingerprint_bars(&bars)?;
    let frame_json = frame.to_json();
    let content_hash = digest(&SHA256, frame_json.as_bytes())
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let missing_intervals = frame
        .ts
        .windows(2)
        .try_fold(0_u64, |total, pair| {
            total.checked_add((pair[1] - pair[0]).div_ceil(interval_ms) - 1)
        })
        .ok_or_else(|| "dataset missing interval count overflow".to_owned())?;
    let source = frame.source.0.clone();
    let manifest = DatasetManifestV2 {
        manifest_version: DATASET_MANIFEST_V2_SCHEMA_VERSION,
        dataset: DatasetManifest {
            dataset_id: dataset_id.to_owned(),
            version: dataset_version.to_owned(),
            source,
            fingerprint,
            schema_version: BAR_FRAME_SCHEMA_VERSION,
            start_timestamp: *frame.ts.first().expect("validated BarFrame is non-empty"),
            end_timestamp: *frame.ts.last().expect("validated BarFrame is non-empty"),
        },
        instrument: frame.instrument.to_string(),
        timezone: timezone.to_owned(),
        tier: DatasetTier::Bar,
        provider_version: provider_version.to_owned(),
        content_hash,
        quality_report: DatasetQualityReport {
            row_count: frame.ts.len() as u64,
            duplicate_rows: 0,
            out_of_order_rows: 0,
            missing_intervals,
            timezone: timezone.to_owned(),
            corporate_action_coverage: false,
            usable_tiers: vec![DatasetTier::Bar.as_str().to_owned()],
        },
        source_lineage: DatasetSourceLineage {
            primary_source: primary_source.to_owned(),
            backup_sources: Vec::new(),
        },
    };
    manifest.validate()?;
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn frame() -> BarFrame {
        BarFrame::from_json(
            r#"{"schema_version":1,"instrument":"BTCUSDT.BINANCE","source":"fixture@v1","ts":[60000,120000,240000],"open_raw":[10,11,12],"high_raw":[12,13,14],"low_raw":[9,10,11],"close_raw":[11,12,13],"volume_raw":[1,2,3]}"#,
        )
        .unwrap()
    }

    #[test]
    fn shared_bar_manifest_tracks_frame_identity_quality_and_content() {
        let manifest = manifest_from_bar_frame(
            &frame(),
            "btc-1m",
            "snapshot-1",
            60_000,
            "UTC",
            "fixture-v1",
            "fixture",
        )
        .unwrap();
        assert_eq!(manifest.dataset.start_timestamp, 60_000);
        assert_eq!(manifest.dataset.end_timestamp, 240_000);
        assert_eq!(manifest.quality_report.row_count, 3);
        assert_eq!(manifest.quality_report.missing_intervals, 1);
        assert_eq!(manifest.source_lineage.primary_source, "fixture");
        assert_eq!(manifest.content_hash.len(), 64);
        assert!(DatasetManifestV2::from_json(&manifest.to_json().unwrap()).is_ok());
    }

    #[test]
    fn shared_bar_manifest_rejects_missing_identity_and_zero_interval() {
        let frame = frame();
        for interval in [0, 60_000] {
            let id = if interval == 0 { "valid" } else { " " };
            assert!(manifest_from_bar_frame(&frame, id, "v1", interval, "UTC", "p1", "p").is_err());
        }
    }
}
