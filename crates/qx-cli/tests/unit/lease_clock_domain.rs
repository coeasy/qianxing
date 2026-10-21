//! 租约域时钟的用例：队列/调度层按 epoch 秒比较，运行时墙钟按毫秒。
//!
//! `JobLease.expires_ts`、`ControlCommandLease.expires_ts`、outbox 租约和
//! `JobRun.deadline_ts` 都是"当前秒 + 租约秒数"写出来、再与"当前秒"比较的字段。
//! 一旦把毫秒墙钟原样喂进去，30 秒租约在 30 毫秒后就"过期"：作业还在执行就能被
//! 第二个 worker 抢走，收尾确认直接 `LeaseExpired`；`deadline_ts` 同理让超时判定
//! 永远不再成立，卡在 Running 的运行既不出队也永远占着并发键。

use super::*;

fn daily_job(job_id: &str, concurrency_key: &str, timeout_seconds: u64) -> JobSpec {
    JobSpec {
        job_id: job_id.into(),
        job_version: "1".into(),
        owner: "scheduler-1".into(),
        enabled: true,
        trigger: Trigger::Cron("* * * * *".into()),
        window: JobWindow::Any,
        depends_on: Vec::new(),
        input_refs: vec![format!("dataset:{job_id}")],
        output_refs: vec![format!("{job_id}-report")],
        timeout_seconds,
        retry_policy: RetryPolicy::default(),
        concurrency_key: concurrency_key.into(),
        idempotency_key: format!("{job_id}:idem"),
        permission_scope: "report".into(),
        audit_reason: "lease-clock-case".into(),
        dry_run: true,
    }
}

fn seeded_scheduler(root: &Path, jobs: &[JobSpec]) -> PathBuf {
    let store = JsonStateStore::new(root.to_path_buf());
    let state_path = root.join("scheduler.json");
    let mut scheduler = Scheduler::default();
    for job in jobs {
        scheduler.register(job.clone()).unwrap();
    }
    store
        .save_scheduler_at(&state_path, &scheduler)
        .expect("写入 Scheduler 状态失败");
    state_path
}

/// 毫秒墙钟只能在一处换算成秒；换算错一位就等于把所有租约缩短一千倍。
#[test]
fn lease_clock_truncates_the_wall_clock_to_seconds() {
    assert_eq!(lease_clock(1_700_000_000_123), 1_700_000_000);
    assert_eq!(lease_clock(999), 0);
    assert_eq!(lease_clock(1_000), 1);
}

/// 30 秒租约必须真的活过 30 秒：作业执行到第 29 秒时不能被第二个 worker 抢走，
/// 第 31 秒才允许接管，且旧 worker 的确认在新租约下必须被 fencing token 拒绝。
#[test]
fn a_thirty_second_lease_survives_twenty_nine_seconds_of_work() {
    let root = temp_cli_case_dir("lease-clock-queue");
    let queue = FileJobQueue::new(root.join("queue"));
    let started_ms = 1_700_000_000_000;
    let job = daily_job("daily-etf", "etf-key", 60);
    let run_id = job.stable_key("20260105");
    let run = qx_scheduler::JobRun {
        run_id,
        job_id: job.job_id.clone(),
        trading_day: "20260105".into(),
        attempt: 1,
        status: JobStatus::Running,
        manifest_digest: Some(7),
        error_code: None,
        next_retry_ts: None,
        started_ts: lease_clock(started_ms),
        deadline_ts: lease_clock(started_ms) + job.timeout_seconds,
    };
    queue
        .enqueue(job.clone(), run.clone(), lease_clock(started_ms))
        .unwrap();

    let lease = queue
        .claim(run_id, "strategy-1", lease_clock(started_ms), 30)
        .expect("首次领取租约失败");
    // 执行中的第 29 秒：租约仍持有，队列不可见。
    assert!(queue
        .available(lease_clock(started_ms + 29_000))
        .unwrap()
        .is_empty());
    // 第 31 秒：租约过期，可被接管，fencing token 递增。
    let takeover = queue
        .claim(run_id, "strategy-2", lease_clock(started_ms + 31_000), 30)
        .expect("过期租约应可被接管");
    assert_eq!(takeover.fencing_token, lease.fencing_token + 1);
    // 旧 worker 干完活再确认必须被拒——它已经不再持有租约。
    assert!(matches!(
        queue.ack_at(
            run_id,
            "strategy-1",
            lease.fencing_token,
            lease_clock(started_ms + 31_000)
        ),
        Err(StorageError::Unauthorized(_))
    ));
    // 新 worker 在同一秒内确认成功，作业真正出队。
    queue
        .ack_at(
            run_id,
            "strategy-2",
            takeover.fencing_token,
            lease_clock(started_ms + 31_000),
        )
        .expect("当前持有者确认失败");
    assert!(queue
        .available(lease_clock(started_ms + 32_000))
        .unwrap()
        .is_empty());
}

