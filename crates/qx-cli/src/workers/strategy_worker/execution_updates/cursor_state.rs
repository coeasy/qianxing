//! Versioned durable execution cursor state and checkpoint persistence.

use super::*;
use qx_core::FileLock;

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(in crate::workers::strategy_worker) struct CursorState {
    pub(in crate::workers::strategy_worker) schema_version: u32,
    /// The next EventLog sequence to inspect for each account/venue log.
    pub(in crate::workers::strategy_worker) next_seq_by_log: BTreeMap<String, u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(in crate::workers::strategy_worker) strategy_checkpoint: Option<StrategyCheckpointV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(in crate::workers::strategy_worker) strategy_binding_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(in crate::workers::strategy_worker) checkpoint_sha256: Option<String>,
    /// If the process stops after checkpoint commit but before scheduler completion,
    /// recovery finalizes this run without invoking the strategy a second time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(in crate::workers::strategy_worker) committed_run_id: Option<u64>,
    /// Lease fence that committed `committed_run_id`; scoped to that run because queue fences
    /// are monotone per run rather than globally across the scheduler.
    #[serde(default)]
    pub(in crate::workers::strategy_worker) committed_fencing_token: u64,
    /// Monotone commit order for checkpoint writes. `run_id` is a stable hash and has no order.
    #[serde(default)]
    pub(in crate::workers::strategy_worker) commit_generation: u64,
    /// Last cursor-format migration, retained so operators can distinguish a migrated cursor
    /// from a cursor that was first created in the current format.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) migration: Option<CursorSchemaMigrationV1>,
    /// Deployment owner currently bound to this cursor/checkpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(in crate::workers::strategy_worker) deployment_revision: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(in crate::workers::strategy_worker) deployment_migration:
        Option<CursorDeploymentMigrationV1>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CursorSchemaMigrationV1 {
    pub(super) source_schema_version: u32,
    pub(super) target_schema_version: u32,
    pub(super) result: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(in crate::workers::strategy_worker) struct CursorDeploymentMigrationV1 {
    pub(in crate::workers::strategy_worker) source_revision: Option<u64>,
    pub(in crate::workers::strategy_worker) target_revision: u64,
    pub(in crate::workers::strategy_worker) result: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(in crate::workers::strategy_worker) converter_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(in crate::workers::strategy_worker) source_checkpoint_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(in crate::workers::strategy_worker) target_checkpoint_sha256: Option<String>,
}

pub(in crate::workers::strategy_worker) fn cursor_path(root: &Path, worker_id: &str) -> PathBuf {
    root.join("strategy-live-state")
        .join(format!("{worker_id}.execution-cursor.json"))
}

pub(in crate::workers::strategy_worker) fn configured_cursor_path(
    root: &Path,
    worker_id: &str,
    config: &qx_runtime::RuntimeConfig,
) -> Result<PathBuf, String> {
    let legacy = cursor_path(root, worker_id);
    if config.storage.backend != qx_runtime::StorageBackend::Postgres {
        return Ok(legacy);
    }
    let env_key = config
        .storage
        .postgres_dsn_env
        .as_deref()
        .ok_or_else(|| "PostgreSQL execution cursor requires postgres_dsn_env".to_string())?;
    let state_key = format!("strategy-execution-cursor/{worker_id}");
    let encoded =
        serde_json::to_string(&(env_key, state_key, legacy.to_string_lossy().to_string()))
            .map_err(|error| format!("encode PostgreSQL cursor reference: {error}"))?;
    Ok(PathBuf::from(format!("qxpgcursor://{encoded}")))
}

const POSTGRES_CURSOR_PATH_PREFIX: &str = "qxpgcursor://";

fn postgres_cursor_reference(path: &Path) -> Option<(String, String, PathBuf)> {
    let text = path.to_str()?.strip_prefix(POSTGRES_CURSOR_PATH_PREFIX)?;
    let (env_key, state_key, legacy_path): (String, String, String) =
        serde_json::from_str(text).ok()?;
    Some((env_key, state_key, PathBuf::from(legacy_path)))
}

#[cfg(feature = "postgres")]
fn postgres_cursor_store(env_key: &str) -> Result<qx_storage::PostgresJsonStateStore, String> {
    let dsn = std::env::var(env_key)
        .map_err(|_| format!("PostgreSQL DSN environment variable {env_key} is not set"))?;
    qx_storage::PostgresJsonStateStore::connect(&dsn)
        .map_err(|error| format!("connect PostgreSQL strategy cursor store: {error:?}"))
}

pub(in crate::workers::strategy_worker) fn load_cursor(path: &Path) -> Result<CursorState, String> {
    if let Some((env_key, state_key, legacy_path)) = postgres_cursor_reference(path) {
        #[cfg(not(feature = "postgres"))]
        {
            let _ = (env_key, state_key, legacy_path);
            return Err("PostgreSQL execution cursor requires the postgres feature".into());
        }
        #[cfg(feature = "postgres")]
        {
            let store = postgres_cursor_store(&env_key)?;
            if let Some(state) = store
                .load_json_at::<CursorState>(&state_key)
                .map_err(|error| format!("load PostgreSQL strategy cursor: {error:?}"))?
            {
                return validate_loaded_cursor(
                    path,
                    serde_json::to_string(&state).map_err(|error| error.to_string())?,
                );
            }
            let state = if legacy_path.exists() || backup_path(&legacy_path).exists() {
                load_cursor(&legacy_path)?
            } else {
                empty_cursor_state()
            };
            if legacy_path.exists() || backup_path(&legacy_path).exists() {
                save_cursor(path, &state)?;
            }
            return Ok(state);
        }
    }
    load_cursor_file(path)
}

fn empty_cursor_state() -> CursorState {
    CursorState {
        schema_version: CURSOR_SCHEMA_VERSION,
        next_seq_by_log: BTreeMap::new(),
        strategy_checkpoint: None,
        strategy_binding_sha256: None,
        checkpoint_sha256: None,
        committed_run_id: None,
        committed_fencing_token: 0,
        commit_generation: 0,
        migration: None,
        deployment_revision: None,
        deployment_migration: None,
    }
}

fn load_cursor_file(path: &Path) -> Result<CursorState, String> {
    let backup = backup_path(path);
    let payload = match std::fs::read_to_string(path) {
        Ok(payload) => payload,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            match std::fs::read_to_string(&backup) {
                Ok(payload) => payload,
                Err(backup_error) if backup_error.kind() == std::io::ErrorKind::NotFound => {
                    return Ok(empty_cursor_state());
                }
                Err(backup_error) => {
                    return Err(format!(
                        "读取策略执行事实游标和恢复副本失败 {} / {}: {backup_error}",
                        path.display(),
                        backup.display()
                    ));
                }
            }
        }
        Err(error) => {
            return Err(format!(
                "读取策略执行事实游标失败 {}: {error}",
                path.display()
            ));
        }
    };
    if payload.len() > MAX_CURSOR_STATE_BYTES {
        return Err(format!(
            "策略执行事实游标超过 {} 字节上限: {}",
            MAX_CURSOR_STATE_BYTES,
            path.display()
        ));
    }
    validate_loaded_cursor(path, payload)
}

fn validate_loaded_cursor(path: &Path, payload: String) -> Result<CursorState, String> {
    let mut state: CursorState = serde_json::from_str(&payload)
        .map_err(|error| format!("策略执行事实游标 JSON 无效 {}: {error}", path.display()))?;
    let source_schema = state.schema_version;
    let legacy_schema = source_schema == LEGACY_CURSOR_SCHEMA_VERSION;
    let previous_schema = source_schema == PREVIOUS_CURSOR_SCHEMA_VERSION;
    if (!legacy_schema && !previous_schema && source_schema != CURSOR_SCHEMA_VERSION)
        || state
            .next_seq_by_log
            .keys()
            .any(|name| name.trim().is_empty())
    {
        return Err(format!(
            "策略执行事实游标版本或 EventLog 名称非法: {}",
            path.display()
        ));
    }
    if let Some(checkpoint) = state.strategy_checkpoint.as_ref() {
        validate_checkpoint_state(checkpoint)?;
        let digest = checkpoint_sha256(checkpoint)?;
        match (legacy_schema, state.checkpoint_sha256.as_deref()) {
            (true, None) => state.checkpoint_sha256 = Some(digest),
            (false, Some(saved)) if saved.eq_ignore_ascii_case(&digest) => {}
            _ => {
                return Err(format!(
                    "策略 checkpoint checksum 缺失或不匹配: {}",
                    path.display()
                ));
            }
        }
    } else if state.checkpoint_sha256.is_some() {
        return Err(format!(
            "策略执行事实游标没有 checkpoint 但含 checksum: {}",
            path.display()
        ));
    }
    if state.strategy_checkpoint.is_some() != state.strategy_binding_sha256.is_some()
        || state
            .strategy_binding_sha256
            .as_deref()
            .is_some_and(|hash| {
                hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
    {
        return Err(format!(
            "策略 checkpoint 与绑定指纹状态不一致: {}",
            path.display()
        ));
    }
    if legacy_schema || previous_schema {
        state.migration = Some(CursorSchemaMigrationV1 {
            source_schema_version: source_schema,
            target_schema_version: CURSOR_SCHEMA_VERSION,
            result: if legacy_schema {
                CURSOR_V1_TO_V3_MIGRATION_RESULT
            } else {
                CURSOR_V2_TO_V3_MIGRATION_RESULT
            }
            .into(),
        });
        if state.committed_run_id.is_some() {
            state.commit_generation = 1;
        }
        state.schema_version = CURSOR_SCHEMA_VERSION;
        save_cursor(path, &state).map_err(|error| {
            format!("将旧版策略执行游标迁移到 schema v{CURSOR_SCHEMA_VERSION} 失败: {error}")
        })?;
    } else if state.migration.as_ref().is_some_and(|migration| {
        migration.source_schema_version != LEGACY_CURSOR_SCHEMA_VERSION
            || migration.target_schema_version != CURSOR_SCHEMA_VERSION
            || migration.result != CURSOR_V1_TO_V3_MIGRATION_RESULT
                && migration.result != CURSOR_V2_TO_V3_MIGRATION_RESULT
    }) {
        return Err(format!("策略执行事实游标迁移记录非法: {}", path.display()));
    }
    if state.deployment_revision == Some(0)
        || state
            .deployment_migration
            .as_ref()
            .is_some_and(|migration| {
                let valid_sha256 = |value: &str| {
                    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
                };
                let has_conversion_receipt = migration.converter_sha256.is_some()
                    && migration.source_checkpoint_sha256.is_some()
                    && migration.target_checkpoint_sha256.is_some();
                let valid_conversion_receipt =
                    if migration.result == CURSOR_DEPLOYMENT_STATE_CONVERTED {
                        has_conversion_receipt
                            && migration
                                .converter_sha256
                                .as_deref()
                                .is_some_and(valid_sha256)
                            && migration
                                .source_checkpoint_sha256
                                .as_deref()
                                .is_some_and(valid_sha256)
                            && migration
                                .target_checkpoint_sha256
                                .as_deref()
                                .is_some_and(valid_sha256)
                    } else {
                        !migration.converter_sha256.is_some()
                            && !migration.source_checkpoint_sha256.is_some()
                            && !migration.target_checkpoint_sha256.is_some()
                    };
                migration.target_revision == 0
                    || migration
                        .source_revision
                        .is_some_and(|source| source == 0 || source >= migration.target_revision)
                    || state.deployment_revision != Some(migration.target_revision)
                    || !matches!(
                        migration.result.as_str(),
                        CURSOR_DEPLOYMENT_INITIAL_BINDING
                            | CURSOR_DEPLOYMENT_CHECKPOINT_BINDING
                            | CURSOR_DEPLOYMENT_STATE_CONVERTED
                    )
                    || !valid_conversion_receipt
            })
    {
        return Err(format!(
            "策略执行事实游标部署修订迁移记录非法: {}",
            path.display()
        ));
    }
    Ok(state)
}

pub(in crate::workers::strategy_worker) fn save_cursor(
    path: &Path,
    state: &CursorState,
) -> Result<(), String> {
    if let Some((env_key, state_key, _)) = postgres_cursor_reference(path) {
        #[cfg(not(feature = "postgres"))]
        {
            let _ = (env_key, state_key);
            return Err("PostgreSQL execution cursor requires the postgres feature".into());
        }
        #[cfg(feature = "postgres")]
        {
            validate_cursor_candidate(state)?;
            let store = postgres_cursor_store(&env_key)?;
            let (_, outcome) = store
                .transact_json_at(
                    &state_key,
                    empty_cursor_state(),
                    |current: &mut CursorState| {
                        validate_cursor_write_against_current(current, state)?;
                        *current = state.clone();
                        Ok::<(), String>(())
                    },
                )
                .map_err(|error| format!("transact PostgreSQL strategy cursor: {error:?}"))?;
            return outcome;
        }
    }
    validate_cursor_candidate(state)?;
    save_cursor_file(path, state)
}

fn validate_cursor_candidate(state: &CursorState) -> Result<(), String> {
    if state.schema_version != CURSOR_SCHEMA_VERSION
        || state
            .next_seq_by_log
            .keys()
            .any(|name| name.trim().is_empty())
    {
        return Err("拒绝保存版本或 EventLog 名称非法的策略执行事实游标".into());
    }
    if state.strategy_checkpoint.is_some() != state.strategy_binding_sha256.is_some()
        || state
            .strategy_binding_sha256
            .as_deref()
            .is_some_and(|hash| {
                hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
    {
        return Err("拒绝保存与策略绑定指纹不一致的 checkpoint".into());
    }
    if state.migration.as_ref().is_some_and(|migration| {
        migration.target_schema_version != CURSOR_SCHEMA_VERSION
            || !matches!(
                (migration.source_schema_version, migration.result.as_str()),
                (
                    LEGACY_CURSOR_SCHEMA_VERSION,
                    CURSOR_V1_TO_V3_MIGRATION_RESULT
                ) | (
                    PREVIOUS_CURSOR_SCHEMA_VERSION,
                    CURSOR_V2_TO_V3_MIGRATION_RESULT
                )
            )
    }) {
        return Err("拒绝保存迁移来源、目标或结果非法的策略游标".into());
    }
    if state.deployment_revision == Some(0)
        || state
            .deployment_migration
            .as_ref()
            .is_some_and(|migration| {
                let valid_sha256 = |value: &str| {
                    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
                };
                let has_conversion_receipt = migration.converter_sha256.is_some()
                    && migration.source_checkpoint_sha256.is_some()
                    && migration.target_checkpoint_sha256.is_some();
                let valid_conversion_receipt =
                    if migration.result == CURSOR_DEPLOYMENT_STATE_CONVERTED {
                        has_conversion_receipt
                            && migration
                                .converter_sha256
                                .as_deref()
                                .is_some_and(valid_sha256)
                            && migration
                                .source_checkpoint_sha256
                                .as_deref()
                                .is_some_and(valid_sha256)
                            && migration
                                .target_checkpoint_sha256
                                .as_deref()
                                .is_some_and(valid_sha256)
                    } else {
                        !migration.converter_sha256.is_some()
                            && !migration.source_checkpoint_sha256.is_some()
                            && !migration.target_checkpoint_sha256.is_some()
                    };
                migration.target_revision == 0
                    || migration
                        .source_revision
                        .is_some_and(|source| source == 0 || source >= migration.target_revision)
                    || state.deployment_revision != Some(migration.target_revision)
                    || !matches!(
                        migration.result.as_str(),
                        CURSOR_DEPLOYMENT_INITIAL_BINDING
                            | CURSOR_DEPLOYMENT_CHECKPOINT_BINDING
                            | CURSOR_DEPLOYMENT_STATE_CONVERTED
                    )
                    || !valid_conversion_receipt
            })
    {
        return Err("拒绝保存部署修订迁移身份非法的策略游标".into());
    }
    if let Some(checkpoint) = state.strategy_checkpoint.as_ref() {
        validate_checkpoint_state(checkpoint)?;
        let expected = checkpoint_sha256(checkpoint)?;
        if state
            .checkpoint_sha256
            .as_deref()
            .is_none_or(|actual| !actual.eq_ignore_ascii_case(&expected))
        {
            return Err("拒绝保存 checksum 缺失或不一致的策略 checkpoint".into());
        }
    } else if state.checkpoint_sha256.is_some() {
        return Err("拒绝保存没有 checkpoint 但含 checksum 的游标".into());
    }
    let payload =
        serde_json::to_vec(state).map_err(|error| format!("编码策略执行事实游标失败: {error}"))?;
    if payload.len() > MAX_CURSOR_STATE_BYTES {
        return Err(format!(
            "拒绝保存超过 {} 字节的策略执行事实游标",
            MAX_CURSOR_STATE_BYTES
        ));
    }
    Ok(())
}

fn save_cursor_file(path: &Path, state: &CursorState) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("策略执行事实游标路径没有父目录: {}", path.display()))?;
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("创建策略执行事实游标目录失败: {error}"))?;
    let _write_lock = FileLock::acquire(path.with_extension("cursor.lock"))
        .map_err(|error| format!("获取策略执行事实游标写锁失败: {error}"))?;
    validate_cursor_write_against_durable_state(path, state)?;
    let temporary = path.with_extension(format!("cursor.tmp.{}", std::process::id()));
    let _temporary_guard = TemporaryCursorFile(temporary.clone());
    let backup = backup_path(path);
    let payload =
        serde_json::to_vec(state).map_err(|error| format!("编码策略执行事实游标失败: {error}"))?;
    use std::io::Write;
    let mut file = std::fs::File::create(&temporary)
        .map_err(|error| format!("创建策略执行事实游标临时文件失败: {error}"))?;
    file.write_all(&payload)
        .and_then(|_| file.sync_all())
        .map_err(|error| format!("同步策略执行事实游标临时文件失败: {error}"))?;
    drop(file);
    if path.exists() && backup.exists() {
        std::fs::remove_file(&backup)
            .map_err(|error| format!("清理策略执行事实游标旧恢复副本失败: {error}"))?;
    }
    if path.exists() {
        std::fs::rename(path, &backup)
            .map_err(|error| format!("备份旧策略执行事实游标失败: {error}"))?;
    }
    if let Err(error) = std::fs::rename(&temporary, path) {
        if backup.exists() {
            std::fs::rename(&backup, path).map_err(|restore| {
                format!("提交策略执行事实游标失败: {error}; 恢复旧游标失败: {restore}")
            })?;
        }
        return Err(format!("提交策略执行事实游标失败: {error}"));
    }
    // The primary cursor is already committed at this point. A stale recovery
    // copy is safe because load_cursor prefers the primary file; reporting a
    // cleanup failure as a failed commit would leave the caller's in-memory
    // state behind the durable state on disk.
    if backup.exists() {
        let _ = std::fs::remove_file(&backup);
    }
    #[cfg(unix)]
    std::fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("同步策略执行事实游标目录失败: {error}"))?;
    Ok(())
}

fn validate_cursor_write_against_durable_state(
    path: &Path,
    candidate: &CursorState,
) -> Result<(), String> {
    let backup = backup_path(path);
    let current_path = if path.exists() {
        Some(path)
    } else if backup.exists() {
        Some(backup.as_path())
    } else {
        None
    };
    let Some(current_path) = current_path else {
        return Ok(());
    };
    let payload = std::fs::read_to_string(current_path)
        .map_err(|error| format!("读取已提交游标用于 fencing 比对失败: {error}"))?;
    let current: CursorState = serde_json::from_str(&payload)
        .map_err(|error| format!("已提交游标无法参与 fencing 比对: {error}"))?;
    validate_cursor_write_against_current(&current, candidate)
}

fn validate_cursor_write_against_current(
    current: &CursorState,
    candidate: &CursorState,
) -> Result<(), String> {
    if !matches!(
        current.schema_version,
        LEGACY_CURSOR_SCHEMA_VERSION | PREVIOUS_CURSOR_SCHEMA_VERSION | CURSOR_SCHEMA_VERSION
    ) {
        return Err("已提交游标版本未知，拒绝覆盖".into());
    }
    for (log_name, current_seq) in &current.next_seq_by_log {
        if candidate
            .next_seq_by_log
            .get(log_name)
            .copied()
            .unwrap_or_default()
            < *current_seq
        {
            return Err(format!("拒绝旧游标覆盖已提交 EventLog 进度: {log_name}"));
        }
    }
    if current.strategy_checkpoint.is_some() && candidate.strategy_checkpoint.is_none() {
        return Err("拒绝清除已提交 Strategy checkpoint".into());
    }
    let binding_changed = current
        .strategy_binding_sha256
        .as_deref()
        .zip(candidate.strategy_binding_sha256.as_deref())
        .is_some_and(|(old, new)| old != new);
    let mut authorized_binding_migration = false;
    if binding_changed {
        let migration = candidate
            .deployment_migration
            .as_ref()
            .filter(|migration| migration.result == CURSOR_DEPLOYMENT_STATE_CONVERTED)
            .ok_or("拒绝覆盖另一策略绑定的 checkpoint")?;
        let source_checkpoint = current
            .strategy_checkpoint
            .as_ref()
            .ok_or("授权 checkpoint 转换缺少持久化来源 checkpoint")?;
        let target_checkpoint = candidate
            .strategy_checkpoint
            .as_ref()
            .ok_or("授权 checkpoint 转换缺少目标 checkpoint")?;
        let source_digest = checkpoint_sha256(source_checkpoint)?;
        let target_digest = checkpoint_sha256(target_checkpoint)?;
        if migration.source_revision != current.deployment_revision
            || migration.target_revision != candidate.deployment_revision.unwrap_or_default()
            || migration.source_checkpoint_sha256.as_deref() != Some(source_digest.as_str())
            || migration.target_checkpoint_sha256.as_deref() != Some(target_digest.as_str())
        {
            return Err("checkpoint 转换 receipt 与持久化来源/目标身份不匹配".into());
        }
        authorized_binding_migration = true;
    }
    if current.committed_run_id.is_some() && candidate.committed_run_id.is_none() {
        return Err("拒绝移除已提交 run/fencing marker".into());
    }
    if candidate.commit_generation < current.commit_generation {
        return Err(format!(
            "拒绝旧 checkpoint 覆盖较新提交: incoming_generation={} committed_generation={}",
            candidate.commit_generation, current.commit_generation
        ));
    }
    if candidate.commit_generation > current.commit_generation.saturating_add(1) {
        return Err(format!(
            "拒绝跳过 checkpoint 提交序号: incoming_generation={} committed_generation={}",
            candidate.commit_generation, current.commit_generation
        ));
    }
    if candidate.commit_generation == current.commit_generation {
        if candidate.committed_run_id != current.committed_run_id {
            return Err("不同 run 必须推进 checkpoint 提交序号".into());
        }
        if candidate.committed_run_id.is_some()
            && candidate.committed_fencing_token < current.committed_fencing_token
        {
            return Err(format!(
                "拒绝旧 fencing token 覆盖游标: incoming={} committed={}",
                candidate.committed_fencing_token, current.committed_fencing_token
            ));
        }
        if candidate.committed_fencing_token == current.committed_fencing_token {
            let current_checkpoint_digest = current
                .strategy_checkpoint
                .as_ref()
                .map(checkpoint_sha256)
                .transpose()?;
            if candidate.checkpoint_sha256 != current_checkpoint_digest
                && !authorized_binding_migration
            {
                return Err("相同 run/fence 不得改写已提交 checkpoint".into());
            }
        }
    } else if candidate.committed_run_id.is_none() {
        return Err("新的 checkpoint 提交必须带有 run marker".into());
    } else if candidate.committed_run_id == current.committed_run_id
        && candidate.committed_fencing_token < current.committed_fencing_token
    {
        return Err(format!(
            "拒绝旧 fencing token 覆盖游标: incoming={} committed={}",
            candidate.committed_fencing_token, current.committed_fencing_token
        ));
    }
    Ok(())
}

pub(in crate::workers::strategy_worker) fn validate_checkpoint_state(
    checkpoint: &StrategyCheckpointV1,
) -> Result<(), String> {
    if checkpoint.schema_version != qx_strategy::contract::STRATEGY_CHECKPOINT_SCHEMA_VERSION
        || checkpoint.strategy_id.trim().is_empty()
        || checkpoint.strategy_version.trim().is_empty()
        || !checkpoint.adapter_state.is_object()
        || !checkpoint.strategy_state.is_object()
    {
        return Err("策略 checkpoint schema 或字段非法".into());
    }
    let payload = serde_json::to_vec(checkpoint)
        .map_err(|error| format!("编码策略 checkpoint 失败: {error}"))?;
    if payload.len() > MAX_STRATEGY_CHECKPOINT_BYTES {
        return Err(format!(
            "策略 checkpoint 超过 {} 字节上限",
            MAX_STRATEGY_CHECKPOINT_BYTES
        ));
    }
    Ok(())
}

pub(in crate::workers::strategy_worker) fn checkpoint_sha256(
    checkpoint: &StrategyCheckpointV1,
) -> Result<String, String> {
    let payload = serde_json::to_vec(checkpoint)
        .map_err(|error| format!("编码策略 checkpoint checksum 失败: {error}"))?;
    Ok(qx_strategy::sha256_hex(&payload))
}

struct TemporaryCursorFile(PathBuf);

impl Drop for TemporaryCursorFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

pub(super) fn backup_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".bak");
    path.with_file_name(name)
}

