//! 调度派发形状的装载闸门：运行时不会派发的 JobSpec 必须在装载时就拒（V12 §18-B #117）。
//!
//! `scheduler-worker` 的一次 tick 只会把「Cron 触发 + window=Any + 一次尝试」的作业入队：
//! 交易日历触发没有生产派发者，窗口判定更没有能填出 `Session` 的日历数据源。在那之前，一个把
//! `window` 写成 `Session` 的作业照样会被 `due_jobs` 放行 —— 于是它在收盘前后一样触发；而
//! `Trigger::Manual`/`Event` 的作业被收下后永远不出队。两种都是「配置声明了、运行时不认」，
//! 比直接拒绝难发现得多，所以闸门放在装载点：`load_scheduler_state` 的两条路径都问
//! `validate_job_triggers`，而判据与派发器共用 `qx_scheduler::undispatchable_by_registry`。

use super::*;

fn job_with(trigger: Trigger, window: JobWindow) -> JobSpec {
    JobSpec {
        job_id: format!("shape-{}", window_name(&window)),
        job_version: "1".into(),
        // owner 必须命中该拓扑里启用的 Strategy worker：装载先问 owner 再问形状，
        // 填一个没人领取的 owner 会让用例拒在另一条判据上，什么也证明不了。
        owner: "strategy-paper".into(),
        enabled: true,
        trigger,
        window,
        depends_on: Vec::new(),
        timeout_seconds: 60,
        retry_policy: RetryPolicy::default(),
        concurrency_key: "shape".into(),
        idempotency_key: "shape:idem".into(),
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

/// 三种运行时不会派发的形状必须当场拒，且报错点名是哪个作业、拒的是哪一格声明。
#[test]
fn load_refuses_dispatch_shapes_the_tick_will_never_honor() {
    for (slug, label, reason, job) in [
        (
            "calendar",
            "交易日历触发",
            "只认 `Trigger::Cron`",
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
            "只认 `Trigger::Cron`",
            job_with(Trigger::Manual, JobWindow::Any),
        ),
        (
            "event",
            "事件触发",
            "只认 `Trigger::Cron`",
            job_with(Trigger::Event("bars-ready".into()), JobWindow::Any),
        ),
        (
            "session-window",
            "盘中窗口",
            "非 `Any` 窗口",
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
            error.contains("的触发声明在运行时派发不到"),
            "{label}：{error}"
        );
        assert!(
            error.contains(reason),
            "{label} 的拒因不是预期的那一条（{reason}）：{error}"
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

/// 装载有两条路径，闸门两条都要问：状态文件是上一轮落盘的产物，载入分支根本不读
/// jobs.json。只挡新建路径等于放行一份已经腐烂的状态——作业文件后来被改成派发不到的
/// 形状、或状态文件本身来自另一套拓扑时，运行照样开始（V11 N4 的 reload 半边）。
#[test]
fn loaded_state_file_is_revalidated_for_the_dispatch_shape() {
    // 先按受支持的形状装配一轮：载入分支必须照常放行，否则下面的红只是"改什么都红"。
    let root = temp_cli_case_dir("dispatch-shape-reload");
    let (config, config_path) = runtime_for_jobs(
        &root,
        &[job_with(Trigger::Cron("* * * * *".into()), JobWindow::Any)],
    );
    let (_, scheduler, _) =
        load_scheduler_state(&config, &root, &config_path).expect("首轮装载应通过并落盘");
    assert_eq!(scheduler.len(), 1);
    assert!(
        root.join(config.scheduler.state_path.as_str()).exists(),
        "首轮装载必须写出调度状态文件，下一轮才真走载入分支"
    );

    // 状态文件里的作业换成 `Trigger::Manual`：只能直接覆写状态文件，改 jobs.json
    // 验到的是另一条路径。owner 仍是启用的 worker，所以拒因只可能是形状那一格。
    let stale = job_with(Trigger::Manual, JobWindow::Any);
    let mut stale_scheduler = Scheduler::default();
    stale_scheduler
        .register(stale.clone())
        .expect("注册只按字段校验，派发不到的形状照样登记得进来");
    JsonStateStore::new(root.clone())
        .save_scheduler_at(
            Path::new(config.scheduler.state_path.as_str()),
            &stale_scheduler,
        )
        .expect("覆写调度状态文件");
    let error = match load_scheduler_state(&config, &root, &config_path) {
        Ok(_) => panic!("载入既有状态时同样要复核派发形状"),
        Err(error) => error,
    };
    assert!(
        error.contains("的触发声明在运行时派发不到"),
        "载入分支没有走那颗形状判据：{error}"
    );
    assert!(error.contains("只认 `Trigger::Cron`"), "{error}");
    assert!(error.contains(&stale.job_id), "报错没点名作业：{error}");
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
