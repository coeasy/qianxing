//! Build the versioned Rust dataset manifest from a validated Provider result.

use pyo3::exceptions::PyValueError;
use pyo3::PyResult;
use qx_data::{
    DatasetManifest, DatasetManifestV2, DatasetQualityReport, DatasetSourceLineage, DatasetTier,
    BAR_FRAME_SCHEMA_VERSION, DATASET_MANIFEST_V2_SCHEMA_VERSION,
};
use qx_datastruct::BarFrame;
use qx_provider::{DataKind, DataQuery, ProviderResult};

pub(super) fn bar_frame_manifest_json(
    result: ProviderResult,
    frame: BarFrame,
    query: &DataQuery,
    dataset_id: &str,
    dataset_version: &str,
    interval_ms: u64,
    timezone: &str,
) -> PyResult<String> {
    if query.kind != DataKind::Bar || query.instrument_set.len() != 1 {
        return Err(PyValueError::new_err(
            "dataset manifest requires Bar data and exactly one instrument",
        ));
    }
    if dataset_id.trim().is_empty()
        || dataset_version.trim().is_empty()
        || interval_ms == 0
        || timezone.trim().is_empty()
    {
        return Err(PyValueError::new_err(
            "dataset identity, positive interval_ms, and timezone are required",
        ));
    }
    let frame_json = frame.to_json();
    let bars = (0..frame.ts.len())
        .map(|index| qx_data::Bar {
            instrument: frame.instrument.to_string(),
            timestamp: frame.ts[index],
            open_raw: frame.open_raw[index],
            high_raw: frame.high_raw[index],
            low_raw: frame.low_raw[index],
            close_raw: frame.close_raw[index],
            volume_raw: frame.volume_raw[index],
        })
        .collect::<Vec<_>>();
    let fingerprint = qx_data::fingerprint_bars(&bars)
        .map_err(|error| PyValueError::new_err(format!("Bar fingerprint failed: {error}")))?;
    let missing_intervals = frame
        .ts
        .windows(2)
        .try_fold(0u64, |total, pair| {
            total.checked_add((pair[1] - pair[0]).div_ceil(interval_ms) - 1)
        })
        .ok_or_else(|| PyValueError::new_err("dataset missing interval count overflow"))?;
    let source = frame.source.0.clone();
    let manifest = DatasetManifestV2 {
        manifest_version: DATASET_MANIFEST_V2_SCHEMA_VERSION,
        dataset: DatasetManifest {
            dataset_id: dataset_id.to_owned(),
            version: dataset_version.to_owned(),
            source: source.clone(),
            fingerprint,
            schema_version: BAR_FRAME_SCHEMA_VERSION,
            start_timestamp: *frame.ts.first().expect("validated BarFrame is non-empty"),
            end_timestamp: *frame.ts.last().expect("validated BarFrame is non-empty"),
        },
        instrument: frame.instrument.to_string(),
        timezone: timezone.to_owned(),
        tier: DatasetTier::Bar,
        provider_version: result.provider_version,
        content_hash: qx_strategy::sha256_hex(frame_json.as_bytes()),
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
            primary_source: result.provider_id,
            backup_sources: Vec::new(),
        },
    };
    manifest
        .validate()
        .map_err(|error| PyValueError::new_err(format!("DatasetManifestV2 invalid: {error}")))?;
    let payload = serde_json::json!({
        "frame_json": frame_json,
        "manifest_json": manifest.to_json().map_err(PyValueError::new_err)?,
    });
    serde_json::to_string(&payload)
        .map_err(|error| PyValueError::new_err(format!("dataset bundle encode failed: {error}")))
}
