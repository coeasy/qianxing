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
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target")
        .join(format!(
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

fn job_and_run_on_day(seed: u64, trading_day: &str) -> (JobSpec, JobRun) {
    let (mut job, mut run) = job_and_run(seed);
    job.job_id = format!("storage-job-order-{seed}");
    job.idempotency_key = format!("storage-job-order-{seed}-daily");
    run.job_id = job.job_id.clone();
    run.trading_day = trading_day.into();
    run.run_id = job.stable_key(trading_day);
    (job, run)
}

fn assert_control_queue_semantics(queue: &dyn ControlCommandQueueBackend) {
    let control = command(101);
    queue.enqueue_command(control.clone(), 10).unwrap();
    queue.enqueue_command(control, 10).unwrap();
    queue.enqueue_command(command(10), 2).unwrap();
    queue.enqueue_command(command(102), 10).unwrap();

    let mut conflicting = command(101);
    conflicting.request_id = "different-request".into();
    assert!(matches!(
        queue.enqueue_command(conflicting, 10),
        Err(StorageError::Conflict(_))
    ));
    let available = queue.available_commands(10).unwrap();
    assert_eq!(
        available
            .iter()
            .map(|queued| (queued.enqueued_ts, queued.command.command_id))
            .collect::<Vec<_>>(),
        vec![(2, 10), (10, 101), (10, 102)]
    );

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
    for command_id in [10, 102] {
        let lease = queue.claim_command(command_id, "worker-c", 22, 10).unwrap();
        queue
            .ack_command_at(command_id, "worker-c", lease.fencing_token, 23)
            .unwrap();
    }
    assert!(queue.available_commands(22).unwrap().is_empty());
}

fn assert_job_queue_semantics(queue: &dyn JobQueueBackend) {
    let ordered = [
        job_and_run_on_day(301, "20260911"),
        job_and_run_on_day(302, "20260910"),
        job_and_run_on_day(303, "20260910"),
    ];
    let mut expected: Vec<_> = ordered
        .iter()
        .map(|(_, run)| (run.trading_day.clone(), run.run_id))
        .collect();
    expected.sort();
    for (job, run) in ordered {
        queue.enqueue_job(job, run, 10).unwrap();
    }
    let available = queue.available_jobs(10).unwrap();
    assert_eq!(
        available
            .iter()
            .map(|queued| (queued.run.trading_day.clone(), queued.run.run_id))
            .collect::<Vec<_>>(),
        expected
    );
    for (_, run_id) in expected {
        let lease = queue.claim_job(run_id, "ordering-worker", 11, 10).unwrap();
        queue
            .ack_job_at(run_id, "ordering-worker", lease.fencing_token, 12)
            .unwrap();
    }

    let (job, run) = job_and_run(202);
    let run_id = run.run_id;

    let mut invalid_run = run.clone();
    invalid_run.run_id = invalid_run.run_id.saturating_add(1);
    assert!(matches!(
        queue.enqueue_job(job.clone(), invalid_run, 10),
        Err(StorageError::Conflict(_))
    ));
    assert!(matches!(
        queue.claim_job(run_id, "  ", 10, 10),
        Err(StorageError::Conflict(_))
    ));
    assert!(matches!(
        queue.claim_job(run_id, "worker-a", 10, 0),
        Err(StorageError::Conflict(_))
    ));

    queue.enqueue_job(job.clone(), run.clone(), 10).unwrap();
    queue.enqueue_job(job, run, 10).unwrap();
    assert_eq!(queue.available_jobs(10).unwrap().len(), 1);

    // A restarted scheduler may retry the same stable run with stale run
    // timestamps. The already-persisted queue envelope remains authoritative.
    let (job, mut retry) = job_and_run(202);
    retry.attempt = 2;
    retry.started_ts = 11;
    retry.deadline_ts = 71;
    queue.enqueue_job(job.clone(), retry, 11).unwrap();
    let available = queue.available_jobs(11).unwrap();
    assert_eq!(available.len(), 1);
    assert_eq!(available[0].run.attempt, 1);
    assert_eq!(available[0].run.started_ts, 10);

    // Reusing the stable run ID for a different job payload is a conflict.
    let mut conflicting_job = job;
    conflicting_job.timeout_seconds = conflicting_job.timeout_seconds.saturating_add(1);
    assert!(matches!(
        queue.enqueue_job(conflicting_job, job_and_run(202).1, 11),
        Err(StorageError::Conflict(_))
    ));

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
