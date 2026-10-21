use crate::{JobLease, JobRun, JobSpec, QueuedJob, StorageError};
use std::cmp::Ordering;
use std::path::PathBuf;

/// File, SQLite, and PostgreSQL expose queued jobs in one deterministic order.
pub(crate) fn compare_queued_job_order(left: &QueuedJob, right: &QueuedJob) -> Ordering {
    (&left.run.trading_day, left.run.run_id).cmp(&(&right.run.trading_day, right.run.run_id))
}

/// File/SQLite/PostgreSQL job queues share request validation and keep storage semantics local.
pub trait JobQueueBackend {
    fn validate_enqueue_job_request(&self, envelope: &QueuedJob) -> Result<(), StorageError> {
        envelope.validate()
    }

    fn validate_claim_job_request(
        &self,
        worker: &str,
        lease_seconds: u64,
    ) -> Result<(), StorageError> {
        if worker.trim().is_empty() || lease_seconds == 0 {
            return Err(StorageError::Conflict("worker 和租约时长不能为空".into()));
        }
        Ok(())
    }

    fn validate_existing_job(
        &self,
        existing: &QueuedJob,
        incoming: &QueuedJob,
    ) -> Result<(), StorageError> {
        if existing.job == incoming.job && existing.run == incoming.run {
            return Ok(());
        }
        Err(StorageError::Conflict(format!(
            "run_id {} 已被不同任务占用",
            incoming.run.run_id
        )))
    }

    fn enqueue_job(
        &self,
        job: JobSpec,
        run: JobRun,
        enqueued_ts: u64,
    ) -> Result<PathBuf, StorageError>;
    fn available_jobs(&self, now: u64) -> Result<Vec<QueuedJob>, StorageError>;
    fn claim_job(
        &self,
        run_id: u64,
        worker: &str,
        now: u64,
        lease_seconds: u64,
    ) -> Result<JobLease, StorageError>;

    /// Fenced acknowledgement is the only terminal write for a queued job.
    fn ack_job_at(
        &self,
        run_id: u64,
        worker: &str,
        fencing_token: u64,
        now: u64,
    ) -> Result<PathBuf, StorageError>;
    fn recover_expired_leases(&self, now: u64) -> Result<Vec<u64>, StorageError>;
}
