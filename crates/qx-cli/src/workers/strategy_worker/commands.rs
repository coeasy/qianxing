use super::lifecycle;
use crate::{command_is_final, runtime_timestamp_ms};
use qx_control::CommandKind;
use qx_runtime::WorkerContext;
use qx_service::configured_backends::ControlStateBackend;
use qx_storage::{ControlCommandQueueBackend, StorageError};
use qx_zhenlu::StrategyRuntime;
use std::sync::Arc;

pub(super) fn process_strategy_control_commands(
    control_store: &ControlStateBackend,
    command_queue: &Arc<dyn ControlCommandQueueBackend>,
    strategy: &mut StrategyRuntime,
    context: &WorkerContext,
    now: u64,
    lease_now: u64,
) -> Result<(), String> {
    let control = control_store.load()?;
    for command in control.pending().filter(|command| {
        matches!(
            command.kind,
            CommandKind::PauseStrategy | CommandKind::ResumeStrategy
        ) && command.target == context.id()
    }) {
        command_queue
            .enqueue_command(command.clone(), lease_now)
            .map_err(|error| format!("补入 Strategy 控制命令队列失败: {error:?}"))?;
    }
    for queued in command_queue
        .available_commands(lease_now)
        .map_err(|error| format!("读取 Strategy 控制命令队列失败: {error:?}"))?
    {
        let command = queued.command.clone();
        if !matches!(
            command.kind,
            CommandKind::PauseStrategy | CommandKind::ResumeStrategy
        ) || command.target != context.id()
        {
            continue;
        }
        let claim = command_queue.claim_command(command.command_id, context.id(), lease_now, 30);
        let lease = match claim {
            Ok(lease) => lease,
            Err(StorageError::LeaseHeld { .. }) => continue,
            Err(error) => {
                return Err(format!("领取 Strategy 控制命令租约失败: {error:?}"));
            }
        };
        if command_is_final(&control, command.command_id) {
            command_queue
                .ack_command_at(
                    command.command_id,
                    context.id(),
                    lease.fencing_token,
                    lease_now,
                )
                .map_err(|error| format!("清理已终态 Strategy 控制命令失败: {error:?}"))?;
            continue;
        }
        let action = if command.dry_run {
            Ok("DRY_RUN_VALIDATED".into())
        } else {
            lifecycle::transition_result(&command.kind, strategy.state).map(str::to_string)
        };
        let (_, record_result) = control_store
            .transact(|plane| plane.execute(command.command_id, now, |_| action.clone()))
            .map_err(|error| format!("回写 Strategy 控制命令终态失败: {error}"))?;
        let record =
            record_result.map_err(|error| format!("Strategy 控制命令执行失败: {error:?}"))?;
        if record.status == qx_control::CommandStatus::Executed && !command.dry_run {
            lifecycle::apply_transition(strategy, &command.kind)?;
            let paused = matches!(strategy.state, qx_zhenlu::StrategyState::Paused);
            context.mark(
                qx_runtime::ServiceStatus::Ready,
                format!(
                    "strategy version={} lifecycle={} strategy_accepting_jobs={}",
                    strategy.version,
                    if paused { "paused" } else { "running" },
                    !paused
                ),
                Some(runtime_timestamp_ms()),
            )?;
        }
        command_queue
            .ack_command_at(
                command.command_id,
                context.id(),
                lease.fencing_token,
                lease_now,
            )
            .map_err(|error| format!("确认 Strategy 控制命令失败: {error:?}"))?;
    }
    Ok(())
}
