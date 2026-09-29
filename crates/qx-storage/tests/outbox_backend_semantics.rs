use qx_storage::{
    outbox_exhausted, ConsumerEngine, ConsumerOutcome, ConsumerProjection, ConsumerStateStore,
    DeadLetterRecord, FileConsumerStateStore, FileOutboxStore, OutboxEvent, OutboxPublisher,
    OutboxRelay, OutboxStore, StorageError, TransactionalConsumerStateStore, OUTBOX_MAX_ATTEMPTS,
};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(feature = "postgres")]
use qx_storage::{PostgresConsumerStateStore, PostgresOutboxStore};
#[cfg(feature = "sqlite")]
use qx_storage::{SqliteConsumerStateStore, SqliteOutboxStore};

fn temp_root(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "qianxing-outbox-contract-{label}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock must be after unix epoch")
            .as_nanos()
    ))
}

fn event() -> OutboxEvent {
    OutboxEvent {
        event_id: "contract-event-1".into(),
        topic: "qx.order.events".into(),
        partition_key: "account-1".into(),
        sequence: 1,
        schema_version: 1,
        trace_id: "trace-1".into(),
        payload: "{\"kind\":\"filled\"}".into(),
        created_ts: 10,
        attempts: 0,
    }
}

fn consumer_event() -> OutboxEvent {
    OutboxEvent {
        event_id: "consumer-event-1".into(),
        topic: "qx.eventlog".into(),
        partition_key: "account-1".into(),
        sequence: 1,
        schema_version: 1,
        trace_id: String::new(),
        payload: "{}".into(),
        created_ts: 10,
        attempts: 0,
    }
}

/// 死信点查的跨后端契约：能按 `(group_id, event_id)` 点名命中、命中的是
/// `attempts` 最大的一行（要跨过 2→10 的数位边界，字典序会把第 9 次念成比
/// 第 10 次新），且没死信过的事件不能被念成存在。
fn assert_dead_letter_point_query<S>(store: S, event_id: &str)
where
    S: ConsumerStateStore + Clone,
{
    let latest = store
        .dead_letter("ledger-reducer", event_id)
        .unwrap()
        .expect("死信必须能按 event_id 点查命中");
    assert_eq!(
        (
            latest.attempts,
            latest.offset,
            latest.failed_ts,
            latest.error.as_str()
        ),
        (2, 6, 13, "permanent")
    );
    assert_eq!(latest.event.event_id, event_id);
    assert!(store
        .dead_letter("ledger-reducer", "consumer-event-1")
        .unwrap()
        .is_none());
    store
        .append_dead_letter(DeadLetterRecord {
            group_id: "ledger-reducer".into(),
            topic: "qx.eventlog".into(),
            partition_key: "account-1".into(),
            event_id: event_id.into(),
            offset: 14,
            attempts: 10,
            error: "permanent-after-crossing-into-two-digits".into(),
            failed_ts: 21,
            event: latest.event,
        })
        .unwrap();
    assert_eq!(
        store
            .dead_letter("ledger-reducer", event_id)
            .unwrap()
            .expect("跨过数位边界的死信行也必须点查得到")
            .attempts,
        10
    );
}

fn assert_consumer_semantics<S>(store: S)
where
    S: ConsumerStateStore + Clone,
{
    let engine = ConsumerEngine::new(store.clone(), "ledger-reducer", 2).unwrap();
    let first = consumer_event();
    assert_eq!(
        engine.consume(&first, 5, 1, 10, |_| Ok(())).unwrap(),
        ConsumerOutcome::Applied
    );
    assert_eq!(
        engine
            .consume(&first, 5, 1, 11, |_| panic!("duplicate handler invoked"))
            .unwrap(),
        ConsumerOutcome::Duplicate
    );
    let second = OutboxEvent {
        event_id: "consumer-event-2".into(),
        sequence: 2,
        ..first
    };
    assert!(matches!(
        engine
            .consume(&second, 6, 1, 12, |_| Err("temporary".into()))
            .unwrap(),
        ConsumerOutcome::Retried { .. }
    ));
    assert_eq!(
        engine
            .consume(&second, 6, 2, 13, |_| Err("permanent".into()))
            .unwrap(),
        ConsumerOutcome::DeadLettered
    );
    assert_dead_letter_point_query(store.clone(), "consumer-event-2");
}

