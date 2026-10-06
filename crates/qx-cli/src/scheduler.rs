//! 调度器：UTC tick 归一、调度状态装载、到期作业分派，以及实时策略作业的规格构造。

use super::*;

pub(crate) fn utc_schedule_tick(timestamp_ms: u64) -> (String, ScheduleTick) {
    let seconds = timestamp_ms / 1_000;
    let days = (seconds / 86_400) as i64;
    let seconds_in_day = seconds % 86_400;
    let hour = (seconds_in_day / 3_600) as u8;
    let minute = ((seconds_in_day % 3_600) / 60) as u8;
    let (year, month, day) = civil_from_days(days);
    let weekday = (days + 4).rem_euclid(7) as u8;
    (
        format!("{year:04}{month:02}{day:02}"),
        ScheduleTick {
            minute,
            hour,
            day,
            month,
            weekday,
        },
    )
}

// Howard Hinnant 的 civil-from-days 算法；调度统一使用 UTC，避免把本机时区
// 隐式带入 RunManifest 和 Cron 判定。
pub(crate) fn civil_from_days(days: i64) -> (i32, u8, u8) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    let year = y + if month <= 2 { 1 } else { 0 };
    (year as i32, month as u8, day as u8)
}

pub(crate) fn scheduler_manifest(
    worker_id: &str,
    trading_day: &str,
    now: u64,
) -> qx_core::RunManifest {
    qx_core::RunManifest {
        run_id: format!("{worker_id}-{trading_day}-{now}"),
        code_commit: build_identity::BUILD_REVISION.into(),
        config_hash: "runtime-scheduler-v1".into(),
        data_fingerprint: format!("scheduler:{trading_day}"),
        input_components: BTreeMap::new(),
        clock_start: now,
        clock_end: now,
        global_seed: 0,
        determinism_mode: true,
        result_hash: format!("scheduler-{now}"),
        strategy_version: "scheduler-dispatch-v1".into(),
        instrument_spec_version: "runtime-v1".into(),
        model_fingerprint: "scheduler-dispatch".into(),
        input_event_hash: format!("input-{now}"),
        output_event_hash: format!("output-{now}"),
        runtime_version: build_identity::RUNTIME_VERSION.into(),
    }
}

/// 实时策略每根闭合 Bar 的作业超时；同时是 `JobRun.deadline_ts` 的推算口径。
pub(crate) const LIVE_STRATEGY_TIMEOUT_SECONDS: u64 = 60;

/// 实时策略 worker 每一根闭合 Bar 对应的作业规格与运行记录。
///
/// `idempotency_key` 就是 BarFrame 指纹：同一根闭合 Bar 重放不会二次下单；
/// `dry_run` 只按环境的 `paper` 判定，与 worker 角色无关。能走到这里的写法已由
/// `RuntimeConfig::validate` 的闭合名单 `ENVIRONMENT_VOCAB` 收口，四种写法各自的提交臂
/// 由 `tests/environment_submit_arm_table.rs` 钉住。
pub(crate) fn live_strategy_job(
    strategy: &StrategyRuntimeConfig,
    worker_id: &str,
    data_fingerprint: u64,
    now: u64,
    environment: &str,
) -> (JobSpec, qx_scheduler::JobRun) {
    let trading_day = utc_schedule_tick(now).0;
    let job = JobSpec {
        job_id: format!("live-strategy:{worker_id}"),
        job_version: strategy.version.clone(),
        owner: worker_id.into(),
        enabled: true,
        trigger: Trigger::Manual,
        window: JobWindow::Any,
        depends_on: Vec::new(),
        input_refs: vec![format!("barframe:{data_fingerprint:016x}")],
        output_refs: vec!["strategy-submit-order".into()],
        timeout_seconds: LIVE_STRATEGY_TIMEOUT_SECONDS,
        retry_policy: RetryPolicy::default(),
        concurrency_key: format!(
            "live-strategy:{}",
            strategy.instrument.as_deref().unwrap_or("")
        ),
        idempotency_key: format!("barframe:{data_fingerprint:016x}"),
        permission_scope: "strategy".into(),
        audit_reason: "live-closed-bar".into(),
        dry_run: environment.eq_ignore_ascii_case("paper"),
    };
    let run_id = job.stable_key(&trading_day);
    // JobRun 的时间戳属于租约域（与 `start_run_at` 写出的记录同一口径），因此这里必须
    // 用秒；否则同一份队列里会同时存在"按秒比较"和"按毫秒填写"的 deadline。
    let lease_now = lease_clock(now);
    let run = qx_scheduler::JobRun {
        run_id,
        job_id: job.job_id.clone(),
        trading_day,
        attempt: 1,
        status: JobStatus::Running,
        manifest_digest: Some(data_fingerprint),
        error_code: None,
        next_retry_ts: None,
        started_ts: lease_now,
        deadline_ts: lease_now.saturating_add(job.timeout_seconds),
    };
    (job, run)
}

