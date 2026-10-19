//! Outbox 中继、事件消费者与死信重放的运行时循环。
//!
//! 传输后端（文件 / SQLite / PostgreSQL / NATS）由 crate 根的选择器决定，
//! 这里的循环对后端保持一致的租约、幂等与 ACK 语义。

#[cfg(feature = "nats")]
use crate::{
    configured_postgres_dsn, join_worker_handle, lease_clock, prometheus_label,
    read_runtime_config, runtime_timestamp_ms, worker_metrics_path, write_worker_metrics,
};
#[cfg(feature = "nats")]
use qx_runtime::{ConsumerMetricTotals, RelayMetricTotals};
#[cfg(feature = "nats")]
use qx_runtime::{MessagingRuntimeConfig, RuntimeSupervisor, StorageBackend, WorkerRole};
#[cfg(feature = "nats")]
use qx_storage::{
    ConsumerStateStore, FileConsumerStateStore, FileOutboxStore, NatsJetStreamConsumer,
    NatsJetStreamPublisher, NatsWaitBudget, OutboxEvent, OutboxPublisher, OutboxRelay, OutboxStore,
};
#[cfg(all(feature = "nats", feature = "postgres"))]
use qx_storage::{PostgresConsumerStateStore, PostgresOutboxStore};
#[cfg(all(feature = "nats", feature = "sqlite"))]
use qx_storage::{SqliteConsumerStateStore, SqliteOutboxStore};
#[cfg(feature = "nats")]
use std::path::{Path, PathBuf};
#[cfg(feature = "nats")]
use std::process::{Command, Stdio};
#[cfg(feature = "nats")]
use std::thread;
#[cfg(feature = "nats")]
use std::time::{Duration, Instant};
#[cfg(feature = "nats")]
#[cfg(feature = "nats")]
pub(crate) fn run_file_outbox_relay(
    data_root: &Path,
    nats_url: &str,
    subject_prefix: &str,
    limit: usize,
) -> Result<(), String> {
    let store = FileOutboxStore::new(data_root);
    let publisher =
        NatsJetStreamPublisher::connect(nats_url, subject_prefix, NatsWaitBudget::default())?;
    let relay = OutboxRelay::new(
        store,
        publisher,
        format!("qx-cli-{}", std::process::id()),
        30,
    )
    .map_err(|error| format!("创建 Outbox relay 失败: {error:?}"))?;
    let report = relay
        .pump_once(lease_clock(runtime_timestamp_ms()), limit)
        .map_err(|error| format!("执行 Outbox relay 失败: {error:?}"))?;
    // `parked` 是"库里还有几条发不出去"的状态量：单次批处理退 0 只说明这一页搬完了，
    // 不念出停在门后的条数，运维面就会把"还剩 N 条毒事件"读成"中继干净"（V13 R5）。
    println!(
        "[Outbox relay] scanned={} published={} retried={} publish_failures={} lease_conflicts={} parked={} last_error={:?}",
        report.scanned,
        report.published,
        report.retried,
        report.publish_failures,
        report.lease_conflicts,
        report.parked,
        report.last_error
    );
    Ok(())
}

#[cfg(all(feature = "nats", feature = "postgres"))]
pub(crate) fn run_postgres_outbox_relay(
    runtime_config_path: &Path,
    nats_url: &str,
    subject_prefix: &str,
    limit: usize,
) -> Result<(), String> {
    let config = read_runtime_config(runtime_config_path)?;
    let dsn = configured_postgres_dsn(&config)?
        .ok_or_else(|| "outbox-relay-postgres 要求 storage.backend=postgres".to_string())?;
    let store =
        PostgresOutboxStore::connect_with_pool_size(&dsn, config.storage.postgres_pool_size)
            .map_err(|error| format!("打开 PostgreSQL Outbox 失败: {error:?}"))?;
    let publisher =
        NatsJetStreamPublisher::connect(nats_url, subject_prefix, NatsWaitBudget::default())?;
    let relay = OutboxRelay::new(
        store,
        publisher,
        format!("qx-cli-pg-{}", std::process::id()),
        30,
    )
    .map_err(|error| format!("创建 PostgreSQL Outbox relay 失败: {error:?}"))?;
    let report = relay
        .pump_once(lease_clock(runtime_timestamp_ms()), limit)
        .map_err(|error| format!("执行 PostgreSQL Outbox relay 失败: {error:?}"))?;
    println!(
        "[PostgreSQL Outbox relay] scanned={} published={} retried={} publish_failures={} lease_conflicts={} parked={} last_error={:?}",
        report.scanned,
        report.published,
        report.retried,
        report.publish_failures,
        report.lease_conflicts,
        report.parked,
        report.last_error
    );
    Ok(())
}

