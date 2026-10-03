use qx_control::{CommandKind, ControlCommand, Permission};
use qx_scheduler::{JobRun, JobSpec, JobStatus, JobWindow, RetryPolicy, Trigger};
use qx_storage::{
    ControlCommandQueue, ControlCommandQueueBackend, FileJobQueue, JobQueueBackend, StorageError,
};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(feature = "postgres")]
use qx_storage::{PostgresControlCommandQueue, PostgresJobQueue};
#[cfg(feature = "sqlite")]
use qx_storage::{SqliteControlCommandQueue, SqliteJobQueue};

fn temp_root(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "qianxing-storage-contract-{label}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock must be after unix epoch")
            .as_nanos()
    ))
}

fn command(command_id: u64) -> ControlCommand {
    ControlCommand {
        command_id,
        request_id: format!("storage-contract-{command_id}"),
        operator_id: "contract-test".into(),
        reason: "backend semantic contract".into(),
        kind: CommandKind::SubmitOrder,
        target: "BTC/USDT.OKX".into(),
        payload: BTreeMap::new(),
        permission: Permission::Trading,
        dry_run: true,
    }
}

fn job_and_run(run_id: u64) -> (JobSpec, JobRun) {
    let job = JobSpec {
        job_id: format!("storage-job-{run_id}"),
        job_version: "v1".into(),
        owner: "contract-test".into(),
        enabled: true,
        trigger: Trigger::Manual,
        window: JobWindow::Any,
        depends_on: Vec::new(),
        input_refs: vec!["input".into()],
        output_refs: vec!["output".into()],
        timeout_seconds: 60,
        retry_policy: RetryPolicy::default(),
        concurrency_key: format!("storage-job-{run_id}"),
        idempotency_key: format!("storage-job-{run_id}-daily"),
        permission_scope: "research".into(),
        audit_reason: "backend semantic contract".into(),
        dry_run: true,
    };
    let run = JobRun {
        run_id: job.stable_key("20260911"),
        job_id: job.job_id.clone(),
        trading_day: "20260911".into(),
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

fn assert_control_queue_semantics(queue: &dyn ControlCommandQueueBackend) {
    let control = command(101);
    queue.enqueue_command(control.clone(), 10).unwrap();
    queue.enqueue_command(control, 10).unwrap();
    assert_eq!(queue.available_commands(10).unwrap().len(), 1);

    let first = queue.claim_command(101, "worker-a", 10, 10).unwrap();
    assert_eq!(first.fencing_token, 1);
    assert!(matches!(
        queue.claim_command(101, "worker-b", 11, 10),
        Err(StorageError::LeaseHeld { .. })
    ));
    let takeover = queue.claim_command(101, "worker-b", 20, 10).unwrap();
    assert_eq!(takeover.fencing_token, 2);
    assert!(matches!(
        queue.ack_command_at(101, "worker-a", first.fencing_token, 21),
        Err(StorageError::Unauthorized(_))
    ));
    queue
        .ack_command_at(101, "worker-b", takeover.fencing_token, 21)
        .unwrap();
    assert!(queue.available_commands(22).unwrap().is_empty());
}

fn assert_job_queue_semantics(queue: &dyn JobQueueBackend) {
    let (job, run) = job_and_run(202);
    let run_id = run.run_id;
    queue.enqueue_job(job.clone(), run.clone(), 10).unwrap();
    queue.enqueue_job(job, run, 10).unwrap();
    assert_eq!(queue.available_jobs(10).unwrap().len(), 1);

    let first = queue.claim_job(run_id, "worker-a", 10, 10).unwrap();
    assert_eq!(first.fencing_token, 1);
    assert!(matches!(
        queue.ack_job_at(run_id, "worker-a", first.fencing_token, 20),
        Err(StorageError::LeaseExpired { .. })
    ));
    assert_eq!(queue.recover_expired_leases(20).unwrap(), vec![run_id]);
    let takeover = queue.claim_job(run_id, "worker-b", 21, 10).unwrap();
    assert_eq!(takeover.fencing_token, 2);
    assert!(matches!(
        queue.ack_job_at(run_id, "worker-a", first.fencing_token, 21),
        Err(StorageError::Unauthorized(_))
    ));
    queue
        .ack_job_at(run_id, "worker-b", takeover.fencing_token, 21)
        .unwrap();
    assert!(queue.available_jobs(22).unwrap().is_empty());
}

#[test]
fn file_backends_share_the_persistent_queue_contract() {
    let root = temp_root("file");
    let control = ControlCommandQueue::new(root.join("control"));
    let jobs = FileJobQueue::new(root.join("jobs"));
    assert_control_queue_semantics(&control);
    assert_job_queue_semantics(&jobs);
    let _ = std::fs::remove_dir_all(root);
}

#[cfg(feature = "sqlite")]
#[test]
fn sqlite_backends_share_the_persistent_queue_contract() {
    let root = temp_root("sqlite");
    let control = SqliteControlCommandQueue::new(root.join("queues.sqlite")).unwrap();
    let jobs = SqliteJobQueue::new(root.join("queues.sqlite")).unwrap();
    assert_control_queue_semantics(&control);
    assert_job_queue_semantics(&jobs);
    let _ = std::fs::remove_dir_all(root);
}

#[cfg(feature = "postgres")]
#[test]
#[ignore = "requires QX_TEST_POSTGRES_DSN and a running PostgreSQL instance"]
fn postgres_backends_share_the_persistent_queue_contract() {
    let dsn = std::env::var("QX_TEST_POSTGRES_DSN")
        .expect("QX_TEST_POSTGRES_DSN must point at an isolated test database");
    let control = PostgresControlCommandQueue::connect_with_pool_size(&dsn, 2).unwrap();
    let jobs = PostgresJobQueue::connect_with_pool_size(&dsn, 2).unwrap();
    assert_control_queue_semantics(&control);
    assert_job_queue_semantics(&jobs);
}
