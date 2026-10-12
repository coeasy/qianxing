//! Python adapters into the shared Rust provider registry.

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyModule};
use qx_provider::{
    DataKind, DataProvider, DataQuery, DataRequest, ProviderCapability, ProviderError,
    ProviderErrorClass, ProviderPayload, ProviderRegistry, ProviderResult,
};
use qx_xingban::{DepthFrame, OrderBookSnapshot};
use serde::Deserialize;
use std::sync::Mutex;

mod dataset_manifest;

/// Python adapters provide bytes only; ordering, failover, provenance checks and
/// Bar quality validation stay inside the shared Rust provider boundary.
struct PythonDataProvider {
    capability: ProviderCapability,
    adapter: Mutex<Py<PyAny>>,
}

impl DataProvider for PythonDataProvider {
    fn capability(&self) -> &ProviderCapability {
        &self.capability
    }

    fn fetch(&self, query: &DataQuery) -> Result<ProviderResult, ProviderError> {
        let query_json = query.to_json()?;
        self.fetch_json(&query_json)
    }

    fn fetch_request(&self, request: &DataRequest) -> Result<ProviderResult, ProviderError> {
        self.fetch_json(&request.to_json()?)
    }
}

impl PythonDataProvider {
    fn fetch_json(&self, query_json: &str) -> Result<ProviderResult, ProviderError> {
        let payload = Python::attach(|py| -> Result<String, ProviderError> {
            let adapter = self.adapter.lock().map_err(|_| {
                ProviderError::new(
                    ProviderErrorClass::Permanent,
                    "Python Provider adapter lock poisoned",
                )
            })?;
            let result = adapter
                .bind(py)
                .call_method1("fetch", (query_json,))
                .and_then(|value| value.extract::<String>());
            result.map_err(|error| {
                let class = error
                    .value(py)
                    .getattr("qx_provider_error_class")
                    .and_then(|value| value.extract::<String>())
                    .unwrap_or_default();
                let error_class = match class.as_str() {
                    "manual_intervention" => ProviderErrorClass::ManualIntervention,
                    "permanent" => ProviderErrorClass::Permanent,
                    _ => ProviderErrorClass::SwitchProvider,
                };
                ProviderError::new(
                    error_class,
                    format!("Python Provider adapter failed: {error}"),
                )
            })
        })?;
        ProviderResult::from_json(&payload)
    }
}

#[pyclass(name = "ProviderRegistry", unsendable)]
struct PyProviderRegistry {
    inner: ProviderRegistry,
}

#[pymethods]
impl PyProviderRegistry {
    #[new]
    fn new() -> Self {
        Self {
            inner: ProviderRegistry::new(),
        }
    }

    /// Register an object implementing `fetch(query_json: str) -> result_json: str`.
    fn register(&mut self, capability_json: &str, adapter: Py<PyAny>) -> PyResult<()> {
        let capability = ProviderCapability::from_json(capability_json)
            .map_err(|error| PyValueError::new_err(format!("{error:?}")))?;
        self.inner
            .register(Box::new(PythonDataProvider {
                capability,
                adapter: Mutex::new(adapter),
            }))
            .map_err(|error| {
                PyValueError::new_err(format!("Provider registration failed: {error:?}"))
            })
    }

    fn candidates(&self, query_json: &str) -> PyResult<Vec<String>> {
        let request = DataRequest::from_json(query_json)
            .map_err(|error| PyValueError::new_err(format!("{error:?}")))?;
        self.inner
            .candidates_for_request(&request)
            .map_err(|error| PyValueError::new_err(format!("Provider selection failed: {error:?}")))
    }

    fn fetch_json(&self, query_json: &str) -> PyResult<String> {
        let request = DataRequest::from_json(query_json)
            .map_err(|error| PyValueError::new_err(format!("{error:?}")))?;
        self.inner
            .fetch_request(&request)
            .and_then(|result| {
                result
                    .to_json()
                    .map_err(qx_provider::RegistryError::Provider)
            })
            .map_err(|error| PyValueError::new_err(format!("Provider fetch failed: {error:?}")))
    }

    fn fetch_bar_frame_json(&self, query_json: &str) -> PyResult<String> {
        let request = DataRequest::from_json(query_json)
            .map_err(|error| PyValueError::new_err(format!("{error:?}")))?;
        let query = &request.query;
        let result = self
            .inner
            .fetch_request(&request)
            .map_err(|error| PyValueError::new_err(format!("Provider fetch failed: {error:?}")))?;
        let frame = result
            .to_bar_frame(query)
            .map_err(|error| PyValueError::new_err(format!("{error:?}")))?;
        Ok(frame.to_json())
    }