fn assert_transactional_consumer_semantics<S>(store: S)
where
    S: TransactionalConsumerStateStore + Clone,
{
    let engine = ConsumerEngine::new(store.clone(), "ledger-reducer", 2).unwrap();
    let event = consumer_event();
    assert_eq!(
        engine
            .consume_with_projection(&event, 5, 1, 10, |event, checkpoint| {
                Ok(ConsumerProjection::for_checkpoint(
                    checkpoint,
                    event.partition_key.clone(),
                    "{\"applied\":true}",
                ))
            })
            .unwrap(),
        ConsumerOutcome::Applied
    );
    assert_eq!(
        store
            .load_projection("ledger-reducer", "account-1")
            .unwrap()
            .unwrap()
            .payload,
        "{\"applied\":true}"
    );
    assert_eq!(
        engine
            .consume_with_projection(&event, 5, 1, 11, |_, _| {
                panic!("atomic consumer duplicate must not invoke reducer")
            })
            .unwrap(),
        ConsumerOutcome::Duplicate
    );
    let failed = OutboxEvent {
        event_id: "consumer-event-2".into(),
        sequence: 2,
        ..event
    };
    assert!(matches!(
        engine
            .consume_with_projection(&failed, 6, 1, 12, |_, _| Err("temporary".into()))
            .unwrap(),
        ConsumerOutcome::Retried { .. }
    ));
    assert_eq!(
        engine
            .consume_with_projection(&failed, 6, 2, 13, |_, _| Err("permanent".into()))
            .unwrap(),
        ConsumerOutcome::DeadLettered
    );
    assert_dead_letter_point_query(store.clone(), "consumer-event-2");
}

fn assert_transactional_projection_failure_is_side_effect_free<S>(store: S)
where
    S: TransactionalConsumerStateStore + Clone,
{
    let engine = ConsumerEngine::new(store.clone(), "ledger-reducer", 2).unwrap();
    let event = consumer_event();
    let result = engine.consume_with_projection(&event, 5, 1, 10, |_, checkpoint| {
        let mut projection =
            ConsumerProjection::for_checkpoint(checkpoint, "account-1", "{\"valid\":true}");
        projection.group_id = "other-group".into();
        Ok(projection)
    });
    assert!(matches!(result, Err(StorageError::Conflict(_))));
    assert!(!store
        .is_processed("ledger-reducer", &event.event_id)
        .unwrap());
    assert!(store
        .load_checkpoint("ledger-reducer", &event.topic, &event.partition_key)
        .unwrap()
        .is_none());
    assert!(store
        .load_projection("ledger-reducer", "account-1")
        .unwrap()
        .is_none());
}

fn assert_semantics(store: &dyn OutboxStore) {
    store.append_outbox(event()).unwrap();
    store.append_outbox(event()).unwrap();
    let first = store
        .claim_outbox("contract-event-1", "relay-a", 10, 5)
        .unwrap();
    assert!(matches!(
        store.claim_outbox("contract-event-1", "relay-b", 11, 5),
        Err(StorageError::LeaseHeld { .. })
    ));
    assert!(matches!(
        store.ack_outbox("contract-event-1", "relay-a", first.fencing_token, 16),
        Err(StorageError::LeaseExpired { .. })
    ));
    let second = store
        .claim_outbox("contract-event-1", "relay-b", 16, 5)
        .unwrap();
    assert_eq!(second.fencing_token, first.fencing_token + 1);
    store
        .retry_outbox("contract-event-1", "relay-b", second.fencing_token, 17)
        .unwrap();
    store.append_outbox(event()).unwrap();
    assert_eq!(
        store.available_outbox(17, usize::MAX).unwrap()[0].attempts,
        1
    );
    let third = store
        .claim_outbox("contract-event-1", "relay-a", 17, 5)
        .unwrap();
    store
        .ack_outbox("contract-event-1", "relay-a", third.fencing_token, 18)
        .unwrap();
    assert!(store.available_outbox(18, usize::MAX).unwrap().is_empty());
}

