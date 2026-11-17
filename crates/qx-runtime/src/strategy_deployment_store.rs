//! Durable single-node registry for immutable strategy deployment revisions and operations.

use crate::{RuntimeConfig, StorageBackend};
use qx_core::FileLock;
use qx_spec::{
    StrategyDeploymentActionV1, StrategyDeploymentOperationStateV1, StrategyDeploymentOperationV1,
    StrategyDeploymentRevisionV1,
};
use qx_storage::JsonStateStore;
use serde::{Deserialize, Serialize};
#[cfg(any(feature = "postgres", test))]
use std::io::{Read, Write};
use std::path::Path;

const REGISTRY_PATH: &str = "strategy-deployments/registry-v1.json";
const REGISTRY_LOCK: &str = "strategy-deployments/registry-v1.lock";
const MAX_REVISIONS: usize = 100_000;
const MAX_OPERATIONS: usize = 100_000;
const MAX_REGISTRY_BYTES: usize = 64 * 1024 * 1024;
const MAX_BUNDLE_FILE_BYTES: u64 = 256 * 1024 * 1024;
#[cfg(feature = "postgres")]
const DEPLOYMENT_CACHE_DIR: &str = "strategy-deployment-cache";
pub const MAX_RECENT_OPERATIONS_PER_WORKER: usize = 20;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StrategyDeploymentRegistryV1 {
    schema_version: u32,
    revisions: Vec<StrategyDeploymentRevisionV1>,
    operations: Vec<StrategyDeploymentOperationV1>,
}

impl StrategyDeploymentRegistryV1 {
    fn validate(&self) -> Result<(), String> {
        if self.schema_version != 1 {
            return Err(format!(
                "unsupported strategy deployment registry version {}",
                self.schema_version
            ));
        }
        if self.revisions.len() > MAX_REVISIONS || self.operations.len() > MAX_OPERATIONS {
            return Err("strategy deployment registry exceeds configured record limits".into());
        }
        if serde_json::to_vec(self)
            .map_err(|error| format!("cannot size deployment registry: {error}"))?
            .len()
            > MAX_REGISTRY_BYTES
        {
            return Err("strategy deployment registry exceeds 64 MiB".into());
        }
        let mut revision_keys = std::collections::BTreeSet::new();
        for revision in &self.revisions {
            revision.validate()?;
            if !revision_keys.insert((revision.worker_id.as_str(), revision.revision)) {
                return Err("strategy deployment registry contains duplicate revisions".into());
            }
        }
        let mut operation_ids = std::collections::BTreeSet::new();
        let mut idempotency_keys = std::collections::BTreeMap::new();
        for operation in &self.operations {
            operation.validate()?;
            if !operation_ids.insert(operation.operation_id.as_str()) {
                return Err("strategy deployment registry contains duplicate operation IDs".into());
            }
            let key = (
                operation.worker_id.as_str(),
                operation.requested_by.as_str(),
                operation.idempotency_key.as_str(),
            );
            if idempotency_keys
                .insert(key, operation.request_sha256.as_str())
                .is_some()
            {
                return Err(
                    "strategy deployment registry contains duplicate idempotency keys".into(),
                );
            }
            if !revision_keys.contains(&(operation.worker_id.as_str(), operation.target_revision)) {
                return Err(format!(
                    "operation {} references a missing worker revision",
                    operation.operation_id
                ));
            }
        }
        Ok(())
    }
}

/// A durable registry rooted at the Runtime `storage.data_dir`.
#[derive(Clone, Debug)]
pub struct StrategyDeploymentStore {
    root: std::path::PathBuf,
    #[cfg(feature = "postgres")]
    postgres: Option<qx_storage::PostgresJsonStateStore>,
    #[cfg(feature = "postgres")]
    postgres_blobs: Option<qx_storage::PostgresBlobStore>,
}

impl StrategyDeploymentStore {
    pub fn new(root: impl Into<std::path::PathBuf>) -> Self {
        Self {
            root: root.into(),
            #[cfg(feature = "postgres")]
            postgres: None,
            #[cfg(feature = "postgres")]
            postgres_blobs: None,
        }
    }

