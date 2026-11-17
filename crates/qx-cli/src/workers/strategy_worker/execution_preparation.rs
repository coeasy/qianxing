use super::execution_updates;
use crate::{live_strategy_job_is_stale_with_config, StrategyJobLease};
use qx_core::InstrumentId;
use qx_runtime::{RuntimeConfig, StrategyRuntimeConfig};
use std::path::Path;

pub(super) struct StrategyExecutionPreparation<'a> {
    pub(super) live_job_digest: Option<u64>,
    pub(super) execution_ranges: Vec<execution_updates::CursorRange>,
    pub(super) execution_updates: execution_updates::ReplayedExecutionUpdates,
    pub(super) live_checkpoint_bootstrap: bool,
    pub(super) input_strategy_checkpoint: Option<&'a qx_strategy::contract::StrategyCheckpointV1>,
    pub(super) execution_facts_bytes: Vec<u8>,
    pub(super) instrument: InstrumentId,
    pub(super) checkpoint_restore_required: bool,
}

pub(super) fn prepare_strategy_execution<'a>(
    job_lease: &StrategyJobLease<'_>,
    runtime_config: &RuntimeConfig,
    strategy_config: &StrategyRuntimeConfig,
    root: &Path,
    now: u64,
    execution_cursor: &'a execution_updates::CursorState,
    durable_checkpoint_enabled: bool,
) -> Result<Option<StrategyExecutionPreparation<'a>>, String> {
    let live_job_digest =
        if job_lease.queued.job.job_id.starts_with("live-strategy:") {
            Some(
                job_lease.queued.run.manifest_digest.ok_or_else(|| {
                    "实时 Strategy Job 缺少 manifest_digest，拒绝执行".to_string()
                })?,
            )
        } else {
            None
        };
    if live_strategy_job_is_stale_with_config(
        job_lease,
        runtime_config,
        root,
        strategy_config,
        live_job_digest,
        now,
        "stale-market-digest",
        "确认过期实时 Strategy Job 失败",
    )? {
        return Ok(None);
    }

    let execution_ranges = execution_updates::ranges_from_job(&job_lease.queued.job)?;
    let strategy_id = strategy_config
        .id
        .clone()
        .unwrap_or_else(|| strategy_config.version.clone());
    let execution_updates = execution_updates::execution_updates_for_ranges(
        runtime_config,
        root,
        &strategy_id,
        &execution_ranges,
    )?;
    let live_checkpoint_bootstrap = live_job_digest.is_some()
        && durable_checkpoint_enabled
        && execution_updates::strategy_checkpoint(execution_cursor).is_none();
    let input_strategy_checkpoint = execution_updates::strategy_checkpoint(execution_cursor);
    let checkpoint_restore_required = strategy_config.external_executable.is_some()
        && input_strategy_checkpoint
            .as_ref()
            .is_some_and(|checkpoint| {
                checkpoint
                    .adapter_state
                    .get("state_mode")
                    .and_then(serde_json::Value::as_str)
                    == Some("external-process")
            });
    let execution_facts_bytes = serde_json::to_vec(&(&execution_ranges, &execution_updates))
        .map_err(|error| format!("编码策略执行事实摘要失败: {error}"))?;
    let instrument = strategy_config
        .instrument
        .as_deref()
        .and_then(InstrumentId::parse)
        .ok_or_else(|| "Strategy instrument 非法或未配置".to_string())?;

    Ok(Some(StrategyExecutionPreparation {
        live_job_digest,
        execution_ranges,
        execution_updates,
        live_checkpoint_bootstrap,
        input_strategy_checkpoint,
        execution_facts_bytes,
        instrument,
        checkpoint_restore_required,
    }))
}
