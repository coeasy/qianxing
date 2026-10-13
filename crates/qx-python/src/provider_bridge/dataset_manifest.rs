//! Build the versioned Rust dataset manifest from a validated Provider result.

use pyo3::exceptions::PyValueError;
use pyo3::PyResult;
use qx_data::manifest_from_bar_frame;
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
    let frame_json = frame.to_json();
    let manifest = manifest_from_bar_frame(
        &frame,
        dataset_id,
        dataset_version,
        interval_ms,
        timezone,
        &result.provider_version,
        &result.provider_id,
    )
    .map_err(|error| PyValueError::new_err(format!("DatasetManifestV2 invalid: {error}")))?;
    let manifest_json = manifest.to_json().map_err(PyValueError::new_err)?;
    let payload = serde_json::json!({
        "frame_json": frame_json,
        "manifest_json": manifest_json,
    });
    serde_json::to_string(&payload)
        .map_err(|error| PyValueError::new_err(format!("dataset bundle encode failed: {error}")))
}
