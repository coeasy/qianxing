//! NATS JetStream 后端契约测试。
//!
//! 无 broker 也能跑的输入校验放在常规测试里；真正的一发一收需要服务容器提供
//! JetStream。CI 的 `service-backends` 作业会拉起 `nats -js` 并注入
//! `QX_TEST_NATS_URL` / `QX_TEST_NATS_SUBJECT_PREFIX`，再以 `--ignored` 运行这些用例。
//! 流与消费者由本测试自行创建、用完即删，因此不依赖任何部署侧脚本，也不会被
//! 历史残留消息污染。

#![cfg(feature = "nats")]

use qx_storage::{
    ConsumerEngine, ConsumerOutcome, ConsumerProjection, FileConsumerStateStore,
    NatsJetStreamConsumer, NatsJetStreamPublisher, NatsWaitBudget, OutboxEvent, OutboxPublisher,
    TransactionalConsumerStateStore,
};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const TOPIC: &str = "qianxing.outbox.acceptance";

fn now_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock must be after unix epoch")
        .as_nanos()
}

fn temp_root(label: &str) -> PathBuf {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target")
        .join(format!(
            "qianxing-nats-{label}-{}-{}",
            std::process::id(),
            now_nanos()
        ));
    std::fs::create_dir_all(&root).expect("创建 NATS 契约测试目录失败");
    root
}

/// 本次运行独占的一组 JetStream 资源标识。
fn broker(label: &str) -> (String, String, String, String) {
    let required = |key: &str| std::env::var(key).unwrap_or_else(|_| panic!("{key} must be set"));
    let prefix = required("QX_TEST_NATS_SUBJECT_PREFIX");
    let stream_name_prefix = prefix.replace(['.', '_'], "-");
    let stream = format!("{}-STREAM", stream_name_prefix.to_ascii_uppercase());
    let url = required("QX_TEST_NATS_URL");
    let consumer = format!("qx-acceptance-{label}");
    provision(&url, &stream, &consumer, &prefix);
    (url, stream, consumer, prefix)
}

/// 幂等地创建 JetStream 流；消费者按运行独占创建，只收新建之后的消息。
fn provision(url: &str, stream: &str, consumer: &str, subject_prefix: &str) {
    use async_nats::jetstream;

    let filter = format!("{subject_prefix}.>");
    let result = with_context(url, move |context| async move {
        let created = match context
            .create_stream(jetstream::stream::Config {
                name: stream.into(),
                subjects: vec![filter.clone()],
                ..Default::default()
            })
            .await
        {
            Ok(created) => created,
            Err(error) => context
                .get_stream(stream)
                .await
                .map_err(|lookup| format!("创建流失败: {error}; 读取既有流失败: {lookup}"))?,
        };
        created
            .create_consumer(jetstream::consumer::pull::Config {
                name: Some(consumer.into()),
                filter_subject: filter,
                max_deliver: 10,
                ack_wait: Duration::from_secs(1),
                deliver_policy: async_nats::jetstream::consumer::DeliverPolicy::New,
                ..Default::default()
            })
            .await
            .map(|_| ())
            .map_err(|error| format!("创建消费者失败: {error}"))
    });
    if let Err(error) = result {
        panic!("预置 NATS JetStream 测试资源失败: {error}");
    }
}

/// 删除本次运行的消费者；流留给后续运行复用。
fn deprovision(url: &str, stream: &str, consumer: &str) {
    let result = with_context(url, move |context| async move {
        context
            .get_stream(stream)
            .await
            .map_err(|error| error.to_string())?
            .delete_consumer(consumer)
            .await
            .map(|_| ())
            .map_err(|error| error.to_string())
    });
    if let Err(error) = result {
        eprintln!("清理 NATS 测试消费者失败（不影响断言）: {error}");
    }
}

