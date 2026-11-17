//! Durable operational observations, bound to the exact Runtime configuration.
use qx_api::{StrategyDeploymentStatusV1, StrategyWorkerObservationV3, StrategyWorkerPhase};
use qx_core::FileLock;
use qx_runtime::{RuntimeConfig, WorkerRole};
use qx_spec::{StrategyDependencyTreeFileV1, StrategyDependencyTreeV1};
use qx_storage::JsonStateStore;
use qx_strategy::capabilities::StrategyHostCapabilitiesV1;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

const OBSERVATION_RETENTION_MS: u64 = 30 * 24 * 60 * 60 * 1000;
const OBSERVATION_PRUNE_INTERVAL_MS: u64 = 60 * 60 * 1000;
const MAX_OBSERVATION_FILES_PER_PRUNE: usize = 100_000;

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

pub fn config_digest(config: &RuntimeConfig) -> Result<String, String> {
    qx_runtime::runtime_config_digest(config)
}

/// Shared validation for a frozen strategy runtime revision.
pub fn deployment_runtime_config(
    base: &RuntimeConfig,
    revision: &qx_spec::StrategyDeploymentRevisionV1,
) -> Result<RuntimeConfig, String> {
    qx_runtime::deployment_runtime_config(base, revision)
}

/// Freeze the exact executable artifact selected by an immutable strategy revision.
/// File-backed Python/external strategies require explicit paths and declared hashes;
/// built-in strategies bind to the current host executable bytes.
pub fn snapshot_strategy_bundle(
    data_dir: &Path,
    runtime_config_path: &Path,
    revision: &mut qx_spec::StrategyDeploymentRevisionV1,
    candidate: &RuntimeConfig,
) -> Result<PathBuf, String> {
    const MAX_BUNDLE_FILE_BYTES: u64 = 256 * 1024 * 1024;
    let strategy = candidate.strategy_for_worker(&revision.worker_id)?;
    let (source, expected_digest, python_dependency_root) = if let Some(library) =
        strategy.c_abi_library.as_deref()
    {
        let digest = strategy
            .c_abi_sha256
            .as_deref()
            .ok_or_else(|| "deployment C ABI bundle requires c_abi_sha256".to_string())?;
        (
            qx_runtime::resolve_runtime_relative_path(runtime_config_path, library),
            digest.to_string(),
            None,
        )
    } else if let Some(executable) = strategy.external_executable.as_deref() {
        let path_like = executable.contains('/')
            || executable.contains('\\')
            || executable.starts_with('.')
            || Path::new(executable).is_absolute();
        if !path_like {
            return Err("deployment external_executable must be an explicit file path".into());
        }
        let digest = strategy
            .strategy_artifact_sha256
            .as_deref()
            .ok_or_else(|| {
                "deployment external strategy requires strategy_artifact_sha256".to_string()
            })?;
        (
            qx_runtime::resolve_runtime_relative_path(runtime_config_path, executable),
            digest.to_string(),
            None,
        )
    } else if let Some(module) = strategy.python_module.as_deref() {
        let path_like = module.ends_with(".py")
            || module.contains('/')
            || module.contains('\\')
            || module.starts_with('.')
            || Path::new(module).is_absolute();
        let digest = strategy
            .strategy_artifact_sha256
            .as_deref()
            .ok_or_else(|| {
                "deployment Python strategy requires strategy_artifact_sha256".to_string()
            })?;
        let (source, root) = if path_like {
            let source = qx_runtime::resolve_runtime_relative_path(runtime_config_path, module);
            let root = source
                .parent()
                .ok_or_else(|| "Python deployment entry has no parent directory".to_string())?
                .to_path_buf();
            (source, root)
        } else {
            resolve_trusted_python_module(runtime_config_path, module)?
        };
        (source, digest.to_string(), Some(root))
    } else if strategy.builtin_strategy.is_some() {
        let executable = std::env::current_exe().map_err(|error| {
            format!("cannot resolve host executable for built-in strategy: {error}")
        })?;
        let digest = bounded_bundle_digest(&executable, MAX_BUNDLE_FILE_BYTES)?;
        (executable, digest, None)
    } else {
        return Err("deployment target must select one built-in or file-backed strategy".into());
    };

    let source = source.canonicalize().map_err(|error| {
        format!(
            "cannot resolve deployment bundle source {}: {error}",
            source.display()
        )
    })?;
    let metadata = fs::metadata(&source).map_err(|error| {
        format!(
            "cannot inspect deployment bundle source {}: {error}",
            source.display()
        )
    })?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_BUNDLE_FILE_BYTES {
        return Err(format!(
            "deployment bundle source must be a regular file no larger than {MAX_BUNDLE_FILE_BYTES} bytes"
        ));
    }
    let source_digest = bounded_bundle_digest(&source, MAX_BUNDLE_FILE_BYTES)?;
    if !source_digest.eq_ignore_ascii_case(&expected_digest)
        || !source_digest.eq_ignore_ascii_case(&revision.bundle_sha256)
    {
        return Err(
            "deployment bundle digest does not match the selected strategy artifact".into(),
        );
    }

    let directory = data_dir.join("strategy-deployment-bundles");
    fs::create_dir_all(&directory)
        .map_err(|error| format!("cannot create deployment bundle store: {error}"))?;
    let _lock = FileLock::acquire(directory.join("bundles.lock"))
        .map_err(|error| format!("cannot lock deployment bundle store: {error}"))?;
    let extension = source
        .extension()
        .and_then(|value| value.to_str())
        .filter(|value| {
            !value.is_empty()
                && value.len() <= 16
                && value.bytes().all(|byte| byte.is_ascii_alphanumeric())
        })
        .unwrap_or("blob");
    let target = directory.join(format!(
        "{}.{}",
        source_digest.to_ascii_lowercase(),
        extension
    ));
    if let Some(python_dependency_root) = python_dependency_root {
        revision.dependency_tree = Some(snapshot_python_dependency_tree(
            data_dir,
            &source,
            &python_dependency_root,
            &strategy.python_resource_paths,
            &source_digest,
        )?);
    }
    if target.exists() {
        if bounded_bundle_digest(&target, MAX_BUNDLE_FILE_BYTES)? != source_digest {
            return Err("content-addressed deployment bundle blob is corrupt".into());
        }
        return Ok(target);
    }
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temporary = directory.join(format!(".bundle-{}-{nonce}.tmp", std::process::id()));
    let result = (|| {
        let input = File::open(&source).map_err(|error| error.to_string())?;
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|error| error.to_string())?;
        let copied_bytes = std::io::copy(
            &mut input.take(MAX_BUNDLE_FILE_BYTES.saturating_add(1)),
            &mut output,
        )
        .map_err(|error| error.to_string())?;
        if copied_bytes > MAX_BUNDLE_FILE_BYTES {
            return Err("deployment bundle source grew beyond the size limit".into());
        }
        fs::set_permissions(&temporary, metadata.permissions())
            .map_err(|error| error.to_string())?;
        output.flush().map_err(|error| error.to_string())?;
        output.sync_all().map_err(|error| error.to_string())?;
        drop(output);
        if bounded_bundle_digest(&temporary, MAX_BUNDLE_FILE_BYTES)? != source_digest {
            return Err("deployment bundle source changed while snapshotting".into());
        }
        fs::rename(&temporary, &target).map_err(|error| error.to_string())?;
        Ok(target.clone())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn snapshot_python_dependency_tree(
    data_dir: &Path,
    entry_source: &Path,
    source_root: &Path,
    declared_resources: &[String],
    entry_sha256: &str,
) -> Result<StrategyDependencyTreeV1, String> {
    const MAX_DIRECTORY_ENTRIES: usize = 100_000;
    const MAX_TREE_DEPTH: usize = 32;
    let root = source_root
        .canonicalize()
        .map_err(|error| format!("cannot resolve Python strategy source root: {error}"))?;
    let entry_source = entry_source
        .canonicalize()
        .map_err(|error| format!("cannot resolve Python strategy entry: {error}"))?;
    let entry_path = entry_source
        .strip_prefix(&root)
        .map_err(|_| "Python deployment entry is outside its source root".to_string())?
        .to_str()
        .ok_or_else(|| "Python deployment entry path is not UTF-8".to_string())?
        .replace(std::path::MAIN_SEPARATOR, "/");
    let mut sources = std::collections::BTreeMap::<String, (PathBuf, u64, String)>::new();
    let mut visited = 0usize;
    collect_python_sources(
        &root,
        &root,
        0,
        &mut visited,
        &mut sources,
        MAX_DIRECTORY_ENTRIES,
        MAX_TREE_DEPTH,
    )?;
    for resource in declared_resources {
        let relative = Path::new(resource);
        if resource.is_empty()
            || resource.contains('\\')
            || resource.contains(':')
            || relative.is_absolute()
            || relative
                .components()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            return Err(format!("unsafe Python resource path {resource:?}"));
        }
        let candidate = root.join(relative);
        let metadata = fs::symlink_metadata(&candidate)
            .map_err(|error| format!("cannot inspect Python resource {resource}: {error}"))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(format!(
                "Python resource must be a regular non-symlink file: {resource}"
            ));
        }
        if metadata.len() == 0 || metadata.len() > qx_spec::MAX_STRATEGY_DEPENDENCY_TREE_FILE_BYTES
        {
            return Err(format!("Python resource is empty or too large: {resource}"));
        }
        let canonical = candidate
            .canonicalize()
            .map_err(|error| format!("cannot resolve Python resource {resource}: {error}"))?;
        if !canonical.starts_with(&root) {
            return Err(format!(
                "Python resource escapes the trusted source root: {resource}"
            ));
        }
        let path = canonical
            .strip_prefix(&root)
            .map_err(|error| error.to_string())?
            .to_str()
            .ok_or_else(|| "Python resource path is not UTF-8".to_string())?
            .replace(std::path::MAIN_SEPARATOR, "/");
        let digest =
            bounded_bundle_digest(&canonical, qx_spec::MAX_STRATEGY_DEPENDENCY_TREE_FILE_BYTES)?;
        if let Some((_, existing_size, existing_digest)) = sources.get(&path) {
            if *existing_size != metadata.len() || !existing_digest.eq_ignore_ascii_case(&digest) {
                return Err(format!(
                    "Python resource identity changed while snapshotting: {resource}"
                ));
            }
        } else {
            sources.insert(path, (canonical, metadata.len(), digest));
        }
    }
    if sources.len() > qx_spec::MAX_STRATEGY_DEPENDENCY_TREE_FILES {
        return Err("Python dependency tree contains too many source and resource files".into());
    }
    let entry = sources
        .get(&entry_path)
        .ok_or_else(|| "Python dependency tree did not include its entry module".to_string())?;
    if !entry.2.eq_ignore_ascii_case(entry_sha256) {
        return Err("Python dependency tree entry digest differs from the bundle digest".into());
    }
    let files = sources
        .iter()
        .map(
            |(path, (_, size_bytes, sha256))| StrategyDependencyTreeFileV1 {
                path: path.clone(),
                size_bytes: *size_bytes,
                sha256: sha256.clone(),
            },
        )
        .collect::<Vec<_>>();
    let manifest = StrategyDependencyTreeV1::new(entry_path, files)?;
    let tree_root = data_dir
        .join("strategy-deployment-trees")
        .join(&manifest.tree_sha256);
    let trees_directory = data_dir.join("strategy-deployment-trees");
    fs::create_dir_all(&trees_directory)
        .map_err(|error| format!("cannot create strategy dependency tree store: {error}"))?;
    let _lock = FileLock::acquire(trees_directory.join("trees.lock"))
        .map_err(|error| format!("cannot lock strategy dependency tree store: {error}"))?;
    if tree_root.exists() {
        verify_python_dependency_tree(&tree_root, &manifest)?;
        return Ok(manifest);
    }
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let staging = trees_directory.join(format!(".tree-{}-{nonce}.tmp", std::process::id()));
    fs::create_dir(&staging)
        .map_err(|error| format!("cannot create dependency tree staging directory: {error}"))?;
    let result = (|| {
        for file in &manifest.files {
            let (source, expected_size, expected_digest) = sources
                .get(&file.path)
                .ok_or_else(|| format!("dependency tree source disappeared: {}", file.path))?;
            let input = File::open(source).map_err(|error| {
                format!(
                    "cannot read Python dependency or resource {}: {error}",
                    source.display()
                )
            })?;
            let mut bytes = Vec::with_capacity((*expected_size).min(1024 * 1024) as usize);
            input
                .take(qx_spec::MAX_STRATEGY_DEPENDENCY_TREE_FILE_BYTES + 1)
                .read_to_end(&mut bytes)
                .map_err(|error| error.to_string())?;
            if bytes.len() as u64 > qx_spec::MAX_STRATEGY_DEPENDENCY_TREE_FILE_BYTES {
                return Err(format!(
                    "Python dependency or resource grew beyond its size limit: {}",
                    source.display()
                ));
            }
            if bytes.len() as u64 != *expected_size
                || !qx_data::sha256_hex_bytes(&bytes).eq_ignore_ascii_case(expected_digest)
            {
                return Err(format!(
                    "Python dependency or resource changed while snapshotting: {}",
                    source.display()
                ));
            }
            let target = staging.join(&file.path);
            let parent = target
                .parent()
                .ok_or_else(|| "dependency tree target has no parent".to_string())?;
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
            let mut output = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&target)
                .map_err(|error| error.to_string())?;
            output
                .write_all(&bytes)
                .map_err(|error| error.to_string())?;
            output.flush().map_err(|error| error.to_string())?;
            output.sync_all().map_err(|error| error.to_string())?;
        }
        verify_python_dependency_tree(&staging, &manifest)?;
        fs::rename(&staging, &tree_root).map_err(|error| error.to_string())?;
        Ok(manifest.clone())
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&staging);
    }
    result
}