/// 分区内的投递顺序口径：`(created_ts, sequence, event_id)` 按数字序，不是这三列
/// 存成 TEXT 时 SQL 给的字典序（V11 R7-2）。夹具刻意跨数位边界——字典序会把
/// sequence 20 排在 2 与 3 之前，等于把同一毫秒落盘的一串事件念反。
///
/// 只按 `namespace` 过滤自己那几行，因此可以多后端共用一个库/目录。
fn assert_outbox_delivery_order(store: &dyn OutboxStore, namespace: &str) {
    for sequence in [2u64, 20, 3] {
        store
            .append_outbox(OutboxEvent {
                event_id: format!("{namespace}-{sequence}"),
                sequence,
                ..event()
            })
            .unwrap();
    }
    let sequences: Vec<u64> = store
        .available_outbox(20, usize::MAX)
        .unwrap()
        .into_iter()
        .filter(|event| event.event_id.starts_with(namespace))
        .map(|event| event.sequence)
        .collect();
    assert_eq!(sequences, vec![2, 3, 20]);
    for sequence in [2u64, 20, 3] {
        let lease = store
            .claim_outbox(&format!("{namespace}-{sequence}"), "relay-order", 20, 5)
            .unwrap();
        store
            .ack_outbox(
                &format!("{namespace}-{sequence}"),
                "relay-order",
                lease.fencing_token,
                21,
            )
            .unwrap();
    }
}

/// K2 说停摆的事件"留在 outbox 里等人工确认"，这一颗钉住那次确认走得通：停摆只让 relay
/// 不再自动投递，`claim_outbox` → `ack_outbox` 这对原语对停摆的行仍然有效，运维因此能把
/// 一条确认过的事件取走，而不是让它永远压在候选集里（读全量时停摆的行也在其中）。
/// 反方向——把 `attempts` 调回去重投——三本后端都没有原语，已按缺口登记在 capabilities.yaml。
fn assert_parked_outbox_has_an_operator_exit(store: &dyn OutboxStore, namespace: &str) {
    // 库里原本有几条不去管，问的是"我自己这两条进没进这个数"：三本后端共用一份 DSN/目录时
    // 绝对值不属于任何一颗用例（V13 第 8 轮 A4）。
    let before = store.count_parked_outbox().unwrap();
    // 两行都停摆，但 attempts 刻意跨数位边界（8 与 14）：这一列在三本后端都存成文本，
    // 两边都写预算值时"按数字比"与"按字典比"给出同一个答案，SQL 里摘掉 `::numeric`/`CAST`
    // 也量不出来（V13 第 8 轮 A4）。
    for (sequence, attempts) in [(1u64, OUTBOX_MAX_ATTEMPTS), (2, OUTBOX_MAX_ATTEMPTS + 6)] {
        store
            .append_outbox(OutboxEvent {
                event_id: format!("{namespace}-parked-{sequence}"),
                sequence,
                attempts,
                ..event()
            })
            .unwrap();
    }
    let counted = store.count_parked_outbox().unwrap();
    assert_eq!(
        counted,
        before + 2,
        "停摆条数是库里的状态量: {counted} vs {before}"
    );
    let parked: Vec<String> = store
        .available_outbox(20, usize::MAX)
        .unwrap()
        .into_iter()
        .filter(|event| event.event_id.starts_with(namespace))
        .map(|event| event.event_id)
        .collect();
    assert_eq!(parked.len(), 2, "停摆的行必须仍在候选集里可见: {parked:?}");
    let lease = store
        .claim_outbox(&parked[0], "operator", 20, 5)
        .expect("停摆不得把 claim 这条路也一起堵死");
    store
        .ack_outbox(&parked[0], "operator", lease.fencing_token, 21)
        .unwrap();
    let left: Vec<String> = store
        .available_outbox(22, usize::MAX)
        .unwrap()
        .into_iter()
        .filter(|event| event.event_id.starts_with(namespace))
        .map(|event| event.event_id)
        .collect();
    assert_eq!(
        left,
        vec![parked[1].clone()],
        "确认过的那条要真的离开候选集"
    );
    let drained = store.count_parked_outbox().unwrap();
    assert_eq!(
        drained,
        before + 1,
        "计数要跟着人工确认走，不是入账那一刻的快照"
    );
}