fn with_context<F, Fut, T>(url: &str, work: F) -> Result<T, String>
where
    F: FnOnce(async_nats::jetstream::Context) -> Fut,
    Fut: std::future::Future<Output = Result<T, String>>,
    T: Send + 'static,
{
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("NATS provisioning runtime");
    runtime.block_on(async {
        let client = async_nats::connect(url)
            .await
            .map_err(|error| format!("连接 NATS 失败: {error}"))?;
        let context = async_nats::jetstream::new(client);
        work(context).await
    })
}

/// Simulate a process exit after the transactional projection/checkpoint commit
/// but before the JetStream ACK reaches the broker. The broker redelivers after
/// `ack_wait`; the durable event-id marker must turn that delivery into a
/// duplicate and the reducer must not run a second time.
#[test]
#[ignore = "requires QX_TEST_NATS_URL and QX_TEST_NATS_SUBJECT_PREFIX"]
fn nats_redelivery_after_projection_commit_before_ack_is_idempotent() {
    use futures_util::StreamExt;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let run = format!("{}-{}", std::process::id(), now_nanos());
    let (url, stream, consumer_name, subject_prefix) = broker(&run);
    let publisher =
        NatsJetStreamPublisher::connect(&url, &subject_prefix, NatsWaitBudget::default())
            .expect("connect NATS publisher");
    let root = temp_root("redelivery-before-ack");
    let store = FileConsumerStateStore::new(&root);
    let event = event(format!("{run}-event",), 1, &format!("trace-{run}"));
    publisher
        .publish(&event)
        .expect("JetStream must persist the event before delivery");

    // Pull the first delivery with a raw client and intentionally drop it
    // without ACKing, just as the consumer process would on abrupt exit.
    let (delivered, offset) = with_context(&url, {
        let stream = stream.clone();
        let consumer_name = consumer_name.clone();
        move |context| async move {
            let mut messages = context
                .get_stream(stream)
                .await
                .map_err(|error| error.to_string())?
                .get_consumer(&consumer_name)
                .await
                .map_err(|error| error.to_string())?
                .fetch()
                .max_messages(1)
                .expires(Duration::from_secs(3))
                .messages()
                .await
                .map_err(|error| error.to_string())?;
            let message = messages
                .next()
                .await
                .ok_or_else(|| "JetStream pull returned no message".to_string())?
                .map_err(|error| error.to_string())?;
            let info = message.info().map_err(|error| error.to_string())?;
            let offset = info.stream_sequence;
            let event = serde_json::from_slice::<OutboxEvent>(&message.payload)
                .map_err(|error| error.to_string())?;
            drop(message);
            drop(messages);
            Ok((event, offset))
        }
    })
    .expect("first delivery must be readable");
    assert_eq!(delivered, event);

    // Persist the business projection and processed marker. Returning from this
    // point while omitting ACK is the crash boundary under test.
    let reducer_calls = Arc::new(AtomicUsize::new(0));
    let engine = ConsumerEngine::new(store.clone(), &run, 3).expect("consumer engine");
    let first_calls = Arc::clone(&reducer_calls);
    assert_eq!(
        engine
            .consume_with_projection(&delivered, offset, 1, 10, move |event, checkpoint| {
                first_calls.fetch_add(1, Ordering::SeqCst);
                Ok(ConsumerProjection::for_checkpoint(
                    checkpoint,
                    event.partition_key.clone(),
                    "{\"applied\":true}",
                ))
            })
            .expect("projection/checkpoint commit must succeed"),
        ConsumerOutcome::Applied
    );
    assert!(store
        .load_projection(&run, "acceptance")
        .expect("projection read")
        .is_some());

    // Let the explicit one-second ack deadline expire, then reconnect through
    // the production adapter. The second delivery must be recognized as a
    // duplicate, ACKed, and leave the reducer call count unchanged.
    std::thread::sleep(Duration::from_millis(1_250));
    let consumer = NatsJetStreamConsumer::connect(
        &url,
        &stream,
        &consumer_name,
        &run,
        3,
        NatsWaitBudget::default(),
    )
    .expect("connect durable consumer after simulated restart");
    let duplicate_calls = Arc::clone(&reducer_calls);
    let report = consumer
        .consume_batch_with_projection(store.clone(), 11, 1, move |_, _| {
            duplicate_calls.fetch_add(1, Ordering::SeqCst);
            Err("duplicate delivery must not invoke reducer".into())
        })
        .expect("redelivery must be consumed and acknowledged");
    assert_eq!(
        report.received, 1,
        "the unacked message must be redelivered"
    );
    assert_eq!(report.duplicates, 1);
    assert_eq!(report.ack_failures, 0);
    assert_eq!(reducer_calls.load(Ordering::SeqCst), 1);

    let _ = std::fs::remove_dir_all(root);
    deprovision(&url, &stream, &consumer_name);
}

