//! 调度器时间与任务清单辅助：UTC 刻度、civil date 换算和 jobs 清单加载。

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
fn civil_from_days(days: i64) -> (i32, u8, u8) {
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
        code_commit: "workspace".into(),
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

fn scheduler_jobs_path(runtime_config_path: &Path, configured: &str) -> PathBuf {
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
        store
            .save_scheduler_at(&state_reference, &scheduler)
            .map_err(|error| format!("初始化 Scheduler 状态失败: {error:?}"))?;
        scheduler
    };
    Ok((store, scheduler, state_reference))
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
            let completed = scheduler.completed_jobs();
            let job_ids = scheduler
                .due_jobs(tick, &completed)
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
