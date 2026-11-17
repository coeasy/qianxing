use qx_control::{CommandKind, ControlPlane};
use qx_storage::JsonStateStore;
use qx_zhenlu::{StrategyRuntime, StrategyState};
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum StrategyJobFailureDisposition {
    AlreadyCommitted,
    RetryDurableEffects,
    StopForRecovery,
    MarkJobFailed,
}

pub(super) enum StrategyOutcomeFailure {
    Retry(String),
    Stop(String),
}

pub(super) fn handle_strategy_outcome_failure(
    committed: bool,
    effects_may_be_durable: bool,
    error: String,
    queue: &crate::ConfiguredJobQueue,
    run_id: u64,
    lease_owner: &str,
    lease_seconds: u64,
    state_store: &JsonStateStore,
    state_path: &Path,
    job_id: &str,
) -> Result<StrategyOutcomeFailure, String> {
    let retryable = super::admission::live_command_outcome_is_retryable(&error);
    match classify_job_failure(committed, effects_may_be_durable, retryable) {
        StrategyJobFailureDisposition::AlreadyCommitted
        | StrategyJobFailureDisposition::StopForRecovery => Ok(StrategyOutcomeFailure::Stop(error)),
        StrategyJobFailureDisposition::RetryDurableEffects => {
            Ok(StrategyOutcomeFailure::Retry(error))
        }
        StrategyJobFailureDisposition::MarkJobFailed => {
            let failure_lease_now = crate::lease_clock(crate::runtime_timestamp_ms());
            queue
                .claim(run_id, lease_owner, failure_lease_now, lease_seconds)
                .map_err(|lease_error| {
                    format!(
                        "Strategy Job 租约已转交，保留作业供当前租约持有人恢复: 原因={error}; 租约={lease_error:?}"
                    )
                })?;
            fail_strategy_job_run(state_store, state_path, job_id, run_id, failure_lease_now)?;
            Ok(StrategyOutcomeFailure::Stop(error))
        }
    }
}

pub(super) fn classify_job_failure(
    cursor_committed: bool,
    effects_may_be_durable: bool,
    retryable: bool,
) -> StrategyJobFailureDisposition {
    if cursor_committed {
        StrategyJobFailureDisposition::AlreadyCommitted
    } else if effects_may_be_durable && retryable {
        StrategyJobFailureDisposition::RetryDurableEffects
    } else if effects_may_be_durable {
        StrategyJobFailureDisposition::StopForRecovery
    } else {
        StrategyJobFailureDisposition::MarkJobFailed
    }
}

pub(super) fn execution_result(
    checkpoint_bootstrap: bool,
    warmup_complete: bool,
    order_results: &[String],
    dry_run: bool,
) -> String {
    if checkpoint_bootstrap {
        "LIVE_READINESS_CHECKPOINT_BOOTSTRAPPED_NO_INTENT".to_string()
    } else if !warmup_complete {
        "LIVE_READINESS_WARMUP_INCOMPLETE_NO_INTENT".to_string()
    } else if !order_results.is_empty() {
        format!(
            "{} orders: {}",
            order_results.len(),
            order_results.join(",")
        )
    } else if dry_run {
        "DRY_RUN_STRATEGY_TASK".to_string()
    } else {
        "STRATEGY_NO_REBALANCE".to_string()
    }
}

pub(super) fn complete_strategy_job_run(
    state_store: &JsonStateStore,
    state_path: &Path,
    run_id: u64,
    finished_ts: u64,
) -> Result<(), String> {
    crate::complete_strategy_job_run(state_store, state_path, run_id, finished_ts)
}

pub(super) fn fail_strategy_job_run(
    state_store: &JsonStateStore,
    state_path: &Path,
    job_id: &str,
    run_id: u64,
    finished_ts: u64,
) -> Result<(), String> {
    crate::fail_strategy_job_run(state_store, state_path, job_id, run_id, finished_ts)
}

pub(super) fn complete_recovered_strategy_job_run(
    state_store: &JsonStateStore,
    state_path: &Path,
    run_id: u64,
    finished_ts: u64,
) -> Result<(), String> {
    complete_strategy_job_run(state_store, state_path, run_id, finished_ts)
}

pub(super) fn persisted_pause_state(control: &ControlPlane, worker_id: &str) -> Option<bool> {
    control.strategy_pause_state(worker_id)
}

pub(super) fn transition_result(
    kind: &CommandKind,
    state: StrategyState,
) -> Result<&'static str, String> {
    match (kind, state) {
        (CommandKind::PauseStrategy, StrategyState::Running) => Ok("STRATEGY_PAUSED"),
        (CommandKind::ResumeStrategy, StrategyState::Paused) => Ok("STRATEGY_RESUMED"),
        (CommandKind::PauseStrategy, _) => Err("only a running strategy can be paused".into()),
        (CommandKind::ResumeStrategy, _) => Err("only a paused strategy can be resumed".into()),
        _ => Err("unsupported strategy control command".into()),
    }
}

pub(super) fn apply_transition(
    strategy: &mut StrategyRuntime,
    kind: &CommandKind,
) -> Result<(), String> {
    match kind {
        CommandKind::PauseStrategy => strategy.pause(),
        CommandKind::ResumeStrategy => strategy.start(),
        _ => return Err("unsupported strategy control command".into()),
    }
    .map_err(|error| format!("strategy lifecycle transition failed: {error:?}"))
}
