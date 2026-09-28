//! SQLite EventLog 后端的契约测试。
//!
//! 三条验收线：一是和文件后端跑**同一断言序列**（通过共享的 `EventLogStore`
//! trait 对象），保证后端可互换；二是 SQLite 才有的行级能力（幂等追加、
//! dedup_key 冲突裁决、按 seq 升序交付、manifest 摘要校验）；三是篡改与
//! 重启路径必须暴露错误，而不是静默返回半个事实日志。
#![cfg(feature = "sqlite")]

use qx_core::{Event, EventKind, EventLog, EventMetadata, Priority};
use qx_storage::{
    project_event_log_to_outbox, EventLogFileStore, EventLogStore, SqliteEventLogStore,
    SqliteOutboxStore, StorageError,
};
use rusqlite::Connection;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_path(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "qianxing-event-log-{label}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock must be after unix epoch")
            .as_nanos()
    ))
}

fn temp_db(label: &str) -> PathBuf {
    let mut path = temp_path(label);
    path.set_extension("sqlite");
    path
}

fn settled_event(seq: u64, ts: u64, dedup_key: &str) -> Event {
    let metadata = EventMetadata {
        dedup_key: dedup_key.to_string(),
        ..EventMetadata::default()
    };
    Event::new(seq, ts, Priority::POST, EventKind::Settle).with_metadata(metadata)
}

fn sample_log(count: u64) -> EventLog {
    let mut log = EventLog::new();
    for index in 0..count {
        log.append(settled_event(index, index + 1, &format!("fact-{index}")));
    }
    log
}

/// 与 `sample_log(count)` 等长、但第 3 条事实内容不同：用于前缀改写检测。
fn conflicting_log(count: u64) -> EventLog {
    let mut log = EventLog::new();
    for index in 0..count {
        let dedup_key = if index == 2 {
            "other-2".to_string()
        } else {
            format!("fact-{index}")
        };
        log.append(settled_event(index, index + 1, &dedup_key));
    }
    log
}

/// 同一日志内出现重复 dedup_key：文件后端由归约器保证不会发生，
/// SQLite 后端把它当作存储层硬约束拒绝。
fn duplicate_dedup_log() -> EventLog {
    let mut log = EventLog::new();
    log.append(settled_event(0, 1, "shared"));
    log.append(settled_event(1, 2, "shared"));
    log
}

/// 文件与 SQLite 后端共享的断言序列。`open_store` 每次都新建一个后端实例，
/// 用于验证重启（重新打开连接/文件）之后事实仍然可读。
fn assert_event_log_contract(mut open_store: impl FnMut() -> Box<dyn EventLogStore>) {
    let log = sample_log(3);
    open_store().save("run", &log).unwrap();

    let store = open_store();
    let restored = store.load("run").unwrap();
    assert_eq!(restored.len(), 3);
    assert_eq!(restored.next_seq(), 3);
    assert_eq!(restored.digest(), log.digest());

    // 同一内容重复保存 = 幂等重试，既不报错也不复制事实。
    store.save("run", &log).unwrap();
    assert_eq!(store.load("run").unwrap().digest(), log.digest());
    assert_eq!(store.load("run").unwrap().len(), 3);

    // 追加扩展。
    let extended = sample_log(5);
    store.save("run", &extended).unwrap();
    assert_eq!(store.load("run").unwrap().len(), 5);

    // append-only：缩短或改写前缀都必须失败。
    let shortened = format!("{:?}", store.save("run", &log));
    assert!(shortened.contains("NonAppendOnly"), "{shortened}");
    let rewritten = format!("{:?}", store.save("run", &conflicting_log(5)));
    assert!(rewritten.contains("NonAppendOnly"), "{rewritten}");
    assert_eq!(store.load("run").unwrap().digest(), extended.digest());

    // 空日志也是可持久化的运行事实，而不是“不存在”。
    store.save("empty", &EventLog::new()).unwrap();
    let empty = store.load("empty").unwrap();
    assert!(empty.is_empty());
    assert_eq!(empty.digest(), EventLog::new().digest());

    // 非法日志名不得触达存储介质。
    assert!(format!("{:?}", store.save("../escape", &log)).contains("InvalidName"));

    // 重启后事实不丢，且未写入的名字不会凭空出现。
    let restarted = open_store();
    assert_eq!(restarted.load("run").unwrap().digest(), extended.digest());
    assert!(restarted.load("empty").unwrap().is_empty());
    assert!(restarted.load("missing").is_err());
}

