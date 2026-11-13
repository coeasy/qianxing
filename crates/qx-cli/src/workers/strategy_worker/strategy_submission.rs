//! Durable ordinary-outcome write-ahead and per-command submission lifecycle.

use super::*;
use crate::{persist_strategy_spread_group, persist_strategy_submit};

pub(super) struct StrategySubmission<'a> {
    pub(super) runtime_config: &'a qx_runtime::RuntimeConfig,
    pub(super) strategy_config: &'a qx_runtime::StrategyRuntimeConfig,
    pub(super) root: &'a Path,
    pub(super) worker_id: &'a str,
    pub(super) now: u64,
    pub(super) live_job_digest: Option<u64>,
    pub(super) execution_ranges: &'a [execution_updates::CursorRange],
    pub(super) execution_facts_bytes: &'a [u8],
    pub(super) orders: &'a [qx_core::Order],
    pub(super) commands: Vec<qx_control::ControlCommand>,
    pub(super) spread_group_id: Option<String>,
    pub(super) strategy_checkpoint: Option<qx_strategy::contract::StrategyCheckpointV1>,
    pub(super) queued: &'a qx_storage::QueuedJob,
    pub(super) job_lease: &'a StrategyJobLease<'a>,
    pub(super) queue: &'a crate::ConfiguredJobQueue,
    pub(super) lease: &'a mut qx_storage::JobLease,
    pub(super) lease_owner: &'a str,
    pub(super) lease_seconds: u64,
    pub(super) prepared_evaluation_path: &'a Path,
    pub(super) control_store: &'a qx_service::configured_backends::ControlStateBackend,
    pub(super) command_queue: &'a dyn qx_storage::ControlCommandQueueBackend,
    pub(super) class_effects_may_be_durable: &'a mut bool,
}

pub(super) struct StrategySubmissionOutcome {
    pub(super) prepared: Option<execution_updates::PreparedEvaluationV3>,
    pub(super) results: Vec<String>,
    pub(super) retry_pending: Option<String>,
}

pub(super) fn persist_and_submit_strategy_outcome(
    input: StrategySubmission<'_>,
) -> Result<Option<StrategySubmissionOutcome>, String> {
    let StrategySubmission {
        runtime_config,
        strategy_config,
        root,
        worker_id,
        now,
        live_job_digest,
        execution_ranges,
        execution_facts_bytes,
        orders,
        commands,
        spread_group_id,
        strategy_checkpoint,
        queued,
        job_lease,
        queue,
        lease,
        lease_owner,
        lease_seconds,
        prepared_evaluation_path,
        control_store,
        command_queue,
        class_effects_may_be_durable,
    } = input;

    let mut prepared = if !queued.job.dry_run && !commands.is_empty() {
        let input_bytes = serde_json::to_vec(&queued.job)
            .map_err(|error| format!("编码 Strategy job 输入摘要失败: {error}"))?;
        Some(execution_updates::PreparedEvaluationV3 {
            schema_version: execution_updates::PREPARED_EVALUATION_SCHEMA_VERSION,
            worker_id: worker_id.to_string(),
            job_id: queued.job.job_id.clone(),
            run_id: queued.run.run_id,
            input_sha256: format!("{:x}", sha2::Sha256::digest(&input_bytes)),
            facts_sha256: format!("{:x}", sha2::Sha256::digest(execution_facts_bytes)),
            config_sha256: format!(
                "{:x}",
                sha2::Sha256::digest(
                    serde_json::to_vec(&runtime_config.strategy)
                        .map_err(|error| format!("编码 Strategy 配置失败: {error}"))?,
                )
            ),
            lease_fencing_token: lease.fencing_token,
            ranges: execution_ranges.to_vec(),
            orders: orders.to_vec(),
            commands: commands.clone(),
            spread_group_id: spread_group_id.clone(),
            checkpoint: strategy_checkpoint.clone(),
            completed_command_ids: Default::default(),
            command_results: Default::default(),
            payload_sha256: String::new(),
        })
    } else {
        None
    };
    if let Some(prepared) = prepared.as_ref() {
        execution_updates::save_prepared_evaluation(prepared_evaluation_path, prepared)?;
    }
    if !queued.job.dry_run && !commands.is_empty() {
        // A failed submit response can still mean the command was persisted.
        *class_effects_may_be_durable = true;
    }
    if !queued.job.dry_run {
        if let Some(group_id) = spread_group_id.as_deref() {
            persist_strategy_spread_group(root, group_id, worker_id, orders)?;
        }
    }

    let mut results = Vec::new();
    for command in commands {
        if !command.dry_run && queued.job.job_id.starts_with("live-strategy:") {
            *lease = queue
                .claim(
                    queued.run.run_id,
                    lease_owner,
                    lease_clock(runtime_timestamp_ms()),
                    lease_seconds,
                )
                .map_err(|error| {
                    format!("实时 Strategy intent 提交前重新领取 fencing 租约失败: {error:?}")
                })?;
            if live_strategy_job_is_stale_with_config(
                job_lease,
                runtime_config,
                root,
                strategy_config,
                live_job_digest,
                runtime_timestamp_ms(),
                "market-changed-before-intent-submit",
                "确认提交前过期实时 Strategy Job 失败",
            )? {
                if prepared
                    .as_ref()
                    .is_some_and(|value| value.completed_command_ids.is_empty())
                {
                    execution_updates::clear_prepared_evaluation_for_run(
                        prepared_evaluation_path,
                        queued.run.run_id,
                    )?;
                    return Ok(None);
                }
                return Err("LIVE_READINESS_MARKET_CHANGED_AFTER_PARTIAL_INTENT".into());
            }
            require_live_intent_reconciliation(strategy_config, root, runtime_timestamp_ms())?;
        }
        let order_result = if command.dry_run {
            "DRY_RUN_SIGNAL_ORDER_INTENT_VALIDATED".to_string()
        } else {
            match persist_strategy_submit(control_store, command_queue, &command, now) {
                Ok(result) => {
                    execution_updates::benchmark_crash_after("control_command_persisted");
                    result
                }
                Err(error) => {
                    if let Some(prepared) = prepared.as_mut() {
                        prepared.command_results.insert(
                            command.command_id,
                            execution_updates::unknown_command_result(&error),
                        );
                        execution_updates::save_prepared_evaluation(
                            prepared_evaluation_path,
                            prepared,
                        )?;
                    }
                    return Err(error);
                }
            }
        };
        if let Some(prepared) = prepared.as_mut() {
            prepared.completed_command_ids.insert(command.command_id);
            prepared
                .command_results
                .insert(command.command_id, order_result.clone());
            execution_updates::save_prepared_evaluation(prepared_evaluation_path, prepared)?;
            execution_updates::benchmark_crash_after("command_result_persisted");
        }
        results.push(order_result);
    }

    if queued.job.job_id.starts_with("live-strategy:") {
        if let Some(prepared) = prepared.as_ref() {
            if let Err(error) = require_live_strategy_commands_resolved(
                runtime_config,
                root,
                &control_store.load()?,
                &prepared.commands,
            ) {
                if live_command_outcome_is_retryable(&error) {
                    return Ok(Some(StrategySubmissionOutcome {
                        prepared: Some(prepared.clone()),
                        results,
                        retry_pending: Some(error),
                    }));
                }
                return Err(error);
            }
        }
    }

    Ok(Some(StrategySubmissionOutcome {
        prepared,
        results,
        retry_pending: None,
    }))
}