/// Import-name deployments are resolved only beneath the runtime config directory. This is the
/// explicit trust root for source deployments; installed site-packages are never searched.
fn resolve_trusted_python_module(
    runtime_config_path: &Path,
    module: &str,
) -> Result<(PathBuf, PathBuf), String> {
    let root = runtime_config_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .canonicalize()
        .map_err(|error| format!("cannot resolve trusted Python module root: {error}"))?;
    let parts = module.split('.').collect::<Vec<_>>();
    if parts.is_empty()
        || parts.iter().any(|part| {
            part.is_empty()
                || !part.bytes().enumerate().all(|(index, byte)| {
                    byte == b'_'
                        || byte.is_ascii_alphanumeric() && (index > 0 || !byte.is_ascii_digit())
                })
        })
    {
        return Err("deployment python_module must be a dotted Python import name".into());
    }
    let mut relative = PathBuf::new();
    for part in parts {
        relative.push(part);
    }
    let module_file = root.join(relative.with_extension("py"));
    let package_file = root.join(relative).join("__init__.py");
    let module_exists = module_file.exists();
    let package_exists = package_file.exists();
    if module_exists == package_exists {
        return Err(if module_exists {
            format!("Python module {module} is ambiguous under the trusted source root")
        } else {
            format!("Python module {module} was not found under the trusted source root")
        });
    }
    let source = if module_exists {
        module_file
    } else {
        package_file
    };
    let canonical = source
        .canonicalize()
        .map_err(|error| format!("cannot resolve trusted Python module {module}: {error}"))?;
    if !canonical.starts_with(&root) || !canonical.is_file() {
        return Err("Python module resolves outside the trusted source root".into());
    }
    Ok((canonical, root))
}

