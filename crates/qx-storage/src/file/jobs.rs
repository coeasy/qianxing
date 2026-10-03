//! 单机可恢复文件任务队列（V10 §4.9 收敛进 `file` 目录模块）。
//!
//! 落盘走 [`JsonRecordStore`] 的共享原子替换路径，序列化 / 版本 / 损坏拒绝走
//! 统一信封（`state_envelope`）；租约首建的 `create_new` 抢占与 queue→done 的
//! 改名搬运是刻意保留的非原子替换点（并发抢占必须立即映射为 `LeaseHeld`；
//! 确认前任务不得离开 queue）。

use super::*;

/// 单机可恢复任务队列：用原子 JSON 文件模拟队列、租约和确认语义。
///
/// 该实现适合开发、单机 worker 和故障注入；生产多进程/多节点应替换为数据库或
/// 消息队列实现，但必须保持 `run_id` 幂等、租约过期接管和确认前不丢任务的语义。
#[derive(Clone, Debug)]
pub struct FileJobQueue {
    root: PathBuf,
}

struct ClaimLock {
    path: PathBuf,
}

impl Drop for ClaimLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

impl JsonRecordStore for FileJobQueue {
    fn records_dir(&self) -> PathBuf {
        self.root.join("queue")
    }
}

impl FileJobQueue {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn enqueue(
        &self,
        job: JobSpec,
        run: JobRun,
        enqueued_ts: u64,
    ) -> Result<PathBuf, StorageError> {
        let envelope = QueuedJob {
            job,
            run,
            enqueued_ts,
        };
        envelope.validate()?;
        let path = self.queue_path(envelope.run.run_id);
        if path.exists() {
            let existing: QueuedJob = read_state_json(&path)?;
            if existing.job == envelope.job && existing.run == envelope.run {
                return Ok(path);
            }
            return Err(StorageError::Conflict(format!(
                "run_id {} 已被不同任务占用",
                envelope.run.run_id
            )));
        }
        self.ensure_dirs()?;
        write_state_json(&self.root, &path, &envelope)?;
        Ok(path)
    }

    pub fn pending(&self) -> Result<Vec<QueuedJob>, StorageError> {
        let mut jobs = Vec::new();
        for path in self.list_records()? {
            let job: QueuedJob = read_state_json(&path)?;
            jobs.push(job);
        }
        jobs.sort_by_key(|job: &QueuedJob| (job.run.trading_day.clone(), job.run.run_id));
        Ok(jobs)
    }

    pub fn available(&self, now: u64) -> Result<Vec<QueuedJob>, StorageError> {
        let mut jobs = Vec::new();
        for job in self.pending()? {
            let lease_path = self.lease_path(job.run.run_id);
            let available = if !lease_path.exists() {
                true
            } else {
                read_state_json::<JobLease>(&lease_path)?.expires_ts <= now
            };
            if available {
                jobs.push(job);
            }
        }
        Ok(jobs)
    }

    pub fn claim(
        &self,
        run_id: u64,
        worker: &str,
        now: u64,
        lease_seconds: u64,
    ) -> Result<JobLease, StorageError> {
        if worker.trim().is_empty() || lease_seconds == 0 {
            return Err(StorageError::Conflict("worker 和租约时长不能为空".into()));
        }
        let queue_path = self.queue_path(run_id);
        self.ensure_dirs()?;
        let _claim_lock = self.acquire_claim_lock(run_id)?;
        if !queue_path.exists() {
            return Err(StorageError::NotFound(format!("run_id {run_id}")));
        }
        // 读取即触发信封校验（身份自洽），非法内容按损坏拒绝。
        read_state_json::<QueuedJob>(&queue_path)?;
        let lease_path = self.lease_path(run_id);
        let mut fencing_token = 1;
        if lease_path.exists() {
            let current: JobLease = read_state_json(&lease_path)?;
            if current.expires_ts > now && current.owner != worker {
                return Err(StorageError::LeaseHeld {
                    run_id,
                    owner: current.owner,
                });
            }
            if current.expires_ts <= now {
                fencing_token = current.fencing_token.saturating_add(1).max(1);
                self.delete_record(&lease_path)?;
            } else if current.owner == worker {
                let lease = JobLease {
                    run_id,
                    owner: worker.into(),
                    expires_ts: now.saturating_add(lease_seconds),
                    fencing_token: current.fencing_token.max(1),
                };
                write_state_json(&self.root, &lease_path, &lease)?;
                return Ok(lease);
            }
        }
        let lease = JobLease {
            run_id,
            owner: worker.into(),
            expires_ts: now.saturating_add(lease_seconds),
            fencing_token,
        };
        // 首建走 create_new（并发抢占要映射为 LeaseHeld），序列化仍复用信封实现。
        let content = encode_state_json(&lease)?;
        let mut file = std::fs::OpenOptions::new();
        file.write(true).create_new(true);
        let mut handle = file.open(&lease_path).map_err(|error| match error.kind() {
            std::io::ErrorKind::AlreadyExists => StorageError::LeaseHeld {
                run_id,
                owner: "concurrent-worker".into(),
            },
            _ => StorageError::Io(error.to_string()),
        })?;
        handle
            .write_all(content.as_bytes())
            .map_err(|error| StorageError::Io(error.to_string()))?;
        handle
            .sync_all()
            .map_err(|error| StorageError::Io(error.to_string()))?;
        Ok(lease)
    }

