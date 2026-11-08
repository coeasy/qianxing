//! Serialized manifest and file reference types for joint-account Evidence.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub(super) const MANIFEST_NAME: &str = "multi-account-joint-evidence.json";
pub(super) const RESULT_NAME: &str = "result.json";
pub(super) const SPEC_NAME: &str = "spec.json";
pub(super) const SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct FileDigest {
    pub(super) path: String,
    pub(super) sha256: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct JointAccountEvidence {
    pub(super) exchange_id: String,
    pub(super) account_id: String,
    pub(super) run_id: String,
    pub(super) result_hash: String,
    pub(super) markets_total: usize,
    pub(super) evidence_dir: String,
    pub(super) evidence_sha256: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct JointEvidenceManifest {
    pub(super) schema_version: u32,
    pub(super) kind: String,
    pub(super) run_id: String,
    pub(super) settlement_currency: String,
    pub(super) spec_fingerprint: String,
    pub(super) result_hash: String,
    pub(super) result: FileDigest,
    pub(super) spec: FileDigest,
    pub(super) parent_artifacts: BTreeMap<String, FileDigest>,
    pub(super) accounts: Vec<JointAccountEvidence>,
}
