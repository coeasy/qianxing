//! P1c（V10 §4.9）验收：一套信封写路径的跨后端语义一致性。
//!
//! 文件后端（信封路径）与 SQLite 后端必须在同一断言序列下给出同一可观察语义：
//! 幂等追加、租约冲突、重试尝试计数、schema/版本拒绝与损坏拒绝。
//! PostgreSQL 契约用例保持 `#[ignore]`（本轮不允许外部服务 / 凭据）。

use qx_storage::{
    ConsumerStateStore, FileConsumerStateStore, FileJobQueue, FileOutboxStore, JsonStateStore,
    OutboxEvent, OutboxStore, StorageError,
};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(feature = "sqlite")]
use qx_storage::SqliteOutboxStore;

fn temp_root(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "qianxing-p1c-envelope-{label}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock must be after unix epoch")
            .as_nanos()
    ))
}

fn event(event_id: &str) -> OutboxEvent {
    OutboxEvent {
        event_id: event_id.into(),
        topic: "qx.p1c".into(),
        partition_key: "acct-1".into(),
        sequence: 7,
        schema_version: 1,
        trace_id: "trace-1".into(),
        payload: "{\"v\":1}".into(),
        created_ts: 100,
        attempts: 0,
    }
}

/// 跨后端共享的 Outbox 语义序列：返回每次 `available` 观察到的 attempts 轨迹。
fn drive_outbox_semantics(store: &dyn OutboxStore) -> Vec<u32> {
    // 版本为 0 / 未来的事件在所有后端一律被拒绝（Conflict），语义单点在 validate。
    let zero = OutboxEvent {
        schema_version: 0,
        ..event("zero-version")
    };
    assert!(matches!(
        store.append_outbox(zero),
        Err(StorageError::Conflict(_))
    ));
    let too_new = OutboxEvent {
        schema_version: 2,
        ..event("future-version")
    };
    assert!(matches!(
        store.append_outbox(too_new),
        Err(StorageError::Conflict(_))
    ));

    store.append_outbox(event("p1c-event")).unwrap();
    // 幂等：同一事实重复追加必须成功；不同事实必须冲突。
    store.append_outbox(event("p1c-event")).unwrap();
    let mutated = OutboxEvent {
        payload: "{\"v\":2}".into(),
        ..event("p1c-event")
    };
    assert!(matches!(
        store.append_outbox(mutated),
        Err(StorageError::Conflict(_))
    ));

    let mut attempts_trace = Vec::new();
    let lease = store.claim_outbox("p1c-event", "relay-a", 101, 10).unwrap();
    assert!(matches!(
        store.claim_outbox("p1c-event", "relay-b", 102, 10),
        Err(StorageError::LeaseHeld { .. })
    ));
    // 重试两次：attempts 依次 1、2（口径来自 qx-core::retry 唯一实现）。
    store
        .retry_outbox("p1c-event", "relay-a", lease.fencing_token, 102)
        .unwrap();
    attempts_trace.push(store.available_outbox(103).unwrap()[0].attempts);
    let second = store.claim_outbox("p1c-event", "relay-a", 103, 10).unwrap();
    store
        .retry_outbox("p1c-event", "relay-a", second.fencing_token, 104)
        .unwrap();
    attempts_trace.push(store.available_outbox(105).unwrap()[0].attempts);
    // ack 后事件消失，租约清理。
    let third = store.claim_outbox("p1c-event", "relay-a", 105, 10).unwrap();
    store
        .ack_outbox("p1c-event", "relay-a", third.fencing_token, 106)
        .unwrap();
    assert!(store.available_outbox(107).unwrap().is_empty());
    attempts_trace
}

#[test]
fn file_outbox_envelope_semantics_are_stable() {
    let root = temp_root("outbox-file");
    let store = FileOutboxStore::new(&root);
    assert_eq!(drive_outbox_semantics(&store), vec![1, 2]);
    let _ = std::fs::remove_dir_all(root);
}

