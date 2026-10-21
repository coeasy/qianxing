//! Shared supervised NATS relay and consumer hosts.

use crate::{worker_host, worker_metrics};
use qx_runtime::{
    ConsumerMetricTotals, MessagingRuntimeConfig, RelayMetricTotals, RuntimeConfig, StorageBackend,
    WorkerRole,
};
#[cfg(all(feature = "nats", feature = "postgres"))]
use qx_storage::PostgresConsumerStateStore;
#[cfg(all(feature = "nats", feature = "postgres"))]
use qx_storage::PostgresOutboxStore;
#[cfg(all(feature = "nats", feature = "sqlite"))]
use qx_storage::SqliteConsumerStateStore;
#[cfg(all(feature = "nats", feature = "sqlite"))]
use qx_storage::SqliteOutboxStore;
use qx_storage::{
    ConsumerStateStore, FileConsumerStateStore, FileOutboxStore, NatsJetStreamConsumer,
    NatsJetStreamPublisher, OutboxEvent, OutboxPublisher, OutboxRelay, OutboxRelayReport,
    OutboxStore,
};
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

fn publish_metrics(config: &RuntimeConfig, worker_id: &str, body: &str) {
    let path = worker_metrics::worker_metrics_path(config, worker_id);
    if let Err(error) = worker_metrics::write_worker_metrics(&path, body) {
        eprintln!(
            "[qx-service · metrics] worker={worker_id} metrics 写入失败，业务处理继续: {error}"
        );
    }
}

fn runtime_timestamp_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

/// Run one bounded OutboxRelay batch using the file-backed store.
pub fn pump_file_outbox_once(
    data_root: &Path,
    nats_url: &str,
    subject_prefix: &str,
    limit: usize,
) -> Result<OutboxRelayReport, String> {
    let store = FileOutboxStore::new(data_root);
    let publisher = NatsJetStreamPublisher::connect(
        nats_url,
        subject_prefix,
        qx_storage::NatsWaitBudget::default(),
    )?;
    let relay = OutboxRelay::new(
        store,
        publisher,
        format!("qx-cli-{}", std::process::id()),
        30,
    )
    .map_err(|error| format!("创建 Outbox relay 失败: {error:?}"))?;
    relay
        .pump_once(qx_runtime::lease_clock(runtime_timestamp_ms()), limit)
        .map_err(|error| format!("执行 Outbox relay 失败: {error:?}"))
}

/// Run one bounded OutboxRelay batch against the configured PostgreSQL store.
#[cfg(feature = "postgres")]
pub fn pump_postgres_outbox_once(
    config: &RuntimeConfig,
    nats_url: &str,
    subject_prefix: &str,
    limit: usize,
) -> Result<OutboxRelayReport, String> {
    let dsn = crate::configured_backends::configured_postgres_dsn(config)?
        .ok_or_else(|| "outbox-relay-postgres 要求 storage.backend=postgres".to_string())?;
    let store =
        PostgresOutboxStore::connect_with_pool_size(&dsn, config.storage.postgres_pool_size)
            .map_err(|error| format!("打开 PostgreSQL Outbox 失败: {error:?}"))?;
    let publisher = NatsJetStreamPublisher::connect(
        nats_url,
        subject_prefix,
        qx_storage::NatsWaitBudget::default(),
    )?;
    let relay = OutboxRelay::new(
        store,
        publisher,
        format!("qx-cli-pg-{}", std::process::id()),
        30,
    )
    .map_err(|error| format!("创建 PostgreSQL Outbox relay 失败: {error:?}"))?;
    relay
        .pump_once(qx_runtime::lease_clock(runtime_timestamp_ms()), limit)
        .map_err(|error| format!("执行 PostgreSQL Outbox relay 失败: {error:?}"))
}

/// Invokes the configured process that handles one delivered event.
///
/// Both writing the serialized envelope and waiting for process exit are bounded by the same
/// timeout. This keeps a live child that never drains stdin from pinning the service worker.
#[derive(Clone, Debug)]
pub struct ProcessEventConsumerHandler {
    executable: String,
    args: Vec<String>,
    timeout_ms: u64,
}

impl ProcessEventConsumerHandler {
    pub fn new(executable: String, args: Vec<String>, timeout_ms: u64) -> Self {
        Self {
            executable,
            args,
            timeout_ms,
        }
    }