    pub fn ack(&self, run_id: u64, worker: &str) -> Result<PathBuf, StorageError> {
        let lease_path = self.lease_path(run_id);
        let lease: JobLease = read_state_json(&lease_path)?;
        if lease.owner != worker {
            return Err(StorageError::Unauthorized(format!(
                "worker {} 不能确认 worker {} 的任务",
                worker, lease.owner
            )));
        }
        self.ack_files(run_id, worker, lease.fencing_token)
    }

    /// 严格确认路径：校验 worker、fencing token 和逻辑时间，拒绝过期租约。
    pub fn ack_at(
        &self,
        run_id: u64,
        worker: &str,
        fencing_token: u64,
        now: u64,
    ) -> Result<PathBuf, StorageError> {
        let lease_path = self.lease_path(run_id);
        let lease: JobLease = read_state_json(&lease_path)?;
        if lease.expires_ts <= now {
            return Err(StorageError::LeaseExpired { run_id });
        }
        if lease.owner != worker || lease.fencing_token != fencing_token {
            return Err(StorageError::Unauthorized(format!(
                "worker {} 的租约 fencing token 无效",
                worker
            )));
        }
        self.ack_files(run_id, worker, fencing_token)
    }

    fn ack_files(
        &self,
        run_id: u64,
        worker: &str,
        fencing_token: u64,
    ) -> Result<PathBuf, StorageError> {
        let lease_path = self.lease_path(run_id);
        let lease: JobLease = read_state_json(&lease_path)?;
        if lease.owner != worker || lease.fencing_token != fencing_token {
            return Err(StorageError::Unauthorized(format!(
                "worker {} 不能确认当前租约",
                worker
            )));
        }
        let source = self.queue_path(run_id);
        let target = self.done_path(run_id);
        self.ensure_dirs()?;
        let _claim_lock = self.acquire_claim_lock(run_id)?;
        read_state_json::<QueuedJob>(&source)?;
        if target.exists() {
            std::fs::remove_file(&source).ok();
        } else {
            // queue→done 的搬运是改名而不是原子替换：确认前任务永远留在 queue。
            std::fs::rename(&source, &target)
                .map_err(|error| StorageError::Io(error.to_string()))?;
        }
        self.delete_record(&lease_path)?;
        Ok(target)
    }

    /// 返回已过期任务；保留旧租约直到下一次 `claim`，以便递增 fencing token。
    pub fn recover_expired(&self, now: u64) -> Result<Vec<u64>, StorageError> {
        let mut recovered = Vec::new();
        for path in list_json_files(&self.root.join("leases"))? {
            let lease: JobLease = read_state_json(&path)?;
            if lease.expires_ts <= now {
                recovered.push(lease.run_id);
            }
        }
        recovered.sort_unstable();
        Ok(recovered)
    }

    fn ensure_dirs(&self) -> Result<(), StorageError> {
        for dir in ["queue", "leases", "done", "locks"] {
            std::fs::create_dir_all(self.root.join(dir))
                .map_err(|error| StorageError::Io(error.to_string()))?;
        }
        Ok(())
    }

    pub(crate) fn queue_path(&self, run_id: u64) -> PathBuf {
        self.root.join("queue").join(format!("{run_id}.json"))
    }

    fn lease_path(&self, run_id: u64) -> PathBuf {
        self.root.join("leases").join(format!("{run_id}.json"))
    }

    fn acquire_claim_lock(&self, run_id: u64) -> Result<ClaimLock, StorageError> {
        let path = self.root.join("locks").join(format!("{run_id}.lock"));
        let result = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path);
        match result {
            Ok(_) => Ok(ClaimLock { path }),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                Err(StorageError::LeaseHeld {
                    run_id,
                    owner: "concurrent-lease-operation".into(),
                })
            }
            Err(error) => Err(StorageError::Io(error.to_string())),
        }
    }

    fn done_path(&self, run_id: u64) -> PathBuf {
        self.root.join("done").join(format!("{run_id}.json"))
    }
}

impl JobQueueBackend for FileJobQueue {
    fn enqueue_job(
        &self,
        job: JobSpec,
        run: JobRun,
        enqueued_ts: u64,
    ) -> Result<PathBuf, StorageError> {
        self.enqueue(job, run, enqueued_ts)
    }

    fn available_jobs(&self, now: u64) -> Result<Vec<QueuedJob>, StorageError> {
        self.available(now)
    }

    fn claim_job(
        &self,
        run_id: u64,
        worker: &str,
        now: u64,
        lease_seconds: u64,
    ) -> Result<JobLease, StorageError> {
        self.claim(run_id, worker, now, lease_seconds)
    }

    fn ack_job(&self, run_id: u64, worker: &str) -> Result<PathBuf, StorageError> {
        self.ack(run_id, worker)
    }

    fn ack_job_at(
        &self,
        run_id: u64,
        worker: &str,
        fencing_token: u64,
        now: u64,
    ) -> Result<PathBuf, StorageError> {
        self.ack_at(run_id, worker, fencing_token, now)
    }

    fn recover_expired_leases(&self, now: u64) -> Result<Vec<u64>, StorageError> {
        self.recover_expired(now)
    }
}
