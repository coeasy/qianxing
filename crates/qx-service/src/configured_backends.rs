//! 后端装配：按配置选择控制面状态后端、命令队列与作业队列，并解析 Postgres DSN。
//!
//! RuntimeConfig-driven storage backend selection for service hosts and workers.

use std::path::Path;
use std::sync::Arc;

use qx_control::ControlPlane;
use qx_runtime::{load_control_state, RuntimeConfig, StorageBackend};
use qx_scheduler::JobSpec;
use qx_storage::{
    ControlCommandQueue, ControlCommandQueueBackend, FileJobQueue, JobLease, JobQueueBackend,
    JsonStateStore, QueuedJob, StorageError,
};
#[cfg(feature = "postgres")]
use qx_storage::{PostgresControlCommandQueue, PostgresControlStore, PostgresJobQueue};
#[cfg(feature = "sqlite")]
use qx_storage::{SqliteControlCommandQueue, SqliteControlStore, SqliteJobQueue};
use std::path::PathBuf;

#[derive(Clone)]
pub enum ControlStateBackend {
    Files(JsonStateStore),
    #[cfg(feature = "sqlite")]
    Sqlite(SqliteControlStore),
    #[cfg(feature = "postgres")]
    Postgres(PostgresControlStore),
}

impl ControlStateBackend {
    pub fn load(&self) -> Result<ControlPlane, String> {
        match self {
            Self::Files(store) => load_control_state(store.root()),
            #[cfg(feature = "sqlite")]
            Self::Sqlite(store) => store
                .load_if_exists()
                .map_err(|error| format!("读取 SQLite 控制面失败: {error:?}"))
                .map(|state| state.unwrap_or_default()),
            #[cfg(feature = "postgres")]
            Self::Postgres(store) => store
                .load_if_exists()
                .map_err(|error| format!("读取 PostgreSQL 控制面失败: {error:?}"))
                .map(|state| state.unwrap_or_default()),
        }
    }

    pub fn transact<T, E, F>(&self, update: F) -> Result<(ControlPlane, Result<T, E>), String>
    where
        F: FnOnce(&mut ControlPlane) -> Result<T, E>,
    {
        match self {
            Self::Files(store) => store
                .transact_control(update)
                .map_err(|error| format!("文件控制面事务失败: {error:?}")),
            #[cfg(feature = "sqlite")]
            Self::Sqlite(store) => store
                .transact_control(update)
                .map_err(|error| format!("SQLite 控制面事务失败: {error:?}")),
            #[cfg(feature = "postgres")]
            Self::Postgres(store) => store
                .transact_control(update)
                .map_err(|error| format!("PostgreSQL 控制面事务失败: {error:?}")),
        }
    }
}

pub fn configured_control_store(config: &RuntimeConfig) -> Result<ControlStateBackend, String> {
    let root = Path::new(&config.storage.data_dir).to_path_buf();
    match config.storage.backend {
        StorageBackend::Files => Ok(ControlStateBackend::Files(JsonStateStore::new(root))),
        StorageBackend::Sqlite => {
            #[cfg(not(feature = "sqlite"))]
            {
                let _ = root;
                Err(
                    "当前 qx-cli 未启用 sqlite feature；请使用 --features sqlite 启动生产配置"
                        .into(),
                )
            }
            #[cfg(feature = "sqlite")]
            {
                let path = config
                    .storage
                    .sqlite_path
                    .as_deref()
                    .ok_or_else(|| "SQLite backend 缺少 sqlite_path".to_string())?;
                SqliteControlStore::new(path)
                    .map(ControlStateBackend::Sqlite)
                    .map_err(|error| format!("初始化 SQLite 控制面失败: {error:?}"))
            }
        }
        StorageBackend::Postgres => {
            #[cfg(not(feature = "postgres"))]
            {
                let _ = root;
                Err(
                    "当前 qx-cli 未启用 postgres feature；请使用 --features postgres 启动 PostgreSQL 配置"
                        .into(),
                )
            }
            #[cfg(feature = "postgres")]
            {
                let dsn = postgres_dsn(config)?;
                PostgresControlStore::connect_with_pool_size(
                    &dsn,
                    config.storage.postgres_pool_size,
                )
                .map(ControlStateBackend::Postgres)
                .map_err(|error| format!("初始化 PostgreSQL 控制面失败: {error:?}"))
            }
        }
    }
}

