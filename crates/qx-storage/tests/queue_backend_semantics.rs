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
        timeout_seconds: 60,
        retry_policy: RetryPolicy::default(),
        concurrency_key: format!("storage-job-{run_id}"),
        idempotency_key: format!("storage-job-{run_id}-daily"),
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

/// 领取顺序的跨后端口径：同一毫秒入队的命令按 `command_id` 的数字序交付，不是
/// 这三列存成 TEXT 时 SQL 给的字典序（V11 R7-2）。生产里 `command_id` 就是本地
/// 订单发号器，第 10 笔起字典序会把它排到第 2 笔前面。
fn assert_control_queue_hands_out_commands_numerically(queue: &dyn ControlCommandQueueBackend) {
    for command_id in [2u64, 20, 3] {
        let mut queued = command(command_id);
        queued.request_id = format!("order-contract-{command_id}");
        queue.enqueue_command(queued, 10).unwrap();
    }
    let ids: Vec<u64> = queue
        .available_commands(10)
        .unwrap()
        .into_iter()
        .filter(|queued| queued.command.request_id.starts_with("order-contract-"))
        .map(|queued| queued.command.command_id)
        .collect();
    assert_eq!(ids, vec![2, 3, 20]);
    for command_id in [2u64, 3, 20] {
        let lease = queue
            .claim_command(command_id, "relay-order", 11, 5)
            .unwrap();
        queue
            .ack_command_at(command_id, "relay-order", lease.fencing_token, 12)
            .unwrap();
    }
}

/// 作业侧的同一判据：`run_id` 是 `stable_key` 的哈希，三份后端都按它的数字序领取。
/// 因为哈希给不出跨数位边界，用例自己守住夹具——一旦字典序与数字序重合，
/// 本用例就锁不住口径，必须换 `job_id` 而不是留着当空跑。
fn assert_job_queue_hands_out_jobs_numerically(queue: &dyn JobQueueBackend) {
    let mut expected = Vec::new();
    for index in 0..10u64 {
        let (job, run) = job_and_run(index);
        queue.enqueue_job(job, run.clone(), 10).unwrap();
        expected.push(run.run_id);
    }
    expected.sort_unstable();
    let mut lexicographic = expected.clone();
    lexicographic.sort_by_key(|run_id| run_id.to_string());
    assert_ne!(
        lexicographic, expected,
        "夹具的 run_id 没有跨数位边界，本用例锁不住排序口径——换 job_id 而不是留着空跑"
    );
    let handed_out: Vec<u64> = queue
        .available_jobs(10)
        .unwrap()
        .into_iter()
        .map(|queued| queued.run.run_id)
        .filter(|run_id| expected.contains(run_id))
        .collect();
    assert_eq!(handed_out, expected);
    for run_id in &expected {
        let lease = queue.claim_job(*run_id, "relay-order", 11, 5).unwrap();
        queue
            .ack_job_at(*run_id, "relay-order", lease.fencing_token, 12)
            .unwrap();
    }
}

#[test]
fn file_backends_share_the_persistent_queue_contract() {
    let root = temp_root("file");
    let control = ControlCommandQueue::new(root.join("control"));
    let jobs = FileJobQueue::new(root.join("jobs"));
    assert_control_queue_semantics(&control);
    assert_job_queue_semantics(&jobs);
    assert_control_queue_hands_out_commands_numerically(&control);
    assert_job_queue_hands_out_jobs_numerically(&jobs);
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
    assert_control_queue_hands_out_commands_numerically(&control);
    assert_job_queue_hands_out_jobs_numerically(&jobs);
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
    assert_control_queue_hands_out_commands_numerically(&control);
    assert_job_queue_hands_out_jobs_numerically(&jobs);
}