    pub fn invoke(&self, event: &OutboxEvent) -> Result<(), String> {
        let payload = serde_json::to_vec(event)
            .map_err(|error| format!("事件 consumer envelope 序列化失败: {error}"))?;
        let mut command = Command::new(&self.executable);
        command
            .args(&self.args)
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
                Duration::from_millis(self.timeout_ms),
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
            if started.elapsed() >= Duration::from_millis(self.timeout_ms) {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("事件 consumer handler 超时: {}ms", self.timeout_ms));
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
}

pub fn run_outbox_relay<S>(
    config: RuntimeConfig,
    worker_id: &str,
    relay: OutboxRelay<S, NatsJetStreamPublisher>,
    messaging: MessagingRuntimeConfig,
    once: bool,
) -> Result<(), String>
where
    S: OutboxStore + 'static,
{
    let metrics_config = config.clone();
    let metrics_worker_id = worker_id.to_string();
    worker_host::run_worker(
        config,
        worker_id,
        WorkerRole::OutboxRelay,
        "Outbox relay",
        move |context| {
            let mut totals = RelayMetricTotals::default();
            qx_runtime::run_outbox_relay_loop(
                context,
                relay,
                messaging,
                once,
                move |report, up, now_ms| {
                    if let Some(report) = report {
                        totals.apply(report);
                    }
                    let body = totals.render(
                        &worker_metrics::prometheus_label(&metrics_worker_id),
                        up,
                        now_ms,
                    );
                    publish_metrics(&metrics_config, &metrics_worker_id, &body);
                },
            )
        },
    )
}

/// Build and supervise an OutboxRelay worker from the already parsed runtime snapshot.
pub fn run_configured_outbox_relay_worker(
    config: RuntimeConfig,
    worker_id: &str,
    once: bool,
) -> Result<(), String> {
    let worker = config
        .workers
        .iter()
        .find(|worker| worker.id == worker_id)
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
    match config.storage.backend {
        StorageBackend::Files => {
            let relay = OutboxRelay::new(
                FileOutboxStore::new(&config.storage.data_dir),
                publisher,
                relay_owner,
                lease_seconds,
            )
            .map_err(|error| format!("创建文件 Outbox relay 失败: {error:?}"))?;
            run_outbox_relay(config, worker_id, relay, messaging, once)
        }
        StorageBackend::Sqlite => {
            #[cfg(not(feature = "sqlite"))]
            {
                Err("当前构建未启用 sqlite feature，无法运行 SQLite Outbox relay".into())
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
                run_outbox_relay(config, worker_id, relay, messaging, once)
            }
        }
        StorageBackend::Postgres => {
            #[cfg(not(feature = "postgres"))]
            {
                Err("当前构建未启用 postgres feature，无法运行 PostgreSQL Outbox relay".into())
            }
            #[cfg(feature = "postgres")]
            {
                let dsn = crate::configured_backends::postgres_dsn(&config)?;
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
                run_outbox_relay(config, worker_id, relay, messaging, once)
            }
        }
    }
}

pub fn run_event_consumer<S, H>(
    config: RuntimeConfig,
    worker_id: &str,
    consumer: NatsJetStreamConsumer,
    store: S,
    messaging: MessagingRuntimeConfig,
    handler: H,
    once: bool,
) -> Result<(), String>
where
    S: ConsumerStateStore + Clone + Send + Sync + 'static,
    H: Fn(&OutboxEvent) -> Result<(), String> + Clone + Send + Sync + 'static,
{
    let metrics_config = config.clone();
    let metrics_worker_id = worker_id.to_string();
    worker_host::run_worker(
        config,
        worker_id,
        WorkerRole::EventConsumer,
        "Event consumer",
        move |context| {
            let mut totals = ConsumerMetricTotals::default();
            qx_runtime::run_event_consumer_loop(
                context,
                consumer,
                store,
                messaging,
                handler,
                once,
                move |report, up, now_ms| {
                    if let Some(report) = report {
                        totals.apply(report);
                    }
                    let body = totals.render(
                        &worker_metrics::prometheus_label(&metrics_worker_id),
                        up,
                        now_ms,
                    );
                    publish_metrics(&metrics_config, &metrics_worker_id, &body);
                },
            )
        },
    )
}

/// Build and supervise an EventConsumer worker from the already parsed runtime snapshot.
pub fn run_configured_event_consumer_worker(
    config: RuntimeConfig,
    worker_id: &str,
    once: bool,
) -> Result<(), String> {
    let worker = config
        .workers
        .iter()
        .find(|worker| worker.id == worker_id)
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
    let handler = ProcessEventConsumerHandler::new(
        handler_executable,
        messaging.consumer_handler_args.clone(),
        messaging.consumer_handler_timeout_ms,
    );
    match config.storage.backend {
        StorageBackend::Files => run_event_consumer(
            config.clone(),
            worker_id,
            consumer,
            FileConsumerStateStore::new(&config.storage.data_dir),
            messaging,
            move |event| handler.invoke(event),
            once,
        ),
        StorageBackend::Sqlite => {
            #[cfg(not(feature = "sqlite"))]
            {
                Err("当前构建未启用 sqlite feature，无法运行 SQLite EventConsumer".into())
            }
            #[cfg(feature = "sqlite")]
            {
                let sqlite_path = config
                    .storage
                    .sqlite_path
                    .as_deref()
                    .ok_or_else(|| "SQLite backend 缺少 sqlite_path".to_string())?;
                let store = SqliteConsumerStateStore::new(sqlite_path)
                    .map_err(|error| format!("打开 SQLite 消费状态失败: {error:?}"))?;
                run_event_consumer(
                    config.clone(),
                    worker_id,
                    consumer,
                    store,
                    messaging,
                    move |event| handler.invoke(event),
                    once,
                )
            }
        }
        StorageBackend::Postgres => {
            #[cfg(not(feature = "postgres"))]
            {
                Err("当前构建未启用 postgres feature，无法运行 PostgreSQL EventConsumer".into())
            }
            #[cfg(feature = "postgres")]
            {
                let dsn = crate::configured_backends::postgres_dsn(&config)?;
                let store = PostgresConsumerStateStore::connect_with_pool_size(
                    &dsn,
                    config.storage.postgres_pool_size,
                )
                .map_err(|error| format!("打开 PostgreSQL 消费状态失败: {error:?}"))?;
                run_event_consumer(
                    config.clone(),
                    worker_id,
                    consumer,
                    store,
                    messaging,
                    move |event| handler.invoke(event),
                    once,
                )
            }
        }
    }
}

/// Replay the newest recorded attempt for one consumer dead-letter event.
pub fn replay_dead_letter_from_store<S, P>(
    store: S,
    publisher: &P,
    group_id: &str,
    event_id: &str,
) -> Result<(), String>
where
    S: ConsumerStateStore,
    P: OutboxPublisher,
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

/// Connect to the configured transport/store and perform one bounded DLQ replay operation.
pub fn run_configured_dead_letter_replay(
    config: RuntimeConfig,
    group_id: &str,
    event_id: &str,
) -> Result<(), String> {
    if group_id.trim().is_empty() || event_id.trim().is_empty() {
        return Err("DLQ 重放要求 group_id 和 event_id".into());
    }
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
                Err("当前构建未启用 sqlite feature，无法读取 SQLite DLQ".into())
            }
            #[cfg(feature = "sqlite")]
            {
                let sqlite_path = config
                    .storage
                    .sqlite_path
                    .as_deref()
                    .ok_or_else(|| "SQLite backend 缺少 sqlite_path".to_string())?;
                let store = SqliteConsumerStateStore::new(sqlite_path)
                    .map_err(|error| format!("打开 SQLite 消费状态失败: {error:?}"))?;
                replay_dead_letter_from_store(store, &publisher, group_id, event_id)
            }
        }
        StorageBackend::Postgres => {
            #[cfg(not(feature = "postgres"))]
            {
                Err("当前构建未启用 postgres feature，无法读取 PostgreSQL DLQ".into())
            }
            #[cfg(feature = "postgres")]
            {
                let dsn = crate::configured_backends::postgres_dsn(&config)?;
                let store = PostgresConsumerStateStore::connect_with_pool_size(
                    &dsn,
                    config.storage.postgres_pool_size,
                )
                .map_err(|error| format!("打开 PostgreSQL 消费状态失败: {error:?}"))?;
                replay_dead_letter_from_store(store, &publisher, group_id, event_id)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn runtime_config() -> RuntimeConfig {
        RuntimeConfig::from_json(include_str!(
            "../../../deploy/qianxing.runtime.paper-strategy.example.json"
        ))
        .expect("paper strategy runtime fixture is valid")
    }

    #[derive(Default)]
    struct CapturingPublisher(Mutex<Vec<OutboxEvent>>);

    impl OutboxPublisher for CapturingPublisher {
        fn publish(&self, event: &OutboxEvent) -> Result<(), String> {
            self.0.lock().expect("publisher lock").push(event.clone());
            Ok(())
        }
    }

    #[test]
    fn configured_relay_rejects_wrong_worker_role_before_connecting() {
        let config = runtime_config();
        let worker_id = config
            .workers
            .first()
            .expect("fixture has a worker")
            .id
            .clone();
        let error = run_configured_outbox_relay_worker(config, &worker_id, true)
            .expect_err("strategy worker cannot start an outbox relay");
        assert!(error.contains("不是启用的 OutboxRelay worker"), "{error}");
    }

    #[test]
    fn configured_relay_requires_messaging_before_connecting() {
        let mut config = runtime_config();
        let worker = config.workers.first_mut().expect("fixture has a worker");
        worker.role = WorkerRole::OutboxRelay;
        worker.enabled = true;
        let worker_id = worker.id.clone();
        config.messaging.enabled = false;
        let error = run_configured_outbox_relay_worker(config, &worker_id, true)
            .expect_err("messaging must be enabled before a NATS connection is opened");
        assert!(error.contains("messaging.enabled=true"), "{error}");
    }

    #[test]
    fn configured_consumer_rejects_wrong_worker_role_before_connecting() {
        let config = runtime_config();
        let worker_id = config
            .workers
            .first()
            .expect("fixture has a worker")
            .id
            .clone();
        let error = run_configured_event_consumer_worker(config, &worker_id, true)
            .expect_err("strategy worker cannot start an event consumer");
        assert!(error.contains("不是启用的 EventConsumer worker"), "{error}");
    }

    #[test]
    fn configured_consumer_requires_stream_before_connecting() {
        let mut config = runtime_config();
        let worker = config.workers.first_mut().expect("fixture has a worker");
        worker.role = WorkerRole::EventConsumer;
        worker.enabled = true;
        let worker_id = worker.id.clone();
        config.messaging.enabled = true;
        config.messaging.consumer_stream = None;
        let error = run_configured_event_consumer_worker(config, &worker_id, true)
            .expect_err("consumer stream is required before a NATS connection is opened");
        assert!(
            error.contains("messaging.consumer_stream 未配置"),
            "{error}"
        );
    }

    #[test]
    fn configured_dlq_replay_rejects_empty_identity_before_connecting() {
        let config = runtime_config();
        let error = run_configured_dead_letter_replay(config, " ", "event-1")
            .expect_err("empty DLQ identity must be rejected before connecting");
        assert!(error.contains("要求 group_id 和 event_id"), "{error}");
    }

    #[test]
    fn configured_dlq_replay_requires_messaging_before_connecting() {
        let config = runtime_config();
        let error = run_configured_dead_letter_replay(config, "consumer-group", "event-1")
            .expect_err("messaging must be enabled before connecting");
        assert!(error.contains("messaging.enabled=true"), "{error}");
    }

    #[cfg(feature = "postgres")]
    #[test]
    fn postgres_one_shot_relay_rejects_file_storage_before_external_io() {
        let config = runtime_config();
        let error = pump_postgres_outbox_once(&config, "nats://127.0.0.1:4222", "qx", 10)
            .expect_err("PostgreSQL command must reject a file-backed runtime before connecting");
        assert!(error.contains("storage.backend=postgres"), "{error}");
    }

    #[test]
    fn dead_letter_replay_uses_record_attempts_and_resets_delivery_attempts() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join(".tmp-test")
            .join(format!("qx-service-dlq-{}", std::process::id()));
        std::fs::create_dir_all(&root).expect("create test data root");
        let store = FileConsumerStateStore::new(&root);
        let engine = qx_storage::ConsumerEngine::new(store.clone(), "consumer-group", 1)
            .expect("consumer engine");
        let event = OutboxEvent {
            event_id: "event-1".into(),
            topic: "qx.eventlog".into(),
            partition_key: "account-1".into(),
            sequence: 1,
            schema_version: OutboxEvent::LATEST_SCHEMA_VERSION,
            trace_id: "trace-1".into(),
            payload: "{}".into(),
            created_ts: 1_700_000_000,
            attempts: 0,
        };
        let outcome = engine
            .consume(
                &event,
                1,
                1,
                1_700_000_001,
                |_| Err("handler failed".into()),
            )
            .expect("record failed delivery");
        assert_eq!(outcome, qx_storage::ConsumerOutcome::DeadLettered);

        let publisher = CapturingPublisher::default();
        replay_dead_letter_from_store(store, &publisher, "consumer-group", "event-1")
            .expect("replay recorded dead letter");
        let published = publisher.0.lock().expect("publisher lock");
        assert_eq!(published.len(), 1);
        assert_eq!(published[0].event_id, "event-1:replay:1");
        assert_eq!(published[0].attempts, 0);
        drop(published);
        std::fs::remove_dir_all(root).expect("remove test data root");
    }
}
