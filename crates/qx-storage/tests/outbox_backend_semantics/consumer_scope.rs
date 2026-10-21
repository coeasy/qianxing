use super::*;
use qx_storage::{ConsumerCheckpoint, DeadLetterRecord};
use std::time::{SystemTime, UNIX_EPOCH};

pub(super) fn unique_scope() -> String {
    let clock = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock must be after unix epoch")
        .as_nanos();
    format!(
        "{:04x}{:06x}",
        std::process::id() & 0xffff,
        clock & 0xffffff
    )
}

pub(super) fn assert_transactional_projection_failure_is_side_effect_free<S>(store: S, scope: &str)
where
    S: TransactionalConsumerStateStore + Clone,
{
    let group_id = format!("ledger-reducer-{scope}");
    let mut event = super::consumer_event();
    event.event_id = format!("{scope}-consumer-event-1");
    let engine = ConsumerEngine::new(store.clone(), &group_id, 2).unwrap();
    let result = engine.consume_with_projection(&event, 5, 1, 10, |_, checkpoint| {
        let mut projection =
            ConsumerProjection::for_checkpoint(checkpoint, "account-1", "{\"valid\":true}");
        projection.group_id = "other-group".into();
        Ok(projection)
    });
    assert!(matches!(result, Err(StorageError::Conflict(_))));
    assert!(!store.is_processed(&group_id, &event.event_id).unwrap());
    assert!(store
        .load_checkpoint(&group_id, &event.topic, &event.partition_key)
        .unwrap()
        .is_none());
    assert!(store
        .load_projection(&group_id, "account-1")
        .unwrap()
        .is_none());
}

pub(super) fn assert_consumer_processed_scope<S>(store: S)
where
    S: TransactionalConsumerStateStore + Clone,
{
    let checkpoint = |group_id: &str, partition_key: &str, event_id: &str| ConsumerCheckpoint {
        group_id: group_id.into(),
        topic: "qx.eventlog".into(),
        partition_key: partition_key.into(),
        offset: 1,
        event_id: event_id.into(),
        updated_ts: 10,
    };

    let first = checkpoint("scope-plain", "partition-a", "shared-plain-event");
    store.commit_processed(first).unwrap();
    store
        .commit_processed(checkpoint(
            "scope-plain",
            "partition-b",
            "shared-plain-event",
        ))
        .unwrap();
    assert!(store
        .is_processed("scope-plain", "shared-plain-event")
        .unwrap());
    assert!(store
        .load_checkpoint("scope-plain", "qx.eventlog", "partition-b")
        .unwrap()
        .is_none());

    let first = checkpoint("scope-projection", "partition-a", "shared-projection-event");
    store
        .commit_processed_with_projection(
            first.clone(),
            ConsumerProjection::for_checkpoint(&first, "projection-a", "{\"applied\":true}"),
        )
        .unwrap();
    let duplicate = checkpoint("scope-projection", "partition-b", "shared-projection-event");
    store
        .commit_processed_with_projection(
            duplicate.clone(),
            ConsumerProjection::for_checkpoint(&duplicate, "projection-b", "{\"applied\":false}"),
        )
        .unwrap();
    assert!(store
        .load_projection("scope-projection", "projection-a")
        .unwrap()
        .is_some());
    assert!(store
        .load_projection("scope-projection", "projection-b")
        .unwrap()
        .is_none());
    assert!(store
        .load_checkpoint("scope-projection", "qx.eventlog", "partition-b")
        .unwrap()
        .is_none());

    let first = checkpoint(
        "scope-dead-letter",
        "partition-a",
        "shared-dead-letter-event",
    );
    let record = DeadLetterRecord {
        group_id: first.group_id.clone(),
        topic: first.topic.clone(),
        partition_key: first.partition_key.clone(),
        event_id: first.event_id.clone(),
        offset: first.offset,
        attempts: 2,
        error: "permanent".into(),
        failed_ts: first.updated_ts,
        event: super::consumer_event(),
    };
    store.append_dead_letter_and_commit(record, first).unwrap();
    let duplicate = checkpoint(
        "scope-dead-letter",
        "partition-b",
        "shared-dead-letter-event",
    );
    let duplicate_record = DeadLetterRecord {
        group_id: duplicate.group_id.clone(),
        topic: duplicate.topic.clone(),
        partition_key: duplicate.partition_key.clone(),
        event_id: duplicate.event_id.clone(),
        offset: duplicate.offset,
        attempts: 2,
        error: "duplicate".into(),
        failed_ts: duplicate.updated_ts,
        event: super::consumer_event(),
    };
    store
        .append_dead_letter_and_commit(duplicate_record, duplicate)
        .unwrap();
    assert_eq!(
        store.dead_letters("scope-dead-letter", 10).unwrap().len(),
        1
    );
    assert!(store
        .load_checkpoint("scope-dead-letter", "qx.eventlog", "partition-b")
        .unwrap()
        .is_none());
}

#[test]
fn file_consumer_processed_scope_is_group_wide_across_partitions() {
    let root = super::temp_root("consumer-scope-file");
    assert_consumer_processed_scope(FileConsumerStateStore::new(&root));
    let _ = std::fs::remove_dir_all(root);
}

#[cfg(feature = "sqlite")]
#[test]
fn sqlite_consumer_processed_scope_is_group_wide_across_partitions() {
    let root = super::temp_root("consumer-scope-sqlite");
    assert_consumer_processed_scope(
        SqliteConsumerStateStore::new(root.join("consumer.db")).unwrap(),
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn file_transactional_projection_failure_is_side_effect_free() {
    let root = super::temp_root("consumer-transactional-failure-file");
    assert_transactional_projection_failure_is_side_effect_free(
        FileConsumerStateStore::new(&root),
        &unique_scope(),
    );
    let _ = std::fs::remove_dir_all(root);
}

#[cfg(feature = "sqlite")]
#[test]
fn sqlite_transactional_projection_failure_is_side_effect_free() {
    let root = super::temp_root("consumer-transactional-failure-sqlite");
    assert_transactional_projection_failure_is_side_effect_free(
        SqliteConsumerStateStore::new(root.join("consumer.db")).unwrap(),
        &unique_scope(),
    );
    let _ = std::fs::remove_dir_all(root);
}