    /// Select the deployment registry and immutable artifact backend from Runtime storage.
    pub fn configured(
        root: impl Into<std::path::PathBuf>,
        config: &RuntimeConfig,
    ) -> Result<Self, String> {
        let root = root.into();
        if config.storage.backend != StorageBackend::Postgres {
            return Ok(Self::new(root));
        }
        #[cfg(not(feature = "postgres"))]
        {
            let _ = root;
            Err("PostgreSQL strategy deployment storage requires the postgres feature".into())
        }
        #[cfg(feature = "postgres")]
        {
            let env_key = config.storage.postgres_dsn_env.as_deref().ok_or_else(|| {
                "PostgreSQL deployment registry requires postgres_dsn_env".to_string()
            })?;
            let dsn = std::env::var(env_key)
                .map_err(|_| format!("PostgreSQL DSN environment variable {env_key} is not set"))?;
            Self::new_postgres(root, &dsn)
        }
    }

    #[cfg(feature = "postgres")]
    pub fn new_postgres(root: impl Into<std::path::PathBuf>, dsn: &str) -> Result<Self, String> {
        let postgres = qx_storage::PostgresJsonStateStore::connect(dsn).map_err(|error| {
            format!("cannot connect PostgreSQL strategy deployment registry: {error:?}")
        })?;
        let postgres_blobs = qx_storage::PostgresBlobStore::connect(dsn).map_err(|error| {
            format!("cannot connect PostgreSQL strategy deployment blob store: {error:?}")
        })?;
        Ok(Self {
            root: root.into(),
            postgres: Some(postgres),
            postgres_blobs: Some(postgres_blobs),
        })
    }

    /// Record a revision once. The per-worker revision sequence is strictly increasing.
    pub fn create_revision(
        &self,
        revision: StrategyDeploymentRevisionV1,
    ) -> Result<StrategyDeploymentRevisionV1, String> {
        revision.validate()?;
        if revision.runtime_config_json.is_none() {
            return Err(
                "new deployment revisions require a materializable runtime config payload".into(),
            );
        }
        if revision.bundle_file_name.is_none() {
            return Err(
                "new deployment revisions require a content-addressed bundle file name".into(),
            );
        }
        let frozen_config =
            RuntimeConfig::from_json(revision.runtime_config_json.as_deref().ok_or_else(
                || "new deployment revision requires runtime_config_json".to_string(),
            )?)?;
        let strategy = frozen_config.strategy_for_worker(&revision.worker_id)?;
        if strategy.python_module.is_none() && !strategy.python_resource_paths.is_empty() {
            return Err("python_resource_paths require a Python strategy module".into());
        }
        match (
            strategy.python_module.is_some(),
            revision.dependency_tree.is_some(),
        ) {
            (true, false) => return Err(
                "new file-backed Python deployment revisions require a dependency tree manifest"
                    .into(),
            ),
            (false, true) => {
                return Err(
                    "non-Python deployment revisions cannot carry a Python dependency tree".into(),
                );
            }
            _ => {}
        }
        #[cfg(feature = "postgres")]
        if self.postgres_blobs.is_some() {
            self.persist_revision_blobs(&revision)?;
        } else {
            self.verify_bundle_blob(&revision)?;
        }
        #[cfg(not(feature = "postgres"))]
        self.verify_bundle_blob(&revision)?;
        self.transact(|registry| {
            let latest = registry
                .revisions
                .iter()
                .filter(|current| current.worker_id == revision.worker_id)
                .map(|current| current.revision)
                .max()
                .unwrap_or(0);
            let expected = latest
                .checked_add(1)
                .ok_or_else(|| "strategy deployment revision sequence exhausted".to_string())?;
            if revision.revision != expected {
                return Err(format!(
                    "deployment revision sequence conflict: expected {expected}, got {}",
                    revision.revision
                ));
            }
            if registry.revisions.len() >= MAX_REVISIONS {
                return Err("strategy deployment revision capacity reached".into());
            }
            registry.revisions.push(revision.clone());
            Ok(revision)
        })
    }