#[cfg(feature = "sqlite")]
#[test]
fn file_and_sqlite_share_outbox_envelope_semantics() {
    let root = temp_root("outbox-consistency");
    let file_store = FileOutboxStore::new(root.join("file-side"));
    let sqlite_path = root.join("consistency.db");
    let sqlite_store = SqliteOutboxStore::new(sqlite_path).unwrap();
    let file_trace = drive_outbox_semantics(&file_store);
    let sqlite_trace = drive_outbox_semantics(&sqlite_store);
    assert_eq!(file_trace, sqlite_trace);
    let _ = std::fs::remove_dir_all(root);
}

/// 信封磁盘兼容回归：四个存储写出的 JSON key 集合与迁移前逐字一致，
/// 信封不得悄悄新增版本 key 或改名既有 key。
#[test]
fn envelope_keeps_on_disk_json_shapes() {
    let root = temp_root("shapes");

    // 1) consumer 状态文件：{checkpoint, processed_event_ids, dead_letters, projections}
    let consumer = FileConsumerStateStore::new(&root);
    let checkpoint = qx_checkpoint("cg", "t-1", "p-1", 3, "evt-1");
    consumer.commit_processed(checkpoint).unwrap();
    let consumer_file = single_json_in(&root.join("consumers"));
    assert_eq!(
        json_keys(&consumer_file),
        vec![
            "checkpoint".to_string(),
            "dead_letters".to_string(),
            "processed_event_ids".to_string(),
            "projections".to_string()
        ]
    );

    // 2) outbox 事件文件：9 个既有 key（含内嵌 schema_version），无新 key。
    let outbox = FileOutboxStore::new(&root);
    outbox.append(event("shape-event")).unwrap();
    let event_file = single_json_in(&root.join("outbox/events"));
    assert_eq!(
        json_keys(&event_file),
        vec![
            "attempts".to_string(),
            "created_ts".to_string(),
            "event_id".to_string(),
            "partition_key".to_string(),
            "payload".to_string(),
            "schema_version".to_string(),
            "sequence".to_string(),
            "topic".to_string(),
            "trace_id".to_string()
        ]
    );

    // 3) 任务队列文件：{job, run, enqueued_ts}
    let queue = FileJobQueue::new(root.join("jobs"));
    let (job, run) = qx_job_and_run("a");
    queue.enqueue(job, run, 900).unwrap();
    let queued_file = single_json_in(&root.join("jobs/queue"));
    assert_eq!(
        json_keys(&queued_file),
        vec![
            "enqueued_ts".to_string(),
            "job".to_string(),
            "run".to_string()
        ]
    );

    // 4) JsonStateStore 通用状态仍是受约束的 pretty JSON，路径约束不变。
    let state = JsonStateStore::new(root.join("states"));
    let payload = event("json-state");
    let saved = state.save_json_at("reports/outbox.json", &payload).unwrap();
    let loaded: OutboxEvent = state.load_json_at("reports/outbox.json").unwrap();
    assert_eq!(loaded, payload);
    assert!(std::fs::read_to_string(&saved).unwrap().contains('\n'));
    assert!(matches!(
        state.save_json_at("../escape.json", &payload),
        Err(StorageError::InvalidName(_))
    ));
    let _ = std::fs::remove_dir_all(root);
}

/// 损坏文件一律被信封拒绝（解析失败 → Io），且不影响其余文件读取。
#[test]
fn envelope_rejects_corrupt_state_files() {
    let root = temp_root("corrupt");
    let outbox = FileOutboxStore::new(&root);
    outbox.append(event("corrupt-event")).unwrap();
    let event_file = single_json_in(&root.join("outbox/events"));
    std::fs::write(&event_file, "{\"event_id\": truncated").unwrap();
    assert!(matches!(outbox.available(999), Err(StorageError::Io(_))));

    let consumer = FileConsumerStateStore::new(&root);
    consumer
        .commit_processed(qx_checkpoint("cg", "t-1", "p-1", 3, "evt-1"))
        .unwrap();
    let consumer_file = single_json_in(&root.join("consumers"));
    std::fs::write(&consumer_file, "{oops").unwrap();
    assert!(matches!(
        consumer.load_checkpoint("cg", "t-1", "p-1"),
        Err(StorageError::Io(_))
    ));

    let queue = FileJobQueue::new(root.join("jobs"));
    let (job, run) = qx_job_and_run("b");
    queue.enqueue(job, run, 900).unwrap();
    let queued_file = single_json_in(&root.join("jobs/queue"));
    std::fs::write(&queued_file, "[]").unwrap();
    assert!(matches!(queue.pending(), Err(StorageError::Io(_))));

    let _ = std::fs::remove_dir_all(root);
}