#[cfg(feature = "nats")]
#[derive(Clone)]
pub(crate) struct WorkerMetricsSink {
    path: PathBuf,
    worker_id: String,
}

#[cfg(feature = "nats")]
impl WorkerMetricsSink {
    fn write(&self, body: &str) {
        if let Err(error) = write_worker_metrics(&self.path, body) {
            eprintln!(
                "worker={} metrics 写入失败，业务处理继续: {}",
                self.worker_id, error
            );
        }
    }
}

#[cfg(feature = "nats")]
pub(crate) fn run_relay_with_store<S>(
    supervisor: RuntimeSupervisor,
    worker_id: String,
    relay: OutboxRelay<S, NatsJetStreamPublisher>,
    messaging: MessagingRuntimeConfig,
    metrics: WorkerMetricsSink,
    once: bool,
) -> Result<(), String>
where
    S: OutboxStore + 'static,
{
    let handle = supervisor.spawn_worker(&worker_id, move |context| {
        let mut totals = RelayMetricTotals::default();
        qx_runtime::run_outbox_relay_loop(context, relay, messaging, once, |report, up, now_ms| {
            if let Some(report) = report {
                totals.apply(report);
            }
            metrics.write(&totals.render(&prometheus_label(&metrics.worker_id), up, now_ms));
        })
    })?;
    join_worker_handle(&supervisor, handle, "Outbox relay", &worker_id)
}

#[cfg(feature = "nats")]
pub(crate) fn run_outbox_relay_worker(
    path: &Path,
    worker_id: &str,
    once: bool,
) -> Result<(), String> {
    let config = read_runtime_config(path)?;
    let worker = config
        .workers
        .iter()
        .find(|worker| worker.id == worker_id)
        .cloned()
        .ok_or_else(|| format!("找不到 worker: {worker_id}"))?;
    if !worker.enabled || worker.role != WorkerRole::OutboxRelay {
        return Err(format!("worker {worker_id} 不是启用的 OutboxRelay worker"));
    }
    if !config.messaging.enabled {
        return Err("OutboxRelay worker 要求 messaging.enabled=true".into());
    }
    let publisher = NatsJetStreamPublisher::connect(
        &config.messaging.nats_url,
        &config.messaging.subject_prefix,
        config.messaging.nats_wait_budget(),
    )?;
    let relay_owner = format!("qx-relay-{worker_id}-{}", std::process::id());
    let lease_seconds = config.messaging.lease_seconds;
    let messaging = config.messaging.clone();
    let metrics = WorkerMetricsSink {
        path: worker_metrics_path(&config, worker_id),
        worker_id: worker_id.into(),
    };
    let supervisor = RuntimeSupervisor::new(config.clone())?;
    match config.storage.backend {
        StorageBackend::Files => {
            let relay = OutboxRelay::new(
                FileOutboxStore::new(&config.storage.data_dir),
                publisher,
                relay_owner,
                lease_seconds,
            )
            .map_err(|error| format!("创建文件 Outbox relay 失败: {error:?}"))?;
            run_relay_with_store(
                supervisor,
                worker_id.into(),
                relay,
                messaging,
                metrics,
                once,
            )
        }
        StorageBackend::Sqlite => {
            #[cfg(not(feature = "sqlite"))]
            {
                Err("当前 qx-cli 未启用 sqlite feature，无法运行 SQLite Outbox relay".into())
            }
            #[cfg(feature = "sqlite")]
            {
                let path = config
                    .storage
                    .sqlite_path
                    .as_deref()
                    .ok_or_else(|| "SQLite backend 缺少 sqlite_path".to_string())?;
                let relay = OutboxRelay::new(
                    SqliteOutboxStore::new(path)
                        .map_err(|error| format!("打开 SQLite Outbox 失败: {error:?}"))?,
                    publisher,
                    relay_owner,
                    lease_seconds,
                )
                .map_err(|error| format!("创建 SQLite Outbox relay 失败: {error:?}"))?;
                run_relay_with_store(
                    supervisor,
                    worker_id.into(),
                    relay,
                    messaging,
                    metrics,
                    once,
                )
            }
        }
        StorageBackend::Postgres => {
            #[cfg(not(feature = "postgres"))]
            {
                Err("当前 qx-cli 未启用 postgres feature，无法运行 PostgreSQL Outbox relay".into())
            }
            #[cfg(feature = "postgres")]
            {
                let dsn = configured_postgres_dsn(&config)?
                    .ok_or_else(|| "PostgreSQL backend 缺少 DSN".to_string())?;
                let relay = OutboxRelay::new(
                    PostgresOutboxStore::connect_with_pool_size(
                        &dsn,
                        config.storage.postgres_pool_size,
                    )
                    .map_err(|error| format!("打开 PostgreSQL Outbox 失败: {error:?}"))?,
                    publisher,
                    relay_owner,
                    lease_seconds,
                )
                .map_err(|error| format!("创建 PostgreSQL Outbox relay 失败: {error:?}"))?;
                run_relay_with_store(
                    supervisor,
                    worker_id.into(),
                    relay,
                    messaging,
                    metrics,
                    once,
                )
            }
        }
    }
}

