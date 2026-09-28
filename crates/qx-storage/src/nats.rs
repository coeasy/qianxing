//! Optional NATS JetStream publisher for the generic Outbox relay.

use super::{
    ConsumerCheckpoint, ConsumerEngine, ConsumerOutcome, ConsumerProjection, ConsumerStateStore,
    OutboxEvent, OutboxPublisher, TransactionalConsumerStateStore,
};
use async_nats::jetstream;
use futures_util::StreamExt;
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::runtime::Runtime;

/// 建链预算：连接与消费端装配最多等多久，超时给出具名错误而不是把启动挂死。
const NATS_BOOT_BUDGET: Duration = Duration::from_secs(10);
/// 单条投递的 ack 预算：Outbox relay 每轮每个事件最多等多久。
const NATS_PUBLISH_ACK_BUDGET: Duration = Duration::from_secs(5);

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
    /// ack 等待用尽后置真：本进程不再投递，否则每轮都会留下一个永不落回的孤儿任务。
    wedged: Arc<AtomicBool>,
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
    pub fn connect(url: &str, subject_prefix: &str) -> Result<Self, String> {
        if url.trim().is_empty() || subject_prefix.trim().is_empty() {
            return Err("NATS url 和 subject_prefix 不能为空".into());
        }
        validate_subject(subject_prefix)?;
        let runtime = Arc::new(
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .map_err(|error| format!("NATS Tokio runtime 初始化失败: {error}"))?,
        );
        let url = url.to_string();
        let client = block_on_runtime_within(
            &runtime,
            async move {
                async_nats::connect(url)
                    .await
                    .map_err(|error| error.to_string())
            },
            NATS_BOOT_BUDGET,
        )
        .map_err(|_| format!("NATS 连接在 {:?} 内没有回话", NATS_BOOT_BUDGET))?
        .map_err(|error| format!("NATS 连接失败: {error}"))?;
        Ok(Self {
            context: jetstream::new(client),
            runtime,
            subject_prefix: subject_prefix.trim_end_matches('.').into(),
            wedged: Arc::new(AtomicBool::new(false)),
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
        if self.wedged.load(Ordering::Relaxed) {
            return Err(
                "NATS publisher 前一笔 ack 等待已用尽，本进程不再投递（重启 worker 才会重连）"
                    .into(),
            );
        }
        let subject = self.subject_for(event)?;
        let payload = serde_json::to_vec(event)
            .map_err(|error| format!("NATS Outbox envelope 序列化失败: {error}"))?;
        let context = self.context.clone();
        match block_on_runtime_within(
            &self.runtime,
            async move {
                let ack = context
                    .publish(subject, payload.into())
                    .await
                    .map_err(|error| format!("JetStream publish 请求失败: {error}"))?;
                ack.await
                    .map_err(|error| format!("JetStream publish ack 失败: {error}"))?;
                Ok::<(), String>(())
            },
            NATS_PUBLISH_ACK_BUDGET,
        ) {
            Ok(inner) => inner,
            Err(_) => {
                // 预算用尽时那次 ack 仍可能晚点落在 runtime 里，再投只会每轮攒一个孤儿任务。
                self.wedged.store(true, Ordering::Relaxed);
                Err(format!(
                    "JetStream publish ack 在 {:?} 内没有回话，已停用本进程的 NATS 投递",
                    NATS_PUBLISH_ACK_BUDGET
                ))
            }
        }
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
        let runtime = Arc::new(
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .map_err(|error| format!("NATS Tokio runtime 初始化失败: {error}"))?,
        );
        let url = url.to_string();
        let stream = stream.to_string();
        let consumer_name = consumer.to_string();
        let jetstream_consumer = block_on_runtime_within(
            &runtime,
            async move {
                let client = async_nats::connect(url)
                    .await
                    .map_err(|error| error.to_string())?;
                jetstream::new(client)
                    .get_stream(stream)
                    .await
                    .map_err(|error| error.to_string())?
                    .get_consumer(&consumer_name)
                    .await
                    .map_err(|error| error.to_string())
            },
            NATS_BOOT_BUDGET,
        )
        .map_err(|_| {
            format!(
                "NATS JetStream consumer 装配在 {:?} 内没有回话",
                NATS_BOOT_BUDGET
            )
        })?
        .map_err(|error| format!("NATS JetStream consumer 连接失败: {error}"))?;
        Ok(Self {
            runtime,
            consumer: jetstream_consumer,
            group_id: group_id.into(),
            max_attempts,
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
                .expires(std::time::Duration::from_secs(1))
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
        block_on_runtime(&self.runtime, async move {
            ConsumerEngine::new(store.clone(), group_id.clone(), max_attempts)
                .map_err(|error| format!("{error:?}"))?;
            let store = Arc::new(store);
            let handler = Arc::new(handler);
            let mut messages = consumer
                .fetch()
                .max_messages(limit)
                .expires(std::time::Duration::from_secs(1))
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

/// 在固定预算内等一次 await 落回调用线程：`Err(())` 只表示"预算用尽"，与链路自己的
/// 失败分开。任务留在 runtime 上跑完，调用方不再等它——否则一颗接得上话却从不答复的
/// broker 会把 pump 的每一轮都挂死，也让 relay 停在同一个位置不动。
fn block_on_runtime_within<F, T>(
    runtime: &Arc<Runtime>,
    future: F,
    budget: Duration,
) -> Result<Result<T, String>, ()>
where
    F: Future<Output = Result<T, String>> + Send + 'static,
    T: Send + 'static,
{
    let (sender, receiver) = std::sync::mpsc::channel();
    runtime.spawn(async move {
        let _ = sender.send(future.await);
    });
    match receiver.recv_timeout(budget) {
        Ok(outcome) => Ok(outcome),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Err(()),
        // sender 被丢弃只可能是等待任务自己退了，按链路失败报，不冒充超时。
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            Ok(Err("NATS 等待任务异常退出，这一笔没有落回".to_string()))
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn runtime() -> Arc<Runtime> {
        Arc::new(
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .enable_all()
                .build()
                .expect("测试用的多线程 runtime"),
        )
    }

    /// 预算内落回就是成功：投递链上的每一次等待都要能在自己的窗口里给出结果。
    #[test]
    fn bounded_wait_returns_the_completion_inside_its_budget() {
        let runtime = runtime();
        let outcome = block_on_runtime_within(
            &runtime,
            async { Ok::<&str, String>("done") },
            Duration::from_secs(5),
        );
        assert_eq!(outcome, Ok(Ok("done")));
    }

    /// 永不答复的 await 必须在预算内报成"超时"这颗名，而不是把调用线程一起带走：
    /// broker 接得上话却回不了 ack 时，relay 停在同一位置不动就是断链。
    #[test]
    fn bounded_wait_names_a_reply_that_never_lands() {
        let runtime = runtime();
        // sender 留在测试线程这一侧且一直活着：receiver 只有等死这一条路，
        // 这正是"broker 接得上话却回不了 ack"的形状。
        let (_sender, never) = tokio::sync::oneshot::channel::<()>();
        let outcome = block_on_runtime_within(
            &runtime,
            async move {
                let _ = never.await;
                Ok::<(), String>(())
            },
            Duration::from_millis(120),
        );
        assert_eq!(
            outcome,
            Err(()),
            "预算用尽没报成超时，调用方会以为链路还活着"
        );
    }

    /// 三处 await 走的必须是带预算的那颗：任一处换回不带预算的兄弟，这棵树就红。
    #[test]
    fn every_nats_await_site_goes_through_the_bounded_wait() {
        let source = include_str!("nats.rs");
        let production = source
            .split_once("#[cfg(test)]")
            .expect("nats.rs 的测试模块标记不在源里")
            .0;
        assert_eq!(
            production.matches("block_on_runtime_within(").count(),
            3,
            "带预算的等待必须恰好落在连接、ack 与消费端装配三处"
        );
        // 无截止的两处只能是批量拉取：它们靠 JetStream pull 的到期收尾，
        // 半路掐断会把已经落盘的 ACK 留在孤儿任务里，比原地等待更危险。
        assert_eq!(
            production.matches("block_on_runtime(&self.runtime").count(),
            2,
            "不带截止的等待只允许 consume_batch 与 consume_batch_with_projection 两处"
        );
        assert_eq!(
            production
                .matches(".expires(std::time::Duration::from_secs(1))")
                .count(),
            2,
            "批量拉取的 pull 到期被摘掉，那两处无截止等待就没有兜底了"
        );
        assert!(
            production.contains("NATS_PUBLISH_ACK_BUDGET")
                && production.contains("NATS_BOOT_BUDGET"),
            "两颗预算常量有一颗没了引用点，就是有人把截止摘了"
        );
    }

    /// 停投递的闩必须两头都在：只置位不读就是每轮再攒一个孤儿任务，只读不置位就是根空守卫。
    #[test]
    fn the_wedged_latch_has_exactly_one_setter_and_one_reader() {
        let source = include_str!("nats.rs");
        let production = source
            .split_once("#[cfg(test)]")
            .expect("nats.rs 的测试模块标记不在源里")
            .0;
        assert_eq!(
            production.matches("self.wedged.store(true").count(),
            1,
            "闩的置位点必须只有一颗，多一处就是在别处偷偷停投递"
        );
        assert_eq!(
            production
                .matches("self.wedged.load(Ordering::Relaxed)")
                .count(),
            1,
            "闩的读取点必须只有一颗，没人读的话它就只是一块写不出的内存"
        );
    }
}