/// Publish the same business event under a fresh broker message ID so the
/// consumer's durable event-id dedupe is exercised independently of JetStream's
/// bounded publish dedupe window.
fn publish_duplicate_payloads(
    url: &str,
    subject_prefix: &str,
    events: Vec<OutboxEvent>,
    run: &str,
) -> Result<(), String> {
    use async_nats::jetstream::message::PublishMessage;

    let subject_prefix = subject_prefix.trim_end_matches('.').to_string();
    let replay = events
        .into_iter()
        .enumerate()
        .map(|(index, event)| {
            let subject = format!("{}.{}", subject_prefix, event.topic.replace(':', "."));
            let payload = serde_json::to_vec(&event)
                .map_err(|error| format!("重复事件序列化失败: {error}"))?;
            Ok((subject, payload, format!("{run}-replay-{index}")))
        })
        .collect::<Result<Vec<_>, String>>()?;

    with_context(url, move |context| async move {
        for (subject, payload, message_id) in replay {
            context
                .send_publish(
                    subject,
                    PublishMessage::build()
                        .message_id(message_id)
                        .payload(payload.into()),
                )
                .await
                .map_err(|error| format!("JetStream duplicate publish failed: {error}"))?
                .await
                .map_err(|error| format!("JetStream duplicate publish ack failed: {error}"))?;
        }
        Ok(())
    })
}

fn event(event_id: String, sequence: u64, trace_id: &str) -> OutboxEvent {
    OutboxEvent {
        event_id,
        topic: TOPIC.into(),
        partition_key: "acceptance".into(),
        sequence,
        schema_version: 1,
        trace_id: trace_id.into(),
        payload: format!("{{\"sequence\":{sequence}}}"),
        created_ts: 10,
        attempts: 0,
    }
}

