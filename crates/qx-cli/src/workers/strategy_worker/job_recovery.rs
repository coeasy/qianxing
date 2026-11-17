use super::{execution_updates, lifecycle};
use crate::{lease_clock, runtime_timestamp_ms, strategy_run_is_final};
use qx_service::configured_backends::ConfiguredJobQueue;
use qx_storage::{JobLease, JsonStateStore, QueuedJob};
use std::path::Path;

pub(super) fn recover_completed_strategy_job(
    queue: &ConfiguredJobQueue,
    state_store: &JsonStateStore,
    state_path: &Path,
    prepared_evaluation_path: &Path,
    execution_cursor: &execution_updates::CursorState,
    queued: &QueuedJob,
    lease_owner: &str,
    lease_seconds: u64,
    lease: &mut JobLease,
    processed: &mut usize,
) -> Result<bool, String> {
    if strategy_run_is_final(state_store, state_path, queued.run.run_id)? {
        queue
            .ack_at(
                queued.run.run_id,
                lease_owner,
                lease.fencing_token,
                lease_clock(runtime_timestamp_ms()),
            )
            .map_err(|error| format!("确认已终态 Strategy Job 失败: {error:?}"))?;
        execution_updates::clear_prepared_evaluation_for_run(
            prepared_evaluation_path,
            queued.run.run_id,
        )?;
        return Ok(true);
    }

    // The cursor/checkpoint commit is the durable boundary. Finalize its Scheduler row and
    // acknowledge the queue entry without invoking the strategy or repeating order effects.
    if execution_updates::is_committed_run(execution_cursor, queued.run.run_id) {
        let finalize_now = lease_clock(runtime_timestamp_ms());
        *lease = queue
            .claim(queued.run.run_id, lease_owner, finalize_now, lease_seconds)
            .map_err(|error| format!("恢复已提交 Strategy Job 前租约已丢失: {error:?}"))?;
        if !queued.job.job_id.starts_with("live-strategy:") {
            lifecycle::complete_recovered_strategy_job_run(
                state_store,
                state_path,
                queued.run.run_id,
                finalize_now,
            )?;
        }
        queue
            .ack_at(
                queued.run.run_id,
                lease_owner,
                lease.fencing_token,
                lease_clock(runtime_timestamp_ms()),
            )
            .map_err(|error| format!("确认已提交 Strategy Job 失败: {error:?}"))?;
        execution_updates::clear_prepared_evaluation_for_run(
            prepared_evaluation_path,
            queued.run.run_id,
        )?;
        *processed += 1;
        return Ok(true);
    }

    Ok(false)
}