    /// Accept an operation exactly once per worker/operator/idempotency key.
    /// Repeating an identical request returns its original operation; changing its
    /// request digest under the same key is a conflict.
    pub fn accept_operation(
        &self,
        operation: StrategyDeploymentOperationV1,
    ) -> Result<StrategyDeploymentOperationV1, String> {
        operation.validate()?;
        if operation.state != StrategyDeploymentOperationStateV1::Accepted
            || operation.completed_at_ms.is_some()
        {
            return Err("new deployment operations must enter as accepted".into());
        }
        self.transact(|registry| {
            if let Some(existing) = registry.operations.iter().find(|existing| {
                existing.worker_id == operation.worker_id
                    && existing.requested_by == operation.requested_by
                    && existing.idempotency_key == operation.idempotency_key
            }) {
                if existing.request_sha256 == operation.request_sha256 {
                    return Ok(existing.clone());
                }
                return Err("idempotency key was already used for a different request".into());
            }
            let revision = registry
                .revisions
                .iter()
                .find(|revision| {
                    revision.worker_id == operation.worker_id
                        && revision.revision == operation.target_revision
                })
                .ok_or_else(|| "deployment operation target revision does not exist".to_string())?;
            if revision.runtime_config_json.is_none() {
                return Err(
                    "deployment operation target revision has no materializable payload".into(),
                );
            }
            self.verify_bundle_blob(revision)?;
            let target_runs = match operation.action {
                StrategyDeploymentActionV1::Deploy
                | StrategyDeploymentActionV1::Rollback
                | StrategyDeploymentActionV1::Pause
                | StrategyDeploymentActionV1::Resume => true,
                StrategyDeploymentActionV1::Stop => false,
            };
            if revision.desired_running != target_runs {
                return Err(format!(
                    "deployment action {:?} conflicts with target revision desired_running={}",
                    operation.action, revision.desired_running
                ));
            }
            if registry
                .operations
                .iter()
                .any(|existing| existing.operation_id == operation.operation_id)
            {
                return Err("deployment operation ID already exists".into());
            }
            if registry.operations.len() >= MAX_OPERATIONS {
                return Err("strategy deployment operation capacity reached".into());
            }
            registry.operations.push(operation.clone());
            Ok(operation)
        })
    }

    /// Return accepted work in a stable order for a single-node coordinator.
    /// R09: intentionally dormant until the worker lifecycle coordinator owns dequeue.
    #[allow(dead_code)]
    pub fn accepted_operations(&self) -> Result<Vec<StrategyDeploymentOperationV1>, String> {
        let mut operations: Vec<_> = self
            .load()?
            .operations
            .into_iter()
            .filter(|operation| operation.state == StrategyDeploymentOperationStateV1::Accepted)
            .collect();
        operations.sort_by(|left, right| {
            (left.accepted_at_ms, &left.operation_id)
                .cmp(&(right.accepted_at_ms, &right.operation_id))
        });
        Ok(operations)
    }

    /// Persist one coordinator-reported terminal outcome.
    ///
    /// The transition is atomic with the registry update. Repeating the exact same
    /// terminal outcome is idempotent; attempting to replace it with a different
    /// outcome is rejected so a retry cannot rewrite history.
    /// R09: only coordinator-owned worker evidence may call this transition.
    #[allow(dead_code)]
    pub fn finish_operation(
        &self,
        operation_id: &str,
        state: StrategyDeploymentOperationStateV1,
        completed_at_ms: u64,
        error_code: Option<String>,
        error_message: Option<String>,
    ) -> Result<StrategyDeploymentOperationV1, String> {
        if !state.is_terminal() {
            return Err("deployment operation outcome must be terminal".into());
        }
        self.transact(|registry| {
            let operation = registry
                .operations
                .iter_mut()
                .find(|operation| operation.operation_id == operation_id)
                .ok_or_else(|| "deployment operation does not exist".to_string())?;
            if operation.state.is_terminal() {
                let identical = operation.state == state
                    && operation.completed_at_ms == Some(completed_at_ms)
                    && operation.error_code == error_code
                    && operation.error_message == error_message;
                return if identical {
                    Ok(operation.clone())
                } else {
                    Err("terminal deployment operation outcome is immutable".into())
                };
            }
            operation.state = state;
            operation.completed_at_ms = Some(completed_at_ms);
            operation.error_code = error_code;
            operation.error_message = error_message;
            operation.validate()?;
            Ok(operation.clone())
        })
    }

    pub fn revision(
        &self,
        worker_id: &str,
        revision: u64,
    ) -> Result<Option<StrategyDeploymentRevisionV1>, String> {
        Ok(self
            .load()?
            .revisions
            .into_iter()
            .find(|item| item.worker_id == worker_id && item.revision == revision))
    }