    fn fetch_bar_dataset_json(
        &self,
        query_json: &str,
        dataset_id: &str,
        dataset_version: &str,
        interval_ms: u64,
        timezone: &str,
    ) -> PyResult<String> {
        let request = DataRequest::from_json(query_json)
            .map_err(|error| PyValueError::new_err(format!("{error:?}")))?;
        let query = &request.query;
        if query.kind != DataKind::Bar || query.instrument_set.len() != 1 {
            return Err(PyValueError::new_err(
                "dataset manifest requires Bar data and exactly one instrument",
            ));
        }
        if interval_ms == 0 || timezone.trim().is_empty() {
            return Err(PyValueError::new_err(
                "dataset manifest requires a positive interval_ms and non-empty timezone",
            ));
        }
        let result = self
            .inner
            .fetch_request(&request)
            .map_err(|error| PyValueError::new_err(format!("Provider fetch failed: {error:?}")))?;
        dataset_manifest::bar_frame_manifest_json(
            result,
            query,
            dataset_id,
            dataset_version,
            interval_ms,
            timezone,
        )
    }

    fn fetch_depth_frame_json(&self, query_json: &str, tier: &str) -> PyResult<String> {
        if !matches!(tier, "l1" | "l2") {
            return Err(PyValueError::new_err("depth tier must be l1 or l2"));
        }
        let request = DataRequest::from_json(query_json)
            .map_err(|error| PyValueError::new_err(format!("{error:?}")))?;
        let query = &request.query;
        if query.kind != qx_provider::DataKind::Quote || query.instrument_set.len() != 1 {
            return Err(PyValueError::new_err(
                "depth provider query requires Quote data and exactly one instrument",
            ));
        }
        let result = self
            .inner
            .fetch_request(&request)
            .map_err(|error| PyValueError::new_err(format!("Provider fetch failed: {error:?}")))?;
        let instrument = query.instrument_set.iter().next().expect("one instrument");
        let mut snapshots = result
            .records
            .iter()
            .zip(&result.payloads)
            .map(|(record, payload)| {
                if payload.content_type != "application/vnd.qianxing.order-book-snapshot+json;v=1"
                    || payload.instrument != *instrument
                {
                    return Err(PyValueError::new_err(
                        "depth provider returned a non-snapshot or mismatched instrument payload",
                    ));
                }
                let snapshot: OrderBookSnapshot = serde_json::from_str(&payload.json)
                    .map_err(|error| PyValueError::new_err(format!("invalid book snapshot: {error}")))?;
                snapshot
                    .validate()
                    .map_err(|error| PyValueError::new_err(format!("invalid book snapshot: {error}")))?;
                if snapshot.ts != record.event_time || snapshot.instrument.to_string() != *instrument {
                    return Err(PyValueError::new_err(
                        "book snapshot instrument or event timestamp does not match provider lineage",
                    ));
                }
                if tier == "l1" && (snapshot.bids.len() != 1 || snapshot.asks.len() != 1) {
                    return Err(PyValueError::new_err(
                        "L1 provider snapshots must contain exactly one bid and one ask",
                    ));
                }
                Ok(snapshot)
            })
            .collect::<PyResult<Vec<_>>>()?;
        snapshots.sort_by_key(|snapshot| (snapshot.ts, snapshot.sequence));
        if snapshots
            .windows(2)
            .any(|pair| pair[0].ts >= pair[1].ts || pair[0].sequence >= pair[1].sequence)
        {
            return Err(PyValueError::new_err(
                "depth snapshots must have strictly increasing timestamps and sequences",
            ));
        }
        let frame = DepthFrame {
            schema_version: DepthFrame::SUPPORTED_SCHEMA_VERSION,
            source: format!("{}#query={:016x}", result.provider_id, query.digest()),
            instrument: snapshots
                .first()
                .ok_or_else(|| PyValueError::new_err("depth provider returned no snapshots"))?
                .instrument
                .clone(),
            snapshots,
        };
        frame
            .validate()
            .map_err(|error| PyValueError::new_err(format!("invalid depth frame: {error}")))?;
        serde_json::to_string(&frame)
            .map_err(|error| PyValueError::new_err(format!("depth frame encode failed: {error}")))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderPayloadInput {
    event_time: u64,
    receive_time: u64,
    instrument: String,
    content_type: String,
    json: String,
}

#[pyfunction]
fn provider_result_json(
    provider_id: &str,
    provider_version: &str,
    request_id: &str,
    received_at: u64,
    payload_rows_json: &str,
) -> PyResult<String> {
    let rows: Vec<ProviderPayloadInput> =
        serde_json::from_str(payload_rows_json).map_err(|error| {
            PyValueError::new_err(format!("Provider payload rows invalid: {error}"))
        })?;
    let result = ProviderResult::from_payloads(
        provider_id,
        provider_version,
        request_id,
        received_at,
        rows.into_iter()
            .map(|row| {
                (
                    row.event_time,
                    row.receive_time,
                    ProviderPayload::new_json(row.instrument, row.content_type, row.json),
                )
            })
            .collect(),
    )
    .map_err(|error| PyValueError::new_err(format!("Provider result invalid: {error:?}")))?;
    result
        .to_json()
        .map_err(|error| PyValueError::new_err(format!("Provider result invalid: {error:?}")))
}

pub(super) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<PyProviderRegistry>()?;
    module.add_function(wrap_pyfunction!(provider_result_json, module)?)?;
    Ok(())
}
