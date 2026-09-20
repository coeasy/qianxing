//! 运行时装配：EventLog、控制面、命令队列、PostgreSQL/Files 后端与 API 读模型。
//!
//! 模块只负责“按 RuntimeConfig 组装既有组件”，不产生第二套订单或账簿语义。

use super::*;

pub(crate) fn read_runtime_config(path: &Path) -> Result<RuntimeConfig, String> {
    let payload = std::fs::read_to_string(path)
        .map_err(|error| format!("读取运行时配置失败 {}: {error}", path.display()))?;
    RuntimeConfig::from_json(&payload)
}

#[derive(Clone, Debug)]
pub(crate) struct PipelineStorage {
    pub(crate) root: PathBuf,
    segment_events: Option<usize>,
    postgres_dsn: Option<String>,
    #[cfg(feature = "postgres")]
    postgres_pool_size: usize,
}

impl PipelineStorage {
    pub(crate) fn from_config(config: &RuntimeConfig) -> Result<Self, String> {
        Ok(Self {
            root: Path::new(&config.storage.data_dir).to_path_buf(),
            segment_events: config.storage.event_log_segment_events,
            postgres_dsn: configured_postgres_dsn(config)?,
            #[cfg(feature = "postgres")]
            postgres_pool_size: config.storage.postgres_pool_size,
        })
    }

    pub(crate) fn open(
        &self,
        log_name: impl Into<String>,
        currency: impl Into<String>,
    ) -> Result<LiveEventPipeline, String> {
        if let Some(dsn) = self.postgres_dsn.as_deref() {
            #[cfg(not(feature = "postgres"))]
            {
                let _ = dsn;
                return Err(
                    "当前 qx-cli 未启用 postgres feature，无法打开 PostgreSQL EventLog".into(),
                );
            }
            #[cfg(feature = "postgres")]
            {
                return LiveEventPipeline::open_postgres_with_pool_size(
                    dsn,
                    self.postgres_pool_size,
                    log_name,
                    currency,
                )
                .map_err(|error| format!("打开 PostgreSQL EventLog 失败: {error:?}"));
            }
        }
        LiveEventPipeline::open_configured(
            self.root.clone(),
            log_name,
            currency,
            self.segment_events,
        )
        .map_err(|error| format!("打开运行时 EventLog 失败: {error:?}"))
    }
}

pub(crate) fn open_runtime_pipeline(
    config: &RuntimeConfig,
    root: &Path,
    log_name: impl Into<String>,
    currency: impl Into<String>,
) -> Result<LiveEventPipeline, String> {
    let storage = PipelineStorage::from_config(config)?;
    let mut storage = storage;
    storage.root = root.to_path_buf();
    storage.open(log_name, currency)
}

pub(crate) fn event_log_exists(
    config: &RuntimeConfig,
    root: &Path,
    log_name: &str,
) -> Result<bool, String> {
    if config.storage.backend == StorageBackend::Postgres {
        #[cfg(not(feature = "postgres"))]
        return Err("当前 qx-cli 未启用 postgres feature；无法查询 PostgreSQL EventLog".into());
        #[cfg(feature = "postgres")]
        {
            let dsn = postgres_dsn(config)?;
            let store = PostgresEventLogStore::connect_with_pool_size(
                &dsn,
                config.storage.postgres_pool_size,
            )
            .map_err(|error| format!("连接 PostgreSQL EventLog 失败: {error:?}"))?;
            return store
                .read_if_exists(log_name)
                .map(|value| value.is_some())
                .map_err(|error| format!("查询 PostgreSQL EventLog 失败: {error:?}"));
        }
    }
    Ok(root.join(format!("{log_name}.json")).exists()
        || (config.storage.event_log_segment_events.is_some()
            && root.join(format!("{log_name}.manifest.json")).exists()))
}

pub(crate) fn runtime_timestamp_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