    pub fn latest_revision(
        &self,
        worker_id: &str,
    ) -> Result<Option<StrategyDeploymentRevisionV1>, String> {
        Ok(self
            .load()?
            .revisions
            .into_iter()
            .filter(|item| item.worker_id == worker_id)
            .max_by_key(|item| item.revision))
    }

    pub fn operation(
        &self,
        operation_id: &str,
    ) -> Result<Option<StrategyDeploymentOperationV1>, String> {
        let registry = self.load()?;
        Ok(registry
            .operations
            .into_iter()
            .find(|item| item.operation_id == operation_id))
    }

    /// Return the most recently completed successful operation for one worker.
    /// This is the durable desired state used when the Runtime supervisor restarts.
    pub fn latest_successful_operation(
        &self,
        worker_id: &str,
    ) -> Result<Option<StrategyDeploymentOperationV1>, String> {
        Ok(self
            .load()?
            .operations
            .into_iter()
            .filter(|operation| {
                operation.worker_id == worker_id
                    && operation.state == StrategyDeploymentOperationStateV1::Succeeded
            })
            .max_by(|left, right| {
                (
                    left.completed_at_ms,
                    left.accepted_at_ms,
                    &left.operation_id,
                )
                    .cmp(&(
                        right.completed_at_ms,
                        right.accepted_at_ms,
                        &right.operation_id,
                    ))
            }))
    }

    /// Return the newest accepted or terminal operation for one worker, including failures.
    /// This is the operator-facing desired/progress/error projection; it does not change the
    /// successful-operation recovery rule used by the supervisor.
    pub fn latest_operation(
        &self,
        worker_id: &str,
    ) -> Result<Option<StrategyDeploymentOperationV1>, String> {
        Ok(self
            .load()?
            .operations
            .into_iter()
            .filter(|operation| operation.worker_id == worker_id)
            .max_by(|left, right| {
                (
                    left.requested_at_ms,
                    left.accepted_at_ms,
                    &left.operation_id,
                )
                    .cmp(&(
                        right.requested_at_ms,
                        right.accepted_at_ms,
                        &right.operation_id,
                    ))
            }))
    }

    /// Return a bounded newest-first operation history for one worker.
    pub fn recent_operations(
        &self,
        worker_id: &str,
        limit: usize,
    ) -> Result<Vec<StrategyDeploymentOperationV1>, String> {
        let limit = limit.clamp(1, MAX_RECENT_OPERATIONS_PER_WORKER);
        let mut operations: Vec<_> = self
            .load()?
            .operations
            .into_iter()
            .filter(|operation| operation.worker_id == worker_id)
            .collect();
        operations.sort_by(|left, right| {
            (
                right.requested_at_ms,
                right.accepted_at_ms,
                &right.operation_id,
            )
                .cmp(&(
                    left.requested_at_ms,
                    left.accepted_at_ms,
                    &left.operation_id,
                ))
        });
        operations.truncate(limit);
        Ok(operations)
    }

    fn load(&self) -> Result<StrategyDeploymentRegistryV1, String> {
        #[cfg(feature = "postgres")]
        if let Some(postgres) = self.postgres.as_ref() {
            let registry = postgres
                .load_json_at::<StrategyDeploymentRegistryV1>(REGISTRY_PATH)
                .map_err(|error| {
                    format!("cannot read PostgreSQL strategy deployment registry: {error:?}")
                })?
                .unwrap_or_else(|| StrategyDeploymentRegistryV1 {
                    schema_version: 1,
                    ..StrategyDeploymentRegistryV1::default()
                });
            registry.validate()?;
            return Ok(registry);
        }
        let store = JsonStateStore::new(&self.root);
        let path = self.root.join(REGISTRY_PATH);
        if !path.exists() {
            return Ok(StrategyDeploymentRegistryV1 {
                schema_version: 1,
                ..StrategyDeploymentRegistryV1::default()
            });
        }
        check_registry_file_size(&path)?;
        let registry: StrategyDeploymentRegistryV1 = store
            .load_json_at(REGISTRY_PATH)
            .map_err(|error| format!("cannot read strategy deployment registry: {error:?}"))?;
        registry.validate()?;
        Ok(registry)
    }

    fn verify_bundle_blob(&self, revision: &StrategyDeploymentRevisionV1) -> Result<(), String> {
        self.verify_revision_bundle(revision).map(|_| ())
    }

