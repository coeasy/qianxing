//! Optional NATS JetStream publisher for the generic Outbox relay.

use super::{
    ConsumerCheckpoint, ConsumerEngine, ConsumerOutcome, ConsumerProjection, ConsumerStateStore,
    NatsWaitBudget, OutboxEvent, OutboxPublisher, TransactionalConsumerStateStore,
};
use async_nats::jetstream;
use futures_util::StreamExt;
use std::future::Future;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::runtime::Runtime;

/// JetStream publisher with a dedicated Tokio runtime.
///
/// The publisher waits for the JetStream publish acknowledgement before
/// returning success. The Outbox relay therefore only removes an event after
/// the broker has acknowledged persistence; consumers still must deduplicate by
/// `event_id` because a connection failure can happen after broker acceptance
/// and before the acknowledgement reaches this process.
pub struct NatsJetStreamPublisher {
    runtime: Arc<Runtime>,
    context: jetstream::Context,
    subject_prefix: String,
}

impl std::fmt::Debug for NatsJetStreamPublisher {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NatsJetStreamPublisher")
            .field("subject_prefix", &self.subject_prefix)
            .finish_non_exhaustive()
    }
}

impl NatsJetStreamPublisher {
    /// Connect to NATS and use an existing JetStream stream. Stream creation is
    /// intentionally deployment-owned so production retention/replication
    /// policy cannot be silently changed by a trading process.
    pub fn connect(
        url: &str,
        subject_prefix: &str,
        budget: NatsWaitBudget,
    ) -> Result<Self, String> {
        if url.trim().is_empty() || subject_prefix.trim().is_empty() {
            return Err("NATS url 和 subject_prefix 不能为空".into());
        }
        budget.validate()?;
        validate_subject(subject_prefix)?;
        let runtime = Arc::new(
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .map_err(|error| format!("NATS Tokio runtime 初始化失败: {error}"))?,
        );
        let url = url.to_string();
        let connect_timeout = budget.connect_timeout();
        let request_timeout = budget.request_timeout();
        // `jetstream::new` spawns the ack-watcher task, so it must run while the
        // Tokio runtime is entered; calling it after `block_on` returned would
        // panic ("there is no reactor running") on any plain worker thread.
        let context = block_on_runtime(&runtime, async move {
            let client = async_nats::ConnectOptions::default()
                .connection_timeout(connect_timeout)
                // `$JS.API.*` 请求的界在连接层（client.request 用 Connection::request_timeout），
                // 不是 Context::timeout；只设后者会让发布有界、查询仍退回依赖默认的 10s。
                .request_timeout(Some(request_timeout))
                .connect(url)
                .await
                .map_err(|error| error.to_string())?;
            let mut context = jetstream::new(client);
            context.set_timeout(request_timeout);
            Ok::<_, String>(context)
        })
        .map_err(|error| format!("NATS 连接失败: {error}"))?;
        Ok(Self {
            context,
            runtime,
            subject_prefix: subject_prefix.trim_end_matches('.').into(),
        })
    }

    fn subject_for(&self, event: &OutboxEvent) -> Result<String, String> {
        let topic = event.topic.replace(':', ".");
        let subject = format!("{}.{}", self.subject_prefix, topic);
        validate_subject(&subject)?;
        Ok(subject)
    }
}

impl OutboxPublisher for NatsJetStreamPublisher {
    fn publish(&self, event: &OutboxEvent) -> Result<(), String> {
        let subject = self.subject_for(event)?;
        let payload = serde_json::to_vec(event)
            .map_err(|error| format!("NATS Outbox envelope 序列化失败: {error}"))?;
        let context = self.context.clone();
        block_on_runtime(&self.runtime, async move {
            let ack = context
                .publish(subject, payload.into())
                .await
                .map_err(|error| format!("JetStream publish 请求失败: {error}"))?;
            ack.await
                .map_err(|error| format!("JetStream publish ack 失败: {error}"))?;
            Ok::<(), String>(())
        })
    }
}

/// Result of one bounded JetStream pull. A batch is deliberately bounded so
/// the caller can expose it as a worker heartbeat and apply its own shutdown,
/// backpressure, and metric policy.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NatsConsumerBatchReport {
    pub received: u64,
    pub applied: u64,
    pub duplicates: u64,
    pub retried: u64,
    pub dead_lettered: u64,
    pub malformed: u64,
    pub ack_failures: u64,
    pub last_error: Option<String>,
}

