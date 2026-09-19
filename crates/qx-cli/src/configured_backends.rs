//! 后端装配：按配置选择控制面状态后端、命令队列与作业队列，并解析 Postgres DSN。
//!
//! 由 `main.rs` 的 crate 根职责簇拆出（Phase 4p），条目经根部的
//! `pub(crate) use configured_backends::*;` 再导出，行为与拆分前逐字相同。

use super::*;

#[derive(Clone)]
pub(crate) enum ControlStateBackend {
    Files(JsonStateStore),
    #[cfg(feature = "sqlite")]
    Sqlite(SqliteControlStore),
    #[cfg(feature = "postgres")]
    Postgres(PostgresControlStore),
}

impl ControlStateBackend {
    pub(crate) fn load(&self) -> Result<ControlPlane, String> {
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

    pub(crate) fn transact<T, E, F>(
        &self,
        update: F,
    ) -> Result<(ControlPlane, Result<T, E>), String>
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

pub(crate) fn configured_control_store(
    config: &RuntimeConfig,
) -> Result<ControlStateBackend, String> {
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

pub(crate) fn configured_command_queue(
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

pub(crate) enum ConfiguredJobQueue {
    Files(FileJobQueue),
    #[cfg(feature = "sqlite")]
    Sqlite(SqliteJobQueue),
    #[cfg(feature = "postgres")]
    Postgres(PostgresJobQueue),
}

impl ConfiguredJobQueue {
    pub(crate) fn enqueue(
        &self,
        job: JobSpec,
        run: qx_scheduler::JobRun,
        enqueued_ts: u64,
    ) -> Result<(), StorageError> {
        match self {
            Self::Files(queue) => queue.enqueue(job, run, enqueued_ts).map(|_| ()),
            #[cfg(feature = "sqlite")]
            Self::Sqlite(queue) => queue.enqueue(job, run, enqueued_ts).map(|_| ()),
            #[cfg(feature = "postgres")]
            Self::Postgres(queue) => queue.enqueue(job, run, enqueued_ts).map(|_| ()),
        }
    }

    pub(crate) fn available(&self, now: u64) -> Result<Vec<QueuedJob>, StorageError> {
        match self {
            Self::Files(queue) => queue.available(now),
            #[cfg(feature = "sqlite")]
            Self::Sqlite(queue) => queue.available(now),
            #[cfg(feature = "postgres")]
            Self::Postgres(queue) => queue.available(now),
        }
    }

    pub(crate) fn claim(
        &self,
        run_id: u64,
        owner: &str,
        now: u64,
        lease_seconds: u64,
    ) -> Result<JobLease, StorageError> {
        match self {
            Self::Files(queue) => queue.claim(run_id, owner, now, lease_seconds),
            #[cfg(feature = "sqlite")]
            Self::Sqlite(queue) => queue.claim(run_id, owner, now, lease_seconds),
            #[cfg(feature = "postgres")]
            Self::Postgres(queue) => queue.claim(run_id, owner, now, lease_seconds),
        }
    }

    pub(crate) fn ack_at(
        &self,
        run_id: u64,
        owner: &str,
        fencing_token: u64,
        now: u64,
    ) -> Result<(), StorageError> {
        match self {
            Self::Files(queue) => queue.ack_at(run_id, owner, fencing_token, now).map(|_| ()),
            #[cfg(feature = "sqlite")]
            Self::Sqlite(queue) => queue.ack_at(run_id, owner, fencing_token, now).map(|_| ()),
            #[cfg(feature = "postgres")]
            Self::Postgres(queue) => queue.ack_at(run_id, owner, fencing_token, now).map(|_| ()),
        }
    }
}

pub(crate) fn runtime_path(root: &Path, configured: &str) -> std::path::PathBuf {
    let path = Path::new(configured);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    }
}

pub(crate) fn configured_job_queue(
    config: &RuntimeConfig,
    root: &Path,
) -> Result<ConfiguredJobQueue, String> {
    match config.storage.backend {
        StorageBackend::Files => Ok(ConfiguredJobQueue::Files(FileJobQueue::new(runtime_path(
            root,
            &config.scheduler.job_queue_path,
        )))),
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
                SqliteJobQueue::new(path)
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
                PostgresJobQueue::connect_with_pool_size(&dsn, config.storage.postgres_pool_size)
                    .map(ConfiguredJobQueue::Postgres)
                    .map_err(|error| format!("初始化 PostgreSQL JobQueue 失败: {error:?}"))
            }
        }
    }
}

#[cfg(feature = "postgres")]
pub(crate) fn postgres_dsn(config: &RuntimeConfig) -> Result<String, String> {
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

pub(crate) fn configured_postgres_dsn(config: &RuntimeConfig) -> Result<Option<String>, String> {
    if config.storage.backend != StorageBackend::Postgres {
        return Ok(None);
    }
    #[cfg(not(feature = "postgres"))]
    {
        Err("当前 qx-cli 未启用 postgres feature，无法初始化 PostgreSQL EventLog".into())
    }
    #[cfg(feature = "postgres")]
    {
        postgres_dsn(config).map(Some)
    }
}