    pub fn verify_revision_bundle(
        &self,
        revision: &StrategyDeploymentRevisionV1,
    ) -> Result<std::path::PathBuf, String> {
        #[cfg(feature = "postgres")]
        if self.postgres_blobs.is_some() {
            return self.materialize_postgres_revision(revision);
        }
        self.verify_revision_bundle_at(revision, &self.root)
    }

    fn verify_revision_bundle_at(
        &self,
        revision: &StrategyDeploymentRevisionV1,
        storage_root: &Path,
    ) -> Result<std::path::PathBuf, String> {
        let file_name = revision
            .bundle_file_name
            .as_deref()
            .ok_or_else(|| "deployment revision has no bundle file name".to_string())?;
        let path = storage_root
            .join("strategy-deployment-bundles")
            .join(file_name);
        let metadata = std::fs::metadata(&path).map_err(|error| {
            format!("deployment revision bundle is missing or unreadable: {error}")
        })?;
        if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_BUNDLE_FILE_BYTES {
            return Err("deployment revision bundle is not a bounded regular file".into());
        }
        let file = std::fs::File::open(&path).map_err(|error| error.to_string())?;
        let digest =
            qx_strategy::file_digest::sha256_reader_hex_bounded(file, MAX_BUNDLE_FILE_BYTES)?;
        if !digest.eq_ignore_ascii_case(&revision.bundle_sha256) {
            return Err("deployment revision bundle digest mismatch".into());
        }
        if let Some(tree) = &revision.dependency_tree {
            tree.validate()?;
            let entry = tree
                .files
                .iter()
                .find(|file| file.path == tree.entry_path)
                .ok_or_else(|| "dependency tree does not contain its entry module".to_string())?;
            if !entry.sha256.eq_ignore_ascii_case(&revision.bundle_sha256) {
                return Err("dependency tree entry digest differs from deployment bundle".into());
            }
            let tree_root = storage_root
                .join("strategy-deployment-trees")
                .join(&tree.tree_sha256);
            for file in &tree.files {
                let dependency = tree_root.join(&file.path);
                let metadata = std::fs::symlink_metadata(&dependency).map_err(|error| {
                    format!("deployment dependency is missing: {}: {error}", file.path)
                })?;
                if metadata.file_type().is_symlink()
                    || !metadata.is_file()
                    || metadata.len() != file.size_bytes
                {
                    return Err(format!(
                        "deployment dependency identity mismatch: {}",
                        file.path
                    ));
                }
                let dependency_handle =
                    std::fs::File::open(&dependency).map_err(|error| error.to_string())?;
                let digest = qx_strategy::file_digest::sha256_reader_hex_bounded(
                    dependency_handle,
                    qx_spec::MAX_STRATEGY_DEPENDENCY_TREE_FILE_BYTES,
                )?;
                if !digest.eq_ignore_ascii_case(&file.sha256) {
                    return Err(format!(
                        "deployment dependency digest mismatch: {}",
                        file.path
                    ));
                }
            }
            let actual = dependency_tree_files(&tree_root)?;
            let expected = tree
                .files
                .iter()
                .map(|file| file.path.clone())
                .collect::<std::collections::BTreeSet<_>>();
            if actual != expected {
                return Err(
                    "deployment dependency tree contains unmanifested dependency files".into(),
                );
            }
            return Ok(tree_root.join(&tree.entry_path));
        }
        Ok(path)
    }

    #[cfg(feature = "postgres")]
    fn persist_revision_blobs(
        &self,
        revision: &StrategyDeploymentRevisionV1,
    ) -> Result<(), String> {
        let blobs = self
            .postgres_blobs
            .as_ref()
            .ok_or_else(|| "PostgreSQL deployment blob store is not configured".to_string())?;
        self.verify_revision_bundle_at(revision, &self.root)?;
        let bundle_path = self.root.join("strategy-deployment-bundles").join(
            revision
                .bundle_file_name
                .as_deref()
                .ok_or_else(|| "deployment revision has no bundle file name".to_string())?,
        );
        let bundle =
            read_verified_artifact(&bundle_path, MAX_BUNDLE_FILE_BYTES, &revision.bundle_sha256)?;
        let mut artifacts = vec![(revision.bundle_sha256.to_ascii_lowercase(), bundle)];
        if let Some(tree) = revision.dependency_tree.as_ref() {
            let tree_root = self
                .root
                .join("strategy-deployment-trees")
                .join(&tree.tree_sha256);
            for file in &tree.files {
                let path = tree_root.join(&file.path);
                let bytes = read_verified_artifact(
                    &path,
                    qx_spec::MAX_STRATEGY_DEPENDENCY_TREE_FILE_BYTES,
                    &file.sha256,
                )?;
                artifacts.push((file.sha256.to_ascii_lowercase(), bytes));
            }
        }
        blobs.put_blobs_at(&artifacts).map_err(|error| {
            format!("cannot persist PostgreSQL strategy deployment blobs: {error:?}")
        })?;
        Ok(())
    }

