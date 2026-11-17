//! Immutable strategy deployment revisions and idempotent operation records.

use serde::{Deserialize, Serialize};

pub const STRATEGY_DEPLOYMENT_REVISION_SCHEMA_VERSION: u32 = 1;
pub const STRATEGY_DEPLOYMENT_OPERATION_SCHEMA_VERSION: u32 = 1;
pub const MAX_STRATEGY_DEPLOYMENT_CONFIG_BYTES: usize = 4 * 1024 * 1024;
pub const STRATEGY_DEPENDENCY_TREE_SCHEMA_VERSION: u32 = 1;
pub const MAX_STRATEGY_DEPENDENCY_TREE_FILES: usize = 2048;
pub const MAX_STRATEGY_DEPENDENCY_TREE_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_STRATEGY_DEPENDENCY_TREE_FILE_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StrategyDependencyTreeFileV1 {
    pub path: String,
    pub size_bytes: u64,
    pub sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StrategyDependencyTreeV1 {
    pub schema_version: u32,
    pub kind: String,
    /// Entry path relative to the content-addressed tree root, using `/` separators.
    pub entry_path: String,
    /// SHA-256 of the canonical identity `{schema_version, kind, entry_path, files}`.
    pub tree_sha256: String,
    /// Sorted manifest of Python source files and explicitly declared runtime resources.
    pub files: Vec<StrategyDependencyTreeFileV1>,
}

#[derive(Serialize)]
struct StrategyDependencyTreeIdentityV1<'a> {
    schema_version: u32,
    kind: &'a str,
    entry_path: &'a str,
    files: &'a [StrategyDependencyTreeFileV1],
}

impl StrategyDependencyTreeV1 {
    pub fn new(
        entry_path: String,
        files: Vec<StrategyDependencyTreeFileV1>,
    ) -> Result<Self, String> {
        let mut value = Self {
            schema_version: STRATEGY_DEPENDENCY_TREE_SCHEMA_VERSION,
            kind: "qianxing_strategy_dependency_tree".into(),
            entry_path,
            tree_sha256: String::new(),
            files,
        };
        value.tree_sha256 = value.computed_sha256()?;
        value.validate()?;
        Ok(value)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != STRATEGY_DEPENDENCY_TREE_SCHEMA_VERSION {
            return Err(format!(
                "unsupported dependency tree schema_version {}; expected {}",
                self.schema_version, STRATEGY_DEPENDENCY_TREE_SCHEMA_VERSION
            ));
        }
        if self.kind != "qianxing_strategy_dependency_tree" {
            return Err("invalid strategy dependency tree kind".into());
        }
        validate_dependency_path(&self.entry_path)?;
        if !self.entry_path.ends_with(".py") {
            return Err("dependency tree entry_path must identify a Python source file".into());
        }
        if self.files.is_empty() || self.files.len() > MAX_STRATEGY_DEPENDENCY_TREE_FILES {
            return Err(format!(
                "dependency tree file count must be in 1..={MAX_STRATEGY_DEPENDENCY_TREE_FILES}"
            ));
        }
        let mut total_bytes = 0u64;
        let mut previous: Option<&str> = None;
        let mut entry_found = false;
        for file in &self.files {
            validate_dependency_path(&file.path)?;
            if previous.is_some_and(|value| value >= file.path.as_str()) {
                return Err("dependency tree files must be sorted and unique by path".into());
            }
            previous = Some(&file.path);
            if file.size_bytes == 0 || file.size_bytes > MAX_STRATEGY_DEPENDENCY_TREE_FILE_BYTES {
                return Err(format!(
                    "dependency tree file {} has an invalid size",
                    file.path
                ));
            }
            validate_sha256("dependency tree file sha256", &file.sha256)?;
            total_bytes = total_bytes
                .checked_add(file.size_bytes)
                .ok_or_else(|| "dependency tree size overflow".to_string())?;
            if total_bytes > MAX_STRATEGY_DEPENDENCY_TREE_BYTES {
                return Err("dependency tree exceeds the total source size limit".into());
            }
            entry_found |= file.path == self.entry_path;
        }
        if !entry_found {
            return Err("dependency tree manifest does not contain its entry_path".into());
        }
        validate_sha256("dependency tree tree_sha256", &self.tree_sha256)?;
        if self.tree_sha256 != self.tree_sha256.to_ascii_lowercase() {
            return Err("dependency tree digest must use lowercase hexadecimal".into());
        }
        if self.computed_sha256()? != self.tree_sha256.to_ascii_lowercase() {
            return Err("dependency tree digest does not match its manifest".into());
        }
        Ok(())
    }

