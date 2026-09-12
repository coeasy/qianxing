use qx_storage::{
    ConsumerEngine, ConsumerOutcome, ConsumerProjection, ConsumerStateStore,
    FileConsumerStateStore, FileOutboxStore, OutboxEvent, OutboxStore, StorageError,
    TransactionalConsumerStateStore,
};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(feature = "postgres")]
use qx_storage::PostgresConsumerStateStore;
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
    assert_eq!(store.dead_letters("ledger-reducer", 10).unwrap().len(), 1);
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
    assert_eq!(store.dead_letters("ledger-reducer", 10).unwrap().len(), 1);
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
    assert_eq!(store.available_outbox(17).unwrap()[0].attempts, 1);
    let third = store
        .claim_outbox("contract-event-1", "relay-a", 17, 5)
        .unwrap();
    store
        .ack_outbox("contract-event-1", "relay-a", third.fencing_token, 18)
        .unwrap();
    assert!(store.available_outbox(18).unwrap().is_empty());
}

#[test]
fn file_outbox_contract() {
    let root = temp_root("file");
    let store = FileOutboxStore::new(&root);
    assert_semantics(&store);
    let _ = std::fs::remove_dir_all(root);
}

#[cfg(feature = "sqlite")]
#[test]
fn sqlite_outbox_contract() {
    let root = temp_root("sqlite");
    let store = SqliteOutboxStore::new(root.join("outbox.db")).unwrap();
    assert_semantics(&store);
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