pub(crate) fn scheduler_jobs_path(runtime_config_path: &Path, configured: &str) -> PathBuf {
    resolve_runtime_relative_path(runtime_config_path, configured)
}

/// 运行时 tick 能派发的 JobSpec 形状；不在集合内的形状必须**在装载时就拒**，
/// 不能收下之后再静默不跑（V12 §18-B #117）。
///
/// `dispatch_scheduled_jobs` 走 `due_jobs`：只认 Cron 触发，且没有任何生产路径能造出
/// `TradingCalendar` 的时段数据 —— 库侧的 `due_jobs_with_calendar` 需要一张有人填的日历，
/// 而日历的 `Session` 至今只在用例里出现过。于是把 `window` 写成 `Session`/`PostClose`
/// 的作业要么永远不出队，要么（今天的 `due_jobs` 不看 window）在收盘前后一样触发。
/// 两种都是"配置声明了、运行时不认"，比拒绝更难发现。
pub(crate) fn unsupported_dispatch_shape(job: &JobSpec) -> Option<String> {
    let cron = matches!(&job.trigger, Trigger::Cron(_));
    if !cron || job.window != JobWindow::Any {
        return Some(format!(
            "作业 {} 声明了运行时不会派发的形状：trigger={:?} window={:?}；scheduler-worker 的 tick 只跑 Cron 触发且 window=Any 的作业（交易日历/事件/手工触发没有生产派发者，窗口判定没有交易日历数据源）",
            job.job_id, job.trigger, job.window
        ));
    }
    None
}

/// 读取并核验声明文件里的 JobSpec。形状闸门对每一次启动都成立，不只是第一次。
fn declared_jobs(jobs_path: &Path) -> Result<Vec<JobSpec>, String> {
    let jobs: Vec<JobSpec> = serde_json::from_str(
        &std::fs::read_to_string(jobs_path)
            .map_err(|error| format!("读取 Scheduler JobSpec 失败: {error}"))?,
    )
    .map_err(|error| format!("Scheduler JobSpec JSON 无效: {error}"))?;
    let refused = jobs
        .iter()
        .filter_map(unsupported_dispatch_shape)
        .collect::<Vec<_>>();
    if !refused.is_empty() {
        return Err(refused.join("；"));
    }
    Ok(jobs)
}

/// 作业集合只在状态文件缺失时从 `jobs_path` 重建，所以第二次启动起，声明改动都不会自动
/// 生效 —— 旧实现连这个分叉都不报：删掉的作业继续按 cron 触发，新增的永远不跑，改过的
/// 用的还是状态里的旧形状（V13 R17-e）。三个方向都能在启动当场判出来，因此 fail closed。
fn reject_job_set_drift(
    scheduler: &Scheduler,
    declared: &[JobSpec],
    jobs_path: &Path,
) -> Result<(), String> {
    let declared_ids = declared
        .iter()
        .map(|job| job.job_id.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let mut drift = Vec::new();
    if declared_ids.len() != declared.len() {
        drift.push("声明文件里有重名 job_id，同名声明的后者永远不会生效".to_string());
    }
    for job in declared {
        match scheduler.job(&job.job_id) {
            None => drift.push(format!(
                "作业 {} 只在声明文件里 —— 新增的声明从未进入调度状态",
                job.job_id
            )),
            Some(state_job) if state_job != job => drift.push(format!(
                "作业 {} 的声明与调度状态里的形状不同 —— 改动过的声明从未生效",
                job.job_id
            )),
            Some(_) => {}
        }
    }
    if scheduler.len() > declared_ids.len() {
        drift.push(format!(
            "调度状态里有 {} 个作业不在声明文件里 —— 删掉的声明不会让它们停手，它们仍会按 cron 触发",
            scheduler.len() - declared_ids.len()
        ));
    }
    if drift.is_empty() {
        return Ok(());
    }
    Err(format!(
        "Scheduler 声明与状态已分叉: {}；作业集合只在 {} 缺失时重建，要让改动生效必须先把状态文件移走",
        drift.join("；"),
        jobs_path.display()
    ))
}

pub(crate) fn load_scheduler_state(
    config: &RuntimeConfig,
    root: &Path,
    runtime_config_path: &Path,
) -> Result<(JsonStateStore, Scheduler, PathBuf), String> {
    let store = JsonStateStore::new(root.to_path_buf());
    let state_reference = Path::new(&config.scheduler.state_path).to_path_buf();
    let state_path = runtime_path(root, &config.scheduler.state_path);
    let jobs_path = scheduler_jobs_path(runtime_config_path, &config.scheduler.jobs_path);
    let declared = if jobs_path.exists() {
        declared_jobs(&jobs_path)?
    } else {
        Vec::new()
    };
    let scheduler = if state_path.exists() {
        let scheduler = store
            .load_scheduler_at(&state_reference)
            .map_err(|error| format!("加载 Scheduler 状态失败: {error:?}"))?;
        reject_job_set_drift(&scheduler, &declared, &jobs_path)?;
        scheduler
    } else {
        let mut scheduler = Scheduler::default();
        for job in declared {
            scheduler
                .register(job)
                .map_err(|error| format!("注册 Scheduler JobSpec 失败: {error:?}"))?;
        }
        store
            .save_scheduler_at(&state_reference, &scheduler)
            .map_err(|error| format!("初始化 Scheduler 状态失败: {error:?}"))?;
        scheduler
    };
    Ok((store, scheduler, state_reference))
}

/// 一次调度 tick 的三类结果：真正入队的作业、被升级的超时运行、到期但没派发出去的作业。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct DispatchSummary {
    pub(crate) queued: usize,
    pub(crate) timed_out: usize,
    pub(crate) skipped: usize,
}

