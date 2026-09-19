//! NATS JetStream 后端契约测试。
//!
//! 无 broker 也能跑的输入校验放在常规测试里；真正的一发一收需要服务容器提供
//! JetStream。CI 的 `service-backends` 作业会拉起 `nats -js` 并注入
//! `QX_TEST_NATS_URL` / `QX_TEST_NATS_SUBJECT_PREFIX`，再以 `--ignored` 运行这些用例。
//! 流与消费者由本测试自行创建、用完即删，因此不依赖任何部署侧脚本，也不会被
//! 历史残留消息污染。

#![cfg(feature = "nats")]

use qx_storage::{
    FileConsumerStateStore, NatsJetStreamConsumer, NatsJetStreamPublisher, OutboxEvent,
    OutboxPublisher,
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
    let root = std::env::temp_dir().join(format!(
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
    let stream = format!("{}_STREAM", prefix.to_ascii_uppercase());
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

fn with_context<F, Fut>(url: &str, work: F) -> Result<(), String>
where
    F: FnOnce(async_nats::jetstream::Context) -> Fut,
    Fut: std::future::Future<Output = Result<(), String>>,
{
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("NATS provisioning runtime");
    let client = runtime
        .block_on(async_nats::connect(url))
        .map_err(|error| format!("连接 NATS 失败: {error}"))?;
    runtime.block_on(work(async_nats::jetstream::new(client)))
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
    let error = NatsJetStreamPublisher::connect("", "qx").unwrap_err();
    assert!(error.contains("不能为空"), "空 url 必须被拒绝: {error}");
    let error = NatsJetStreamPublisher::connect("nats://127.0.0.1:4222", "").unwrap_err();
    assert!(
        error.contains("不能为空"),
        "空 subject_prefix 必须被拒绝: {error}"
    );
    let error = NatsJetStreamConsumer::connect("nats://127.0.0.1:4222", "", "worker", "grp", 3)
        .unwrap_err();
    assert!(
        error.contains("stream 不能为空"),
        "空 stream 必须被拒绝: {error}"
    );
    let error = NatsJetStreamConsumer::connect("nats://127.0.0.1:4222", "QX", "worker", "grp", 0)
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
    let publisher = NatsJetStreamPublisher::connect(&url, &subject_prefix)
        .expect("connect NATS publisher; is the broker and stream available?");
    let consumer = NatsJetStreamConsumer::connect(&url, &stream, &consumer_name, &run, 3)
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
    for sequence in 0..3_u64 {
        publisher
            .publish(&event(
                format!("{run}-{sequence}"),
                sequence,
                &format!("trace-{run}"),
            ))
            .expect("republish must be acknowledged");
    }
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