/// JetStream pull consumer bound to the durable consumer created by deployment.
///
/// The adapter delegates checkpoint, idempotency, retry and dead-letter
/// semantics to [`ConsumerEngine`]. A successfully applied or terminally
/// dead-lettered event is acknowledged; a retryable handler error is NAKed so
/// JetStream performs the redelivery according to the consumer policy.
pub struct NatsJetStreamConsumer {
    runtime: Arc<Runtime>,
    consumer: jetstream::consumer::PullConsumer,
    group_id: String,
    max_attempts: u32,
    pull_expires: std::time::Duration,
}

impl std::fmt::Debug for NatsJetStreamConsumer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NatsJetStreamConsumer")
            .field("group_id", &self.group_id)
            .field("max_attempts", &self.max_attempts)
            .finish_non_exhaustive()
    }
}

impl NatsJetStreamConsumer {
    /// Connect to an existing stream and durable pull consumer. Creating the
    /// stream/consumer is deployment-owned so retention, replication, filters,
    /// ack policy and max-deliver cannot be changed accidentally by a worker.
    pub fn connect(
        url: &str,
        stream: &str,
        consumer: &str,
        group_id: &str,
        max_attempts: u32,
        budget: NatsWaitBudget,
    ) -> Result<Self, String> {
        for (value, field) in [
            (url, "NATS url"),
            (stream, "stream"),
            (consumer, "consumer"),
            (group_id, "group_id"),
        ] {
            if value.trim().is_empty() {
                return Err(format!("{field} 不能为空"));
            }
        }
        if max_attempts == 0 {
            return Err("NATS consumer max_attempts 必须大于 0".into());
        }
        budget.validate()?;
        let runtime = Arc::new(
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .map_err(|error| format!("NATS Tokio runtime 初始化失败: {error}"))?,
        );
        let url = url.to_string();
        let stream = stream.to_string();
        let consumer_name = consumer.to_string();
        let connect_timeout = budget.connect_timeout();
        let request_timeout = budget.request_timeout();
        let jetstream_consumer = block_on_runtime(&runtime, async move {
            let client = async_nats::ConnectOptions::default()
                .connection_timeout(connect_timeout)
                .request_timeout(Some(request_timeout))
                .connect(url)
                .await
                .map_err(|error| error.to_string())?;
            let mut context = jetstream::new(client);
            // 连接层的 `request_timeout` 管到 `$JS.API.*` 请求本身，`set_timeout` 管到
            // 发布 ack；两者都接同一个预算，否则有一侧仍退回依赖默认值。
            context.set_timeout(request_timeout);
            context
                .get_stream(stream)
                .await
                .map_err(|error| error.to_string())?
                .get_consumer(&consumer_name)
                .await
                .map_err(|error| error.to_string())
        })
        .map_err(|error| format!("NATS JetStream consumer 连接失败: {error}"))?;
        Ok(Self {
            runtime,
            consumer: jetstream_consumer,
            group_id: group_id.into(),
            max_attempts,
            pull_expires: budget.pull_expires(),
        })
    }