pub(crate) fn validate_research_snapshot_binding(
    strategy: &StrategyRuntimeConfig,
    research: &StrategyResearchSnapshot,
) -> Result<(), String> {
    if let Some(expected) = strategy.research_data_fingerprint.as_deref() {
        let actual = research.candidate.config.data_fingerprint.as_str();
        if actual != expected {
            return Err(format!(
                "research snapshot data_fingerprint 不匹配: expected={expected} actual={actual}"
            ));
        }
    }
    Ok(())
}

pub(crate) fn non_empty_env(name: &str) -> bool {
    !name.trim().is_empty() && std::env::var(name).is_ok_and(|value| !value.trim().is_empty())
}

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

fn build_configured_api_service(
    config: &RuntimeConfig,
    runtime_config_path: &Path,
) -> Result<ApiService, String> {
    std::fs::create_dir_all(&config.storage.data_dir)
        .map_err(|error| format!("创建运行时 data_dir 失败: {error}"))?;
    let control_root = Path::new(&config.storage.data_dir).to_path_buf();
    let control_store = configured_control_store(config)?;
    let control = control_store.load()?;
    let command_queue = configured_command_queue(config, &control_root)?;
    let mut state = ApiState::default();
    state.control = control;
    let (job_runs, ledger_entries, reconcile_reports) = load_api_query_models(config)?;
    state.job_runs = job_runs;
    state.ledger_entries = ledger_entries;
    state.reconcile_reports = reconcile_reports
        .into_iter()
        .map(|report| (report.worker_id.clone(), report))
        .collect();
    for (account_id, venue_id, log_name, currency) in configured_account_event_logs(config) {
        let root = Path::new(&config.storage.data_dir);
        if event_log_exists(config, root, &log_name)? {
            let pipeline = open_runtime_pipeline(config, root, log_name, currency)
                .map_err(|error| format!("读取 API 事件投影失败: {error}"))?;
            state
                .project_account_event_log(&account_id, &venue_id, pipeline.log())
                .map_err(|error| format!("初始化 API 账户事件投影失败: {error}"))?;
        }
    }
    for snapshot in load_api_account_snapshots(config)? {
        state
            .publish_snapshot(snapshot)
            .map_err(|error| format!("装载 API 账户查询快照失败: {error}"))?;
    }
    let mut policy = ApiPolicy::new();
    for (operator_id, operator) in &config.api.operators {
        policy = policy.grant(operator_id.clone(), operator.permission);
    }
    let metrics_dir = worker_metrics_dir(config);
    let worker_metrics_stale_after_ms = config.messaging.worker_stale_after_ms;
    let readiness_store = control_store.clone();
    let readiness_config = config.clone();
    let readiness_runtime_config_path = runtime_config_path.to_path_buf();
    let readiness_metrics_dir = metrics_dir.clone();
    let service = if config.api.operators.is_empty() {
        ApiService::new(state)
    } else {
        ApiService::with_policy(state, policy)
    }
    .with_worker_metrics_provider(move || {
        read_worker_metrics(
            &metrics_dir,
            runtime_timestamp_ms(),
            worker_metrics_stale_after_ms,
        )
    })
    .with_readiness_provider(move || {
        configured_api_readiness(
            &readiness_config,
            &readiness_runtime_config_path,
            &readiness_store,
            &readiness_metrics_dir,
            runtime_timestamp_ms(),
            worker_metrics_stale_after_ms,
        )
    })
    .with_control_submitter({
        let store = control_store.clone();
        move |command, granted, ts| {
            if matches!(&command.kind, CommandKind::SubmitOrder) {
                order_from_submit_command(&command).map_err(|error| {
                    ControlSubmitError::Rejected(qx_control::ControlError::Invalid(format!(
                        "SubmitOrder 载荷非法: {error:?}"
                    )))
                })?;
            }
            let (plane, result) = store
                .transact(|plane| {
                    plane
                        .submit_as(command, granted, ts)
                        .map_err(ControlSubmitError::Rejected)
                })
                .map_err(ControlSubmitError::Unavailable)?;
            result.map(|audit| (plane, audit))
        }
    })
    .with_command_enqueuer({
        let queue = Arc::clone(&command_queue);
        move |command, ts| {
            queue
                .enqueue_command(command, ts)
                .map(|_| ())
                .map_err(|error| format!("写入控制命令队列失败: {error:?}"))
        }
    });
    if config.storage.backend == StorageBackend::Sqlite {
        #[cfg(not(feature = "sqlite"))]
        return Err("当前 qx-cli 未启用 sqlite feature；请使用 cargo run -p qx-cli --features sqlite -- serve ...".into());
        #[cfg(feature = "sqlite")]
        {
            let path = config
                .storage
                .sqlite_path
                .as_deref()
                .ok_or_else(|| "SQLite backend 缺少 sqlite_path".to_string())?;
            let bucket = SqliteTokenBucket::new(path, "api", 100, 100)
                .map_err(|error| format!("初始化 SQLite API 限流失败: {error:?}"))?;
            let service = service.with_sqlite_rate_limit(bucket);
            return Ok(service);
        }
    }
    Ok(service)
}