    #[cfg(feature = "postgres")]
    fn materialize_postgres_revision(
        &self,
        revision: &StrategyDeploymentRevisionV1,
    ) -> Result<std::path::PathBuf, String> {
        let cache_root = self.root.join(DEPLOYMENT_CACHE_DIR);
        let file_name = revision
            .bundle_file_name
            .as_deref()
            .ok_or_else(|| "deployment revision has no bundle file name".to_string())?;
        let bundle = self.load_revision_blob_or_import(
            revision,
            &revision.bundle_sha256,
            MAX_BUNDLE_FILE_BYTES,
        )?;
        materialize_verified_artifact(
            &cache_root
                .join("strategy-deployment-bundles")
                .join(file_name),
            &bundle,
            &revision.bundle_sha256,
        )?;
        if let Some(tree) = revision.dependency_tree.as_ref() {
            tree.validate()?;
            let tree_root = cache_root
                .join("strategy-deployment-trees")
                .join(&tree.tree_sha256);
            for file in &tree.files {
                let bytes = self.load_revision_blob_or_import(
                    revision,
                    &file.sha256,
                    qx_spec::MAX_STRATEGY_DEPENDENCY_TREE_FILE_BYTES,
                )?;
                materialize_verified_artifact(&tree_root.join(&file.path), &bytes, &file.sha256)?;
            }
        }
        self.verify_revision_bundle_at(revision, &cache_root)
    }

    #[cfg(feature = "postgres")]
    fn load_revision_blob_or_import(
        &self,
        revision: &StrategyDeploymentRevisionV1,
        digest: &str,
        max_bytes: u64,
    ) -> Result<Vec<u8>, String> {
        let blobs = self
            .postgres_blobs
            .as_ref()
            .ok_or_else(|| "PostgreSQL deployment blob store is not configured".to_string())?;
        match load_verified_postgres_blob(blobs, digest, max_bytes) {
            Ok(bytes) => Ok(bytes),
            Err(read_error) => {
                // Migrate pre-v4 PostgreSQL registries lazily from their verified legacy files.
                self.persist_revision_blobs(revision).map_err(|import_error| {
                    format!(
                        "PostgreSQL deployment blob is unavailable ({read_error}); legacy file import failed: {import_error}"
                    )
                })?;
                load_verified_postgres_blob(blobs, digest, max_bytes)
            }
        }
    }

    fn transact<R>(
        &self,
        update: impl FnOnce(&mut StrategyDeploymentRegistryV1) -> Result<R, String>,
    ) -> Result<R, String> {
        #[cfg(feature = "postgres")]
        if let Some(postgres) = self.postgres.as_ref() {
            let initial = StrategyDeploymentRegistryV1 {
                schema_version: 1,
                ..StrategyDeploymentRegistryV1::default()
            };
            let (registry, result) = postgres
                .transact_json_at(REGISTRY_PATH, initial, |registry| {
                    registry.validate()?;
                    let result = update(registry)?;
                    registry.validate()?;
                    Ok::<_, String>(result)
                })
                .map_err(|error| {
                    format!("cannot persist PostgreSQL strategy deployment registry: {error:?}")
                })?;
            registry.validate()?;
            return result;
        }
        let lock_path = self.root.join(REGISTRY_LOCK);
        let _lock = FileLock::acquire(lock_path)
            .map_err(|error| format!("cannot lock strategy deployment registry: {error}"))?;
        let store = JsonStateStore::new(&self.root);
        let path = self.root.join(REGISTRY_PATH);
        if !path.exists() {
            store
                .create_json_at(
                    REGISTRY_PATH,
                    &StrategyDeploymentRegistryV1 {
                        schema_version: 1,
                        ..StrategyDeploymentRegistryV1::default()
                    },
                )
                .map_err(|error| {
                    format!("cannot initialize strategy deployment registry: {error:?}")
                })?;
        }
        check_registry_file_size(&path)?;
        let (registry, result) = store
            .transact_json_at(
                REGISTRY_PATH,
                |registry: &mut StrategyDeploymentRegistryV1| {
                    registry.validate()?;
                    let result = update(registry)?;
                    registry.validate()?;
                    Ok::<_, String>(result)
                },
            )
            .map_err(|error| format!("cannot persist strategy deployment registry: {error:?}"))?;
        // The read path and transactional path both validate the exact same registry invariants.
        registry.validate()?;
        result
    }
}