#[cfg(feature = "nats")]
#[derive(Clone)]
pub(crate) struct EventConsumerHandler {
    executable: String,
    args: Vec<String>,
    timeout_ms: u64,
}

#[cfg(all(test, feature = "nats"))]
impl EventConsumerHandler {
    /// 测试构造：三个字段是本模块私有的，`tests/` 作为兄弟模块拿不到写权限，
    /// 只能由定义模块开一个仅在 `test + nats` 下编译的门控入口。
    pub(crate) fn for_test(executable: String, args: Vec<String>, timeout_ms: u64) -> Self {
        Self {
            executable,
            args,
            timeout_ms,
        }
    }
}

#[cfg(feature = "nats")]
pub(crate) struct EventConsumerRuntime {
    messaging: MessagingRuntimeConfig,
    handler: EventConsumerHandler,
    metrics: WorkerMetricsSink,
    once: bool,
}

#[cfg(feature = "nats")]
pub(crate) fn invoke_event_consumer_handler(
    handler: &EventConsumerHandler,
    event: &OutboxEvent,
) -> Result<(), String> {
    let payload = serde_json::to_vec(event)
        .map_err(|error| format!("事件 consumer envelope 序列化失败: {error}"))?;
    let mut command = Command::new(&handler.executable);
    command
        .args(&handler.args)
        .env_clear()
        .env("QX_EVENT_CONSUMER", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Ok(path) = std::env::var("PATH") {
        command.env("PATH", path);
    }
    let mut child = command
        .spawn()
        .map_err(|error| format!("启动事件 consumer handler 失败: {error}"))?;
    if let Some(stdin) = child.stdin.take() {
        let mut envelope = payload;
        envelope.push(b'\n');
        let written = qx_adapter::write_all_within(
            stdin,
            envelope,
            Duration::from_millis(handler.timeout_ms),
            || {
                let _ = child.kill();
            },
        );
        if let Err(error) = written {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("consumer stdin 写入失败: {error}"));
        }
    } else {
        let _ = child.kill();
        let _ = child.wait();
        return Err("事件 consumer handler stdin 不可用".into());
    }
    let started = Instant::now();
    loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|error| format!("读取事件 consumer handler 状态失败: {error}"))?
        {
            if status.success() {
                return Ok(());
            }
            return Err(format!(
                "事件 consumer handler 退出失败: {}",
                status
                    .code()
                    .map(|code| code.to_string())
                    .unwrap_or_else(|| "signal".into())
            ));
        }
        if started.elapsed() >= Duration::from_millis(handler.timeout_ms) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "事件 consumer handler 超时: {}ms",
                handler.timeout_ms
            ));
        }
        thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(feature = "nats")]