pub fn configured_command_queue(
    config: &RuntimeConfig,
    root: &Path,
) -> Result<Arc<dyn ControlCommandQueueBackend>, String> {
    match config.storage.backend {
        StorageBackend::Files => Ok(Arc::new(ControlCommandQueue::new(
            root.join("control-queue"),
        ))),
        StorageBackend::Sqlite => {
            #[cfg(not(feature = "sqlite"))]
            {
                let _ = root;
                Err("当前 qx-cli 未启用 sqlite feature，无法初始化 SQLite 控制命令队列".into())
            }
            #[cfg(feature = "sqlite")]
            {
                let path = config
                    .storage
                    .sqlite_path
                    .as_deref()
                    .ok_or_else(|| "SQLite backend 缺少 sqlite_path".to_string())?;
                SqliteControlCommandQueue::new(path)
                    .map(|queue| Arc::new(queue) as Arc<dyn ControlCommandQueueBackend>)
                    .map_err(|error| format!("初始化 SQLite 控制命令队列失败: {error:?}"))
            }
        }
        StorageBackend::Postgres => {
            #[cfg(not(feature = "postgres"))]
            {
                let _ = root;
                Err(
                    "当前 qx-cli 未启用 postgres feature，无法初始化 PostgreSQL 控制命令队列"
                        .into(),
                )
            }
            #[cfg(feature = "postgres")]
            {
                let dsn = postgres_dsn(config)?;
                PostgresControlCommandQueue::connect_with_pool_size(
                    &dsn,
                    config.storage.postgres_pool_size,
                )
                .map(|queue| Arc::new(queue) as Arc<dyn ControlCommandQueueBackend>)
                .map_err(|error| format!("初始化 PostgreSQL 控制命令队列失败: {error:?}"))
            }
        }
    }
}

pub enum ConfiguredJobQueue {
    Files(FileJobQueue),
    #[cfg(feature = "sqlite")]
    Sqlite(SqliteJobQueue),
    #[cfg(feature = "postgres")]
    Postgres(PostgresJobQueue),
}

impl ConfiguredJobQueue {
    pub fn enqueue(
        &self,
        job: JobSpec,
        run: qx_scheduler::JobRun,
        enqueued_ts: u64,
    ) -> Result<(), StorageError> {
        JobQueueBackend::enqueue_job(self, job, run, enqueued_ts).map(|_| ())
    }

    pub fn available(&self, now: u64) -> Result<Vec<QueuedJob>, StorageError> {
        JobQueueBackend::available_jobs(self, now)
    }

    pub fn available_for_owner(
        &self,
        now: u64,
        worker_id: &str,
        limit: usize,
    ) -> Result<Vec<QueuedJob>, StorageError> {
        JobQueueBackend::available_jobs_for_owner(self, now, worker_id, limit)
    }

    pub fn claim(
        &self,
        run_id: u64,
        owner: &str,
        now: u64,
        lease_seconds: u64,
    ) -> Result<JobLease, StorageError> {
        JobQueueBackend::claim_job(self, run_id, owner, now, lease_seconds)
    }

    pub fn ack_at(
        &self,
        run_id: u64,
        owner: &str,
        fencing_token: u64,
        now: u64,
    ) -> Result<(), StorageError> {
        JobQueueBackend::ack_job_at(self, run_id, owner, fencing_token, now).map(|_| ())
    }
}

impl JobQueueBackend for ConfiguredJobQueue {
    fn enqueue_job(
        &self,
        job: JobSpec,
        run: qx_scheduler::JobRun,
        enqueued_ts: u64,
    ) -> Result<PathBuf, StorageError> {
        match self {
            Self::Files(queue) => JobQueueBackend::enqueue_job(queue, job, run, enqueued_ts),
            #[cfg(feature = "sqlite")]
            Self::Sqlite(queue) => JobQueueBackend::enqueue_job(queue, job, run, enqueued_ts),
            #[cfg(feature = "postgres")]
            Self::Postgres(queue) => JobQueueBackend::enqueue_job(queue, job, run, enqueued_ts),
        }
    }

