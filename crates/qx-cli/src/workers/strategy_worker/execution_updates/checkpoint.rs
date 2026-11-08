//! Strategy checkpoint identity, cursor progress and deployment revision transitions.

use super::*;

pub(in crate::workers::strategy_worker) fn advance_cursor(
    state: &mut CursorState,
    ranges: &[CursorRange],
) -> Result<(), String> {
    for range in ranges {
        let next_seq = range
            .through_seq
            .checked_add(1)
            .ok_or_else(|| "Strategy EventLog 游标溢出".to_string())?;
        let current = state
            .next_seq_by_log
            .entry(range.log_name.clone())
            .or_default();
        *current = (*current).max(next_seq);
    }
    Ok(())
}

pub(in crate::workers::strategy_worker) fn strategy_checkpoint(
    state: &CursorState,
) -> Option<&StrategyCheckpointV1> {
    state.strategy_checkpoint.as_ref()
}

pub(in crate::workers::strategy_worker) fn is_committed_run(
    state: &CursorState,
    run_id: u64,
) -> bool {
    state.committed_run_id == Some(run_id)
}

pub(in crate::workers::strategy_worker) fn mark_run_committed(
    state: &mut CursorState,
    path: &Path,
    run_id: u64,
    fencing_token: u64,
) -> Result<(), String> {
    let mut candidate = state.clone();
    validate_run_fence(&candidate, run_id, fencing_token)?;
    candidate.commit_generation = candidate
        .commit_generation
        .checked_add(1)
        .ok_or_else(|| "Strategy checkpoint 提交序号溢出".to_string())?;
    candidate.committed_run_id = Some(run_id);
    candidate.committed_fencing_token = fencing_token;
    save_cursor(path, &candidate)?;
    *state = candidate;
    Ok(())
}

#[cfg(test)]
pub(super) fn validate_checkpoint_binding(
    state: &CursorState,
    strategy: &StrategyRuntimeConfig,
) -> Result<(), String> {
    validate_checkpoint_binding_for_mode(state, strategy, strategy.python_module.is_some())
}

pub(in crate::workers::strategy_worker) fn validate_checkpoint_binding_for_mode(
    state: &CursorState,
    strategy: &StrategyRuntimeConfig,
    checkpoint_enabled: bool,
) -> Result<(), String> {
    if !checkpoint_enabled {
        if state.strategy_checkpoint.is_some() {
            return Err(
                "当前 Strategy runtime 不支持恢复已提交 checkpoint；拒绝继续推进执行游标".into(),
            );
        }
        return Ok(());
    }
    let expected = strategy_binding_sha256(strategy)?;
    match state.strategy_binding_sha256.as_deref() {
        Some(actual) if actual != expected => Err(
            "Strategy durable checkpoint 与当前 strategy_id/version/artifact/spec 不匹配；拒绝从旧状态继续"
                .into(),
        ),
        Some(_) => {
            let expected_id = strategy.id.as_deref().unwrap_or(&strategy.version);
            if state.strategy_checkpoint.as_ref().is_some_and(|checkpoint| {
                checkpoint.strategy_id != expected_id
                    || checkpoint.strategy_version != strategy.version
            }) {
                return Err(
                    "Strategy checkpoint 内的 strategy_id/version 与当前配置不匹配；拒绝恢复"
                        .into(),
                );
            }
            Ok(())
        }
        None
            if state.strategy_checkpoint.is_some()
                || state.next_seq_by_log.values().any(|next_seq| *next_seq > 0) =>
        {
            Err("Strategy 执行游标已推进但没有可恢复 checkpoint；拒绝静默重启".into())
        }
        None => Ok(()),
    }
}