#[cfg(all(test, feature = "postgres"))]
mod postgres_tests {
    use super::*;

    #[test]
    #[ignore = "requires QX_TEST_POSTGRES_DSN and a running PostgreSQL instance"]
    fn postgres_cursor_imports_legacy_file_and_fences_stale_progress() {
        let suffix = format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock after epoch")
                .as_nanos()
        );
        let worker_id = format!("cursor-contract-{suffix}");
        let root = std::env::temp_dir().join(format!("qx-postgres-cursor-{suffix}"));
        let legacy_path = cursor_path(&root, &worker_id);
        std::fs::create_dir_all(legacy_path.parent().expect("cursor parent"))
            .expect("create legacy cursor directory");

        let mut legacy = empty_cursor_state();
        legacy.next_seq_by_log.insert("acct/venue".into(), 7);
        std::fs::write(
            &legacy_path,
            serde_json::to_vec(&legacy).expect("serialize legacy cursor"),
        )
        .expect("write legacy cursor");

        let state_key = format!("strategy-execution-cursor/{worker_id}");
        let encoded = serde_json::to_string(&(
            "QX_TEST_POSTGRES_DSN",
            state_key,
            legacy_path.to_string_lossy().to_string(),
        ))
        .expect("encode cursor reference");
        let postgres_path = PathBuf::from(format!("{POSTGRES_CURSOR_PATH_PREFIX}{encoded}"));

        let imported = load_cursor(&postgres_path).expect("import legacy cursor into PostgreSQL");
        assert_eq!(imported.next_seq_by_log.get("acct/venue"), Some(&7));
        assert_eq!(
            load_cursor(&postgres_path).expect("reload imported cursor"),
            imported
        );

        let mut advanced = imported.clone();
        advanced.next_seq_by_log.insert("acct/venue".into(), 8);
        save_cursor(&postgres_path, &advanced).expect("advance PostgreSQL cursor");
        assert_eq!(
            load_cursor(&postgres_path)
                .expect("load advanced PostgreSQL cursor")
                .next_seq_by_log
                .get("acct/venue"),
            Some(&8)
        );

        assert!(save_cursor(&postgres_path, &imported)
            .expect_err("stale cursor must not overwrite committed progress")
            .contains("拒绝旧游标覆盖已提交 EventLog 进度"));
        let _ = std::fs::remove_dir_all(root);
    }
}
