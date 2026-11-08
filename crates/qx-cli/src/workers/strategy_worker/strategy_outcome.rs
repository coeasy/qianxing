//! Ordinary Strategy job outcome orchestration.

use super::{
    execution_preparation, execution_updates, lifecycle, order_intents, prepared_finalize,
    strategy_evaluation, strategy_submission,
};
use crate::{
    lease_clock, live_strategy_job_is_stale_with_config, runtime_timestamp_ms,
    PythonStrategyClient, StrategyContractClients, StrategyJobLease, StrategyProcessClient,
};
use qx_runtime::{RuntimeConfig, StrategyRuntimeConfig};
use qx_service::configured_backends::{ConfiguredJobQueue, ControlStateBackend};
use qx_storage::{ControlCommandQueueBackend, JobLease, JsonStateStore, QueuedJob};
use qx_strategy::DynamicCAbiStrategy;
use qx_zhenlu::StrategyRuntime;
use std::path::Path;

pub(super) struct StrategyJobOutcome<'a> {
    pub(super) runtime_config: &'a RuntimeConfig,
    pub(super) strategy_config: &'a StrategyRuntimeConfig,
    pub(super) root: &'a Path,
    pub(super) worker_id: &'a str,
    pub(super) now: u64,
    pub(super) queued: &'a QueuedJob,
    pub(super) queue: &'a ConfiguredJobQueue,
    pub(super) lease: &'a mut JobLease,
    pub(super) lease_owner: &'a str,
    pub(super) lease_seconds: u64,
    pub(super) durable_checkpoint_enabled: bool,
    pub(super) execution_cursor: &'a mut execution_updates::CursorState,
    pub(super) execution_cursor_path: &'a Path,
    pub(super) prepared_evaluation_path: &'a Path,
    pub(super) state_store: &'a JsonStateStore,
    pub(super) state_path: &'a Path,
    pub(super) control_store: &'a ControlStateBackend,
    pub(super) command_queue: &'a dyn ControlCommandQueueBackend,
    pub(super) strategy: &'a mut StrategyRuntime,
    pub(super) checkpoint_restore_required: &'a mut bool,
    pub(super) host_lifecycle: &'a mut qx_api::StrategyHostLifecycleEvidenceV2,
    pub(super) python_client: &'a mut Option<PythonStrategyClient>,
    pub(super) external_client: &'a mut Option<StrategyProcessClient>,
    pub(super) native_client: &'a mut Option<DynamicCAbiStrategy>,
    pub(super) native_initialized: &'a mut bool,
    pub(super) runtime_config_path: &'a Path,
    pub(super) checkpoint_roundtrip_observed: &'a mut bool,
    pub(super) processed: &'a mut usize,
    pub(super) quiet: bool,
}

pub(super) enum StrategyJobOutcomeDisposition {
    Continue,
    Retry,
}