fn collect_python_sources(
    root: &Path,
    current: &Path,
    depth: usize,
    visited: &mut usize,
    sources: &mut std::collections::BTreeMap<String, (PathBuf, u64, String)>,
    entry_limit: usize,
    depth_limit: usize,
) -> Result<(), String> {
    if depth > depth_limit {
        return Err("Python dependency tree exceeds maximum directory depth".into());
    }
    let mut entries = fs::read_dir(current)
        .map_err(|error| format!("cannot scan Python dependency root: {error}"))?
        .map(|entry| entry.map_err(|error| error.to_string()))
        .collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        *visited = visited.saturating_add(1);
        if *visited > entry_limit {
            return Err("Python dependency source directory contains too many entries".into());
        }
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
        if metadata.file_type().is_symlink() {
            return Err(format!(
                "Python dependency tree cannot contain symlinks: {}",
                path.display()
            ));
        }
        if metadata.is_dir() {
            let name = entry.file_name();
            if matches!(
                name.to_str(),
                Some(".git" | ".venv" | "venv" | "__pycache__" | "target" | "node_modules")
            ) {
                continue;
            }
            collect_python_sources(
                root,
                &path,
                depth + 1,
                visited,
                sources,
                entry_limit,
                depth_limit,
            )?;
            continue;
        }
        if !metadata.is_file() || path.extension().and_then(|value| value.to_str()) != Some("py") {
            continue;
        }
        if metadata.len() == 0 || metadata.len() > qx_spec::MAX_STRATEGY_DEPENDENCY_TREE_FILE_BYTES
        {
            return Err(format!(
                "Python dependency source is empty or too large: {}",
                path.display()
            ));
        }
        let relative = path
            .strip_prefix(root)
            .map_err(|error| error.to_string())?
            .to_str()
            .ok_or_else(|| "Python dependency path is not UTF-8".to_string())?
            .replace(std::path::MAIN_SEPARATOR, "/");
        let digest =
            bounded_bundle_digest(&path, qx_spec::MAX_STRATEGY_DEPENDENCY_TREE_FILE_BYTES)?;
        sources.insert(relative, (path, metadata.len(), digest));
        if sources.len() > qx_spec::MAX_STRATEGY_DEPENDENCY_TREE_FILES {
            return Err(
                "Python dependency tree contains too many source and resource files".into(),
            );
        }
    }
    Ok(())
}