/// 累计若干次有界拉取，直到 `target` 条消息被处理完或超时。
///
/// 返回 `(applied, duplicates, ack_failures)`；`seen` 记录业务副作用真正见过的事件号。
fn drain(
    consumer: &NatsJetStreamConsumer,
    store: &FileConsumerStateStore,
    seen: &Arc<Mutex<Vec<String>>>,
    mut now: u64,
    target: u64,
) -> (u64, u64, u64) {
    let mut applied = 0;
    let mut duplicates = 0;
    let mut ack_failures = 0;
    for _ in 0..40 {
        now += 1;
        let handler = {
            let seen = Arc::clone(seen);
            move |event: &OutboxEvent| {
                seen.lock()
                    .expect("consumer sink must not be poisoned")
                    .push(event.event_id.clone());
                Ok(())
            }
        };
        let report = consumer
            .consume_batch(store.clone(), now, 16, handler)
            .expect("consume batch must not fail");
        applied += report.applied;
        duplicates += report.duplicates;
        ack_failures += report.ack_failures;
        if applied + duplicates >= target {
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    (applied, duplicates, ack_failures)
}

#[test]
fn nats_adapters_reject_blank_configuration_without_connecting() {
    let error = NatsJetStreamPublisher::connect("", "qx", NatsWaitBudget::default()).unwrap_err();
    assert!(error.contains("不能为空"), "空 url 必须被拒绝: {error}");
    let error =
        NatsJetStreamPublisher::connect("nats://127.0.0.1:4222", "", NatsWaitBudget::default())
            .unwrap_err();
    assert!(
        error.contains("不能为空"),
        "空 subject_prefix 必须被拒绝: {error}"
    );
    let error = NatsJetStreamConsumer::connect(
        "nats://127.0.0.1:4222",
        "",
        "worker",
        "grp",
        3,
        NatsWaitBudget::default(),
    )
    .unwrap_err();
    assert!(
        error.contains("stream 不能为空"),
        "空 stream 必须被拒绝: {error}"
    );
    let error = NatsJetStreamConsumer::connect(
        "nats://127.0.0.1:4222",
        "QX",
        "worker",
        "grp",
        0,
        NatsWaitBudget::default(),
    )
    .unwrap_err();
    assert!(
        error.contains("max_attempts"),
        "max_attempts 为 0 必须被拒绝: {error}"
    );
}

/// 端到端：Outbox 事件经 JetStream 落盘后由拉取消费者消费，重复投递按 event_id 幂等。
#[test]
#[ignore = "requires QX_TEST_NATS_URL and QX_TEST_NATS_SUBJECT_PREFIX"]
fn nats_jetstream_publish_consume_deduplicates_by_event_id() {
    let run = format!("{}-{}", std::process::id(), now_nanos());
    let (url, stream, consumer_name, subject_prefix) = broker(&run);
    let publisher =
        NatsJetStreamPublisher::connect(&url, &subject_prefix, NatsWaitBudget::default())
            .expect("connect NATS publisher; is the broker and stream available?");
    let consumer = NatsJetStreamConsumer::connect(
        &url,
        &stream,
        &consumer_name,
        &run,
        3,
        NatsWaitBudget::default(),
    )
    .expect("connect NATS consumer; is the durable pull consumer provisioned?");
    let root = temp_root("consumer");
    let store = FileConsumerStateStore::new(&root);
    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

    for sequence in 0..3_u64 {
        publisher
            .publish(&event(
                format!("{run}-{sequence}"),
                sequence,
                &format!("trace-{run}"),
            ))
            .expect("publish must be acknowledged by JetStream");
    }
    let (applied, duplicates, ack_failures) = drain(&consumer, &store, &seen, 100, 3);
    assert_eq!(ack_failures, 0, "ACK 失败说明 broker 或消费者配置异常");
    assert_eq!(duplicates, 0);
    assert_eq!(applied, 3, "三条事件必须各应用一次");
    let mut ids = seen.lock().expect("sink poisoned").clone();
    ids.sort();
    assert_eq!(
        ids,
        vec![format!("{run}-0"), format!("{run}-1"), format!("{run}-2")],
        "业务副作用必须恰好评收到三条事件"
    );

    // 同一批 event_id 再次发布：消费端识别为重复并仍然 ACK，业务副作用不重放。
    publish_duplicate_payloads(
        &url,
        &subject_prefix,
        (0..3_u64)
            .map(|sequence| {
                event(
                    format!("{run}-{sequence}"),
                    sequence,
                    &format!("trace-{run}"),
                )
            })
            .collect(),
        &run,
    )
    .expect("same business events under fresh broker IDs must be acknowledged");
    let (replay_applied, duplicates, ack_failures) = drain(&consumer, &store, &seen, 500, 3);
    assert_eq!(ack_failures, 0);
    assert_eq!(replay_applied, 0, "重复投递不得再次触发业务副作用");
    assert_eq!(duplicates, 3, "三条重复投递必须全部识别为 Duplicate");
    assert_eq!(
        seen.lock().expect("sink poisoned").len(),
        3,
        "去重后 handler 只能被调用三次"
    );
    let _ = std::fs::remove_dir_all(root);
    deprovision(&url, &stream, &consumer_name);
}
