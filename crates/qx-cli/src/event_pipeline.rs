//! Outbox 中继、事件消费者与死信重放的运行时循环。
//!
//! 传输后端（文件 / SQLite / PostgreSQL / NATS）由 crate 根的选择器决定，
//! 这里的循环对后端保持一致的租约、幂等与 ACK 语义。

#[allow(unused_imports)] // 默认特性下本模块条目全部为 nats/postgres 门控
use super::*;
#[cfg(feature = "nats")]
pub(crate) fn run_file_outbox_relay(
    data_root: &Path,
    nats_url: &str,
    subject_prefix: &str,
    limit: usize,
) -> Result<(), String> {
    let store = FileOutboxStore::new(data_root);
    let publisher = NatsJetStreamPublisher::connect(nats_url, subject_prefix)?;
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
    println!(
        "[Outbox relay] scanned={} published={} retried={} publish_failures={} lease_conflicts={} last_error={:?}",
        report.scanned,
        report.published,
        report.retried,
        report.publish_failures,
        report.lease_conflicts,
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
    let publisher = NatsJetStreamPublisher::connect(nats_url, subject_prefix)?;
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
        "[PostgreSQL Outbox relay] scanned={} published={} retried={} publish_failures={} lease_conflicts={} last_error={:?}",
        report.scanned,
        report.published,
        report.retried,
        report.publish_failures,
        report.lease_conflicts,
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
#[derive(Default)]
pub(crate) struct RelayMetricTotals {
    scanned: u64,
    published: u64,
    retried: u64,
    lease_conflicts: u64,
    publish_failures: u64,
}

#[cfg(feature = "nats")]
impl RelayMetricTotals {
    fn apply(&mut self, report: &qx_storage::OutboxRelayReport) {
        self.scanned += report.scanned;
        self.published += report.published;
        self.retried += report.retried;
        self.lease_conflicts += report.lease_conflicts;
        self.publish_failures += report.publish_failures;
    }

    fn render(&self, sink: &WorkerMetricsSink, up: bool, now_ms: u64) -> String {
        let worker = prometheus_label(&sink.worker_id);
        format!(
            "qx_worker_up{{worker=\"{worker}\"}} {}\n\
qx_worker_heartbeat_timestamp_seconds{{worker=\"{worker}\"}} {}\n\
qx_outbox_relay_scanned_total{{worker=\"{worker}\"}} {}\n\
qx_outbox_relay_published_total{{worker=\"{worker}\"}} {}\n\
qx_outbox_relay_retried_total{{worker=\"{worker}\"}} {}\n\
qx_outbox_relay_lease_conflicts_total{{worker=\"{worker}\"}} {}\n\
qx_outbox_relay_publish_failures_total{{worker=\"{worker}\"}} {}\n",
            u8::from(up),
            now_ms / 1_000,
            self.scanned,
            self.published,
            self.retried,
            self.lease_conflicts,
            self.publish_failures,
        )
    }
}

#[cfg(feature = "nats")]
#[derive(Default)]
pub(crate) struct ConsumerMetricTotals {
    received: u64,
    applied: u64,
    duplicates: u64,
    retried: u64,
    dead_lettered: u64,
    malformed: u64,
    ack_failures: u64,
}

#[cfg(feature = "nats")]
impl ConsumerMetricTotals {
    fn apply(&mut self, report: &qx_storage::NatsConsumerBatchReport) {
        self.received += report.received;
        self.applied += report.applied;
        self.duplicates += report.duplicates;
        self.retried += report.retried;
        self.dead_lettered += report.dead_lettered;
        self.malformed += report.malformed;
        self.ack_failures += report.ack_failures;
    }

    fn render(&self, sink: &WorkerMetricsSink, up: bool, now_ms: u64) -> String {
        let worker = prometheus_label(&sink.worker_id);
        format!(
            "qx_worker_up{{worker=\"{worker}\"}} {}\n\
qx_worker_heartbeat_timestamp_seconds{{worker=\"{worker}\"}} {}\n\
qx_event_consumer_received_total{{worker=\"{worker}\"}} {}\n\
qx_event_consumer_applied_total{{worker=\"{worker}\"}} {}\n\
qx_event_consumer_duplicates_total{{worker=\"{worker}\"}} {}\n\
qx_event_consumer_retried_total{{worker=\"{worker}\"}} {}\n\
qx_event_consumer_dead_lettered_total{{worker=\"{worker}\"}} {}\n\
qx_event_consumer_malformed_total{{worker=\"{worker}\"}} {}\n\
qx_event_consumer_ack_failures_total{{worker=\"{worker}\"}} {}\n",
            u8::from(up),
            now_ms / 1_000,
            self.received,
            self.applied,
            self.duplicates,
            self.retried,
            self.dead_lettered,
            self.malformed,
            self.ack_failures,
        )
    }
}

#[cfg(feature = "nats")]
pub(crate) fn wait_for_worker_interval(context: &WorkerContext, interval_ms: u64) {
    let deadline = Instant::now() + Duration::from_millis(interval_ms);
    while !context.should_stop() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        thread::sleep(remaining.min(Duration::from_millis(50)));
    }
}

#[cfg(feature = "nats")]
pub(crate) fn run_outbox_relay_loop<S>(
    context: WorkerContext,
    relay: OutboxRelay<S, NatsJetStreamPublisher>,
    messaging: MessagingRuntimeConfig,
    metrics: WorkerMetricsSink,
    once: bool,
) -> Result<(), String>
where
    S: OutboxStore + 'static,
{
    let mut totals = RelayMetricTotals::default();
    metrics.write(&totals.render(&metrics, true, runtime_timestamp_ms()));
    loop {
        if context.should_stop() {
            break;
        }
        let now = runtime_timestamp_ms();
        let report = relay
            .pump_once(lease_clock(now), messaging.relay_batch_size)
            .map_err(|error| format!("Outbox relay 批次失败: {error:?}"))?;
        totals.apply(&report);
        metrics.write(&totals.render(&metrics, true, now));
        context.heartbeat(now)?;
        println!(
            "[Outbox relay worker={}] scanned={} published={} retried={} failures={} conflicts={} last_error={:?}",
            context.id(),
            report.scanned,
            report.published,
            report.retried,
            report.publish_failures,
            report.lease_conflicts,
            report.last_error
        );
        if once {
            break;
        }
        wait_for_worker_interval(&context, messaging.relay_interval_ms);
    }
    metrics.write(&totals.render(&metrics, false, runtime_timestamp_ms()));
    Ok(())
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
        run_outbox_relay_loop(context, relay, messaging, metrics, once)
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
    if let Some(mut stdin) = child.stdin.take() {
        let write_result = stdin
            .write_all(&payload)
            .and_then(|_| stdin.write_all(b"\n"));
        if let Err(error) = write_result {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("写入事件 consumer handler stdin 失败: {error}"));
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
pub(crate) fn run_event_consumer_loop<S>(
    context: WorkerContext,
    consumer: NatsJetStreamConsumer,
    store: S,
    messaging: MessagingRuntimeConfig,
    handler: EventConsumerHandler,
    metrics: WorkerMetricsSink,
    once: bool,
) -> Result<(), String>
where
    S: ConsumerStateStore + Clone + Send + Sync + 'static,
{
    let mut totals = ConsumerMetricTotals::default();
    metrics.write(&totals.render(&metrics, true, runtime_timestamp_ms()));
    loop {
        if context.should_stop() {
            break;
        }
        let handler = handler.clone();
        let report = consumer.consume_batch(
            store.clone(),
            NatsJetStreamConsumer::current_timestamp(),
            messaging.consumer_batch_size,
            move |event| invoke_event_consumer_handler(&handler, event),
        )?;
        let now = runtime_timestamp_ms();
        totals.apply(&report);
        metrics.write(&totals.render(&metrics, true, now));
        context.heartbeat(now)?;
        println!(
            "[Event consumer worker={}] received={} applied={} duplicates={} retried={} dead_lettered={} malformed={} ack_failures={} last_error={:?}",
            context.id(),
            report.received,
            report.applied,
            report.duplicates,
            report.retried,
            report.dead_lettered,
            report.malformed,
            report.ack_failures,
            report.last_error
        );
        if once {
            break;
        }
        wait_for_worker_interval(&context, messaging.relay_interval_ms);
    }
    metrics.write(&totals.render(&metrics, false, runtime_timestamp_ms()));
    Ok(())
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
        run_event_consumer_loop(
            context,
            consumer,
            store,
            runtime.messaging,
            runtime.handler,
            runtime.metrics,
            runtime.once,
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