/// 分页上界与「停摆不占页首」（V11 R7-d）：修前每轮 pump 都全表读 payload，而停摆的行按
/// `created_ts` 永远排在最前——`limit` 再小也白拿。这一颗钉两面：
/// 读全量时自己那两行的相对顺序必须是「投得出去的在前、停摆的在后」（即便停摆那条更旧），
/// 而 `limit=1` 的一页只准端回一行（把 LIMIT 写没后端就红）。
/// 停摆那行的 attempts 用 `预算 + 6` 而不是预算本身：这一列存成文本时 14 的字典序在 8 之前，
/// 「按数字比是否用尽预算」与「按字典比」在这里分岔，摘掉 CAST 就退回按落盘时间占住页首。
/// 只按 `namespace` 过滤自己那几行，因此可以多后端共用一个库/目录。
fn assert_parked_rows_yield_the_page_head(store: &dyn OutboxStore, namespace: &str) {
    store
        .append_outbox(OutboxEvent {
            event_id: format!("{namespace}-parked"),
            created_ts: 1,
            sequence: 1,
            attempts: OUTBOX_MAX_ATTEMPTS + 6,
            ..event()
        })
        .unwrap();
    store
        .append_outbox(OutboxEvent {
            event_id: format!("{namespace}-deliverable"),
            created_ts: 2,
            sequence: 2,
            ..event()
        })
        .unwrap();
    let mine: Vec<String> = store
        .available_outbox(20, usize::MAX)
        .unwrap()
        .into_iter()
        .filter(|event| event.event_id.starts_with(namespace))
        .map(|event| event.event_id)
        .collect();
    assert_eq!(
        mine,
        vec![
            format!("{namespace}-deliverable"),
            format!("{namespace}-parked")
        ],
        "停摆的行要退到页尾，而不是按落盘时间占住页首"
    );
    assert_eq!(
        store.available_outbox(20, 1).unwrap().len(),
        1,
        "limit 必须真的落到读上：返回全量等于每轮 pump 仍是 O(全库)"
    );
    for suffix in ["parked", "deliverable"] {
        let lease = store
            .claim_outbox(&format!("{namespace}-{suffix}"), "relay-page", 20, 5)
            .unwrap();
        store
            .ack_outbox(
                &format!("{namespace}-{suffix}"),
                "relay-page",
                lease.fencing_token,
                21,
            )
            .unwrap();
    }
}

#[test]
fn file_outbox_contract() {
    let root = temp_root("file");
    let store = FileOutboxStore::new(&root);
    assert_semantics(&store);
    assert_outbox_delivery_order(&store, "file-order");
    assert_parked_outbox_has_an_operator_exit(&store, "file-park");
    assert_parked_rows_yield_the_page_head(&store, "file-page");
    let _ = std::fs::remove_dir_all(root);
}