#[test]
fn file_event_log_contract_baseline() {
    let root = temp_path("file");
    assert_event_log_contract(|| Box::new(EventLogFileStore::new(root.clone())));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn sqlite_event_log_contract_matches_file_backend() {
    let path = temp_db("sqlite-contract");
    assert_event_log_contract(|| Box::new(SqliteEventLogStore::new(path.clone()).unwrap()));
    let _ = std::fs::remove_file(path);
}

#[test]
fn sqlite_event_log_append_is_idempotent_by_dedup_key() {
    let path = temp_db("sqlite-append");
    let store = SqliteEventLogStore::new(&path).unwrap();
    assert_eq!(store.event_count("run").unwrap(), 0);
    assert!(store.last_seq("run").unwrap().is_none());
    assert!(store.digest("run").unwrap().is_none());
    assert!(matches!(store.read("run"), Err(StorageError::NotFound(_))));

    assert!(store.append("run", &settled_event(0, 1, "fact-0")).unwrap());
    assert_eq!(
        store
            .append_many("run", &[settled_event(1, 2, "fact-1")])
            .unwrap(),
        1
    );
    assert_eq!(store.event_count("run").unwrap(), 2);

    // 同一 dedup_key 承载不同事实必须冲突，且不能留下半个事实。
    // 这里刻意不引入"查一下这个键存过吗"的第二入口：唯一可信的裁决就是
    // append 本身返回什么，因此下面的每条断言都直接读写入结果与行数。
    assert!(matches!(
        store.append("run", &settled_event(2, 3, "fact-0")),
        Err(StorageError::Conflict(_))
    ));
    assert_eq!(store.event_count("run").unwrap(), 2);

    // 完全相同的事实重试是幂等 no-op。
    assert!(!store.append("run", &settled_event(0, 1, "fact-0")).unwrap());
    assert_eq!(
        store
            .append_many(
                "run",
                &[settled_event(0, 1, "fact-0"), settled_event(1, 2, "fact-1")]
            )
            .unwrap(),
        0
    );
    assert_eq!(store.event_count("run").unwrap(), 2);

    // 没有 dedup_key 的事实不参与去重，只按 seq 幂等；空键更不是去重键，
    // 两条不同 seq 的空键事实都要各自落库。
    assert!(store
        .append("run", &Event::new(2, 3, Priority::POST, EventKind::Settle))
        .unwrap());
    assert!(!store
        .append("run", &Event::new(2, 3, Priority::POST, EventKind::Settle))
        .unwrap());
    assert!(store
        .append("run", &Event::new(3, 4, Priority::POST, EventKind::Settle))
        .unwrap());
    assert_eq!(store.event_count("run").unwrap(), 4);

    // seq 单调：乱序事实被 Kernel 不变量拒绝，且不会污染已落库事实。
    assert_eq!(store.last_seq("run").unwrap(), Some(3));
    assert_eq!(store.next_seq("run").unwrap(), Some(4));
    assert!(matches!(
        store.append("run", &settled_event(4, 2, "fact-4")),
        Err(StorageError::Core(_))
    ));
    assert_eq!(store.event_count("run").unwrap(), 4);
    // 被拒的键没有留下痕迹：同键换一个时间戳必须正常追加，而不是判成冲突。
    assert!(store.append("run", &settled_event(4, 5, "fact-4")).unwrap());
    assert_eq!(store.last_seq("run").unwrap(), Some(4));
    assert!(store.validate("run").is_ok());
    let digest = store.digest("run").unwrap();
    assert_eq!(digest, Some(store.read("run").unwrap().digest()));

    // 重开连接后可以继续按同一 seq 序列追加。
    let reopened = SqliteEventLogStore::new(&path).unwrap();
    assert_eq!(reopened.digest("run").unwrap(), digest);
    assert_eq!(reopened.event_count("run").unwrap(), 5);
    assert!(reopened
        .append("run", &settled_event(5, 6, "fact-5"))
        .unwrap());
    assert_eq!(reopened.last_seq("run").unwrap(), Some(5));
    assert!(reopened.validate("run").is_ok());
    assert_eq!(reopened.list().unwrap(), vec!["run".to_string()]);
    assert!(matches!(
        reopened.read("../escape"),
        Err(StorageError::InvalidName(_))
    ));
    let _ = std::fs::remove_file(path);
}

/// 重放游标不在存储层：SQLite 后端只按 seq 升序交付整条日志，
/// `after` 语义由 `qx-api` 的 `events_after_cursor` 单点实现（V12 R4-g）。
/// 因此这里钉住的是"交付顺序 + 追加不可改写"，而不是某个 SQL 范围查询。
#[test]
fn sqlite_event_log_delivers_in_seq_order_and_rejects_rewrites() {
    let path = temp_db("sqlite-order");
    let store = SqliteEventLogStore::new(&path).unwrap();
    store.write("run", &sample_log(6)).unwrap();

    let events = store.read("run").unwrap();
    assert_eq!(
        events
            .events()
            .iter()
            .enumerate()
            .map(|(index, event)| {
                assert_eq!(event.seq, index as u64, "整条日志必须按 seq 升序交付");
                event.seq
            })
            .collect::<Vec<_>>(),
        (0..6u64).collect::<Vec<u64>>()
    );

    // 同一日志整体重写是幂等的；改写前缀被拒绝。
    let before = store.digest("run").unwrap();
    store.write("run", &sample_log(6)).unwrap();
    assert_eq!(store.digest("run").unwrap(), before);
    assert_eq!(store.event_count("run").unwrap(), 6);
    assert!(matches!(
        store.write("run", &conflicting_log(6)),
        Err(StorageError::NonAppendOnly(_))
    ));
    assert_eq!(store.read("run").unwrap().digest(), sample_log(6).digest());

    // 日志内重复 dedup_key 被存储层拒绝。
    assert!(matches!(
        store.write("dup", &duplicate_dedup_log()),
        Err(StorageError::Conflict(_))
    ));
    assert!(matches!(
        store.read("missing"),
        Err(StorageError::NotFound(_))
    ));
    let _ = std::fs::remove_file(path);
}

#[test]
fn sqlite_event_log_is_manifest_verified_across_restart() {
    let path = temp_db("sqlite-tamper");
    let store = SqliteEventLogStore::new(&path).unwrap();
    let log = sample_log(4);
    store.write("run", &log).unwrap();
    assert_eq!(store.digest("run").unwrap(), Some(log.digest()));
    assert!(store.validate("run").is_ok());

    // 绕过存储层直接删掉尾行：manifest 计数与摘要必须发现事实被截断。
    let connection = Connection::open(&path).unwrap();
    connection
        .execute(
            "DELETE FROM qx_event_log_entries WHERE name = 'run' AND position = 3",
            [],
        )
        .unwrap();
    drop(connection);
    assert!(matches!(
        store.validate("run"),
        Err(StorageError::Conflict(message)) if message.contains("摘要不一致")
    ));
    assert!(matches!(store.read("run"), Err(StorageError::Conflict(_))));
    // read_if_exists 与 read 的错误语义一致：损坏不能被当成“首次启动”。
    assert!(store.read_if_exists("run").is_err());
    let _ = std::fs::remove_file(path);
}

#[test]
fn sqlite_event_log_and_outbox_commit_atomically() {
    let path = temp_db("sqlite-outbox");
    let store = SqliteEventLogStore::new(&path).unwrap();
    let outbox = SqliteOutboxStore::new(&path).unwrap();
    let log = sample_log(2);
    let events = project_event_log_to_outbox("run", &log).unwrap();
    store.write_with_outbox("run", &log, &events).unwrap();
    assert_eq!(store.event_count("run").unwrap(), 2);
    assert_eq!(outbox.available(0).unwrap().len(), 2);

    // 同一批事实重试：EventLog 与 Outbox 都保持幂等。
    store.write_with_outbox("run", &log, &events).unwrap();
    assert_eq!(store.event_count("run").unwrap(), 2);
    assert_eq!(outbox.available(0).unwrap().len(), 2);

    // Outbox 冲突必须回滚同事务内的 EventLog 追加。
    let extended = sample_log(5);
    let mut events = project_event_log_to_outbox("run", &extended).unwrap();
    events[0].payload = "{\"tampered\":true}".into();
    assert!(matches!(
        store.write_with_outbox("run", &extended, &events),
        Err(StorageError::Conflict(_))
    ));
    assert_eq!(store.event_count("run").unwrap(), 2);
    assert_eq!(store.read("run").unwrap().digest(), log.digest());
    assert_eq!(outbox.available(0).unwrap().len(), 2);

    // 事实与出站事件一起推进。
    let extended_events = project_event_log_to_outbox("run", &extended).unwrap();
    store
        .write_with_outbox("run", &extended, &extended_events)
        .unwrap();
    assert_eq!(store.event_count("run").unwrap(), 5);
    assert_eq!(outbox.available(0).unwrap().len(), 5);
    let _ = std::fs::remove_file(path);
}
