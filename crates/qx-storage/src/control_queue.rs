use crate::{ControlCommand, ControlCommandLease, QueuedControlCommand, StorageError};
use std::cmp::Ordering;
use std::path::PathBuf;

pub(crate) fn compare_control_command_order(
    left: &QueuedControlCommand,
    right: &QueuedControlCommand,
) -> Ordering {
    (left.enqueued_ts, left.command.command_id).cmp(&(right.enqueued_ts, right.command.command_id))
}

/// File/SQLite/MQ command queues share validation while retaining their own persistence strategy.
pub trait ControlCommandQueueBackend: Send + Sync {
    fn validate_enqueue_request(&self, command: &ControlCommand) -> Result<(), StorageError> {
        command
            .validate()
            .map_err(|error| StorageError::Conflict(format!("控制命令非法: {error:?}")))
    }

    fn validate_claim_request(
        &self,
        command_id: u64,
        owner: &str,
        lease_seconds: u64,
    ) -> Result<(), StorageError> {
        if command_id == 0 || owner.trim().is_empty() || lease_seconds == 0 {
            return Err(StorageError::Conflict(
                "command_id、worker 和租约时长不能为空".into(),
            ));
        }
        Ok(())
    }

    fn validate_existing_command(
        &self,
        existing: &ControlCommand,
        incoming: &ControlCommand,
    ) -> Result<(), StorageError> {
        if existing == incoming {
            return Ok(());
        }
        Err(StorageError::Conflict(format!(
            "command_id {} 已被不同命令占用",
            incoming.command_id
        )))
    }

    fn enqueue_command(
        &self,
        command: ControlCommand,
        enqueued_ts: u64,
    ) -> Result<PathBuf, StorageError>;
    fn available_commands(&self, now: u64) -> Result<Vec<QueuedControlCommand>, StorageError>;
    fn claim_command(
        &self,
        command_id: u64,
        owner: &str,
        now: u64,
        lease_seconds: u64,
    ) -> Result<ControlCommandLease, StorageError>;
    fn ack_command_at(
        &self,
        command_id: u64,
        owner: &str,
        fencing_token: u64,
        now: u64,
    ) -> Result<PathBuf, StorageError>;

    /// Pending means every unacked command, including commands with an active lease.
    fn pending_commands(&self) -> Result<Vec<QueuedControlCommand>, StorageError> {
        self.available_commands(u64::MAX)
    }
}
