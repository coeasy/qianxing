//! Durable worker observations shared by the process supervisor and API projection.

use qx_strategy::capabilities::StrategyHostCapabilitiesV1;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StrategyWorkerPhase {
    Initializing,
    Running,
    Paused,
    Stopped,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StrategyHostLifecycleEvidenceV1 {
    pub schema_version: u32,
    pub host_id: String,
    pub constructed: bool,
    pub initialized: bool,
    pub checkpoint_input_supplied: bool,
    pub warmup_applicable: bool,
    pub warmup_required_instruments: Vec<String>,
    pub warmup_completed_instruments: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StrategyHostLifecycleEvidenceV2 {
    pub schema_version: u32,
    pub host_id: String,
    pub constructed: bool,
    pub initialized: bool,
    pub checkpoint_input_supplied: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint_restore_required: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint_restore_acknowledged: Option<bool>,
    pub warmup_applicable: bool,
    pub warmup_required_instruments: Vec<String>,
    pub warmup_completed_instruments: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StrategyWorkerObservationV2 {
    pub schema_version: u32,
    pub worker_id: String,
    pub config_sha256: String,
    pub process_id: u32,
    pub owner_id: String,
    pub observed_at_ms: u64,
    pub phase: StrategyWorkerPhase,
    pub host_capabilities: Option<StrategyHostCapabilitiesV1>,
    pub checkpoint_roundtrip_observed: bool,
    #[serde(default)]
    pub host_lifecycle: Option<StrategyHostLifecycleEvidenceV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StrategyWorkerObservationV3 {
    pub schema_version: u32,
    pub worker_id: String,
    pub config_sha256: String,
    pub process_id: u32,
    pub owner_id: String,
    pub observed_at_ms: u64,
    pub phase: StrategyWorkerPhase,
    pub host_capabilities: Option<StrategyHostCapabilitiesV1>,
    pub checkpoint_roundtrip_observed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_lifecycle: Option<StrategyHostLifecycleEvidenceV2>,
}

/// Source-compatible name retained for consumers compiled against the v1 Rust type.
pub type StrategyWorkerObservationV1 = StrategyWorkerObservationV2;

pub fn runtime_config_digest(config: &crate::RuntimeConfig) -> Result<String, String> {
    Ok(qx_strategy::sha256_hex(
        &serde_json::to_vec(config).map_err(|error| error.to_string())?,
    ))
}