fn verify_python_dependency_tree(
    root: &Path,
    manifest: &StrategyDependencyTreeV1,
) -> Result<(), String> {
    manifest.validate()?;
    for file in &manifest.files {
        let path = root.join(&file.path);
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| format!("dependency tree file is missing: {}: {error}", file.path))?;
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || metadata.len() != file.size_bytes
        {
            return Err(format!(
                "dependency tree file identity mismatch: {}",
                file.path
            ));
        }
        if !bounded_bundle_digest(&path, qx_spec::MAX_STRATEGY_DEPENDENCY_TREE_FILE_BYTES)?
            .eq_ignore_ascii_case(&file.sha256)
        {
            return Err(format!(
                "dependency tree file digest mismatch: {}",
                file.path
            ));
        }
    }
    let actual = list_dependency_tree_paths(root)?;
    let expected = manifest
        .files
        .iter()
        .map(|file| file.path.clone())
        .collect::<std::collections::BTreeSet<_>>();
    if actual != expected {
        return Err("dependency tree contains unmanifested or missing dependency files".into());
    }
    Ok(())
}

fn list_dependency_tree_paths(root: &Path) -> Result<std::collections::BTreeSet<String>, String> {
    fn visit(
        root: &Path,
        current: &Path,
        depth: usize,
        result: &mut std::collections::BTreeSet<String>,
        visited: &mut usize,
    ) -> Result<(), String> {
        if depth > 32 {
            return Err("dependency tree exceeds maximum directory depth".into());
        }
        for entry in fs::read_dir(current).map_err(|error| error.to_string())? {
            *visited += 1;
            if *visited > 100_000 {
                return Err("dependency tree contains too many filesystem entries".into());
            }
            let entry = entry.map_err(|error| error.to_string())?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
            if metadata.file_type().is_symlink() {
                return Err("dependency tree contains a symlink".into());
            }
            if metadata.is_dir() {
                if matches!(entry.file_name().to_str(), Some("__pycache__")) {
                    continue;
                }
                visit(root, &path, depth + 1, result, visited)?;
            } else if metadata.is_file() {
                result.insert(
                    path.strip_prefix(root)
                        .map_err(|error| error.to_string())?
                        .to_str()
                        .ok_or_else(|| "dependency path is not UTF-8".to_string())?
                        .replace(std::path::MAIN_SEPARATOR, "/"),
                );
            } else {
                return Err(format!(
                    "dependency tree contains an unsupported filesystem entry: {}",
                    path.display()
                ));
            }
        }
        Ok(())
    }
    let mut result = std::collections::BTreeSet::new();
    let mut visited = 0;
    visit(root, root, 0, &mut result, &mut visited)?;
    Ok(result)
}

fn bounded_bundle_digest(path: &Path, byte_limit: u64) -> Result<String, String> {
    let file = File::open(path)
        .map_err(|error| format!("cannot read deployment bundle {}: {error}", path.display()))?;
    qx_strategy::file_digest::sha256_reader_hex_bounded(file, byte_limit)
}

fn observation_directory(worker_id: &str) -> PathBuf {
    PathBuf::from("strategy-observations")
        .join(&qx_strategy::sha256_hex(worker_id.as_bytes())[..32])
}

fn observation_path(worker_id: &str, owner_id: &str) -> PathBuf {
    observation_directory(worker_id).join(format!(
        "{}.json",
        &qx_strategy::sha256_hex(owner_id.as_bytes())[..32]
    ))
}

pub struct StrategyObservationReporter {
    root: PathBuf,
    observation: StrategyWorkerObservationV3,
    finished: bool,
    interval_ms: u64,
    stale_after_ms: u64,
    last_prune_at_ms: u64,
}

impl StrategyObservationReporter {
    pub fn new(config: &RuntimeConfig, worker_id: &str, owner_id: &str) -> Result<Self, String> {
        let mut reporter = Self {
            root: PathBuf::from(&config.storage.data_dir),
            observation: StrategyWorkerObservationV3 {
                schema_version: 3,
                worker_id: worker_id.into(),
                config_sha256: config_digest(config)?,
                process_id: std::process::id(),
                owner_id: owner_id.into(),
                observed_at_ms: 0,
                phase: StrategyWorkerPhase::Initializing,
                host_capabilities: None,
                checkpoint_roundtrip_observed: false,
                host_lifecycle: None,
            },
            finished: false,
            interval_ms: (config.messaging.worker_stale_after_ms / 2).clamp(1, 1000),
            stale_after_ms: config.messaging.worker_stale_after_ms,
            last_prune_at_ms: 0,
        };
        reporter.persist(now_ms())?;
        Ok(reporter)
    }

