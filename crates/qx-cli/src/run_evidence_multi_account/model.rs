//! Serialized manifest and file reference types for multi-account Bar Evidence.

use serde::{Deserialize, Serialize};

pub(super) const SCHEMA_VERSION: u32 = 2;
pub(super) const LEGACY_SCHEMA_VERSION: u32 = 1;
pub(super) const MANIFEST_NAME: &str = "multi-account-evidence.json";
pub(super) const RESULT_NAME: &str = "multi-account-result.json";
pub(super) const SPEC_NAME: &str = "multi-account-spec.json";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct FileDigest {
    pub(super) path: String,
    pub(super) sha256: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AccountEvidence {
    pub(super) account_id: String,
    pub(super) run_id: String,
    pub(super) result_hash: String,
    pub(super) instrument: String,
    pub(super) strategy_id: String,
    pub(super) data_fingerprint: String,
    pub(super) evidence_dir: String,
    pub(super) evidence_sha256: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AccountInputEvidence {
    pub(super) account_id: String,
    pub(super) path: String,
    pub(super) sha256: Option<String>,
    pub(super) missing: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct MultiAccountEvidenceManifest {
    pub(super) schema_version: u32,
    pub(super) kind: String,
    pub(super) batch_id: String,
    pub(super) settlement_currency: String,
    pub(super) spec_fingerprint: String,
    pub(super) result: FileDigest,
    pub(super) spec: FileDigest,
    pub(super) accounts: Vec<AccountEvidence>,
    #[serde(default)]
    pub(super) inputs: Vec<AccountInputEvidence>,
}