    fn available_jobs(&self, now: u64) -> Result<Vec<QueuedJob>, StorageError> {
        match self {
            Self::Files(queue) => JobQueueBackend::available_jobs(queue, now),
            #[cfg(feature = "sqlite")]
            Self::Sqlite(queue) => JobQueueBackend::available_jobs(queue, now),
            #[cfg(feature = "postgres")]
            Self::Postgres(queue) => JobQueueBackend::available_jobs(queue, now),
        }
    }

    fn available_jobs_for_owner(
        &self,
        now: u64,
        worker_id: &str,
        limit: usize,
    ) -> Result<Vec<QueuedJob>, StorageError> {
        match self {
            Self::Files(queue) => {
                JobQueueBackend::available_jobs_for_owner(queue, now, worker_id, limit)
            }
            #[cfg(feature = "sqlite")]
            Self::Sqlite(queue) => {
                JobQueueBackend::available_jobs_for_owner(queue, now, worker_id, limit)
            }
            #[cfg(feature = "postgres")]
            Self::Postgres(queue) => {
                JobQueueBackend::available_jobs_for_owner(queue, now, worker_id, limit)
            }
        }
    }

    fn claim_job(
        &self,
        run_id: u64,
        worker: &str,
        now: u64,
        lease_seconds: u64,
    ) -> Result<JobLease, StorageError> {
        match self {
            Self::Files(queue) => {
                JobQueueBackend::claim_job(queue, run_id, worker, now, lease_seconds)
            }
            #[cfg(feature = "sqlite")]
            Self::Sqlite(queue) => {
                JobQueueBackend::claim_job(queue, run_id, worker, now, lease_seconds)
            }
            #[cfg(feature = "postgres")]
            Self::Postgres(queue) => {
                JobQueueBackend::claim_job(queue, run_id, worker, now, lease_seconds)
            }
        }
    }

    fn ack_job_at(
        &self,
        run_id: u64,
        worker: &str,
        fencing_token: u64,
        now: u64,
    ) -> Result<PathBuf, StorageError> {
        match self {
            Self::Files(queue) => {
                JobQueueBackend::ack_job_at(queue, run_id, worker, fencing_token, now)
            }
            #[cfg(feature = "sqlite")]
            Self::Sqlite(queue) => {
                JobQueueBackend::ack_job_at(queue, run_id, worker, fencing_token, now)
            }
            #[cfg(feature = "postgres")]
            Self::Postgres(queue) => {
                JobQueueBackend::ack_job_at(queue, run_id, worker, fencing_token, now)
            }
        }
    }

    fn recover_expired_leases(&self, now: u64) -> Result<Vec<u64>, StorageError> {
        match self {
            Self::Files(queue) => JobQueueBackend::recover_expired_leases(queue, now),
            #[cfg(feature = "sqlite")]
            Self::Sqlite(queue) => JobQueueBackend::recover_expired_leases(queue, now),
            #[cfg(feature = "postgres")]
            Self::Postgres(queue) => JobQueueBackend::recover_expired_leases(queue, now),
        }
    }
}

pub fn runtime_path(root: &Path, configured: &str) -> std::path::PathBuf {
    let path = Path::new(configured);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    }
}