/// 从持久化调度状态、账户 EventLog 和对账报告构造 API 读模型。
///
/// 这些数据只进入 QueryPort，不会被 API 反向写入 Scheduler、Ledger 或
/// Reconcile worker，避免查询层成为第二个事实拥有者。
type ApiQueryModels = (
    Vec<qx_scheduler::JobRun>,
    Vec<qx_core::LedgerEntry>,
    Vec<ReconcileReportSnapshot>,
);

fn load_api_query_models(config: &RuntimeConfig) -> Result<ApiQueryModels, String> {
    let root = Path::new(&config.storage.data_dir);
    let job_runs = {
        let state_path = runtime_path(root, &config.scheduler.state_path);
        if state_path.exists() {
            JsonStateStore::new(root)
                .load_scheduler_at(Path::new(&config.scheduler.state_path))
                .map_err(|error| format!("读取 API Scheduler 读模型失败: {error:?}"))?
                .runs()
        } else {
            Vec::new()
        }
    };

    let mut ledger_entries = Vec::new();
    if let Some(worker) = config.workers.iter().find(|worker| {
        worker.enabled
            && matches!(
                worker.role,
                WorkerRole::UserStream
                    | WorkerRole::Execution
                    | WorkerRole::SpreadRecovery
                    | WorkerRole::Reconciler
            )
            && worker.account_id.is_some()
            && worker.venue_id.is_some()
    }) {
        if let (Some(account_id), Some(venue_id)) =
            (worker.account_id.as_deref(), worker.venue_id.as_deref())
        {
            if let Some(log_name) = account_event_log_name(account_id, venue_id) {
                if event_log_exists(config, root, &log_name)? {
                    let pipeline = open_runtime_pipeline(config, root, log_name, "USDT")
                        .map_err(|error| format!("读取 API Ledger 读模型失败: {error}"))?;
                    ledger_entries = pipeline.ledger().entries().to_vec();
                }
            }
        }
    }

    let mut reconcile_reports = Vec::new();
    let report_root = root.join("reconcile");
    if report_root.exists() {
        for entry in std::fs::read_dir(&report_root)
            .map_err(|error| format!("读取 API 对账报告目录失败: {error}"))?
        {
            let path = entry
                .map_err(|error| format!("读取 API 对账报告目录项失败: {error}"))?
                .path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let report: ReconcileReportSnapshot = serde_json::from_str(
                &std::fs::read_to_string(&path)
                    .map_err(|error| format!("读取对账报告失败 {}: {error}", path.display()))?,
            )
            .map_err(|error| format!("对账报告 JSON 无效 {}: {error}", path.display()))?;
            report
                .validate()
                .map_err(|error| format!("对账报告校验失败 {}: {error}", path.display()))?;
            reconcile_reports.push(report);
        }
        reconcile_reports.sort_by(|left, right| left.worker_id.cmp(&right.worker_id));
    }
    Ok((job_runs, ledger_entries, reconcile_reports))
}

