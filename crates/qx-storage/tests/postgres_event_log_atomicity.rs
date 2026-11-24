//! PostgreSQL EventLog 与 Outbox 的事务原子性契约。
#![cfg(feature = "postgres")]

use qx_core::{Event, EventKind, EventLog, EventMetadata, Priority};
use qx_storage::{
    project_event_log_to_outbox, OutboxStore, PostgresEventLogStore, PostgresOutboxStore,
    StorageError,
};
use std::time::{SystemTime, UNIX_EPOCH};

fn sample_log(count: u64) -> EventLog {
    let mut log = EventLog::new();
    for seq in 0..count {
        log.append(
            Event::new(seq, seq + 1, Priority::POST, EventKind::Settle).with_metadata(
                EventMetadata {
                    dedup_key: format!("fact-{seq}"),
                    ..EventMetadata::default()
                },
            ),
        );
    }
    log
}

fn unique_name(label: &str) -> String {
    format!(
        "pg-atomic-{label}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock must be after unix epoch")
            .as_nanos()
    )
}

#[test]
#[ignore = "requires QX_TEST_POSTGRES_DSN and a running PostgreSQL instance"]
fn postgres_event_log_and_outbox_commit_atomically_and_retry_idempotently() {
    let dsn = std::env::var("QX_TEST_POSTGRES_DSN")
        .expect("QX_TEST_POSTGRES_DSN must point at an isolated test database");
    let event_log = PostgresEventLogStore::connect(&dsn).expect("connect PostgreSQL EventLog");
    let outbox = PostgresOutboxStore::connect(&dsn).expect("connect PostgreSQL Outbox");

    // 新日志写入时若 Outbox event_id 已存在但内容不一致，整笔事务必须回滚。
    let rollback_name = unique_name("rollback");
    let attempted_log = sample_log(3);
    let mut conflicting = project_event_log_to_outbox(&rollback_name, &attempted_log, 0).unwrap();
    conflicting[1].payload = "{\"tampered\":true}".into();
    outbox.append(conflicting[1].clone()).unwrap();
    assert!(matches!(
        event_log.write_with_outbox(
            &rollback_name,
            &attempted_log,
            &project_event_log_to_outbox(&rollback_name, &attempted_log, 0).unwrap()
        ),
        Err(StorageError::Conflict(_))
    ));
    assert!(event_log.read_if_exists(&rollback_name).unwrap().is_none());
    let rollback_events = outbox
        .available_outbox(0, usize::MAX)
        .unwrap()
        .into_iter()
        .filter(|event| event.event_id.starts_with(&format!("{rollback_name}:")))
        .collect::<Vec<_>>();
    assert_eq!(rollback_events.len(), 1);
    assert_eq!(rollback_events[0].event_id, format!("{rollback_name}:1"));

    // 正常写入的同批重试不复制 EventLog 事实或 Outbox 行。
    let success_name = unique_name("retry");
    let log = sample_log(2);
    let events = project_event_log_to_outbox(&success_name, &log, 0).unwrap();
    event_log
        .write_with_outbox(&success_name, &log, &events)
        .unwrap();
    event_log
        .write_with_outbox(&success_name, &log, &events)
        .unwrap();
    assert_eq!(
        event_log.read(&success_name).unwrap().digest(),
        log.digest()
    );
    let retry_events = outbox
        .available_outbox(0, usize::MAX)
        .unwrap()
        .into_iter()
        .filter(|event| event.event_id.starts_with(&format!("{success_name}:")))
        .collect::<Vec<_>>();
    assert_eq!(retry_events.len(), 2);
}

#[test]
#[ignore = "requires QX_TEST_POSTGRES_DSN and a running PostgreSQL instance"]
fn postgres_event_log_read_since_returns_only_a_verified_tail() {
    let dsn = std::env::var("QX_TEST_POSTGRES_DSN")
        .expect("QX_TEST_POSTGRES_DSN must point at an isolated test database");
    let event_log = PostgresEventLogStore::connect(&dsn).expect("connect PostgreSQL EventLog");
    let name = unique_name("tail");
    let prefix = sample_log(3);
    event_log.write(&name, &prefix).unwrap();

    let extended = sample_log(5);
    event_log.write(&name, &extended).unwrap();
    let expected_tail = prefix.events().last().expect("prefix has a tail");
    assert_eq!(
        event_log
            .read_since(
                &name,
                Some(expected_tail.seq),
                Some((expected_tail, prefix.len())),
                prefix.event_digest(),
                prefix.next_seq(),
            )
            .unwrap(),
        Some(extended.events()[prefix.len()..].to_vec())
    );
    assert_eq!(
        event_log
            .read_since(
                &name,
                extended.events().last().map(|event| event.seq),
                Some((extended.events().last().unwrap(), extended.len(),)),
                extended.event_digest(),
                extended.next_seq(),
            )
            .unwrap(),
        Some(Vec::new())
    );

    let wrong_tail = Event::new(2, 99, Priority::POST, EventKind::Settle);
    assert_eq!(
        event_log
            .read_since(
                &name,
                Some(wrong_tail.seq),
                Some((&wrong_tail, prefix.len())),
                prefix.event_digest(),
                prefix.next_seq(),
            )
            .unwrap(),
        None
    );
}