pub(crate) fn run_consumer_with_store<S>(
    supervisor: RuntimeSupervisor,
    worker_id: String,
    consumer: NatsJetStreamConsumer,
    store: S,
    runtime: EventConsumerRuntime,
) -> Result<(), String>
where
    S: ConsumerStateStore + Clone + Send + Sync + 'static,
{
    let handle = supervisor.spawn_worker(&worker_id, move |context| {
        let mut totals = ConsumerMetricTotals::default();
        let metrics = runtime.metrics;
        qx_runtime::run_event_consumer_loop(
            context,
            consumer,
            store,
            runtime.messaging,
            move |event| invoke_event_consumer_handler(&runtime.handler, event),
            runtime.once,
            |report, up, now_ms| {
                if let Some(report) = report {
                    totals.apply(report);
                }
                metrics.write(&totals.render(&prometheus_label(&metrics.worker_id), up, now_ms));
            },
        )
    })?;
    join_worker_handle(&supervisor, handle, "Event consumer", &worker_id)
}

#[cfg(feature = "nats")]
pub(crate) fn run_event_consumer_worker(
    path: &Path,
    worker_id: &str,
    once: bool,
) -> Result<(), String> {
    let config = read_runtime_config(path)?;
    let worker = config
        .workers
        .iter()
        .find(|worker| worker.id == worker_id)
        .cloned()
        .ok_or_else(|| format!("找不到 worker: {worker_id}"))?;
    if !worker.enabled || worker.role != WorkerRole::EventConsumer {
        return Err(format!(
            "worker {worker_id} 不是启用的 EventConsumer worker"
        ));
    }
    if !config.messaging.enabled {
        return Err("EventConsumer worker 要求 messaging.enabled=true".into());
    }
    let stream = config
        .messaging
        .consumer_stream
        .as_deref()
        .ok_or_else(|| "messaging.consumer_stream 未配置".to_string())?;
    let consumer_name = config
        .messaging
        .consumer_name
        .as_deref()
        .ok_or_else(|| "messaging.consumer_name 未配置".to_string())?;
    let group_id = config
        .messaging
        .consumer_group_id
        .as_deref()
        .ok_or_else(|| "messaging.consumer_group_id 未配置".to_string())?;
    let handler_executable = config
        .messaging
        .consumer_handler_executable
        .clone()
        .ok_or_else(|| "messaging.consumer_handler_executable 未配置".to_string())?;
    let consumer = NatsJetStreamConsumer::connect(
        &config.messaging.nats_url,
        stream,
        consumer_name,
        group_id,
        config.messaging.consumer_max_attempts,
        config.messaging.nats_wait_budget(),
    )?;
    let messaging = config.messaging.clone();
    let supervisor = RuntimeSupervisor::new(config.clone())?;
    let metrics = WorkerMetricsSink {
        path: worker_metrics_path(&config, worker_id),
        worker_id: worker_id.into(),
    };
    let handler = EventConsumerHandler {
        executable: handler_executable,
        args: messaging.consumer_handler_args.clone(),
        timeout_ms: messaging.consumer_handler_timeout_ms,
    };
    let runtime = EventConsumerRuntime {
        messaging,
        handler,
        metrics,
        once,
    };
    match config.storage.backend {
        StorageBackend::Files => run_consumer_with_store(
            supervisor,
            worker_id.into(),
            consumer,
            FileConsumerStateStore::new(&config.storage.data_dir),
            runtime,
        ),
        StorageBackend::Sqlite => {
            #[cfg(not(feature = "sqlite"))]
            {
                Err("当前 qx-cli 未启用 sqlite feature，无法运行 SQLite EventConsumer".into())
            }
            #[cfg(feature = "sqlite")]
            {
                let sqlite_path = config
                    .storage
                    .sqlite_path
                    .as_deref()
                    .ok_or_else(|| "SQLite backend 缺少 sqlite_path".to_string())?;
                run_consumer_with_store(
                    supervisor,
                    worker_id.into(),
                    consumer,
                    SqliteConsumerStateStore::new(sqlite_path)
                        .map_err(|error| format!("打开 SQLite 消费状态失败: {error:?}"))?,
                    runtime,
                )
            }
        }
        StorageBackend::Postgres => {
            #[cfg(not(feature = "postgres"))]
            {
                Err("当前 qx-cli 未启用 postgres feature，无法运行 PostgreSQL EventConsumer".into())
            }
            #[cfg(feature = "postgres")]
            {
                let dsn = configured_postgres_dsn(&config)?
                    .ok_or_else(|| "PostgreSQL backend 缺少 DSN".to_string())?;
                run_consumer_with_store(
                    supervisor,
                    worker_id.into(),
                    consumer,
                    PostgresConsumerStateStore::connect_with_pool_size(
                        &dsn,
                        config.storage.postgres_pool_size,
                    )
                    .map_err(|error| format!("打开 PostgreSQL 消费状态失败: {error:?}"))?,
                    runtime,
                )
            }
        }
    }
}

