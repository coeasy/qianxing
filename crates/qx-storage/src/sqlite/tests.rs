//! SQLite 后端的常驻用例：事务内接链、检查点对到链尾、篡改逐环可见。
//!
//! 从 `sqlite.rs` 外置：行数预算只降不升（同 qx-datastruct 的口径）。

use super::*;
use crate::verify_audit_chain;
use qx_control::{CommandKind, CommandStatus, Permission};
use qx_protocol::AccountSnapshot;
use qx_scheduler::{JobStatus, JobWindow, RetryPolicy, Trigger};
use std::collections::BTreeMap;

fn temp_db(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "qianxing-{label}-{}-{}.sqlite",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

fn queued_job() -> (JobSpec, JobRun) {
    let job = JobSpec {
        job_id: "sqlite-job".into(),
        job_version: "v1".into(),
        owner: "research".into(),
        enabled: true,
        trigger: Trigger::Manual,
        window: JobWindow::Any,
        depends_on: Vec::new(),
        timeout_seconds: 60,
        retry_policy: RetryPolicy::default(),
        concurrency_key: "sqlite-job".into(),
        idempotency_key: "sqlite-job-daily".into(),
        audit_reason: "sqlite test".into(),
        dry_run: true,
    };
    let run = JobRun {
        run_id: job.stable_key("20260910"),
        job_id: job.job_id.clone(),
        trading_day: "20260910".into(),
        attempt: 1,
        status: JobStatus::Running,
        manifest_digest: Some(7),
        error_code: None,
        next_retry_ts: None,
        started_ts: 10,
        deadline_ts: 70,
    };
    (job, run)
}

fn queued_command(command_id: u64) -> ControlCommand {
    ControlCommand {
        command_id,
        request_id: format!("sqlite-command-{command_id}"),
        operator_id: "ops".into(),
        reason: "sqlite command queue test".into(),
        kind: CommandKind::SubmitOrder,
        target: command_id.to_string(),
        payload: BTreeMap::new(),
        permission: Permission::Trading,
        dry_run: true,
    }
}

/// sqlite 这本链的整条路：控制面事务写出链行 → 读侧核对通过 → 改一行摘要就必须拒绝。
/// 这里没有"单独追加"的入口可走（V11 R5-2 删掉了链上的第二个写入者），用例因此从
/// `transact_control` 进去，判的是自家冷读与检查点对造假的反应。
#[test]
fn sqlite_audit_chain_is_written_by_the_transaction_and_tamper_evident() {
    let path = temp_db("audit");
    let control = SqliteControlStore::new(&path).unwrap();
    let (plane, outcome) = control
        .transact_control::<(), String, _>(|plane| {
            plane
                .submit(queued_command(1), 10)
                .map_err(|error| format!("{error:?}"))
                .map(|_| ())
        })
        .unwrap();
    outcome.unwrap();
    let audit = SqliteAuditStore::new(&path).unwrap();
    let chain = audit.read().unwrap();
    assert_eq!(chain.len(), 1, "一笔受理在链上就是一行");
    assert_eq!(chain[0].record.command_id, 1);
    // 检查点与链尾互相指认：doctor 的 `audit_chain` 检查用的就是这同一颗判据。
    verify_audit_chain(&plane, &chain).unwrap();
    let connection = Connection::open(&path).unwrap();
    connection
        .execute(
            "UPDATE qx_audit_entries SET entry_hash = '1' WHERE sequence = 0",
            [],
        )
        .unwrap();
    assert!(matches!(
        audit.read(),
        Err(StorageError::Conflict(message)) if message.contains("摘要不一致")
    ));
    let _ = std::fs::remove_file(path);
}

#[test]
fn sqlite_job_queue_preserves_fencing_and_expiry_semantics() {
    let path = temp_db("jobs");
    let queue = SqliteJobQueue::new(&path).unwrap();
    let (job, run) = queued_job();
    let run_id = run.run_id;
    queue.enqueue(job.clone(), run.clone(), 10).unwrap();
    queue.enqueue(job, run, 10).unwrap();
    assert_eq!(queue.available(10).unwrap().len(), 1);
    let first = queue.claim(run_id, "worker-a", 10, 10).unwrap();
    assert_eq!(first.fencing_token, 1);
    assert!(matches!(
        queue.ack_at(run_id, "worker-a", first.fencing_token, 20),
        Err(StorageError::LeaseExpired { .. })
    ));
    assert_eq!(queue.recover_expired(20).unwrap(), vec![run_id]);
    let takeover = queue.claim(run_id, "worker-b", 21, 10).unwrap();
    assert_eq!(takeover.fencing_token, 2);
    assert!(matches!(
        queue.ack_at(run_id, "worker-a", first.fencing_token, 21),
        Err(StorageError::Unauthorized(_))
    ));
    queue
        .ack_at(run_id, "worker-b", takeover.fencing_token, 21)
        .unwrap();
    assert!(queue.available(22).unwrap().is_empty());
    let _ = std::fs::remove_file(path);
}

#[test]
fn sqlite_token_bucket_is_transactional_and_persistent() {
    let path = temp_db("bucket");
    let first = SqliteTokenBucket::new(&path, "api", 2, 0).unwrap();
    assert!(first.try_acquire(10, 1).unwrap());
    let second = SqliteTokenBucket::new(&path, "api", 2, 0).unwrap();
    assert!(second.try_acquire(10, 1).unwrap());
    assert!(!first.try_acquire(10, 1).unwrap());
    assert!(matches!(
        first.try_acquire(10, 3),
        Err(StorageError::Conflict(_))
    ));
    let _ = std::fs::remove_file(path);
}

#[test]
fn sqlite_control_state_and_command_queue_are_transactional_and_fenced() {
    let path = temp_db("control");
    let store = SqliteControlStore::new(&path).unwrap();
    let command = queued_command(91);
    let (_, accepted) = store
        .transact_control(|plane| plane.submit(command.clone(), 10))
        .unwrap();
    assert_eq!(accepted.unwrap().status, CommandStatus::Accepted);
    let (_, duplicate) = store
        .transact_control(|plane| plane.submit(command.clone(), 11))
        .unwrap();
    assert!(duplicate.is_err());
    assert_eq!(store.load_if_exists().unwrap().unwrap().audit().len(), 1);

    let queue = SqliteControlCommandQueue::new(&path).unwrap();
    queue.enqueue(command.clone(), 10).unwrap();
    queue.enqueue(command, 10).unwrap();
    assert_eq!(queue.available(10).unwrap().len(), 1);
    let lease = queue.claim(91, "execution-a", 10, 10).unwrap();
    assert!(matches!(
        queue.claim(91, "execution-b", 11, 10),
        Err(StorageError::LeaseHeld { .. })
    ));
    let takeover = queue.claim(91, "execution-b", 20, 10).unwrap();
    assert_eq!(takeover.fencing_token, lease.fencing_token + 1);
    assert!(matches!(
        queue.ack_at(91, "execution-a", lease.fencing_token, 21),
        Err(StorageError::Unauthorized(_))
    ));
    queue
        .ack_at(91, "execution-b", takeover.fencing_token, 21)
        .unwrap();
    assert!(queue.available(22).unwrap().is_empty());
    let _ = std::fs::remove_file(path);
}

#[test]
fn sqlite_snapshot_store_seals_and_revalidates_account_state() {
    let path = temp_db("snapshot");
    let store = SqliteSnapshotStore::new(&path).unwrap();
    let mut snapshot = AccountSnapshot::new(1, "main", "default", "BINANCE", 10);
    snapshot.cash_raw.insert("USDT".into(), 1000);
    snapshot.seal();
    store.save(&snapshot).unwrap();
    store.save(&snapshot).unwrap();
    let json = store
        .load_json(snapshot.header.snapshot_id, snapshot.state_hash())
        .unwrap();
    assert_eq!(AccountSnapshot::from_json(&json).unwrap(), snapshot);
    let connection = Connection::open(&path).unwrap();
    connection
        .execute(
            "UPDATE qx_snapshots SET content = '{\"tampered\":true}'",
            [],
        )
        .unwrap();
    assert!(store
        .load_json(snapshot.header.snapshot_id, snapshot.state_hash())
        .is_err());
    let _ = std::fs::remove_file(path);
}