pub(super) fn execute_strategy_job_outcome(
    request: StrategyJobOutcome<'_>,
) -> Result<StrategyJobOutcomeDisposition, String> {
    let StrategyJobOutcome {
        runtime_config,
        strategy_config,
        root,
        worker_id,
        now,
        queued,
        queue,
        lease,
        lease_owner,
        lease_seconds,
        durable_checkpoint_enabled,
        execution_cursor,
        execution_cursor_path,
        prepared_evaluation_path,
        state_store,
        state_path,
        control_store,
        command_queue,
        strategy,
        checkpoint_restore_required,
        host_lifecycle,
        python_client,
        external_client,
        native_client,
        native_initialized,
        runtime_config_path,
        checkpoint_roundtrip_observed,
        processed,
        quiet,
    } = request;
    let mut class_effects_may_be_durable = false;
    let mut retry_pending = false;
    let outcome = (|| -> Result<(), String> {
        let job_lease = StrategyJobLease {
            queue,
            queued,
            worker_id,
            lease_owner,
            lease_seconds,
        };
        let Some(preparation) = execution_preparation::prepare_strategy_execution(
            &job_lease,
            runtime_config,
            strategy_config,
            root,
            now,
            execution_cursor,
            durable_checkpoint_enabled,
        )?
        else {
            *processed += 1;
            return Ok(());
        };
        let live_job_digest = preparation.live_job_digest;
        let execution_ranges = preparation.execution_ranges;
        let execution_updates = preparation.execution_updates;
        let live_checkpoint_bootstrap = preparation.live_checkpoint_bootstrap;
        let input_strategy_checkpoint = preparation.input_strategy_checkpoint;
        let execution_facts_bytes = preparation.execution_facts_bytes;
        let instrument = preparation.instrument;
        *checkpoint_restore_required = preparation.checkpoint_restore_required;
        let evaluation = strategy_evaluation::evaluate_strategy_for_job(
            root,
            runtime_config,
            &instrument,
            queued.run.run_id,
            now,
            &execution_updates,
            input_strategy_checkpoint,
            live_job_digest,
            live_checkpoint_bootstrap,
            execution_updates::strategy_checkpoint(execution_cursor).is_some(),
            *checkpoint_restore_required,
            host_lifecycle,
            StrategyContractClients {
                python_client,
                external_client,
                native_client,
                native_initialized,
                runtime_config_path,
            },
        )?;
        let target_qty = evaluation.target_qty;
        let contract_output = evaluation.contract_output;
        let strategy_checkpoint = evaluation.strategy_checkpoint;
        let live_warmup_complete = evaluation.live_warmup_complete;
        let live_readiness_checkpoint_only = live_checkpoint_bootstrap || !live_warmup_complete;
        let orders = order_intents::project_strategy_orders(
            root,
            runtime_config,
            runtime_config_path,
            worker_id,
            queued.run.run_id,
            now,
            live_readiness_checkpoint_only,
            &instrument,
            target_qty,
            contract_output.as_ref(),
        )?;
        let evaluation_lease_now = lease_clock(runtime_timestamp_ms());
        *lease = queue
            .claim(
                queued.run.run_id,
                lease_owner,
                evaluation_lease_now,
                lease_seconds,
            )
            .map_err(|error| format!("Strategy Job 在策略评估后失去租约: {error:?}"))?;
        if live_strategy_job_is_stale_with_config(
            &job_lease,
            runtime_config,
            root,
            strategy_config,
            live_job_digest,
            runtime_timestamp_ms(),
            "market-changed-during-evaluation",
            "确认执行期间过期实时 Strategy Job 失败",
        )? {
            *processed += 1;
            return Ok(());
        }
        // Validate every strategy quota and command before persisting the spread-group snapshot.
        let (spread_group_id, commands) = order_intents::reserve_strategy_commands(
            &orders,
            strategy,
            worker_id,
            queued.run.run_id,
            queued.job.dry_run,
            contract_output.as_ref(),
        )?;
        let submission = strategy_submission::persist_and_submit_strategy_outcome(
            strategy_submission::StrategySubmission {
                runtime_config,
                strategy_config,
                root,
                worker_id,
                now,
                live_job_digest,
                execution_ranges: &execution_ranges,
                execution_facts_bytes: &execution_facts_bytes,
                orders: &orders,
                commands,
                spread_group_id,
                strategy_checkpoint: strategy_checkpoint.clone(),
                queued,
                job_lease: &job_lease,
                queue,
                lease,
                lease_owner,
                lease_seconds,
                prepared_evaluation_path,
                control_store,
                command_queue,
                class_effects_may_be_durable: &mut class_effects_may_be_durable,
            },
        )?;
        let Some(submission) = submission else {
            *processed += 1;
            return Ok(());
        };
        if let Some(error) = submission.retry_pending {
            if !quiet {
                println!(
                    "[策略 · Strategy] worker={} run_id={} intent recovery pending: {}",
                    worker_id, queued.run.run_id, error
                );
            }
            *processed += 1;
            retry_pending = true;
            return Ok(());
        }
        let prepared = submission.prepared;
        let result = lifecycle::execution_result(
            live_checkpoint_bootstrap,
            live_warmup_complete,
            &submission.results,
            queued.job.dry_run,
        );
        prepared_finalize::finalize_strategy_run(prepared_finalize::FinalizeStrategyRun {
            strategy_config,
            queued,
            queue,
            lease,
            lease_owner,
            refresh_lease_before_commit: true,
            lease_seconds,
            durable_checkpoint_enabled,
            checkpoint: strategy_checkpoint,
            checkpoint_missing_error: "Strategy did not return a durable checkpoint",
            ranges: &execution_ranges,
            mark_run_committed: !durable_checkpoint_enabled && prepared.is_some(),
            execution_cursor,
            execution_cursor_path,
            state_store,
            state_path,
            prepared_evaluation_path,
            journal_cleanup: prepared_finalize::PreparedJournalCleanup::CurrentJournalBefore,
            live_commands_resolved: true,
            ack_failure_context: "确认 Strategy JobQueue ",
            checkpoint_roundtrip_observed,
            processed,
        })?;
        if !quiet {
            println!(
                "[策略 · Strategy] worker={} job={} run_id={} result={}",
                worker_id, queued.job.job_id, queued.run.run_id, result
            );
        }
        Ok(())
    })();

    if let Err(error) = outcome {
        // Once the cursor/checkpoint marker is durable, recording Failed would contradict committed effects.
        let committed = execution_updates::is_committed_run(execution_cursor, queued.run.run_id);
        match lifecycle::handle_strategy_outcome_failure(
            committed,
            class_effects_may_be_durable,
            error,
            queue,
            queued.run.run_id,
            lease_owner,
            lease_seconds,
            state_store,
            state_path,
            &queued.job.job_id,
        ) {
            Ok(lifecycle::StrategyOutcomeFailure::Stop(error)) => Err(error),
            Ok(lifecycle::StrategyOutcomeFailure::Retry(error)) => {
                if !quiet {
                    println!(
                        "[策略 · Strategy] worker={} run_id={} intent recovery pending: {}",
                        worker_id, queued.run.run_id, error
                    );
                }
                *processed += 1;
                Ok(StrategyJobOutcomeDisposition::Retry)
            }
            Err(error) => Err(error),
        }
    } else if retry_pending {
        // Resolution is deliberately retried from the durable PreparedEvaluation after
        // the queue lease expires. Do not finalize or advance the cursor/checkpoint here.
        Ok(StrategyJobOutcomeDisposition::Retry)
    } else {
        Ok(StrategyJobOutcomeDisposition::Continue)
    }
}
