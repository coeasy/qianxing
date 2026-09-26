//! 调度派发形状的装载闸门：运行时不会派发的 JobSpec 必须在装载时就拒（V12 §18-B #117）。
//!
//! `scheduler-worker` 的一次 tick 只会把「Cron 触发 + window=Any」的作业入队：交易日历触发
//! 没有生产派发者，窗口判定更没有能填出 `Session` 的日历数据源。在那之前，一个把 `window`
//! 写成 `Session` 的作业照样会被 `due_jobs` 放行 —— 于是它在收盘前后一样触发；而
//! `Trigger::Manual`/`Event` 的作业被收下后永远不出队。两种都是「配置声明了、运行时不认」，
//! 比直接拒绝难发现得多，所以闸门放在装载点。

use super::*;

fn job_with(trigger: Trigger, window: JobWindow) -> JobSpec {
    JobSpec {
        job_id: format!("shape-{}", window_name(&window)),
        job_version: "1".into(),
        owner: "scheduler-1".into(),
        enabled: true,
        trigger,
        window,
        depends_on: Vec::new(),
        input_refs: vec!["dataset:shape".into()],
        output_refs: vec!["shape-report".into()],
        timeout_seconds: 60,
        retry_policy: RetryPolicy::default(),
        concurrency_key: "shape".into(),
        idempotency_key: "shape:idem".into(),
        permission_scope: "report".into(),
        audit_reason: "dispatch-shape-case".into(),
        dry_run: true,
    }
}

fn window_name(window: &JobWindow) -> &'static str {
    match window {
        JobWindow::Any => "any",
        JobWindow::PreOpen => "pre-open",
        JobWindow::Session => "session",
        JobWindow::PostClose => "post-close",
    }
}

fn runtime_for_jobs(root: &Path, jobs: &[JobSpec]) -> (RuntimeConfig, PathBuf) {
    let jobs_path = root.join("jobs.json");
    std::fs::write(
        &jobs_path,
        serde_json::to_string(jobs).expect("序列化 JobSpec 失败"),
    )
    .unwrap();
    let template = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy")
        .join("qianxing.runtime.paper-strategy.example.json");
    let mut config = read_runtime_config(&template).unwrap();
    config.scheduler.jobs_path = jobs_path.to_string_lossy().into_owned();
    config.scheduler.state_path = "scheduler-state.json".into();
    let config_path = root.join("runtime.json");
    std::fs::write(&config_path, config.to_json().unwrap()).unwrap();
    (config, config_path)
}

/// 三种运行时不会派发的形状必须当场拒，且报错点名是哪个作业、哪一格声明的。
#[test]
fn load_refuses_dispatch_shapes_the_tick_will_never_honor() {
    for (slug, label, job) in [
        (
            "calendar",
            "交易日历触发",
            job_with(
                Trigger::TradingCalendar {
                    session: "post_close".into(),
                },
                JobWindow::PostClose,
            ),
        ),
        (
            "manual",
            "手工触发",
            job_with(Trigger::Manual, JobWindow::Any),
        ),
        (
            "event",
            "事件触发",
            job_with(Trigger::Event("bars-ready".into()), JobWindow::Any),
        ),
        (
            "session-window",
            "盘中窗口",
            job_with(Trigger::Cron("* * * * *".into()), JobWindow::Session),
        ),
    ] {
        let root = temp_cli_case_dir(&format!("dispatch-shape-{slug}"));
        let (config, config_path) = runtime_for_jobs(&root, std::slice::from_ref(&job));
        let error = match load_scheduler_state(&config, &root, &config_path) {
            Ok(_) => panic!("{label} 不该被装载进调度状态"),
            Err(error) => error,
        };
        assert!(
            error.contains("只跑 Cron 触发且 window=Any"),
            "{label}：{error}"
        );
        assert!(
            error.contains(&job.job_id),
            "{label} 的报错没点名作业：{error}"
        );
    }
}

/// 反过来：运行时支持的那一种形状必须照常装载，闸门不能顺手把生产作业也拦掉。
#[test]
fn load_still_accepts_the_cron_any_shape_the_tick_dispatches() {
    let root = temp_cli_case_dir("dispatch-shape-accepted");
    let (config, config_path) = runtime_for_jobs(
        &root,
        &[job_with(Trigger::Cron("* * * * *".into()), JobWindow::Any)],
    );
    let (store, scheduler, state_path) =
        load_scheduler_state(&config, &root, &config_path).expect("受支持的形状应能装载");
    assert_eq!(scheduler.len(), 1);
    assert_eq!(store.load_scheduler_at(&state_path).unwrap().len(), 1);
}

/// 派发写进 JobRun 的血缘锚点来自 manifest.digest()：manifest 自身非法时摘要指向一条
/// 无法复现的运行，因此整轮 tick 必须在动任何状态之前就拒掉。
#[test]
fn dispatch_refuses_a_manifest_that_cannot_be_reproduced() {
    let root = temp_cli_case_dir("dispatch-manifest-invalid");
    let state_path = seeded_scheduler_for(
        &root,
        &[job_with(Trigger::Cron("* * * * *".into()), JobWindow::Any)],
    );
    let queue = ConfiguredJobQueue::Files(FileJobQueue::new(root.join("queue")));
    let now = 1_700_000_000_000;
    let (trading_day, tick) = utc_schedule_tick(now);
    let mut manifest = scheduler_manifest("scheduler-1", &trading_day, now);
    manifest.run_id.clear();
    let error = dispatch_scheduled_jobs(
        &JsonStateStore::new(root.clone()),
        &state_path,
        &queue,
        &tick,
        &trading_day,
        &manifest,
        now,
    )
    .expect_err("非法 manifest 不该被拿去盖章");
    assert!(error.contains("run_id"), "{error}");
    assert!(
        JsonStateStore::new(root.clone())
            .load_scheduler_at(&state_path)
            .unwrap()
            .runs()
            .is_empty(),
        "拒绝必须发生在写入 JobRun 之前"
    );
}

fn seeded_scheduler_for(root: &Path, jobs: &[JobSpec]) -> PathBuf {
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