    fn computed_sha256(&self) -> Result<String, String> {
        let identity = StrategyDependencyTreeIdentityV1 {
            schema_version: self.schema_version,
            kind: &self.kind,
            entry_path: &self.entry_path,
            files: &self.files,
        };
        let canonical = serde_json::to_vec(&identity)
            .map_err(|error| format!("cannot serialize dependency tree identity: {error}"))?;
        Ok(qx_data::sha256_hex_bytes(&canonical))
    }
}

fn validate_dependency_path(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 1024
        || value.starts_with('/')
        || value.contains('\\')
        || value.contains(':')
        || value
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(format!("unsafe dependency tree path {value:?}"));
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StrategyDeploymentRevisionV1 {
    pub schema_version: u32,
    pub kind: String,
    pub worker_id: String,
    pub revision: u64,
    pub bundle_sha256: String,
    pub plan_sha256: String,
    /// Safe basename of the content-addressed strategy artifact blob.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bundle_file_name: Option<String>,
    /// Frozen full RuntimeConfig JSON used to materialize this worker revision.
    /// Optional only for reading pre-payload V1 records; new API writes require it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_config_json: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_config_sha256: Option<String>,
    /// Complete immutable source manifest for file-backed Python strategies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dependency_tree: Option<StrategyDependencyTreeV1>,
    pub desired_running: bool,
    pub created_at_ms: u64,
    pub created_by: String,
}

impl StrategyDeploymentRevisionV1 {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != STRATEGY_DEPLOYMENT_REVISION_SCHEMA_VERSION {
            return Err(format!(
                "unsupported deployment revision schema_version {}; expected {}",
                self.schema_version, STRATEGY_DEPLOYMENT_REVISION_SCHEMA_VERSION
            ));
        }
        if self.kind != "qianxing_strategy_deployment_revision" {
            return Err("invalid deployment revision kind".into());
        }
        if !bounded_non_empty(&self.worker_id, 256) || !bounded_non_empty(&self.created_by, 256) {
            return Err("deployment revision worker_id and created_by must be non-empty and at most 256 bytes".into());
        }
        if self.revision == 0 {
            return Err("deployment revision must be positive".into());
        }
        validate_sha256("bundle_sha256", &self.bundle_sha256)?;
        validate_sha256("plan_sha256", &self.plan_sha256)?;
        if let Some(tree) = &self.dependency_tree {
            tree.validate()?;
        }
        if let Some(name) = self.bundle_file_name.as_deref() {
            let prefix = format!("{}.", self.bundle_sha256.to_ascii_lowercase());
            let suffix = name.strip_prefix(&prefix).ok_or_else(|| {
                "bundle_file_name must begin with the lowercase bundle_sha256 and a dot".to_string()
            })?;
            if suffix.is_empty()
                || suffix.len() > 16
                || !suffix.bytes().all(|byte| byte.is_ascii_alphanumeric())
            {
                return Err("bundle_file_name must have a short alphanumeric extension".into());
            }
        }
        match (
            self.runtime_config_json.as_deref(),
            self.runtime_config_sha256.as_deref(),
        ) {
            (None, None) => {}
            (Some(payload), Some(digest)) => {
                if payload.trim().is_empty() || payload.len() > MAX_STRATEGY_DEPLOYMENT_CONFIG_BYTES
                {
                    return Err(format!(
                        "runtime_config_json must be non-empty and at most {MAX_STRATEGY_DEPLOYMENT_CONFIG_BYTES} bytes"
                    ));
                }
                let parsed: serde_json::Value = serde_json::from_str(payload)
                    .map_err(|error| format!("runtime_config_json is invalid JSON: {error}"))?;
                if !parsed.is_object() {
                    return Err("runtime_config_json must contain a JSON object".into());
                }
                validate_sha256("runtime_config_sha256", digest)?;
                let actual = qx_data::sha256_hex_bytes(payload.as_bytes());
                if actual != digest {
                    return Err("runtime_config_sha256 does not match the frozen payload".into());
                }
            }
            _ => {
                return Err(
                    "runtime_config_json and runtime_config_sha256 must be supplied together"
                        .into(),
                );
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StrategyDeploymentActionV1 {
    Deploy,
    Stop,
    Rollback,
    Pause,
    Resume,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StrategyDeploymentOperationStateV1 {
    Accepted,
    Succeeded,
    Failed,
    Cancelled,
}

impl StrategyDeploymentOperationStateV1 {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StrategyDeploymentOperationV1 {
    pub schema_version: u32,
    pub kind: String,
    pub operation_id: String,
    pub idempotency_key: String,
    pub request_sha256: String,
    pub worker_id: String,
    pub action: StrategyDeploymentActionV1,
    pub target_revision: u64,
    pub requested_at_ms: u64,
    pub requested_by: String,
    pub state: StrategyDeploymentOperationStateV1,
    pub accepted_at_ms: u64,
    pub completed_at_ms: Option<u64>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
}

impl StrategyDeploymentOperationV1 {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != STRATEGY_DEPLOYMENT_OPERATION_SCHEMA_VERSION {
            return Err(format!(
                "unsupported deployment operation schema_version {}; expected {}",
                self.schema_version, STRATEGY_DEPLOYMENT_OPERATION_SCHEMA_VERSION
            ));
        }
        if self.kind != "qianxing_strategy_deployment_operation" {
            return Err("invalid deployment operation kind".into());
        }
        for (name, value) in [
            ("operation_id", self.operation_id.as_str()),
            ("idempotency_key", self.idempotency_key.as_str()),
            ("worker_id", self.worker_id.as_str()),
            ("requested_by", self.requested_by.as_str()),
        ] {
            if !bounded_non_empty(value, 256) {
                return Err(format!(
                    "deployment operation {name} must be non-empty and at most 256 bytes"
                ));
            }
        }
        validate_sha256("request_sha256", &self.request_sha256)?;
        if self.target_revision == 0 {
            return Err("deployment operation target_revision must be positive".into());
        }
        if self.accepted_at_ms < self.requested_at_ms {
            return Err("deployment operation cannot be accepted before it was requested".into());
        }
        if self.state.is_terminal() != self.completed_at_ms.is_some() {
            return Err("terminal deployment operations require completed_at_ms; non-terminal operations forbid it".into());
        }
        if self
            .completed_at_ms
            .is_some_and(|completed| completed < self.accepted_at_ms)
        {
            return Err("deployment operation cannot complete before it was accepted".into());
        }
        let failed = self.state == StrategyDeploymentOperationStateV1::Failed;
        if failed != self.error_code.is_some() || failed != self.error_message.is_some() {
            return Err(
                "only failed deployment operations require both error_code and error_message"
                    .into(),
            );
        }
        if self
            .error_code
            .as_ref()
            .is_some_and(|code| !bounded_non_empty(code, 128))
            || self
                .error_message
                .as_ref()
                .is_some_and(|message| !bounded_non_empty(message, 2048))
        {
            return Err(
                "deployment operation failure code/message is empty or exceeds its size limit"
                    .into(),
            );
        }
        Ok(())
    }
}

fn validate_sha256(name: &str, value: &str) -> Result<(), String> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!("{name} must be a 64-character SHA-256 hex digest"));
    }
    Ok(())
}

fn bounded_non_empty(value: &str, max_bytes: usize) -> bool {
    !value.trim().is_empty() && value.len() <= max_bytes
}