    /// Pull and process at most `limit` messages. The handler should make its
    /// side effects idempotent. For an atomic projection/checkpoint boundary,
    /// use [`Self::consume_batch_with_projection`] instead.
    pub fn consume_batch<S, H>(
        &self,
        store: S,
        now: u64,
        limit: usize,
        handler: H,
    ) -> Result<NatsConsumerBatchReport, String>
    where
        S: ConsumerStateStore + Clone + Send + Sync + 'static,
        H: Fn(&OutboxEvent) -> Result<(), String> + Send + Sync + 'static,
    {
        if limit == 0 {
            return Ok(NatsConsumerBatchReport::default());
        }
        let consumer = self.consumer.clone();
        let group_id = self.group_id.clone();
        let max_attempts = self.max_attempts;
        let pull_expires = self.pull_expires;
        block_on_runtime(&self.runtime, async move {
            // The storage implementations intentionally use synchronous
            // transactions. Keep them off the Tokio reactor so a slow disk or
            // database cannot stall NATS heartbeats and pull requests.
            ConsumerEngine::new(store.clone(), group_id.clone(), max_attempts)
                .map_err(|error| format!("{error:?}"))?;
            let store = Arc::new(store);
            let handler = Arc::new(handler);
            let mut messages = consumer
                .fetch()
                .max_messages(limit)
                // Bound an empty pull so the owning worker can observe its
                // shutdown token promptly instead of waiting for JetStream's
                // long default batch expiry.
                .expires(pull_expires)
                .messages()
                .await
                .map_err(|error| format!("JetStream pull 请求失败: {error}"))?;
            let mut report = NatsConsumerBatchReport::default();
            while let Some(message) = messages.next().await {
                let message =
                    message.map_err(|error| format!("JetStream 消息读取失败: {error}"))?;
                report.received += 1;
                let info = message
                    .info()
                    .map_err(|error| format!("JetStream 消息元数据解析失败: {error}"))?;
                let offset = info.stream_sequence;
                let delivery_attempt = info.delivered.max(1) as u32;
                let event = match serde_json::from_slice::<OutboxEvent>(&message.payload) {
                    Ok(event) => event,
                    Err(error) => {
                        let error = format!("Outbox envelope 解析失败: {error}");
                        message
                            .double_ack_with(async_nats::jetstream::AckKind::Term)
                            .await
                            .map_err(|ack_error| {
                                report.ack_failures += 1;
                                format!("{error}; poison message 终止确认失败: {ack_error}")
                            })?;
                        report.malformed += 1;
                        report.last_error = Some(error);
                        continue;
                    }
                };
                let store_for_message = Arc::clone(&store);
                let handler_for_message = Arc::clone(&handler);
                let group_for_message = group_id.clone();
                let outcome = tokio::task::spawn_blocking(move || {
                    let engine = ConsumerEngine::new(
                        store_for_message.as_ref().clone(),
                        group_for_message,
                        max_attempts,
                    )
                    .map_err(|error| format!("{error:?}"))?;
                    engine
                        .consume(&event, offset, delivery_attempt, now, |event| {
                            handler_for_message(event)
                        })
                        .map_err(|error| format!("{error:?}"))
                })
                .await
                .map_err(|error| format!("consumer worker thread 失败: {error}"))??;
                match outcome {
                    ConsumerOutcome::Applied => {
                        message.double_ack().await.map_err(|error| {
                            report.ack_failures += 1;
                            format!("JetStream ACK 失败: {error}")
                        })?;
                        report.applied += 1;
                    }
                    ConsumerOutcome::Duplicate => {
                        message.double_ack().await.map_err(|error| {
                            report.ack_failures += 1;
                            format!("JetStream duplicate ACK 失败: {error}")
                        })?;
                        report.duplicates += 1;
                    }
                    ConsumerOutcome::DeadLettered => {
                        message.double_ack().await.map_err(|error| {
                            report.ack_failures += 1;
                            format!("JetStream dead-letter ACK 失败: {error}")
                        })?;
                        report.dead_lettered += 1;
                    }
                    ConsumerOutcome::Retried { error } => {
                        message
                            .ack_with(async_nats::jetstream::AckKind::Nak(None))
                            .await
                            .map_err(|ack_error| {
                                report.ack_failures += 1;
                                format!("JetStream NAK 失败: {ack_error}")
                            })?;
                        report.retried += 1;
                        report.last_error = Some(error);
                    }
                }
            }
            Ok(report)
        })
    }