/// 从已持久化的账户 EventLog 构造 API 查询快照。
///
/// 该快照只是 QueryPort 的读模型：所有余额、持仓、订单和成交仍由 EventLog
/// 重放得到，不会反向写入 Ledger，也不会把柜台观察当成交易事实。
fn load_api_account_snapshots(config: &RuntimeConfig) -> Result<Vec<AccountSnapshot>, String> {
    let mut seen = BTreeSet::new();
    let mut snapshots = Vec::new();
    for worker in config.workers.iter().filter(|worker| {
        worker.enabled
            && matches!(
                worker.role,
                WorkerRole::UserStream
                    | WorkerRole::Execution
                    | WorkerRole::SpreadRecovery
                    | WorkerRole::Reconciler
            )
            && worker.account_id.is_some()
            && worker.venue_id.is_some()
    }) {
        let key = (
            worker.account_id.clone().unwrap_or_default(),
            worker.venue_id.clone().unwrap_or_default(),
        );
        if !seen.insert(key) {
            continue;
        }
        if let Some(snapshot) = load_api_account_snapshot_for_worker(config, worker)? {
            snapshots.push(snapshot);
        }
    }
    Ok(snapshots)
}

/// 保留单快照兼容入口给旧 CLI 校验；生产 API 启动使用上面的多账户入口。
#[cfg(test)]
pub(crate) fn load_api_account_snapshot(
    config: &RuntimeConfig,
) -> Result<Option<AccountSnapshot>, String> {
    for worker in config.workers.iter().filter(|worker| {
        worker.enabled
            && matches!(
                worker.role,
                WorkerRole::UserStream
                    | WorkerRole::Execution
                    | WorkerRole::SpreadRecovery
                    | WorkerRole::Reconciler
            )
            && worker.account_id.is_some()
            && worker.venue_id.is_some()
    }) {
        if let Some(snapshot) = load_api_account_snapshot_for_worker(config, worker)? {
            return Ok(Some(snapshot));
        }
    }
    Ok(None)
}

