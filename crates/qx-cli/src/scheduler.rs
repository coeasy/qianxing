//! 调度器：UTC tick 归一、调度状态装载与到期作业分派。

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
        code_commit: env!("QX_GIT_COMMIT").into(),
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
        runtime_version: env!("CARGO_PKG_VERSION").into(),
    }
}

pub(crate) fn scheduler_jobs_path(runtime_config_path: &Path, configured: &str) -> PathBuf {
    resolve_runtime_relative_path(runtime_config_path, configured)
}

pub(crate) fn load_scheduler_state(
    config: &RuntimeConfig,
    root: &Path,
    runtime_config_path: &Path,
) -> Result<(JsonStateStore, Scheduler, PathBuf), String> {
    let store = JsonStateStore::new(root.to_path_buf());
    let state_reference = Path::new(&config.scheduler.state_path).to_path_buf();
    let state_path = runtime_path(root, &config.scheduler.state_path);
    let scheduler = if state_path.exists() {
        store
            .load_scheduler_at(&state_reference)
            .map_err(|error| format!("加载 Scheduler 状态失败: {error:?}"))?
    } else {
        let mut scheduler = Scheduler::default();
        let jobs_path = scheduler_jobs_path(runtime_config_path, &config.scheduler.jobs_path);
        if jobs_path.exists() {
            let jobs: Vec<JobSpec> = serde_json::from_str(
                &std::fs::read_to_string(&jobs_path)
                    .map_err(|error| format!("读取 Scheduler JobSpec 失败: {error}"))?,
            )
            .map_err(|error| format!("Scheduler JobSpec JSON 无效: {error}"))?;
            for job in jobs {
                scheduler
                    .register(job)
                    .map_err(|error| format!("注册 Scheduler JobSpec 失败: {error:?}"))?;
            }
        }
        // 先判再落盘：被拒绝的拓扑不该留下一份调度状态文件让下一次运行继续读它。
        validate_job_owners(config, &scheduler)?;
        validate_job_triggers(&scheduler)?;
        store
            .save_scheduler_at(&state_reference, &scheduler)
            .map_err(|error| format!("初始化 Scheduler 状态失败: {error:?}"))?;
        scheduler
    };
    // 载入的既有状态同样要问：状态文件可能是另一套拓扑或改坏的 owner 留下的。
    validate_job_owners(config, &scheduler)?;
    validate_job_triggers(&scheduler)?;
    Ok((store, scheduler, state_reference))
}

/// 作业 owner 必须真有人领取。Scheduler 只负责入队，领取判据在
/// `workers.rs` 的 `queued.job.owner != context.id()`；owner 拼错或指向未启用的
/// worker 时，作业永远留在队列里，而 `start_run_at` 已把 JobRun 标成 Running，
/// 命令面照样打印 `READY processed=0`——整段调度事实就这样丢了（V11 §41 E7）。
fn validate_job_owners(config: &RuntimeConfig, scheduler: &Scheduler) -> Result<(), String> {
    let claimants = config
        .workers
        .iter()
        .filter(|worker| worker.enabled && worker.role == WorkerRole::Strategy)
        .map(|worker| worker.id.as_str())
        .collect::<Vec<_>>();
    let claimants_note = if claimants.is_empty() {
        "该拓扑没有启用的 Strategy worker".to_string()
    } else {
        format!("启用的 Strategy worker: {}", claimants.join(", "))
    };
    for job in scheduler.jobs() {
        if !job.enabled {
            continue;
        }
        let routable = claimants
            .iter()
            .any(|claimant| qx_scheduler::claimable_by(&job.owner, claimant));
        if !routable {
            return Err(format!(
                "Scheduler 作业 {} 的 owner {:?} 无人领取（{claimants_note}）；\
                 请把 owner 改成启用的 Strategy worker id，或使用 {:?} 交给任意 worker",
                job.job_id,
                job.owner,
                qx_scheduler::JOB_OWNER_ANY
            ));
        }
    }
    Ok(())
}

/// 作业文件里的触发形状必须落在派发器真走得到的那一面上。运行时注册表只派发
/// `Trigger::Cron` + `JobWindow::Any` + 一次尝试：其余声明今天**什么都不触发**，
/// 而 `config validate` 会把它们逐条打印成合法——挡在启动前比留着当暗雷诚实
/// （判据与派发器共用 `qx_scheduler::undispatchable_by_registry`，V11 N4）。
fn validate_job_triggers(scheduler: &Scheduler) -> Result<(), String> {
    for job in scheduler.jobs() {
        if !job.enabled {
            continue;
        }
        if let Some(reason) = qx_scheduler::undispatchable_by_registry(job) {
            return Err(format!(
                "Scheduler 作业 {} 的触发声明在运行时派发不到：{reason}；\
                 请改成 `\"trigger\": {{\"Cron\": ...}}` + `\"window\": \"Any\"` + \
                 `retry_policy.max_attempts: 1`，或把这类作业交给自己的派发端",
                job.job_id
            ));
        }
    }
    Ok(())
}

pub(crate) fn dispatch_scheduled_jobs(
    state_store: &JsonStateStore,
    state_path: &Path,
    queue: &ConfiguredJobQueue,
    tick: &ScheduleTick,
    trading_day: &str,
    manifest: &qx_core::RunManifest,
    now: u64,
) -> Result<usize, String> {
    let (_, result) = state_store
        .transact_scheduler_at(state_path, |scheduler| {
            // 先收超时：一条卡死的 Running 会一直占着并发键，之后每一轮派发都拿
            // NotReady，而这份事实只在状态文件里躺着（V11 N2）。
            scheduler
                .sweep_timed_out(now)
                .map_err(|error| format!("收口超时 JobRun 失败: {error:?}"))?;
            let completed = scheduler.completed_jobs();
            // 窗口与交易日历的判定只有 `due_jobs_with_calendar` 这一颗；生产派发走它，
            // 传空历是因为运行时还没有日历写入者——装配处已经挡掉非 `Any` 窗口的作业，
            // 所以空历不改变任何被接受作业的判定结果。接上日历源时把这一颗的入参换掉，
            // 不要退回只认 Cron 的 `due_jobs`（那等于把窗口判定重新变成没人调的孤儿）。
            let job_ids = scheduler
                .due_jobs_with_calendar(
                    tick,
                    trading_day,
                    now,
                    &qx_scheduler::TradingCalendar::default(),
                    &completed,
                )
                .map_err(|error| format!("计算 Scheduler 到期任务失败: {error:?}"))?
                .into_iter()
                .map(|job| job.job_id.clone())
                .collect::<Vec<_>>();
            let mut queued = 0_usize;
            for job_id in job_ids {
                let job = scheduler
                    .job(&job_id)
                    .cloned()
                    .ok_or_else(|| format!("Scheduler JobSpec 不存在: {job_id}"))?;
                let run = scheduler
                    .start_run_at(&job_id, trading_day, manifest.digest(), now)
                    .map_err(|error| format!("创建 JobRun 失败: {error:?}"))?;
                if run.status == JobStatus::Running {
                    queue
                        .enqueue(job, run, now)
                        .map_err(|error| format!("写入 JobQueue 失败: {error:?}"))?;
                    queued += 1;
                }
            }
            Ok(queued)
        })
        .map_err(|error| format!("Scheduler 状态事务失败: {error:?}"))?;
    result
}