    /// Pull and process through the atomic projection boundary. The handler
    /// returns a durable projection; SQLite/PostgreSQL/file stores commit it
    /// together with the processed marker and checkpoint before this adapter
    /// ACKs JetStream.
    pub fn consume_batch_with_projection<S, H>(
        &self,
        store: S,
        now: u64,
        limit: usize,
        handler: H,
    ) -> Result<NatsConsumerBatchReport, String>
    where
        S: TransactionalConsumerStateStore + Clone + Send + Sync + 'static,
        H: Fn(&OutboxEvent, &ConsumerCheckpoint) -> Result<ConsumerProjection, String>
            + Send
            + Sync
            + 'static,
    {
        if limit == 0 {
            return Ok(NatsConsumerBatchReport::default());
        }
        let consumer = self.consumer.clone();
        let group_id = self.group_id.clone();
        let max_attempts = self.max_attempts;
        let pull_expires = self.pull_expires;
        block_on_runtime(&self.runtime, async move {
            ConsumerEngine::new(store.clone(), group_id.clone(), max_attempts)
                .map_err(|error| format!("{error:?}"))?;
            let store = Arc::new(store);
            let handler = Arc::new(handler);
            let mut messages = consumer
                .fetch()
                .max_messages(limit)
                .expires(pull_expires)
                .messages()
                .await
                .map_err(|error| format!("JetStream pull 请求失败: {error}"))?;
            let mut report = NatsConsumerBatchReport::default();
            while let Some(message) = messages.next().await {
                let message =
                    message.map_err(|error| format!("JetStream 消息读取失败: {error}"))?;
                report.received += 1;
                let info = message
                    .info()
                    .map_err(|error| format!("JetStream 消息元数据解析失败: {error}"))?;
                let offset = info.stream_sequence;
                let delivery_attempt = info.delivered.max(1) as u32;
                let event = match serde_json::from_slice::<OutboxEvent>(&message.payload) {
                    Ok(event) => event,
                    Err(error) => {
                        let error = format!("Outbox envelope 解析失败: {error}");
                        message
                            .double_ack_with(async_nats::jetstream::AckKind::Term)
                            .await
                            .map_err(|ack_error| {
                                report.ack_failures += 1;
                                format!("{error}; poison message 终止确认失败: {ack_error}")
                            })?;
                        report.malformed += 1;
                        report.last_error = Some(error);
                        continue;
                    }
                };
                let store_for_message = Arc::clone(&store);
                let handler_for_message = Arc::clone(&handler);
                let group_for_message = group_id.clone();
                let outcome = tokio::task::spawn_blocking(move || {
                    let engine = ConsumerEngine::new(
                        store_for_message.as_ref().clone(),
                        group_for_message,
                        max_attempts,
                    )
                    .map_err(|error| format!("{error:?}"))?;
                    engine
                        .consume_with_projection(
                            &event,
                            offset,
                            delivery_attempt,
                            now,
                            |event, checkpoint| handler_for_message(event, checkpoint),
                        )
                        .map_err(|error| format!("{error:?}"))
                })
                .await
                .map_err(|error| format!("consumer worker thread 失败: {error}"))??;
                match outcome {
                    ConsumerOutcome::Applied => {
                        message.double_ack().await.map_err(|error| {
                            report.ack_failures += 1;
                            format!("JetStream ACK 失败: {error}")
                        })?;
                        report.applied += 1;
                    }
                    ConsumerOutcome::Duplicate => {
                        message.double_ack().await.map_err(|error| {
                            report.ack_failures += 1;
                            format!("JetStream duplicate ACK 失败: {error}")
                        })?;
                        report.duplicates += 1;
                    }
                    ConsumerOutcome::DeadLettered => {
                        message.double_ack().await.map_err(|error| {
                            report.ack_failures += 1;
                            format!("JetStream dead-letter ACK 失败: {error}")
                        })?;
                        report.dead_lettered += 1;
                    }
                    ConsumerOutcome::Retried { error } => {
                        message
                            .ack_with(async_nats::jetstream::AckKind::Nak(None))
                            .await
                            .map_err(|ack_error| {
                                report.ack_failures += 1;
                                format!("JetStream NAK 失败: {ack_error}")
                            })?;
                        report.retried += 1;
                        report.last_error = Some(error);
                    }
                }
            }
            Ok(report)
        })
    }

    /// Current Unix timestamp in seconds, suitable for `ConsumerCheckpoint`.
    pub fn current_timestamp() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_secs())
    }
}

fn block_on_runtime<F, T>(runtime: &Arc<Runtime>, future: F) -> Result<T, String>
where
    F: Future<Output = Result<T, String>> + Send + 'static,
    T: Send + 'static,
{
    if tokio::runtime::Handle::try_current().is_ok() {
        let runtime = Arc::clone(runtime);
        std::thread::Builder::new()
            .name("qianxing-nats-publisher".into())
            .spawn(move || runtime.block_on(future))
            .map_err(|error| format!("NATS publisher thread 启动失败: {error}"))?
            .join()
            .map_err(|_| "NATS publisher thread 异常退出".to_string())?
    } else {
        runtime.block_on(future)
    }
}

fn validate_subject(subject: &str) -> Result<(), String> {
    if subject.trim().is_empty()
        || subject.chars().any(char::is_whitespace)
        || subject.contains('>')
        || subject.contains('*')
    {
        return Err(format!("NATS subject 非法: {subject}"));
    }
    Ok(())
}