fn load_api_account_snapshot_for_worker(
    config: &RuntimeConfig,
    worker: &WorkerConfig,
) -> Result<Option<AccountSnapshot>, String> {
    let account_id = worker.account_id.as_deref().unwrap_or_default();
    let venue_id = worker.venue_id.as_deref().unwrap_or_default();
    let Some(log_name) = account_event_log_name(account_id, venue_id) else {
        return Ok(None);
    };
    let root = Path::new(&config.storage.data_dir);
    if !event_log_exists(config, root, &log_name)? {
        return Ok(None);
    }
    let pipeline = open_runtime_pipeline(config, root, log_name, "USDT")
        .map_err(|error| format!("打开 API 账户 EventLog 失败: {error}"))?;
    let runtime_snapshot = pipeline.snapshot();
    let as_of = runtime_snapshot.last_engine_ts.max(1);
    let mut snapshot = AccountSnapshot::new(1, account_id, "default", venue_id, as_of);
    snapshot.header.event_seq = pipeline
        .log()
        .events()
        .last()
        .map(|event| event.seq)
        .unwrap_or(0);
    snapshot.cash_raw = pipeline.ledger().cash_balances_for(account_id);
    snapshot.equity_raw = pipeline
        .ledger()
        .equity_for(account_id, pipeline.marks(), "USDT")
        .unwrap_or_else(|| pipeline.ledger().cash_for(account_id, "USDT"));
    snapshot.available_raw = snapshot.equity_raw;
    snapshot.orders = runtime_snapshot
        .orders
        .iter()
        .map(|order| {
            (
                order.client_id,
                qx_protocol::OrderSnapshot {
                    order_id: order.client_id,
                    client_order_id: order.client_id,
                    instrument: order.instrument.clone(),
                    side: order.side,
                    quantity_raw: order.qty.raw(),
                    filled_raw: order.filled.raw(),
                    status: order.status,
                },
            )
        })
        .collect();
    snapshot.fills = pipeline
        .log()
        .events()
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::Filled { fill } => Some((
                event.seq,
                qx_protocol::FillSnapshot {
                    fill_id: event.seq,
                    order_id: fill.order_id,
                    quantity_raw: fill.qty.raw(),
                    price_raw: fill.price.raw(),
                    fee_raw: fill.fee.raw(),
                    ts: fill.ts,
                },
            )),
            _ => None,
        })
        .collect();

    let mut instruments = BTreeSet::new();
    instruments.extend(
        worker
            .symbols
            .iter()
            .filter_map(|symbol| InstrumentId::parse(symbol)),
    );
    instruments.extend(
        snapshot
            .orders
            .values()
            .map(|order| order.instrument.clone()),
    );
    instruments.extend(
        runtime_snapshot
            .account_positions
            .get(&(account_id.to_string(), venue_id.to_string()))
            .into_iter()
            .flat_map(|positions| positions.iter().map(|position| position.instrument.clone())),
    );
    for instrument in instruments {
        let position = pipeline.ledger().position_for(account_id, &instrument);
        let venue_position = runtime_snapshot
            .account_positions
            .get(&(account_id.to_string(), venue_id.to_string()))
            .and_then(|positions| positions.iter().find(|item| item.instrument == instrument));
        let quantity_raw = venue_position
            .map(|position| position.quantity.raw())
            .unwrap_or_else(|| position.quantity.raw());
        if quantity_raw == 0 && venue_position.is_none() {
            continue;
        }
        let average_price_raw = venue_position
            .and_then(|position| position.average_price)
            .map(|price| price.raw())
            .unwrap_or_else(|| position.average_entry.raw());
        let mark_price_raw = venue_position
            .and_then(|position| position.mark_price)
            .or_else(|| pipeline.marks().get(&instrument).copied())
            .map(|price| price.raw())
            .unwrap_or(0);
        snapshot.positions.insert(
            instrument.clone(),
            qx_protocol::PositionSnapshot {
                instrument,
                quantity_raw,
                today_quantity_raw: quantity_raw,
                average_price_raw,
                mark_price_raw,
                unrealized_pnl_raw: venue_position
                    .map(|position| position.unrealized_pnl.raw())
                    .unwrap_or(0),
                margin_raw: venue_position
                    .map(|position| position.initial_margin.raw())
                    .unwrap_or(0),
            },
        );
    }
    snapshot.reconcile.recovery_state = "eventlog-replayed".into();
    Ok(Some(snapshot))
}

pub(crate) fn account_event_log_name(account_id: &str, venue_id: &str) -> Option<String> {
    let venue = venue_id.trim().to_ascii_lowercase();
    if venue.is_empty() {
        return None;
    }
    let prefix = if venue == "paper" {
        "paper"
    } else if venue.contains("binance") {
        "binance"
    } else {
        "ccxt"
    };
    Some(format!("{prefix}-{account_id}-{venue_id}-events"))
}

/// 返回所有启用的账户级运行时 EventLog。相同 account/venue 可能同时由
/// user-stream、execution、reconciler 等 worker 使用，但 API 只能建立一个
/// 隔离投影，避免多个 worker 重复写同一游标。
fn configured_account_event_logs(config: &RuntimeConfig) -> Vec<(String, String, String, String)> {
    let keys = config
        .workers
        .iter()
        .filter(|worker| {
            worker.enabled
                && matches!(
                    worker.role,
                    WorkerRole::UserStream
                        | WorkerRole::Execution
                        | WorkerRole::SpreadRecovery
                        | WorkerRole::Reconciler
                )
                && worker.account_id.is_some()
                && worker.venue_id.is_some()
        })
        .filter_map(|worker| {
            Some((
                worker.account_id.as_deref()?.to_string(),
                worker.venue_id.as_deref()?.to_string(),
            ))
        })
        .collect::<BTreeSet<_>>();
    keys.into_iter()
        .filter_map(|(account_id, venue_id)| {
            Some((
                account_id.clone(),
                venue_id.clone(),
                account_event_log_name(&account_id, &venue_id)?,
                "USDT".into(),
            ))
        })
        .collect()
}