#[cfg(feature = "sqlite")]
#[test]
fn sqlite_outbox_contract() {
    let root = temp_root("sqlite");
    let store = SqliteOutboxStore::new(root.join("outbox.db")).unwrap();
    assert_semantics(&store);
    assert_outbox_delivery_order(&store, "sqlite-order");
    assert_parked_outbox_has_an_operator_exit(&store, "sqlite-park");
    assert_parked_rows_yield_the_page_head(&store, "sqlite-page");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn file_consumer_contract() {
    let root = temp_root("consumer-file");
    assert_consumer_semantics(FileConsumerStateStore::new(&root));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn file_transactional_consumer_projection_contract() {
    let root = temp_root("consumer-transactional-file");
    assert_transactional_consumer_semantics(FileConsumerStateStore::new(&root));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn file_transactional_projection_failure_is_side_effect_free() {
    let root = temp_root("consumer-transactional-failure-file");
    assert_transactional_projection_failure_is_side_effect_free(FileConsumerStateStore::new(&root));
    let _ = std::fs::remove_dir_all(root);
}

#[cfg(feature = "sqlite")]
#[test]
fn sqlite_consumer_contract() {
    let root = temp_root("consumer-sqlite");
    assert_consumer_semantics(SqliteConsumerStateStore::new(root.join("consumer.db")).unwrap());
    let _ = std::fs::remove_dir_all(root);
}

#[cfg(feature = "sqlite")]
#[test]
fn sqlite_transactional_consumer_projection_contract() {
    let root = temp_root("consumer-transactional-sqlite");
    assert_transactional_consumer_semantics(
        SqliteConsumerStateStore::new(root.join("consumer.db")).unwrap(),
    );
    let _ = std::fs::remove_dir_all(root);
}

#[cfg(feature = "sqlite")]
#[test]
fn sqlite_transactional_projection_failure_is_side_effect_free() {
    let root = temp_root("consumer-transactional-failure-sqlite");
    assert_transactional_projection_failure_is_side_effect_free(
        SqliteConsumerStateStore::new(root.join("consumer.db")).unwrap(),
    );
    let _ = std::fs::remove_dir_all(root);
}

#[cfg(feature = "postgres")]
#[test]
#[ignore = "requires QX_TEST_POSTGRES_DSN"]
fn postgres_transactional_consumer_projection_contract() {
    let dsn = std::env::var("QX_TEST_POSTGRES_DSN")
        .expect("QX_TEST_POSTGRES_DSN must be set for PostgreSQL integration");
    assert_transactional_consumer_semantics(PostgresConsumerStateStore::connect(&dsn).unwrap());
}

/// PostgreSQL 上 Outbox 的租约、围栏令牌与重试语义。
///
/// 这里刻意不复用 `assert_semantics`：它包含 `available_outbox` 全表为空的断言，
/// 只能独占一个数据库，而服务容器作业里多个后端契约测试共用同一个 DSN。
#[cfg(feature = "postgres")]
#[test]
#[ignore = "requires QX_TEST_POSTGRES_DSN"]
fn postgres_outbox_lease_fencing_and_retry_contract() {
    let dsn = std::env::var("QX_TEST_POSTGRES_DSN")
        .expect("QX_TEST_POSTGRES_DSN must point at an isolated test database");
    let store = PostgresOutboxStore::connect(&dsn).expect("connect PostgreSQL OutboxStore");
    let event_id = format!(
        "postgres-outbox-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock must be after unix epoch")
            .as_nanos()
    );
    let append = || OutboxEvent {
        event_id: event_id.clone(),
        ..event()
    };
    store.append_outbox(append()).unwrap();
    store.append_outbox(append()).unwrap();
    let attempts = |store: &PostgresOutboxStore| -> Vec<OutboxEvent> {
        store
            .available_outbox(17, usize::MAX)
            .unwrap()
            .into_iter()
            .filter(|event| event.event_id == event_id)
            .collect()
    };

    let first = store
        .claim_outbox(&event_id, "relay-a", 10, 5)
        .expect("首次租约");
    assert_eq!(
        attempts(&store)
            .first()
            .expect("同一 event_id 的重复投递必须合并成一行")
            .attempts,
        0
    );
    assert!(matches!(
        store.claim_outbox(&event_id, "relay-b", 11, 5),
        Err(StorageError::LeaseHeld { .. })
    ));
    assert!(matches!(
        store.ack_outbox(&event_id, "relay-a", first.fencing_token, 16),
        Err(StorageError::LeaseExpired { .. })
    ));
    let second = store
        .claim_outbox(&event_id, "relay-b", 16, 5)
        .expect("租约过期后可以接管");
    assert_eq!(second.fencing_token, first.fencing_token + 1);
    store
        .retry_outbox(&event_id, "relay-b", second.fencing_token, 17)
        .unwrap();
    assert_eq!(attempts(&store)[0].attempts, 1);
    let third = store
        .claim_outbox(&event_id, "relay-a", 17, 5)
        .expect("重试后可以再次投递");
    store
        .ack_outbox(&event_id, "relay-a", third.fencing_token, 18)
        .unwrap();
    assert!(store
        .available_outbox(18, usize::MAX)
        .unwrap()
        .into_iter()
        .all(|event| event.event_id != event_id));
    assert_outbox_delivery_order(&store, "postgres-order");
    assert_parked_rows_yield_the_page_head(&store, "postgres-page");
    assert_parked_outbox_has_an_operator_exit(&store, "postgres-park");
}
/// 只拒绝点名事件的投递器：让"某一条永远发不出去"成为确定现场，其余照常ack。
struct SelectivePublisher {
    refused: &'static [&'static str],
}

impl OutboxPublisher for SelectivePublisher {
    fn publish(&self, event: &OutboxEvent) -> Result<(), String> {
        if self.refused.contains(&event.event_id.as_str()) {
            return Err("refused by SelectivePublisher".into());
        }
        Ok(())
    }
}

/// 预算用尽的头部不得阻断链尾（V11 K2）：修前 `pump_once` 取最旧的 `limit` 条，
/// 头部一条毒事件就让后面全部事件无限期停摆，且没有任何一格说出"有条事件发不出去"。
fn assert_relay_unblocks_the_tail<S>(store: S)
where
    S: OutboxStore + Clone,
{
    let now = 1_000_u64;
    store
        .append_outbox(OutboxEvent {
            event_id: "poison-head".into(),
            created_ts: 1,
            sequence: 1,
            attempts: OUTBOX_MAX_ATTEMPTS,
            ..event()
        })
        .unwrap();
    store
        .append_outbox(OutboxEvent {
            event_id: "healthy-tail".into(),
            created_ts: 2,
            sequence: 2,
            ..event()
        })
        .unwrap();
    let relay = OutboxRelay::new(
        store.clone(),
        SelectivePublisher {
            refused: &["poison-head"],
        },
        "relay-a",
        5,
    )
    .unwrap();
    // limit=1 是修前会卡死的那一档：只端一条的话，端到的永远是头部。
    let report = relay.pump_once(now, 1).unwrap();
    assert_eq!(report.parked, 1, "预算用尽的那条必须被数出来: {report:?}");
    assert_eq!(
        report.published, 1,
        "链尾必须跨过停摆的头部投递出去: {report:?}"
    );
    assert_eq!(report.scanned, 1, "停摆的那条不再占用投递名额: {report:?}");
    assert_eq!(report.retried, 0, "停摆的那条不再被重试: {report:?}");
    // 停摆不等于丢弃：它仍留在 outbox 里等人工确认，attempts 也不再增长。
    let available = store.available_outbox(now, usize::MAX).unwrap();
    let poison = available
        .iter()
        .find(|event| event.event_id == "poison-head")
        .expect("预算用尽的事件要留在 outbox 里可见，不能被静默删除");
    assert_eq!(poison.attempts, OUTBOX_MAX_ATTEMPTS);
    assert!(
        !available
            .iter()
            .any(|event| event.event_id == "healthy-tail"),
        "链尾已 ack 后不应再出现在候选集里"
    );
}

#[test]
fn file_outbox_relay_unblocks_the_tail() {
    let root = temp_root("relay-tail-file");
    assert_relay_unblocks_the_tail(FileOutboxStore::new(&root));
    let _ = std::fs::remove_dir_all(root);
}

#[cfg(feature = "sqlite")]
#[test]
fn sqlite_outbox_relay_unblocks_the_tail() {
    let root = temp_root("relay-tail-sqlite");
    assert_relay_unblocks_the_tail(SqliteOutboxStore::new(root.join("outbox.db")).unwrap());
    let _ = std::fs::remove_dir_all(root);
}

/// 投递预算是"重试出来的"，不是配置里凭空出现的：每条事件被拒 OUTBOX_MAX_ATTEMPTS 次
/// 之后转入停摆，此后这一轮既不再claim、也不再报空转成功。
#[test]
fn file_outbox_relay_parks_events_after_the_attempt_budget() {
    let root = temp_root("relay-budget-file");
    let store = FileOutboxStore::new(&root);
    store
        .append_outbox(OutboxEvent {
            event_id: "always-refused".into(),
            created_ts: 1,
            ..event()
        })
        .unwrap();
    let relay = OutboxRelay::new(
        store.clone(),
        SelectivePublisher {
            refused: &["always-refused"],
        },
        "relay-a",
        5,
    )
    .unwrap();
    for round in 1..=OUTBOX_MAX_ATTEMPTS {
        let report = relay.pump_once(1_000, 4).unwrap();
        assert_eq!(report.scanned, 1, "第 {round} 轮应仍尝试投递");
        assert_eq!(report.retried, 1, "第 {round} 轮被拒后要释放租约再等下一轮");
        assert_eq!(report.parked, 0, "预算未用尽前不该报停摆: 第 {round} 轮");
        assert_eq!(
            report.last_error.as_deref(),
            Some("refused by SelectivePublisher")
        );
    }
    let report = relay.pump_once(1_000, 4).unwrap();
    assert_eq!(report.scanned, 0, "用尽预算后不再尝试投递");
    assert_eq!(report.retried, 0);
    assert_eq!(report.parked, 1, "但必须自报停摆，不能伪装成没有事件要发");
    assert_eq!(
        report.last_error, None,
        "本轮什么都没投，不该留着上一轮的错误"
    );
    assert_eq!(
        store.available_outbox(1_000, usize::MAX).unwrap()[0].attempts,
        OUTBOX_MAX_ATTEMPTS,
        "停摆后 attempts 不再增长"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// 停摆条数是库里的状态量，不是「这一页数到几条」（V11 R7-d）：三条都停摆而 `limit=1`
/// 的一页只装得下一条，逐行数只会报 1。把 `count_parked_outbox` 换回页内累加，这一颗红。
fn assert_relay_parked_count_ignores_the_page<S>(store: S)
where
    S: OutboxStore + Clone,
{
    for sequence in [1u64, 2, 3] {
        store
            .append_outbox(OutboxEvent {
                event_id: format!("page-blind-{sequence}"),
                created_ts: sequence,
                sequence,
                attempts: OUTBOX_MAX_ATTEMPTS,
                ..event()
            })
            .unwrap();
    }
    let relay = OutboxRelay::new(
        store.clone(),
        SelectivePublisher { refused: &[] },
        "relay-a",
        5,
    )
    .unwrap();
    let report = relay.pump_once(1_000, 1).unwrap();
    assert_eq!(report.scanned, 0, "三条都停摆，页里端到的那条也不该被投递");
    assert_eq!(report.parked, 3, "停摆条数不随页数变小: {report:?}");
    assert_eq!(store.count_parked_outbox().unwrap(), 3);
}

#[test]
fn file_outbox_relay_parked_count_ignores_the_page() {
    let root = temp_root("relay-parked-count-file");
    assert_relay_parked_count_ignores_the_page(FileOutboxStore::new(&root));
    let _ = std::fs::remove_dir_all(root);
}

#[cfg(feature = "sqlite")]
#[test]
fn sqlite_outbox_relay_parked_count_ignores_the_page() {
    let root = temp_root("relay-parked-count-sqlite");
    assert_relay_parked_count_ignores_the_page(
        SqliteOutboxStore::new(root.join("outbox.db")).unwrap(),
    );
    let _ = std::fs::remove_dir_all(root);
}

/// 预算判据本身的边界：判据只有这一处出口，三本后端的候选集形状不变。
#[test]
fn outbox_attempt_budget_boundaries() {
    // 停摆点由谓词扫出来，而不是抄一遍常量的值：预算改成别的数字时这条仍说真话。
    let first_parked = (0..=OUTBOX_MAX_ATTEMPTS + 1)
        .find(|&attempts| outbox_exhausted(attempts))
        .expect("预算内必须出现停摆");
    assert_eq!(
        first_parked, OUTBOX_MAX_ATTEMPTS,
        "停摆必须正好落在预算那一格，之前每一格都还可重试"
    );
    assert!(first_parked > 1, "预算为 1 等于不给毒事件任何退避机会");
    assert!(
        outbox_exhausted(first_parked + 1_000_000),
        "耗尽之后不得在高 attempts 上回到可重试"
    );
}