pub(crate) fn dispatch_scheduled_jobs(
    state_store: &JsonStateStore,
    state_path: &Path,
    queue: &ConfiguredJobQueue,
    tick: &ScheduleTick,
    trading_day: &str,
    manifest: &qx_core::RunManifest,
    now: u64,
) -> Result<DispatchSummary, String> {
    // 每一次派发都会把 manifest.digest() 写进 JobRun 作为血缘锚点：摘要来自一个自身非法的
    // manifest 时，跑完的作业会指向一条无法复现的运行身份，因此先 fail-closed（V12 §18-B #117）。
    manifest.validate()?;
    // Scheduler 的 deadline/retry 与 JobQueue 的租约都按秒比较，墙钟是毫秒。
    let lease_now = lease_clock(now);
    let (_, result) = state_store
        .transact_scheduler_at(state_path, |scheduler| {
            // 先把已过截止时间的 Running 运行升级为人工接管：Running 运行持有并发键，
            // 不升级就会永远挡住同一 key 的后续交易日，且卡住的作业自身再也不会出队。
            let mut timed_out = 0_usize;
            for run in scheduler.runs() {
                if scheduler
                    .is_timed_out(run.run_id, lease_now)
                    .map_err(|error| format!("判定 JobRun 超时失败: {error:?}"))?
                {
                    scheduler
                        .mark_timed_out(run.run_id, lease_now)
                        .map_err(|error| format!("升级超时 JobRun 失败: {error:?}"))?;
                    timed_out += 1;
                }
            }
            let completed = scheduler.completed_jobs();
            let job_ids = scheduler
                .due_jobs(tick, &completed)
                .map_err(|error| format!("计算 Scheduler 到期任务失败: {error:?}"))?
                .into_iter()
                .map(|job| job.job_id.clone())
                .collect::<Vec<_>>();
            let mut queued = 0_usize;
            let mut skipped = 0_usize;
            for job_id in job_ids {
                let job = scheduler
                    .job(&job_id)
                    .cloned()
                    .ok_or_else(|| format!("Scheduler JobSpec 不存在: {job_id}"))?;
                let run = match scheduler.start_run_at(
                    &job_id,
                    trading_day,
                    manifest.digest(),
                    lease_now,
                ) {
                    Ok(run) => run,
                    // 并发键被占用只是"这一轮不能跑"，不能让一个作业卡死整个调度器。
                    Err(qx_scheduler::SchedulerError::NotReady(_)) => {
                        skipped += 1;
                        continue;
                    }
                    Err(error) => return Err(format!("创建 JobRun 失败: {error:?}")),
                };
                if run.status == JobStatus::Running {
                    queue
                        .enqueue(job, run, lease_now)
                        .map_err(|error| format!("写入 JobQueue 失败: {error:?}"))?;
                    queued += 1;
                } else {
                    // 同一交易日已有终态/待接管运行：到期判定成立但不再派发。
                    skipped += 1;
                }
            }
            Ok(DispatchSummary {
                queued,
                timed_out,
                skipped,
            })
        })
        .map_err(|error| format!("Scheduler 状态事务失败: {error:?}"))?;
    result
}