/// 未来版本的文件（哪怕 JSON 合法）也必须被版本闸门拒绝，防止旧读者吞掉新事实。
#[test]
fn envelope_rejects_future_schema_version_on_read() {
    let root = temp_root("future-version");
    let outbox = FileOutboxStore::new(&root);
    outbox.append(event("versioned")).unwrap();
    let event_file = single_json_in(&root.join("outbox/events"));
    let text = std::fs::read_to_string(&event_file).unwrap();
    let bumped = text.replace("\"schema_version\":1", "\"schema_version\":9");
    assert_ne!(bumped, text, "fixture must contain embedded version key");
    std::fs::write(&event_file, bumped).unwrap();
    assert!(matches!(
        outbox.available(999),
        Err(StorageError::Conflict(_))
    ));
    let _ = std::fs::remove_dir_all(root);
}

fn json_keys(path: &Path) -> Vec<String> {
    let text = std::fs::read_to_string(path).unwrap();
    let value: serde_json::Value = serde_json::from_str(&text).unwrap();
    let mut keys: Vec<String> = value
        .as_object()
        .expect("state file must be a JSON object")
        .keys()
        .cloned()
        .collect();
    keys.sort();
    keys
}

fn single_json_in(dir: &Path) -> PathBuf {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|entry| {
            let path = entry.unwrap().path();
            (path.extension().and_then(|value| value.to_str()) == Some("json")).then_some(path)
        })
        .collect();
    files.sort();
    assert_eq!(
        files.len(),
        1,
        "expected exactly one JSON state file in {dir:?}"
    );
    files.pop().unwrap()
}

fn qx_checkpoint(
    group_id: &str,
    topic: &str,
    partition_key: &str,
    offset: u64,
    event_id: &str,
) -> qx_storage::ConsumerCheckpoint {
    qx_storage::ConsumerCheckpoint {
        group_id: group_id.into(),
        topic: topic.into(),
        partition_key: partition_key.into(),
        offset,
        event_id: event_id.into(),
        updated_ts: 50,
    }
}

fn qx_job_and_run(run_id_seed: &str) -> (qx_scheduler::JobSpec, qx_scheduler::JobRun) {
    use qx_scheduler::{JobRun, JobSpec, JobStatus, JobWindow, RetryPolicy, Trigger};
    let job = JobSpec {
        job_id: format!("p1c-job-{run_id_seed}"),
        job_version: "1".into(),
        owner: "ops".into(),
        enabled: true,
        trigger: Trigger::Manual,
        window: JobWindow::Any,
        depends_on: Vec::new(),
        input_refs: Vec::new(),
        output_refs: Vec::new(),
        timeout_seconds: 60,
        retry_policy: RetryPolicy::default(),
        concurrency_key: format!("p1c-{run_id_seed}"),
        idempotency_key: format!("p1c-{run_id_seed}"),
        permission_scope: "read".into(),
        audit_reason: "p1c test".into(),
        dry_run: false,
    };
    let run = JobRun {
        run_id: job.stable_key("20260920"),
        job_id: job.job_id.clone(),
        trading_day: "20260920".into(),
        attempt: 1,
        status: JobStatus::Running,
        manifest_digest: Some(7),
        error_code: None,
        next_retry_ts: None,
        started_ts: 900,
        deadline_ts: 960,
    };
    (job, run)
}