/// 在 API 服务旁启动只读投影桥。它只读取 Runtime EventLog，调用 API 的
/// `project_event_log` 更新查询/订阅读模型，不拥有订单、账本或外部副作用。
fn spawn_api_projection_bridge(
    config: &RuntimeConfig,
    service: ApiService,
    stop: Arc<AtomicBool>,
) -> Option<thread::JoinHandle<()>> {
    let sources = configured_account_event_logs(config);
    if sources.is_empty() {
        return None;
    }
    let storage = match PipelineStorage::from_config(config) {
        Ok(storage) => storage,
        Err(error) => {
            eprintln!("[运行时 · API] 初始化 EventLog 投影桥失败: {error}");
            return None;
        }
    };
    let poll_interval = Duration::from_millis(250);
    Some(thread::spawn(move || {
        let mut pipelines = BTreeMap::<(String, String), LiveEventPipeline>::new();
        while !stop.load(Ordering::Acquire) {
            for (account_id, venue_id, log_name, currency) in &sources {
                let pipeline_key = (account_id.clone(), venue_id.clone());
                if let std::collections::btree_map::Entry::Vacant(entry) =
                    pipelines.entry(pipeline_key.clone())
                {
                    match storage.open(log_name.clone(), currency.clone()) {
                        Ok(opened) => {
                            entry.insert(opened);
                        }
                        Err(error) => {
                            eprintln!(
                                "[运行时 · API] 打开账户 EventLog 投影源失败 account={} venue={}: {error}",
                                account_id, venue_id
                            );
                            continue;
                        }
                    }
                }
                let Some(current) = pipelines.get_mut(&pipeline_key) else {
                    continue;
                };
                if let Err(error) = current.refresh() {
                    eprintln!(
                        "[运行时 · API] 刷新账户 EventLog 投影源失败 account={} venue={}: {error:?}",
                        account_id, venue_id
                    );
                    pipelines.remove(&pipeline_key);
                    continue;
                }
                if let Err(error) =
                    service.project_account_event_log(account_id, venue_id, current.log())
                {
                    eprintln!(
                        "[运行时 · API] 写入账户查询投影失败 account={} venue={}: {error}",
                        account_id, venue_id
                    );
                    pipelines.remove(&(account_id.clone(), venue_id.clone()));
                }
            }
            thread::sleep(poll_interval);
        }
    }))
}

pub(crate) fn command_is_final(control: &ControlPlane, command_id: u64) -> bool {
    control
        .audit()
        .iter()
        .rev()
        .find(|record| record.command_id == command_id)
        .is_some_and(|record| {
            matches!(
                record.status,
                qx_control::CommandStatus::Rejected
                    | qx_control::CommandStatus::Executed
                    | qx_control::CommandStatus::Failed
            )
        })
}