/// 这条运行是否已经收口。队列条目可能在 Scheduler 回写终态之后、`ack` 之前掉电：条目还在，
/// 运行却已经是终态，再执行一次就是二次提交，所以 worker 领取租约后先问这一句（V13 R2 第十二遍 #190）。
///
/// 读不到运行记录按「未终态」处理：实时策略作业的运行从来不入 Scheduler 状态（只在 JobQueue 里），
/// 把它当成终态会让这类作业永远跑不了。终态只有这三种（`JobStatus` 已退役 `Pending`/`Paused`
/// 两颗零构造档位），所以「不是终态」等价于「还在 `Running`」。
pub(crate) fn strategy_run_is_final(
    state_store: &JsonStateStore,
    state_path: &Path,
    run_id: u64,
) -> Result<bool, String> {
    Ok(state_store
        .load_scheduler_at(state_path)
        .map_err(|error| format!("读取 JobRun 终态失败: {error:?}"))?
        .run(run_id)
        .is_some_and(|run| {
            matches!(
                run.status,
                JobStatus::Succeeded | JobStatus::Failed | JobStatus::NeedsIntervention
            )
        }))
}

/// 把策略作业的失败写回它自己的 JobRun（`JobStatus::Failed` + 固定错误码）。
///
/// 失败不回写的话，这条运行只剩「被下一轮调度 tick 升级成 `error_code="TIMEOUT"`」这一条出口，
/// 于是「策略自己报错了」会被读成「策略跑太久」，而 `JobStatus::Failed` 在生产里一个生产者都没有。
/// 错误码是固定的 `STRATEGY_JOB_FAILED`：结果码（`3 orders: ORDER_INTENT_ACCEPTED`）不进 `JobRun`，
/// 那是接口文档写明的口径。实时策略作业没有 Scheduler 侧的运行记录，与成功收口共用同一条豁免。
pub(crate) fn fail_strategy_job_run(
    state_store: &JsonStateStore,
    state_path: &Path,
    job_id: &str,
    run_id: u64,
    finished_ts: u64,
) -> Result<(), String> {
    if job_id.starts_with("live-strategy:") {
        return Ok(());
    }
    state_store
        .transact_scheduler_at(state_path, |scheduler| {
            scheduler
                .finish_run_with_code(run_id, false, Some("STRATEGY_JOB_FAILED"), finished_ts)
                .map(|_| ())
                .map_err(|error| format!("失败收口 JobRun 被拒绝: {error:?}"))
        })
        .map_err(|error| format!("回写失败 JobRun 状态失败: {error:?}"))?
        .1
}

/// 一条队列条目此刻的执行现场：确认条目（`ack_at`）要用到的队列、租约与身份。
///
/// 与快照指纹一起，正是 `live_strategy_job_is_stale` 需要的全部入参；把它打包是因为
/// 执行前与执行中两处调用共用同一份现场，而十入参的函数在 `clippy::too_many_arguments`
/// 那格里是判红的（V13 R2 收口 #209：九步构建的 `[5/9]` 才第一次跑到这条）。
pub(crate) struct StrategyJobLease<'a> {
    pub(crate) queue: &'a ConfiguredJobQueue,
    pub(crate) queued: &'a qx_storage::QueuedJob,
    pub(crate) worker_id: &'a str,
    pub(crate) fencing_token: u64,
    pub(crate) lease_now: u64,
}

/// 实时策略作业的快照指纹闸门：指纹一变，这笔订单的依据就不是生成时看到的那一根闭合 Bar，
/// 于是确认掉条目并播报跳过。执行前与执行中两处判定共用这段收口（V13 R2 第十二遍 #190 顺带收口）。
///
/// 返回 `true` 表示调用方应当跳过这次执行；`expected_digest` 为 `None` 的不是实时策略作业，直接放行。
/// `digest_now` 是取快照指纹的时钟：执行前用本轮 tick，执行中用重新读取的墙钟。
pub(crate) fn live_strategy_job_is_stale(
    lease: &StrategyJobLease<'_>,
    strategy: &StrategyRuntimeConfig,
    expected_digest: Option<u64>,
    digest_now: u64,
    reason: &str,
    ack_failure: &str,
) -> Result<bool, String> {
    let Some(expected_digest) = expected_digest else {
        return Ok(false);
    };
    let current_digest = live_strategy_snapshot_digest(strategy, digest_now)?;
    if current_digest == Some(expected_digest) {
        return Ok(false);
    }
    lease
        .queue
        .ack_at(
            lease.queued.run.run_id,
            lease.worker_id,
            lease.fencing_token,
            lease.lease_now,
        )
        .map_err(|error| format!("{ack_failure}: {error:?}"))?;
    println!(
        "[策略 · Strategy] worker={} job={} skipped={reason} expected={expected_digest:016x} actual={}",
        lease.worker_id,
        lease.queued.job.job_id,
        current_digest
            .map(|digest| format!("{digest:016x}"))
            .unwrap_or_else(|| "none".into())
    );
    Ok(true)
}