pub(in crate::workers::strategy_worker) fn bind_cursor_to_deployment_revision(
    state: &mut CursorState,
    path: &Path,
    strategy: &StrategyRuntimeConfig,
    checkpoint_enabled: bool,
    target_revision: Option<u64>,
) -> Result<(), String> {
    let Some(target_revision) = target_revision else {
        return if state.deployment_revision.is_some() {
            Err(
                "deployment-bound Strategy cursor must be launched by the Runtime supervisor"
                    .into(),
            )
        } else {
            Ok(())
        };
    };
    if target_revision == 0 {
        return Err("Strategy deployment revision must be positive".into());
    }
    let source_revision = state.deployment_revision;
    if source_revision == Some(target_revision) {
        return Ok(());
    }
    if source_revision.is_some_and(|source| source > target_revision) {
        return Err(format!(
            "refusing stale Strategy deployment revision {target_revision}; cursor is already bound to {}",
            source_revision.unwrap_or_default()
        ));
    }
    let mut conversion_receipt = None;
    let mut converted_checkpoint = None;
    let mut converted_binding = None;
    if state.strategy_checkpoint.is_some() {
        let expected_binding = strategy_binding_sha256(strategy)?;
        if state.strategy_binding_sha256.as_deref() != Some(expected_binding.as_str()) {
            let migration = strategy.checkpoint_state_migration.as_ref().ok_or_else(|| {
                "deployment revision changes Strategy checkpoint identity; an authorized state converter is required".to_string()
            })?;
            let checkpoint = state
                .strategy_checkpoint
                .as_ref()
                .ok_or_else(|| "checkpoint migration requires a saved checkpoint".to_string())?;
            let source_binding = state
                .strategy_binding_sha256
                .as_deref()
                .ok_or_else(|| "checkpoint migration source binding is missing".to_string())?;
            let source_checkpoint_sha256 = checkpoint_sha256(checkpoint)?;
            if migration.schema_version != 1
                || migration.source_deployment_revision != source_revision.unwrap_or_default()
                || migration.target_deployment_revision != target_revision
                || !migration
                    .source_strategy_binding_sha256
                    .eq_ignore_ascii_case(source_binding)
                || !migration
                    .source_checkpoint_sha256
                    .eq_ignore_ascii_case(&source_checkpoint_sha256)
                || !migration.target_strategy_state.is_object()
            {
                return Err(
                    "checkpoint migration is not authorized for this exact source checkpoint and target revision".into(),
                );
            }
            let converter_bytes = serde_json::to_vec(migration)
                .map_err(|error| format!("编码 checkpoint migration manifest 失败: {error}"))?;
            if converter_bytes.len() > MAX_STRATEGY_CHECKPOINT_BYTES {
                return Err("checkpoint migration manifest exceeds 64 KiB".into());
            }
            let target_id = strategy.id.as_deref().unwrap_or(&strategy.version);
            let mut converted = checkpoint.clone();
            converted.strategy_id = target_id.to_string();
            converted.strategy_version = strategy.version.clone();
            converted.strategy_state = migration.target_strategy_state.clone();
            super::cursor_state::validate_checkpoint_state(&converted)?;
            validate_checkpoint_adapter_for_target(strategy, &converted.adapter_state)?;
            let target_checkpoint_sha256 = checkpoint_sha256(&converted)?;
            converted_checkpoint = Some(converted);
            converted_binding = Some(expected_binding);
            conversion_receipt = Some((
                qx_strategy::sha256_hex(&converter_bytes),
                source_checkpoint_sha256,
                target_checkpoint_sha256,
            ));
        }
    }
    if conversion_receipt.is_some() && !checkpoint_enabled {
        return Err("checkpoint migration target host does not support durable checkpoints".into());
    }
    let result = if conversion_receipt.is_some() {
        CURSOR_DEPLOYMENT_STATE_CONVERTED
    } else if state.strategy_checkpoint.is_some() && checkpoint_enabled {
        CURSOR_DEPLOYMENT_CHECKPOINT_BINDING
    } else {
        CURSOR_DEPLOYMENT_INITIAL_BINDING
    };
    let mut candidate = state.clone();
    if let Some(checkpoint) = converted_checkpoint {
        candidate.checkpoint_sha256 = Some(checkpoint_sha256(&checkpoint)?);
        candidate.strategy_checkpoint = Some(checkpoint);
        candidate.strategy_binding_sha256 = converted_binding;
    }
    candidate.deployment_revision = Some(target_revision);
    candidate.deployment_migration = Some(CursorDeploymentMigrationV1 {
        source_revision,
        target_revision,
        result: result.into(),
        converter_sha256: conversion_receipt.as_ref().map(|value| value.0.clone()),
        source_checkpoint_sha256: conversion_receipt.as_ref().map(|value| value.1.clone()),
        target_checkpoint_sha256: conversion_receipt.as_ref().map(|value| value.2.clone()),
    });
    validate_checkpoint_binding_for_mode(&candidate, strategy, checkpoint_enabled)?;
    save_cursor(path, &candidate)?;
    *state = candidate;
    Ok(())
}

