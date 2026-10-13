//! Convert Python callbacks and immutable identities into Rust strategy slots.

use super::PythonBacktestStrategy;
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::PyAny;
use qx_app::StrategyArtifactIdentity;
use qx_strategy::Strategy;
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StrategyIdentityDocument {
    strategy_id: String,
    strategy_version: String,
    source_sha256: String,
}

pub(super) fn strategy_slots(
    identities_json: &str,
    callbacks: Vec<Py<PyAny>>,
) -> PyResult<Vec<(StrategyArtifactIdentity, Box<dyn Strategy>)>> {
    let identities = serde_json::from_str::<Vec<StrategyIdentityDocument>>(identities_json)
        .map_err(|error| PyValueError::new_err(format!("invalid strategy identities: {error}")))?;
    if identities.len() != callbacks.len() {
        return Err(PyValueError::new_err(format!(
            "strategy identity count {} does not match callback count {}",
            identities.len(),
            callbacks.len()
        )));
    }
    Ok(identities
        .into_iter()
        .zip(callbacks)
        .map(|(identity, callback)| {
            (
                StrategyArtifactIdentity {
                    strategy_id: identity.strategy_id,
                    strategy_version: identity.strategy_version,
                    source_sha256: identity.source_sha256,
                },
                Box::new(PythonBacktestStrategy { callback }) as Box<dyn Strategy>,
            )
        })
        .collect())
}