pub(crate) fn run_runtime_api(path: &Path) -> Result<(), String> {
    let config = read_runtime_config(path)?;
    let supervisor = RuntimeSupervisor::new(config.clone())?;
    let service = build_configured_api_service(&config, path)?;
    let listener = TcpListener::bind(&config.api.bind)
        .map_err(|error| format!("绑定 API 地址失败 {}: {error}", config.api.bind))?;
    println!(
        "[运行时 · API] bind={} transport={:?}，按 Ctrl+C 停止",
        config.api.bind, config.api.transport
    );
    match config.api.transport {
        ApiTransport::Plaintext => {
            let projection_stop = Arc::new(AtomicBool::new(false));
            let mut projection_thread =
                spawn_api_projection_bridge(&config, service.clone(), Arc::clone(&projection_stop));
            let worker = match supervisor.spawn_worker("api", move |context| {
                context.heartbeat(runtime_timestamp_ms())?;
                service
                    .serve(listener, runtime_timestamp_ms())
                    .map_err(|error| format!("API 服务停止: {error}"))
            }) {
                Ok(worker) => worker,
                Err(error) => {
                    stop_api_projection_bridge(&projection_stop, &mut projection_thread);
                    return Err(error);
                }
            };
            let result = worker.join().map_err(|_| "API worker panic".to_string())?;
            stop_api_projection_bridge(&projection_stop, &mut projection_thread);
            result
        }
        ApiTransport::Mtls => {
            let tls = config
                .api
                .tls
                .as_ref()
                .ok_or_else(|| "mTLS API 缺少 tls 配置".to_string())?;
            let server_config = load_mtls_server_config_from_pem(
                &tls.certificate_chain,
                &tls.private_key,
                &tls.client_ca,
            )?;
            let operator_paths = config
                .api
                .operators
                .iter()
                .map(|(operator_id, operator)| {
                    (operator_id.clone(), PathBuf::from(&operator.certificate))
                })
                .collect::<BTreeMap<_, _>>();
            let identity_reloader = MtlsIdentityPemReloader::new(operator_paths)?;
            let identity_store = MtlsIdentityStore::new(identity_reloader.load()?);
            let reloader = TlsPemReloader::new(
                tls.certificate_chain.clone(),
                tls.private_key.clone(),
                tls.client_ca.clone(),
            );
            let store = TlsConfigStore::new(server_config);
            let projection_stop = Arc::new(AtomicBool::new(false));
            let mut projection_thread =
                spawn_api_projection_bridge(&config, service.clone(), Arc::clone(&projection_stop));
            let reload_stop = Arc::new(AtomicBool::new(false));
            let reload_stop_thread = Arc::clone(&reload_stop);
            let reload_store = store.clone();
            let reload_identity_store = identity_store.clone();
            let reload_thread = thread::spawn(move || {
                while !reload_stop_thread.load(Ordering::Acquire) {
                    if let Err(error) = reloader.reload_if_changed(&reload_store) {
                        eprintln!("[运行时 · TLS] 证书轮询重载失败，保留当前配置: {error}");
                    }
                    if let Err(error) = identity_reloader.reload_if_changed(&reload_identity_store)
                    {
                        eprintln!(
                            "[运行时 · TLS] Operator 证书轮询重载失败，保留当前映射: {error}"
                        );
                    }
                    thread::sleep(Duration::from_secs(1));
                }
            });
            let worker = match supervisor.spawn_worker("api", move |context| {
                context.heartbeat(runtime_timestamp_ms())?;
                service
                    .serve_tls_mtls_with_stores(
                        listener,
                        &store,
                        &identity_store,
                        runtime_timestamp_ms(),
                    )
                    .map_err(|error| format!("mTLS API 服务停止: {error}"))
            }) {
                Ok(worker) => worker,
                Err(error) => {
                    reload_stop.store(true, Ordering::Release);
                    let _ = reload_thread.join();
                    stop_api_projection_bridge(&projection_stop, &mut projection_thread);
                    return Err(error);
                }
            };
            let result = worker
                .join()
                .map_err(|_| "mTLS API worker panic".to_string());
            reload_stop.store(true, Ordering::Release);
            let _ = reload_thread.join();
            stop_api_projection_bridge(&projection_stop, &mut projection_thread);
            result?
        }
    }
}

fn stop_api_projection_bridge(
    stop: &Arc<AtomicBool>,
    thread: &mut Option<std::thread::JoinHandle<()>>,
) {
    stop.store(true, Ordering::Release);
    if let Some(thread) = thread.take() {
        let _ = thread.join();
    }
}

pub(crate) fn serve_command(argv: &[String]) {
    let path = argv
        .get(2)
        .cloned()
        .unwrap_or_else(|| "deploy/qianxing.runtime.example.json".into());
    if let Err(error) = run_runtime_api(Path::new(&path)) {
        eprintln!("运行时 API 启动失败: {error}");
        std::process::exit(2);
    }
}