pub(in crate::workers::strategy_worker) fn commit_strategy_checkpoint(
    state: &mut CursorState,
    path: &Path,
    strategy: &StrategyRuntimeConfig,
    run_id: u64,
    fencing_token: u64,
    ranges: &[CursorRange],
    checkpoint: StrategyCheckpointV1,
) -> Result<(), String> {
    if strategy.python_module.is_none()
        && strategy.external_executable.is_none()
        && strategy.c_abi_library.is_none()
    {
        return Err("拒绝为不支持 durable checkpoint 的 Strategy worker 保存 checkpoint".into());
    }
    let expected_id = strategy
        .id
        .clone()
        .unwrap_or_else(|| strategy.version.clone());
    if checkpoint.strategy_id != expected_id || checkpoint.strategy_version != strategy.version {
        return Err("Strategy checkpoint 与 worker strategy_id/version 不匹配".into());
    }
    let fingerprint = strategy_binding_sha256(strategy)?;
    if state
        .strategy_binding_sha256
        .as_deref()
        .is_some_and(|current| current != fingerprint)
    {
        return Err("Strategy checkpoint 绑定指纹在 worker 生命周期中发生变化".into());
    }
    let mut candidate = state.clone();
    validate_run_fence(&candidate, run_id, fencing_token)?;
    advance_cursor(&mut candidate, ranges)?;
    candidate.checkpoint_sha256 = Some(checkpoint_sha256(&checkpoint)?);
    candidate.strategy_checkpoint = Some(checkpoint);
    candidate.strategy_binding_sha256 = Some(fingerprint);
    candidate.commit_generation = candidate
        .commit_generation
        .checked_add(1)
        .ok_or_else(|| "Strategy checkpoint 提交序号溢出".to_string())?;
    candidate.committed_run_id = Some(run_id);
    candidate.committed_fencing_token = fencing_token;
    save_cursor(path, &candidate)?;
    *state = candidate;
    Ok(())
}

fn validate_checkpoint_adapter_for_target(
    strategy: &StrategyRuntimeConfig,
    adapter_state: &serde_json::Value,
) -> Result<(), String> {
    let Some(adapter) = adapter_state.as_object() else {
        return Err("checkpoint migration adapter_state must remain a JSON object".into());
    };
    if strategy.c_abi_library.is_some() {
        if adapter
            .get("state_mode")
            .and_then(serde_json::Value::as_str)
            != Some("c-abi")
        {
            return Err(
                "checkpoint migration adapter_state is incompatible with the target C ABI host"
                    .into(),
            );
        }
    } else if strategy.external_executable.is_some() {
        if adapter
            .get("state_mode")
            .and_then(serde_json::Value::as_str)
            != Some("external-process")
        {
            return Err("checkpoint migration adapter_state is incompatible with the target external process host".into());
        }
    } else if strategy.python_strategy_spec.is_some() {
        let expected = [
            "schema_version",
            "bars_seen",
            "last_bar_ts",
            "warmup_completed",
            "update_cursors",
        ]
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>();
        if adapter
            .keys()
            .map(String::as_str)
            .collect::<std::collections::BTreeSet<_>>()
            != expected
            || adapter
                .get("schema_version")
                .and_then(serde_json::Value::as_u64)
                != Some(1)
        {
            return Err("checkpoint migration adapter_state is incompatible with the target Python class host".into());
        }
    } else if strategy.python_module.is_some() {
        let mode = adapter
            .get("state_mode")
            .and_then(serde_json::Value::as_str);
        if adapter
            .get("schema_version")
            .and_then(serde_json::Value::as_u64)
            != Some(1)
            || !matches!(mode, Some("module-function" | "stateless"))
        {
            return Err("checkpoint migration adapter_state is incompatible with the target Python module host".into());
        }
    } else {
        return Err("checkpoint migration target has no durable-checkpoint host".into());
    }
    Ok(())
}

pub(super) fn validate_run_fence(
    state: &CursorState,
    run_id: u64,
    fencing_token: u64,
) -> Result<(), String> {
    if fencing_token == 0 {
        return Err("拒绝使用空 fencing token 提交 Strategy 状态".into());
    }
    if state.committed_run_id == Some(run_id) && fencing_token < state.committed_fencing_token {
        return Err(format!(
            "拒绝旧 owner 写入 Strategy 状态: run_id={run_id} incoming_fence={fencing_token} committed_fence={}",
            state.committed_fencing_token
        ));
    }
    Ok(())
}

pub(super) fn strategy_binding_sha256(strategy: &StrategyRuntimeConfig) -> Result<String, String> {
    // Bind every behavior-affecting StrategyRuntimeConfig field, including fields added later,
    // so a checkpoint cannot silently cross a config change. The one-time migration manifest is
    // control-plane authorization, not strategy behavior, and is deliberately excluded. Only
    // this digest is persisted; runtime config (which may contain sensitive values) is never
    // copied into the cursor. Paths are included conservatively: even a path-only change requires
    // explicit conversion/revalidation rather than assuming the referenced bytes match.
    let mut strategy_config = serde_json::to_value(strategy)
        .map_err(|error| format!("编码 Strategy checkpoint identity 失败: {error}"))?;
    strategy_config
        .as_object_mut()
        .ok_or_else(|| "Strategy checkpoint identity must be an object".to_string())?
        .remove("checkpoint_state_migration");
    let identity = serde_json::json!({
        "identity_schema_version": 2,
        "strategy_runtime_config": strategy_config,
    });
    let encoded = serde_json::to_vec(&identity)
        .map_err(|error| format!("编码 Strategy checkpoint identity 失败: {error}"))?;
    Ok(qx_strategy::sha256_hex(&encoded))
}