/// 实时策略作业的运行记录必须与 `timeout_seconds` 同一单位：
/// `deadline_ts` 只能等于 `started_ts + timeout_seconds`。
#[test]
fn live_strategy_run_deadline_is_measured_in_the_lease_domain() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let config = read_runtime_config(
        &workspace_root
            .join("deploy")
            .join("qianxing.runtime.paper-strategy.example.json"),
    )
    .unwrap();
    let (job, run) = live_strategy_job(
        &config.strategy,
        "strategy-1",
        0xabcd,
        1_700_000_000_123,
        "paper",
    );
    assert_eq!(run.started_ts, 1_700_000_000);
    assert_eq!(run.deadline_ts, run.started_ts + job.timeout_seconds);
    assert_eq!(
        run.trading_day,
        utc_schedule_tick(1_700_000_000_123).0,
        "交易日仍按毫秒墙钟判定"
    );
}

/// 超时升级必须真的接进调度 tick：worker 死掉后 Running 运行在下一轮 tick 被升级为
/// 人工接管（`TIMEOUT`），未到点的运行不受影响，被释放的并发键让同一键的下一个交易日
/// 作业重新派发出去。
#[test]
fn an_overdue_running_job_is_escalated_and_frees_its_concurrency_key() {
    let root = temp_cli_case_dir("lease-clock-scheduler");
    let state_path = seeded_scheduler(&root, &[daily_job("daily-etf", "etf-key", 60)]);
    let queue = ConfiguredJobQueue::Files(FileJobQueue::new(root.join("queue")));
    let started_ms = 1_700_000_000_000;
    let trading_day = utc_schedule_tick(started_ms).0;
    let manifest = scheduler_manifest("scheduler-1", &trading_day, started_ms);
    let dispatch = |now_ms: u64, day: &str| {
        let (day_label, tick) = utc_schedule_tick(now_ms);
        assert_eq!(day_label, day, "用例只在同一交易日内推进时钟");
        dispatch_scheduled_jobs(
            &JsonStateStore::new(root.clone()),
            &state_path,
            &queue,
            &tick,
            day,
            &manifest,
            now_ms,
        )
        .unwrap()
    };
    let run_of = || {
        JsonStateStore::new(root.clone())
            .load_scheduler_at(&state_path)
            .unwrap()
            .runs()
            .into_iter()
            .next()
            .expect("调度器没有留下 JobRun")
    };

    let first = dispatch(started_ms, &trading_day);
    assert_eq!((first.queued, first.timed_out, first.skipped), (1, 0, 0));
    let run = run_of();
    assert_eq!(run.status, JobStatus::Running);
    assert_eq!(run.started_ts, lease_clock(started_ms));
    assert_eq!(run.deadline_ts, lease_clock(started_ms) + 60);

    // 还在超时窗口内：绝不能把正在跑的作业判死。
    let mid = dispatch(started_ms + 30_000, &trading_day);
    assert_eq!(mid.timed_out, 0);
    assert_eq!(run_of().status, JobStatus::Running);

    // 超过 deadline 且 worker 再也没回来：下一轮 tick 必须升级。
    let late = dispatch(started_ms + 61_000, &trading_day);
    assert_eq!(late.timed_out, 1);
    let run = run_of();
    assert_eq!(run.status, JobStatus::NeedsIntervention);
    assert_eq!(run.error_code.as_deref(), Some("TIMEOUT"));
}

/// 卡住的 Running 运行持有并发键：不升级它就会永久挡住同一键的后续交易日作业。
#[test]
fn escalating_the_stuck_run_lets_the_next_trading_day_dispatch() {
    let root = temp_cli_case_dir("lease-clock-key-release");
    let jobs = [
        daily_job("daily-etf", "etf-key", 60),
        daily_job("daily-etf-next", "etf-key", 60),
    ];
    let state_path = seeded_scheduler(&root, &jobs);
    let queue = ConfiguredJobQueue::Files(FileJobQueue::new(root.join("queue")));
    let started_ms = 1_700_000_000_000;
    let (trading_day, tick) = utc_schedule_tick(started_ms);
    let manifest = scheduler_manifest("scheduler-1", &trading_day, started_ms);
    let store = JsonStateStore::new(root.clone());

    // 第一个作业起跑后 worker 消失：同一并发键的第二个作业只能被跳过。
    let first = dispatch_scheduled_jobs(
        &store,
        &state_path,
        &queue,
        &tick,
        &trading_day,
        &manifest,
        started_ms,
    )
    .unwrap();
    assert_eq!((first.queued, first.skipped), (1, 1));
    // 超过 deadline 后，第二个作业必须拿得到并发键。
    let (_, late_tick) = utc_schedule_tick(started_ms + 61_000);
    let late = dispatch_scheduled_jobs(
        &store,
        &state_path,
        &queue,
        &late_tick,
        &trading_day,
        &manifest,
        started_ms + 61_000,
    )
    .unwrap();
    assert_eq!(late.timed_out, 1);
    assert_eq!(late.queued, 1, "升级后同一并发键必须能被重新派发");
    let statuses: Vec<(String, JobStatus)> = store
        .load_scheduler_at(&state_path)
        .unwrap()
        .runs()
        .into_iter()
        .map(|run| (run.job_id, run.status))
        .collect();
    assert_eq!(
        statuses,
        vec![
            ("daily-etf".to_string(), JobStatus::NeedsIntervention),
            ("daily-etf-next".to_string(), JobStatus::Running),
        ]
    );
}
