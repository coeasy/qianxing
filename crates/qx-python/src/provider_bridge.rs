//! Python adapters into the shared Rust provider registry.

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyModule};
use qx_provider::{
    DataProvider, DataQuery, ProviderCapability, ProviderError, ProviderErrorClass,
    ProviderPayload, ProviderRegistry, ProviderResult,
};
use serde::Deserialize;
use std::sync::Mutex;

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
        let query = DataQuery::from_json(query_json)
            .map_err(|error| PyValueError::new_err(format!("{error:?}")))?;
        self.inner
            .candidates(&query)
            .map_err(|error| PyValueError::new_err(format!("Provider selection failed: {error:?}")))
    }

    fn fetch_json(&self, query_json: &str) -> PyResult<String> {
        let query = DataQuery::from_json(query_json)
            .map_err(|error| PyValueError::new_err(format!("{error:?}")))?;
        self.inner
            .fetch_with_failover(&query)
            .and_then(|result| {
                result
                    .to_json()
                    .map_err(qx_provider::RegistryError::Provider)
            })
            .map_err(|error| PyValueError::new_err(format!("Provider fetch failed: {error:?}")))
    }

    fn fetch_bar_frame_json(&self, query_json: &str) -> PyResult<String> {
        let query = DataQuery::from_json(query_json)
            .map_err(|error| PyValueError::new_err(format!("{error:?}")))?;
        let result = self
            .inner
            .fetch_with_failover(&query)
            .map_err(|error| PyValueError::new_err(format!("Provider fetch failed: {error:?}")))?;
        let frame = result
            .to_bar_frame(&query)
            .map_err(|error| PyValueError::new_err(format!("{error:?}")))?;
        Ok(frame.to_json())
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