#[cfg(any(feature = "postgres", test))]
fn read_verified_artifact(path: &Path, max_bytes: u64, digest: &str) -> Result<Vec<u8>, String> {
    let metadata = std::fs::symlink_metadata(path).map_err(|error| {
        format!(
            "cannot inspect deployment artifact {}: {error}",
            path.display()
        )
    })?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() == 0
        || metadata.len() > max_bytes
    {
        return Err(format!(
            "deployment artifact is not a bounded regular file: {}",
            path.display()
        ));
    }
    let mut bytes = Vec::with_capacity(metadata.len().min(1024 * 1024) as usize);
    std::fs::File::open(path)
        .map_err(|error| error.to_string())?
        .take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > max_bytes
        || !qx_strategy::sha256_hex(&bytes).eq_ignore_ascii_case(digest)
    {
        return Err(format!(
            "deployment artifact size or digest mismatch: {}",
            path.display()
        ));
    }
    Ok(bytes)
}

#[cfg(feature = "postgres")]
fn load_verified_postgres_blob(
    blobs: &qx_storage::PostgresBlobStore,
    digest: &str,
    max_bytes: u64,
) -> Result<Vec<u8>, String> {
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("deployment blob digest is not a SHA-256 value".into());
    }
    let bytes = blobs
        .load_blob_at(&digest.to_ascii_lowercase())
        .map_err(|error| format!("cannot load PostgreSQL deployment blob: {error:?}"))?
        .ok_or_else(|| format!("PostgreSQL deployment blob {digest} is missing"))?;
    if bytes.is_empty()
        || bytes.len() as u64 > max_bytes
        || !qx_strategy::sha256_hex(&bytes).eq_ignore_ascii_case(digest)
    {
        return Err(format!(
            "PostgreSQL deployment blob {digest} failed integrity checks"
        ));
    }
    Ok(bytes)
}

#[cfg(any(feature = "postgres", test))]
fn materialize_verified_artifact(path: &Path, bytes: &[u8], digest: &str) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("deployment artifact path has no parent: {}", path.display()))?;
    std::fs::create_dir_all(parent).map_err(|error| {
        format!(
            "cannot create deployment artifact cache {}: {error}",
            parent.display()
        )
    })?;
    let _lock = FileLock::acquire(path.with_extension("blob.lock"))
        .map_err(|error| format!("cannot lock deployment artifact cache: {error}"))?;
    match std::fs::symlink_metadata(path) {
        Ok(_) => {
            let existing = read_verified_artifact(path, bytes.len() as u64, digest)?;
            if existing == bytes {
                return Ok(());
            }
            return Err(format!(
                "deployment artifact cache digest collision: {}",
                path.display()
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!("cannot inspect deployment artifact cache: {error}"));
        }
    }
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| format!("system clock unavailable: {error}"))?
        .as_nanos();
    let temporary = parent.join(format!(
        ".deployment-blob-{}-{nonce}.tmp",
        std::process::id()
    ));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|error| format!("cannot create deployment artifact cache file: {error}"))?;
        file.write_all(bytes)
            .and_then(|_| file.sync_all())
            .map_err(|error| format!("cannot sync deployment artifact cache file: {error}"))?;
        std::fs::rename(&temporary, path)
            .map_err(|error| format!("cannot publish deployment artifact cache file: {error}"))
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result?;
    Ok(())
}