#[cfg(feature = "nats")]
pub(crate) fn replay_dead_letter_from_store<S>(
    store: S,
    publisher: &NatsJetStreamPublisher,
    group_id: &str,
    event_id: &str,
) -> Result<(), String>
where
    S: ConsumerStateStore,
{
    let records = store
        .dead_letters(group_id, 10_000)
        .map_err(|error| format!("读取消费者死信失败: {error:?}"))?;
    let record = records
        .into_iter()
        .filter(|record| record.event_id == event_id)
        .max_by_key(|record| record.attempts)
        .ok_or_else(|| format!("找不到 group={group_id} event_id={event_id} 的死信记录"))?;
    let mut replay = record.event;
    // Replay is a new logical delivery. The deterministic suffix makes an
    // operator retry safe even though JetStream publisher itself is at-least-once.
    replay.event_id = format!("{}:replay:{}", replay.event_id, record.attempts);
    replay.attempts = 0;
    replay
        .validate()
        .map_err(|error| format!("重放事件校验失败: {error:?}"))?;
    publisher
        .publish(&replay)
        .map_err(|error| format!("发布死信重放事件失败: {error}"))?;
    println!(
        "[DLQ replay] group={} source_event_id={} replay_event_id={} published=true",
        group_id, event_id, replay.event_id
    );
    Ok(())
}

#[cfg(feature = "nats")]
pub(crate) fn run_dead_letter_replay(
    path: &Path,
    group_id: &str,
    event_id: &str,
) -> Result<(), String> {
    if group_id.trim().is_empty() || event_id.trim().is_empty() {
        return Err("DLQ 重放要求 group_id 和 event_id".into());
    }
    let config = read_runtime_config(path)?;
    if !config.messaging.enabled {
        return Err("DLQ 重放要求 messaging.enabled=true".into());
    }
    let publisher = NatsJetStreamPublisher::connect(
        &config.messaging.nats_url,
        &config.messaging.subject_prefix,
        config.messaging.nats_wait_budget(),
    )?;
    match config.storage.backend {
        StorageBackend::Files => replay_dead_letter_from_store(
            FileConsumerStateStore::new(&config.storage.data_dir),
            &publisher,
            group_id,
            event_id,
        ),
        StorageBackend::Sqlite => {
            #[cfg(not(feature = "sqlite"))]
            {
                Err("当前 qx-cli 未启用 sqlite feature，无法读取 SQLite DLQ".into())
            }
            #[cfg(feature = "sqlite")]
            {
                let sqlite_path = config
                    .storage
                    .sqlite_path
                    .as_deref()
                    .ok_or_else(|| "SQLite backend 缺少 sqlite_path".to_string())?;
                replay_dead_letter_from_store(
                    SqliteConsumerStateStore::new(sqlite_path)
                        .map_err(|error| format!("打开 SQLite 消费状态失败: {error:?}"))?,
                    &publisher,
                    group_id,
                    event_id,
                )
            }
        }
        StorageBackend::Postgres => {
            #[cfg(not(feature = "postgres"))]
            {
                Err("当前 qx-cli 未启用 postgres feature，无法读取 PostgreSQL DLQ".into())
            }
            #[cfg(feature = "postgres")]
            {
                let dsn = configured_postgres_dsn(&config)?
                    .ok_or_else(|| "PostgreSQL backend 缺少 DSN".to_string())?;
                replay_dead_letter_from_store(
                    PostgresConsumerStateStore::connect_with_pool_size(
                        &dsn,
                        config.storage.postgres_pool_size,
                    )
                    .map_err(|error| format!("打开 PostgreSQL 消费状态失败: {error:?}"))?,
                    &publisher,
                    group_id,
                    event_id,
                )
            }
        }
    }
}