    fn persist(&mut self, timestamp: u64) -> Result<(), String> {
        self.observation.observed_at_ms = timestamp;
        let directory = self
            .root
            .join(observation_directory(&self.observation.worker_id));
        std::fs::create_dir_all(&directory)
            .map_err(|error| format!("cannot create strategy observation directory: {error}"))?;
        let _owner_lock = FileLock::acquire(directory.join("owners.lock"))
            .map_err(|error| format!("cannot lock strategy observations: {error}"))?;
        JsonStateStore::new(&self.root)
            .save_json_at(
                observation_path(&self.observation.worker_id, &self.observation.owner_id),
                &self.observation,
            )
            .map_err(|error| format!("cannot persist strategy observation: {error:?}"))?;
        if timestamp.saturating_sub(self.last_prune_at_ms) >= OBSERVATION_PRUNE_INTERVAL_MS {
            prune_expired_observations(
                &directory,
                &self.observation.worker_id,
                timestamp,
                self.stale_after_ms,
                OBSERVATION_RETENTION_MS,
            )?;
            self.last_prune_at_ms = timestamp;
        }
        Ok(())
    }

    pub fn record(
        &mut self,
        phase: StrategyWorkerPhase,
        host: Option<StrategyHostCapabilitiesV1>,
        checkpoint_roundtrip: bool,
    ) -> Result<(), String> {
        let timestamp = now_ms();
        let changed = self.observation.phase != phase
            || self.observation.host_capabilities != host
            || self.observation.checkpoint_roundtrip_observed != checkpoint_roundtrip;
        self.observation.phase = phase;
        self.observation.host_capabilities = host;
        self.observation.checkpoint_roundtrip_observed = checkpoint_roundtrip;
        if changed || timestamp.saturating_sub(self.observation.observed_at_ms) >= self.interval_ms
        {
            self.persist(timestamp)?;
        }
        Ok(())
    }

    pub fn record_host_lifecycle(
        &mut self,
        lifecycle: qx_api::StrategyHostLifecycleEvidenceV2,
    ) -> Result<(), String> {
        if lifecycle.schema_version != 2 || lifecycle.host_id.trim().is_empty() {
            return Err("invalid strategy host lifecycle evidence identity/schema".into());
        }
        if lifecycle.checkpoint_restore_required == Some(true)
            && !lifecycle.checkpoint_input_supplied
            || lifecycle.checkpoint_restore_acknowledged.is_some()
                && lifecycle.checkpoint_restore_required != Some(true)
        {
            return Err("inconsistent strategy checkpoint restore lifecycle evidence".into());
        }
        if lifecycle
            .warmup_required_instruments
            .iter()
            .any(|value| value.trim().is_empty() || value.len() > 256)
            || lifecycle
                .warmup_completed_instruments
                .iter()
                .any(|value| value.trim().is_empty() || value.len() > 256)
        {
            return Err("strategy host lifecycle instrument IDs must be non-empty".into());
        }
        if lifecycle.host_id.len() > 128
            || lifecycle.warmup_required_instruments.len() > 4096
            || lifecycle.warmup_completed_instruments.len() > 4096
            || lifecycle
                .warmup_required_instruments
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                != lifecycle.warmup_required_instruments.len()
            || lifecycle
                .warmup_completed_instruments
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                != lifecycle.warmup_completed_instruments.len()
        {
            return Err(
                "strategy host lifecycle instrument evidence exceeds the configured limit".into(),
            );
        }
        if self.observation.host_lifecycle.as_ref() != Some(&lifecycle) {
            self.observation.host_lifecycle = Some(lifecycle);
            self.persist(now_ms())?;
        }
        Ok(())
    }

    pub fn finish(&mut self) -> Result<(), String> {
        self.observation.phase = StrategyWorkerPhase::Stopped;
        self.persist(now_ms())?;
        self.finished = true;
        Ok(())
    }
}

fn prune_expired_observations(
    directory: &Path,
    worker_id: &str,
    timestamp: u64,
    stale_after_ms: u64,
    retention_ms: u64,
) -> Result<(), String> {
    let entries = std::fs::read_dir(directory).map_err(|error| error.to_string())?;
    for (index, entry) in entries.enumerate() {
        if index >= MAX_OBSERVATION_FILES_PER_PRUNE {
            return Err(format!(
                "too many strategy observation files for {worker_id}; refusing unbounded retention scan"
            ));
        }
        let path = entry.map_err(|error| error.to_string())?.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let metadata = std::fs::metadata(&path).map_err(|error| error.to_string())?;
        if metadata.len() > 64 * 1024 {
            return Err(format!(
                "strategy observation for {worker_id} exceeds size limit"
            ));
        }
        let observation: StrategyWorkerObservationV3 =
            serde_json::from_slice(&std::fs::read(&path).map_err(|error| error.to_string())?)
                .map_err(|error| {
                    format!("invalid strategy observation for {worker_id}: {error}")
                })?;
        if !(1..=3).contains(&observation.schema_version)
            || observation.worker_id != worker_id
            || observation.owner_id.trim().is_empty()
        {
            return Err(format!(
                "strategy observation identity/schema mismatch for {worker_id}"
            ));
        }
        let expired = match observation.phase {
            StrategyWorkerPhase::Stopped | StrategyWorkerPhase::Failed => {
                observation.observed_at_ms.saturating_add(retention_ms) <= timestamp
            }
            StrategyWorkerPhase::Initializing
            | StrategyWorkerPhase::Running
            | StrategyWorkerPhase::Paused => {
                observation
                    .observed_at_ms
                    .saturating_add(stale_after_ms)
                    .saturating_add(retention_ms)
                    <= timestamp
            }
        };
        if expired {
            std::fs::remove_file(&path).map_err(|error| {
                format!(
                    "cannot prune expired strategy observation {}: {error}",
                    path.display()
                )
            })?;
        }
    }
    Ok(())
}

impl Drop for StrategyObservationReporter {
    fn drop(&mut self) {
        if !self.finished {
            self.observation.phase = StrategyWorkerPhase::Failed;
            let _ = self.persist(now_ms());
        }
    }
}

pub fn read_strategy_deployments(
    config: &RuntimeConfig,
    timestamp: u64,
) -> Result<Vec<StrategyDeploymentStatusV1>, String> {
    let root = Path::new(&config.storage.data_dir);
    let deployment_store =
        qx_runtime::strategy_deployment_store::StrategyDeploymentStore::configured(root, config)?;
    read_strategy_deployments_with_store(config, timestamp, &deployment_store)
}