fn dependency_tree_files(root: &Path) -> Result<std::collections::BTreeSet<String>, String> {
    fn visit(
        root: &Path,
        current: &Path,
        depth: usize,
        visited: &mut usize,
        paths: &mut std::collections::BTreeSet<String>,
    ) -> Result<(), String> {
        if depth > 32 {
            return Err("deployment dependency tree exceeds maximum directory depth".into());
        }
        for entry in std::fs::read_dir(current).map_err(|error| error.to_string())? {
            *visited += 1;
            if *visited > 100_000 {
                return Err(
                    "deployment dependency tree contains too many filesystem entries".into(),
                );
            }
            let entry = entry.map_err(|error| error.to_string())?;
            let path = entry.path();
            let metadata = std::fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
            if metadata.file_type().is_symlink() {
                return Err("deployment dependency tree contains a symlink".into());
            }
            if metadata.is_dir() {
                if entry.file_name().to_str() == Some("__pycache__") {
                    continue;
                }
                visit(root, &path, depth + 1, visited, paths)?;
            } else if metadata.is_file() {
                let relative = path
                    .strip_prefix(root)
                    .map_err(|error| error.to_string())?
                    .to_str()
                    .ok_or_else(|| "deployment dependency path is not UTF-8".to_string())?
                    .replace(std::path::MAIN_SEPARATOR, "/");
                paths.insert(relative);
            } else {
                return Err(format!(
                    "deployment dependency tree contains an unsupported filesystem entry: {}",
                    path.display()
                ));
            }
        }
        Ok(())
    }
    let mut visited = 0;
    let mut paths = std::collections::BTreeSet::new();
    visit(root, root, 0, &mut visited, &mut paths)?;
    Ok(paths)
}

fn check_registry_file_size(path: &Path) -> Result<(), String> {
    let metadata = std::fs::metadata(path)
        .map_err(|error| format!("cannot inspect strategy deployment registry: {error}"))?;
    if metadata.len() > MAX_REGISTRY_BYTES as u64 {
        return Err("strategy deployment registry exceeds 64 MiB".into());
    }
    Ok(())
}

#[cfg(test)]
mod artifact_cache_tests {
    use super::*;

    #[test]
    fn deployment_artifact_cache_materializes_verified_content_idempotently() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock after epoch")
            .as_nanos();
        let root = std::env::current_dir()
            .expect("current workspace directory")
            .join("target")
            .join(format!("qx-deployment-cache-{nonce}"));
        let path = root.join("bundle/module.py");
        let bytes = b"strategy = 'immutable'";
        let digest = qx_strategy::sha256_hex(bytes);

        materialize_verified_artifact(&path, bytes, &digest).expect("materialize bundle");
        materialize_verified_artifact(&path, bytes, &digest).expect("reuse verified bundle");
        assert_eq!(read_verified_artifact(&path, 1024, &digest).unwrap(), bytes);

        std::fs::write(&path, b"tampered").expect("tamper cache fixture");
        assert!(materialize_verified_artifact(&path, bytes, &digest).is_err());
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(feature = "postgres")]
    #[test]
    #[ignore = "requires QX_TEST_POSTGRES_DSN and a running PostgreSQL instance"]
    fn postgres_deployment_blob_rehydrates_into_verified_runtime_cache() {
        let dsn = std::env::var("QX_TEST_POSTGRES_DSN")
            .expect("QX_TEST_POSTGRES_DSN must point at an isolated test database");
        let bytes = format!(
            "strategy-runtime-cache-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock after epoch")
                .as_nanos()
        )
        .into_bytes();
        let digest = qx_strategy::sha256_hex(&bytes);
        let store = qx_storage::PostgresBlobStore::connect(&dsn).expect("connect blob store");
        store
            .put_blobs_at(&[(digest.clone(), bytes.clone())])
            .expect("persist PostgreSQL blob");
        let restored = load_verified_postgres_blob(&store, &digest, 1024)
            .expect("read and verify PostgreSQL blob");
        let root = std::env::current_dir()
            .expect("current workspace directory")
            .join("target")
            .join(format!("qx-pg-artifact-cache-{digest}"));
        let path = root.join("bundle.bin");
        materialize_verified_artifact(&path, &restored, &digest)
            .expect("materialize PostgreSQL blob");
        assert_eq!(read_verified_artifact(&path, 1024, &digest).unwrap(), bytes);
        let _ = std::fs::remove_dir_all(root);
    }
}