pub fn configured_job_queue(
    config: &RuntimeConfig,
    root: &Path,
) -> Result<ConfiguredJobQueue, String> {
    match config.storage.backend {
        StorageBackend::Files => Ok(ConfiguredJobQueue::Files(
            FileJobQueue::with_pending_limit(
                runtime_path(root, &config.scheduler.job_queue_path),
                config.scheduler.max_pending_jobs,
            )
            .map_err(|error| format!("初始化有界文件 JobQueue 失败: {error:?}"))?,
        )),
        StorageBackend::Sqlite => {
            #[cfg(not(feature = "sqlite"))]
            {
                let _ = root;
                Err("当前 qx-cli 未启用 sqlite feature，无法初始化 SQLite JobQueue".into())
            }
            #[cfg(feature = "sqlite")]
            {
                let path = config
                    .storage
                    .sqlite_path
                    .as_deref()
                    .ok_or_else(|| "SQLite backend 缺少 sqlite_path".to_string())?;
                SqliteJobQueue::with_pending_limit(path, config.scheduler.max_pending_jobs)
                    .map(ConfiguredJobQueue::Sqlite)
                    .map_err(|error| format!("初始化 SQLite JobQueue 失败: {error:?}"))
            }
        }
        StorageBackend::Postgres => {
            #[cfg(not(feature = "postgres"))]
            {
                let _ = root;
                Err("当前 qx-cli 未启用 postgres feature，无法初始化 PostgreSQL JobQueue".into())
            }
            #[cfg(feature = "postgres")]
            {
                let dsn = postgres_dsn(config)?;
                PostgresJobQueue::connect_with_pool_size_and_pending_limit(
                    &dsn,
                    config.storage.postgres_pool_size,
                    config.scheduler.max_pending_jobs,
                )
                .map(ConfiguredJobQueue::Postgres)
                .map_err(|error| format!("初始化 PostgreSQL JobQueue 失败: {error:?}"))
            }
        }
    }
}

#[cfg(feature = "postgres")]
pub fn postgres_dsn(config: &RuntimeConfig) -> Result<String, String> {
    let env_name = config
        .storage
        .postgres_dsn_env
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| "PostgreSQL backend 缺少 postgres_dsn_env".to_string())?;
    std::env::var(env_name).map_err(|error| {
        format!(
            "PostgreSQL DSN 环境变量 {} 不可用；凭证不能写入运行时配置: {}",
            env_name, error
        )
    })
}

#[cfg(all(feature = "nats", feature = "postgres"))]
pub fn configured_postgres_dsn(config: &RuntimeConfig) -> Result<Option<String>, String> {
    if config.storage.backend != StorageBackend::Postgres {
        return Ok(None);
    }
    postgres_dsn(config).map(Some)
}

#[cfg(test)]
mod job_queue_backend_tests {
    use super::*;
    use qx_scheduler::{JobRun, JobStatus, JobWindow, RetryPolicy, Trigger};
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn configured_file_queue_dispatches_the_production_trait_contract() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target")
            .join(format!(
                "qx-service-job-queue-contract-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .expect("system clock must be after unix epoch")
                    .as_nanos()
            ));
        let queue = ConfiguredJobQueue::Files(FileJobQueue::new(root.clone()));
        let backend: &dyn JobQueueBackend = &queue;
        let job = JobSpec {
            job_id: "trait-contract".into(),
            job_version: "v1".into(),
            owner: "contract-test".into(),
            enabled: true,
            trigger: Trigger::Manual,
            window: JobWindow::Any,
            depends_on: Vec::new(),
            input_refs: Vec::new(),
            output_refs: Vec::new(),
            timeout_seconds: 60,
            retry_policy: RetryPolicy::default(),
            concurrency_key: "trait-contract".into(),
            idempotency_key: "trait-contract-daily".into(),
            permission_scope: "research".into(),
            audit_reason: "production trait dispatch".into(),
            dry_run: true,
        };
        let run = JobRun {
            run_id: job.stable_key("20260911"),
            job_id: job.job_id.clone(),
            trading_day: "20260911".into(),
            attempt: 1,
            status: JobStatus::Running,
            manifest_digest: Some(7),
            error_code: None,
            next_retry_ts: None,
            started_ts: 10,
            deadline_ts: 70,
        };
        let run_id = run.run_id;

        backend.enqueue_job(job, run, 10).unwrap();
        let available = backend.available_jobs(10).unwrap();
        assert_eq!(available.len(), 1);
        assert_eq!(available[0].run.run_id, run_id);

        let lease = backend
            .claim_job(run_id, "contract-worker", 10, 10)
            .unwrap();
        assert_eq!(lease.fencing_token, 1);
        backend
            .ack_job_at(run_id, "contract-worker", lease.fencing_token, 11)
            .unwrap();
        assert!(backend.recover_expired_leases(11).unwrap().is_empty());
        drop(queue);
        std::fs::remove_dir_all(root).unwrap();
    }
}