pub fn read_strategy_deployments_with_store(
    config: &RuntimeConfig,
    timestamp: u64,
    deployment_store: &qx_runtime::strategy_deployment_store::StrategyDeploymentStore,
) -> Result<Vec<StrategyDeploymentStatusV1>, String> {
    let digest = config_digest(config)?;
    let root = Path::new(&config.storage.data_dir);
    let reconciliation_reports = crate::api_read_model::load_reconcile_reports(root)?;
    let mut statuses = Vec::new();
    for worker in config
        .workers
        .iter()
        .filter(|worker| worker.role == WorkerRole::Strategy && worker.enabled)
    {
        let strategy = config.strategy_for_worker(&worker.id)?;
        let latest_operation = deployment_store.latest_operation(&worker.id)?;
        let operation_history = deployment_store.recent_operations(
            &worker.id,
            qx_runtime::strategy_deployment_store::MAX_RECENT_OPERATIONS_PER_WORKER,
        )?;
        let desired_operation = match latest_operation.as_ref() {
            Some(operation)
                if matches!(
                    operation.state,
                    qx_spec::StrategyDeploymentOperationStateV1::Failed
                        | qx_spec::StrategyDeploymentOperationStateV1::Cancelled
                ) =>
            {
                deployment_store.latest_successful_operation(&worker.id)?
            }
            _ => latest_operation.clone(),
        };
        let desired_revision = desired_operation
            .as_ref()
            .map(|operation| operation.target_revision);
        let desired_running = match desired_operation.as_ref().map(|operation| operation.action) {
            Some(qx_spec::StrategyDeploymentActionV1::Stop) => Some(false),
            Some(
                qx_spec::StrategyDeploymentActionV1::Pause
                | qx_spec::StrategyDeploymentActionV1::Resume,
            ) => Some(true),
            Some(
                qx_spec::StrategyDeploymentActionV1::Deploy
                | qx_spec::StrategyDeploymentActionV1::Rollback,
            ) => {
                let revision = desired_operation
                    .as_ref()
                    .map(|operation| operation.target_revision)
                    .unwrap_or_default();
                deployment_store
                    .revision(&worker.id, revision)?
                    .map(|value| value.desired_running)
            }
            None => Some(worker.enabled),
        };
        let directory = root.join(observation_directory(&worker.id));
        let mut observations: Vec<StrategyWorkerObservationV3> = Vec::new();
        match std::fs::read_dir(&directory) {
            Ok(entries) => {
                for entry in entries {
                    let path = entry.map_err(|error| error.to_string())?.path();
                    if path.extension().and_then(|value| value.to_str()) != Some("json") {
                        continue;
                    }
                    if observations.len() >= 1024 {
                        return Err(format!(
                            "too many strategy owner observations for {}",
                            worker.id
                        ));
                    }
                    if std::fs::metadata(&path)
                        .map_err(|error| error.to_string())?
                        .len()
                        > 64 * 1024
                    {
                        return Err(format!(
                            "strategy observation for {} exceeds size limit",
                            worker.id
                        ));
                    }
                    let value: StrategyWorkerObservationV3 = serde_json::from_slice(
                        &std::fs::read(path).map_err(|error| error.to_string())?,
                    )
                    .map_err(|error| {
                        format!("invalid strategy observation for {}: {error}", worker.id)
                    })?;
                    if !(1..=3).contains(&value.schema_version)
                        || value.worker_id != worker.id
                        || value.owner_id.is_empty()
                    {
                        return Err(format!(
                            "strategy observation identity/schema mismatch for {}",
                            worker.id
                        ));
                    }
                    observations.push(value);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
        let is_active = |observed: &StrategyWorkerObservationV3| {
            observed.observed_at_ms <= timestamp
                && timestamp - observed.observed_at_ms <= config.messaging.worker_stale_after_ms
                && matches!(
                    observed.phase,
                    StrategyWorkerPhase::Initializing
                        | StrategyWorkerPhase::Running
                        | StrategyWorkerPhase::Paused
                )
        };
        let mut active_owner_ids: Vec<_> = observations
            .iter()
            .filter(|value| is_active(value))
            .map(|value| value.owner_id.clone())
            .collect();
        active_owner_ids.sort();
        active_owner_ids.dedup();
        // A late shutdown report from an old process must not replace a live owner's observation.
        observations.sort_by_key(|value| {
            (
                is_active(value),
                value.config_sha256 == digest,
                value.observed_at_ms,
                value.owner_id.clone(),
            )
        });
        let observation = observations.pop();
        let mut reasons = vec!["live_readiness_not_evaluated".to_string()];
        let state = if active_owner_ids.len() > 1 {
            "ownership_conflict"
        } else if !worker.enabled {
            "disabled"
        } else if let Some(observed) = &observation {
            if !(1..=3).contains(&observed.schema_version)
                || observed.worker_id != worker.id
                || observed.owner_id.is_empty()
            {
                return Err(format!(
                    "strategy observation identity/schema mismatch for {}",
                    worker.id
                ));
            }
            if let Some(host) = &observed.host_capabilities {
                host.validate_required(&[])?;
            }
            if observed.config_sha256 != digest {
                "configuration_changed"
            } else if observed.observed_at_ms > timestamp
                || timestamp.saturating_sub(observed.observed_at_ms)
                    > config.messaging.worker_stale_after_ms
            {
                "stale"
            } else {
                match observed.phase {
                    StrategyWorkerPhase::Initializing => "initializing",
                    StrategyWorkerPhase::Running
                        if observed
                            .host_lifecycle
                            .as_ref()
                            .is_some_and(|value| !value.initialized) =>
                    {
                        "strategy_initializing"
                    }
                    StrategyWorkerPhase::Running => "running",
                    StrategyWorkerPhase::Paused => "paused",
                    StrategyWorkerPhase::Stopped => "stopped",
                    StrategyWorkerPhase::Failed => "failed",
                }
            }
        } else {
            "unobserved"
        };
        if state != "running" {
            reasons.push(format!("worker_{state}"));
        }
        if observation
            .as_ref()
            .is_none_or(|value| value.host_lifecycle.is_none())
        {
            reasons.push("host_lifecycle_not_observed".into());
        } else if observation
            .as_ref()
            .and_then(|value| value.host_lifecycle.as_ref())
            .is_some_and(|value| !value.constructed)
        {
            reasons.push("host_construction_not_observed".into());
        } else if observation
            .as_ref()
            .and_then(|value| value.host_lifecycle.as_ref())
            .is_some_and(|value| !value.initialized)
        {
            reasons.push("strategy_initialization_not_observed".into());
        }
        let legacy_external_restore_evidence_missing =
            config.strategy.external_executable.is_some()
                && observation.as_ref().is_some_and(|value| {
                    value.schema_version < 3
                        && value
                            .host_lifecycle
                            .as_ref()
                            .is_some_and(|lifecycle| lifecycle.checkpoint_input_supplied)
                });
        if legacy_external_restore_evidence_missing {
            reasons.push("host_checkpoint_restore_ack_not_observed".into());
        } else if observation
            .as_ref()
            .and_then(|value| value.host_lifecycle.as_ref())
            .is_some_and(|value| {
                value.checkpoint_restore_required == Some(true)
                    && value.checkpoint_restore_acknowledged != Some(true)
            })
        {
            reasons.push("host_checkpoint_restore_not_acknowledged".into());
        }
        if observation
            .as_ref()
            .and_then(|value| value.host_lifecycle.as_ref())
            .is_some_and(|value| {
                value.warmup_applicable
                    && !value.warmup_required_instruments.is_empty()
                    && value
                        .warmup_required_instruments
                        .iter()
                        .any(|instrument| !value.warmup_completed_instruments.contains(instrument))
            })
        {
            reasons.push("strategy_warmup_incomplete".into());
        }
        if observation
            .as_ref()
            .is_none_or(|value| value.host_capabilities.is_none())
        {
            reasons.push("host_capabilities_not_observed".into());
        }
        if observation
            .as_ref()
            .is_none_or(|value| !value.checkpoint_roundtrip_observed)
        {
            reasons.push("checkpoint_roundtrip_not_observed".into());
        }
        let reconciliation = strategy_reconciliation_projection(
            &reconciliation_reports,
            strategy.account_id.as_deref(),
            strategy.venue_id.as_deref(),
            timestamp,
        );
        statuses.push(StrategyDeploymentStatusV1 {
            schema_version: 1,
            worker_id: worker.id.clone(),
            strategy_id: strategy.id,
            strategy_version: strategy.version,
            account_id: strategy.account_id,
            venue_id: strategy.venue_id,
            enabled: worker.enabled,
            state: state.into(),
            observed_at_ms: observation.as_ref().map(|value| value.observed_at_ms),
            observation,
            active_owner_ids,
            trading_enabled: None,
            blocking_reasons: reasons,
            desired_revision,
            desired_running,
            latest_operation,
            reconciliation: Some(reconciliation),
            operation_history,
        });
    }
    Ok(statuses)
}

fn strategy_reconciliation_projection(
    reports: &[qx_api::ReconcileReportSnapshot],
    account_id: Option<&str>,
    venue_id: Option<&str>,
    timestamp: u64,
) -> qx_api::StrategyDeploymentReconciliationV1 {
    use qx_api::StrategyDeploymentReconciliationV1;

    let Some((account_id, venue_id)) = account_id
        .filter(|value| !value.trim().is_empty())
        .zip(venue_id.filter(|value| !value.trim().is_empty()))
    else {
        return StrategyDeploymentReconciliationV1 {
            schema_version: 1,
            state: "not_configured".into(),
            matching_report_count: 0,
            fresh_report_count: 0,
            observed_at_ms: None,
            freshest_age_ms: None,
            blocking_reasons: vec!["live_account_venue_identity_not_configured".into()],
        };
    };
    let matching: Vec<_> = reports
        .iter()
        .filter(|report| {
            report.account_id == account_id && report.venue_id.eq_ignore_ascii_case(venue_id)
        })
        .collect();
    let observed_at_ms = matching.iter().map(|report| report.observed_ts).max();
    let freshest_age_ms = matching
        .iter()
        .filter_map(|report| timestamp.checked_sub(report.observed_ts))
        .min();
    let fresh_report_count = matching
        .iter()
        .filter(|report| {
            timestamp
                .checked_sub(report.observed_ts)
                .is_some_and(|age| age <= crate::reconcile_report::LIVE_RECONCILE_MAX_AGE_MS)
        })
        .count();
    let state = match crate::reconcile_report::require_fresh_clean_reconciliation(
        reports, account_id, venue_id, timestamp,
    ) {
        Ok(()) => "clean",
        Err(error) if error.contains("RECONCILE_MISSING") => "missing",
        Err(error) if error.contains("RECONCILE_FROM_FUTURE") => "future",
        Err(error) if error.contains("RECONCILE_INVALID") => "invalid",
        Err(error) if error.contains("RECONCILE_DISCREPANCY") => "discrepant",
        Err(error) if error.contains("RECONCILE_STALE") => "stale",
        Err(_) => "invalid",
    };
    let blocking_reasons = if state == "clean" {
        Vec::new()
    } else {
        vec![format!("live_reconciliation_{state}")]
    };
    StrategyDeploymentReconciliationV1 {
        schema_version: 1,
        state: state.into(),
        matching_report_count: matching.len(),
        fresh_report_count,
        observed_at_ms,
        freshest_age_ms,
        blocking_reasons,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (RuntimeConfig, String, PathBuf) {
        static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let mut config = RuntimeConfig::from_json(include_str!(
            "../../../deploy/qianxing.runtime.paper-strategy.example.json"
        ))
        .unwrap();
        let directory = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!(
                "strategy-observation-{}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
        std::fs::create_dir_all(&directory).unwrap();
        config.storage.data_dir = directory.to_string_lossy().into_owned();
        let worker_id = config
            .workers
            .iter()
            .find(|worker| worker.role == WorkerRole::Strategy && worker.enabled)
            .unwrap()
            .id
            .clone();
        (config, worker_id, directory)
    }
    #[test]
    fn observed_state_is_fresh_config_bound_and_never_a_live_authorization() {
        let (config, worker_id, directory) = fixture();
        assert_eq!(
            read_strategy_deployments(&config, now_ms()).unwrap()[0].state,
            "unobserved"
        );
        let mut reporter =
            StrategyObservationReporter::new(&config, &worker_id, "owner-1").unwrap();
        let host = StrategyHostCapabilitiesV1::runtime_python_class("bar", true).unwrap();
        reporter
            .record(StrategyWorkerPhase::Running, Some(host.clone()), true)
            .unwrap();
        let statuses = read_strategy_deployments(&config, now_ms()).unwrap();
        assert_eq!(statuses[0].state, "running");
        assert!(statuses[0].trading_enabled.is_none());
        assert_eq!(
            statuses[0].blocking_reasons,
            vec![
                "live_readiness_not_evaluated",
                "host_lifecycle_not_observed"
            ]
        );
        let stale_at =
            statuses[0].observed_at_ms.unwrap() + config.messaging.worker_stale_after_ms + 1;
        assert_eq!(
            read_strategy_deployments(&config, stale_at).unwrap()[0].state,
            "stale"
        );
        assert_eq!(
            read_strategy_deployments(&config, 0).unwrap()[0].state,
            "stale"
        );
        let mut changed = config.clone();
        changed.strategy.version.push_str("-changed");
        assert_eq!(
            read_strategy_deployments(&changed, now_ms()).unwrap()[0].state,
            "configuration_changed"
        );
        reporter
            .record(StrategyWorkerPhase::Paused, Some(host), true)
            .unwrap();
        assert_eq!(
            read_strategy_deployments(&config, now_ms()).unwrap()[0].state,
            "paused"
        );
        reporter.finish().unwrap();
        assert_eq!(
            read_strategy_deployments(&config, now_ms()).unwrap()[0].state,
            "stopped"
        );
        drop(reporter);
        assert_eq!(
            read_strategy_deployments(&config, now_ms()).unwrap()[0].state,
            "stopped"
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn aborted_reporter_and_corrupt_identity_are_visible() {
        let (config, worker_id, directory) = fixture();
        {
            let _reporter =
                StrategyObservationReporter::new(&config, &worker_id, "owner-2").unwrap();
        }
        assert_eq!(
            read_strategy_deployments(&config, now_ms()).unwrap()[0].state,
            "failed"
        );
        let store = JsonStateStore::new(&directory);
        let mut observation: StrategyWorkerObservationV3 = store
            .load_json_at(observation_path(&worker_id, "owner-2"))
            .unwrap();
        observation.worker_id = "wrong-worker".into();
        store
            .save_json_at(observation_path(&worker_id, "owner-2"), &observation)
            .unwrap();
        assert!(read_strategy_deployments(&config, now_ms()).is_err());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn concurrent_owners_are_visible_and_old_shutdown_cannot_hide_the_live_owner() {
        let (config, worker_id, directory) = fixture();
        let mut first = StrategyObservationReporter::new(&config, &worker_id, "owner-old").unwrap();
        first
            .record(StrategyWorkerPhase::Running, None, false)
            .unwrap();
        let mut second =
            StrategyObservationReporter::new(&config, &worker_id, "owner-new").unwrap();
        second
            .record(StrategyWorkerPhase::Running, None, false)
            .unwrap();
        let statuses = read_strategy_deployments(&config, now_ms()).unwrap();
        assert_eq!(statuses[0].state, "ownership_conflict");
        assert_eq!(statuses[0].active_owner_ids.len(), 2);
        assert!(statuses[0].trading_enabled.is_none());
        first.finish().unwrap();
        let statuses = read_strategy_deployments(&config, now_ms()).unwrap();
        assert_eq!(statuses[0].state, "running");
        assert_eq!(
            statuses[0].observation.as_ref().unwrap().owner_id,
            "owner-new"
        );
        assert_eq!(statuses[0].active_owner_ids, vec!["owner-new"]);
        second.finish().unwrap();
        drop((first, second));
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn api_reads_the_durable_observation_and_filters_exact_worker() {
        let (config, worker_id, directory) = fixture();
        let mut reporter =
            StrategyObservationReporter::new(&config, &worker_id, "owner-3").unwrap();
        reporter
            .record(StrategyWorkerPhase::Paused, None, false)
            .unwrap();
        let service = qx_api::ApiService::new(qx_api::ApiState::default())
            .with_strategy_deployments_provider(move || {
                read_strategy_deployments(&config, now_ms())
            });
        let response = service.handle(
            "GET",
            &format!("/strategies/deployments?worker_id={worker_id}"),
            "",
            now_ms(),
        );
        assert_eq!(response.status, 200);
        let values: Vec<StrategyDeploymentStatusV1> = serde_json::from_str(&response.body).unwrap();
        assert_eq!(values[0].state, "paused");
        assert!(values[0].trading_enabled.is_none());
        reporter.finish().unwrap();
        drop(service);
        drop(reporter);
        std::fs::remove_dir_all(directory).unwrap();
    }
}
