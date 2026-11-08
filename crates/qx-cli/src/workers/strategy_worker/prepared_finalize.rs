use super::{execution_updates, lifecycle};
use crate::{lease_clock, runtime_timestamp_ms};
use qx_runtime::StrategyRuntimeConfig;
use qx_service::configured_backends::ConfiguredJobQueue;
use qx_storage::{JobLease, JsonStateStore, QueuedJob};
use qx_strategy::contract::StrategyCheckpointV1;
use std::path::Path;

#[derive(Clone, Copy)]
pub(super) enum PreparedJournalCleanup {
    RunJournalBefore,
    RunJournalAfter,
    CurrentJournalBefore,
}

pub(super) struct FinalizeStrategyRun<'a> {
    pub(super) strategy_config: &'a StrategyRuntimeConfig,
    pub(super) queued: &'a QueuedJob,
    pub(super) queue: &'a ConfiguredJobQueue,
    pub(super) lease: &'a JobLease,
    pub(super) lease_owner: &'a str,
    pub(super) refresh_lease_before_commit: bool,
    pub(super) lease_seconds: u64,
    pub(super) durable_checkpoint_enabled: bool,
    pub(super) checkpoint: Option<StrategyCheckpointV1>,
    pub(super) checkpoint_missing_error: &'a str,
    pub(super) ranges: &'a [execution_updates::CursorRange],
    pub(super) mark_run_committed: bool,
    pub(super) execution_cursor: &'a mut execution_updates::CursorState,
    pub(super) execution_cursor_path: &'a Path,
    pub(super) state_store: &'a JsonStateStore,
    pub(super) state_path: &'a Path,
    pub(super) prepared_evaluation_path: &'a Path,
    pub(super) journal_cleanup: PreparedJournalCleanup,
    pub(super) live_commands_resolved: bool,
    pub(super) ack_failure_context: &'a str,
    pub(super) checkpoint_roundtrip_observed: &'a mut bool,
    pub(super) processed: &'a mut usize,
}

pub(super) fn finalize_strategy_run(request: FinalizeStrategyRun<'_>) -> Result<(), String> {
    let FinalizeStrategyRun {
        strategy_config,
        queued,
        queue,
        lease,
        lease_owner,
        refresh_lease_before_commit,
        lease_seconds,
        durable_checkpoint_enabled,
        checkpoint,
        checkpoint_missing_error,
        ranges,
        mark_run_committed,
        execution_cursor,
        execution_cursor_path,
        state_store,
        state_path,
        prepared_evaluation_path,
        journal_cleanup,
        live_commands_resolved,
        ack_failure_context,
        checkpoint_roundtrip_observed,
        processed,
    } = request;

    require_live_commands_resolved_before_finalize(
        queued.job.job_id.starts_with("live-strategy:"),
        live_commands_resolved,
    )?;

    let lease = if refresh_lease_before_commit {
        let commit_lease_now = lease_clock(runtime_timestamp_ms());
        queue
            .claim(
                queued.run.run_id,
                lease_owner,
                commit_lease_now,
                lease_seconds,
            )
            .map_err(|error| {
                format!("Strategy Job 在提交 cursor/checkpoint 前失去租约: {error:?}")
            })?
    } else {
        lease.clone()
    };

    if durable_checkpoint_enabled {
        let checkpoint = checkpoint.ok_or_else(|| checkpoint_missing_error.to_string())?;
        execution_updates::commit_strategy_checkpoint(
            execution_cursor,
            execution_cursor_path,
            strategy_config,
            queued.run.run_id,
            lease.fencing_token,
            ranges,
            checkpoint,
        )?;
        *checkpoint_roundtrip_observed = true;
    } else if !ranges.is_empty() {
        execution_updates::advance_cursor(execution_cursor, ranges)?;
        execution_updates::save_cursor(execution_cursor_path, execution_cursor)?;
    }

    if mark_run_committed {
        execution_updates::mark_run_committed(
            execution_cursor,
            execution_cursor_path,
            queued.run.run_id,
            lease.fencing_token,
        )?;
    }
    execution_updates::benchmark_crash_after("cursor_checkpoint_committed");

    if matches!(journal_cleanup, PreparedJournalCleanup::RunJournalBefore) {
        execution_updates::clear_prepared_evaluation_for_run(
            prepared_evaluation_path,
            queued.run.run_id,
        )?;
    } else if matches!(
        journal_cleanup,
        PreparedJournalCleanup::CurrentJournalBefore
    ) {
        execution_updates::clear_prepared_evaluation(prepared_evaluation_path)?;
    }
    execution_updates::benchmark_crash_after("prepared_journal_cleaned_before_ack");

    if !queued.job.job_id.starts_with("live-strategy:") {
        lifecycle::complete_strategy_job_run(
            state_store,
            state_path,
            queued.run.run_id,
            lease_clock(runtime_timestamp_ms()),
        )?;
    }
    execution_updates::benchmark_crash_after("scheduler_run_completed");
    queue
        .ack_at(
            queued.run.run_id,
            lease_owner,
            lease.fencing_token,
            lease_clock(runtime_timestamp_ms()),
        )
        .map_err(|error| format!("{ack_failure_context}失败: {error:?}"))?;
    execution_updates::benchmark_crash_after("queue_job_acknowledged");

    if matches!(journal_cleanup, PreparedJournalCleanup::RunJournalAfter) {
        execution_updates::clear_prepared_evaluation_for_run(
            prepared_evaluation_path,
            queued.run.run_id,
        )?;
    }
    execution_updates::benchmark_crash_after("prepared_journal_cleaned_after_ack");
    *processed += 1;
    Ok(())
}

fn require_live_commands_resolved_before_finalize(
    live_job: bool,
    live_commands_resolved: bool,
) -> Result<(), String> {
    if live_job && !live_commands_resolved {
        return Err("LIVE_COMMAND_RESOLUTION_REQUIRED_BEFORE_CURSOR_COMMIT".into());
    }
    Ok(())
}

#[cfg(test)]
mod live_finalize_gate_tests {
    use super::require_live_commands_resolved_before_finalize;

    #[test]
    fn cursor_finalize_is_blocked_until_live_command_resolution_completes() {
        assert!(require_live_commands_resolved_before_finalize(true, false)
            .unwrap_err()
            .contains("LIVE_COMMAND_RESOLUTION_REQUIRED_BEFORE_CURSOR_COMMIT"));
        assert!(require_live_commands_resolved_before_finalize(true, true).is_ok());
        assert!(require_live_commands_resolved_before_finalize(false, false).is_ok());
    }
}
