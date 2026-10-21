//! Reusable pure helpers for the scheduler runtime boundary.

use crate::{claimable_by, JobSpec, JobWindow, ScheduleTick, Scheduler, Trigger, JOB_OWNER_ANY};
use qx_core::RunManifest;
use std::collections::BTreeMap;
use std::path::Path;

/// Per-closed-bar inputs for the live strategy queue record.
pub struct LiveStrategyJobInput<'a> {
    pub job_version: &'a str,
    pub worker_id: &'a str,
    pub instrument: Option<&'a str>,
    pub data_fingerprint: u64,
    pub trading_day: String,
    /// Lease-clock seconds, not wall-clock milliseconds.
    pub lease_now: u64,
    pub environment: &'a str,
}

pub const LIVE_STRATEGY_TIMEOUT_SECONDS: u64 = 60;

/// Convert a Unix timestamp in milliseconds into its UTC trading-day label and cron tick.
pub fn utc_schedule_tick(timestamp_ms: u64) -> (String, ScheduleTick) {
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

/// Convert days since the Unix epoch to a Gregorian civil date.
///
/// This is Howard Hinnant's civil-from-days algorithm and is independent of the host timezone.
pub fn civil_from_days(days: i64) -> (i32, u8, u8) {
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

/// Return a diagnostic for a job shape that the current runtime cannot dispatch.
pub fn unsupported_dispatch_shape(job: &JobSpec) -> Option<String> {
    let cron = matches!(&job.trigger, Trigger::Cron(_));
    if !cron || job.window != JobWindow::Any {
        return Some(format!(
            "作业 {} 声明了运行时不会派发的形状：trigger={:?} window={:?}；scheduler-worker 的 tick 只跑 Cron 触发且 window=Any 的作业（交易日历/事件/手工触发没有生产派发者，窗口判定没有交易日历数据源）",
            job.job_id, job.trigger, job.window
        ));
    }
    if job.retry_policy.max_attempts > 1 {
        return Some(format!(
            "作业 {} 声明了运行时不会派发的形状：retry_policy.max_attempts={}；重试没有派发者（作业队列按 run_id 落盘、信封不携带 attempt，重投会撞已结算信封），请改成 1",
            job.job_id, job.retry_policy.max_attempts
        ));
    }
    None
}

/// Reject enabled jobs that no enabled strategy worker can claim.
///
/// The caller supplies worker IDs from its runtime topology; matching and diagnostics stay with
/// the scheduler contract so assembly and claim routing use the same predicate.
pub fn validate_job_owners(scheduler: &Scheduler, claimants: &[String]) -> Result<(), String> {
    let claimants_note = if claimants.is_empty() {
        "该拓扑没有启用的 Strategy worker".to_string()
    } else {
        format!("启用的 Strategy worker: {}", claimants.join(", "))
    };
    for job in scheduler.jobs().filter(|job| job.enabled) {
        let routable = claimants
            .iter()
            .any(|claimant| claimable_by(&job.owner, claimant));
        if !routable {
            return Err(format!(
                "Scheduler 作业 {} 的 owner {:?} 无人领取（{claimants_note}）；\
                 请把 owner 改成启用的 Strategy worker id，或使用 {:?} 交给任意 worker",
                job.job_id, job.owner, JOB_OWNER_ANY
            ));
        }
    }
    Ok(())
}

/// Parse a declared scheduler job set and reject enabled shapes without a dispatcher.
pub fn declared_jobs(payload: &str) -> Result<Vec<JobSpec>, String> {
    let jobs: Vec<JobSpec> = serde_json::from_str(payload)
        .map_err(|error| format!("Scheduler JobSpec JSON 无效: {error}"))?;
    let refused = jobs
        .iter()
        .filter(|job| job.enabled)
        .filter_map(unsupported_dispatch_shape)
        .collect::<Vec<_>>();
    if !refused.is_empty() {
        return Err(refused.join("；"));
    }
    Ok(jobs)
}

/// Reject additions, edits, removals, and duplicate IDs between the declared and persisted sets.
pub fn reject_job_set_drift(
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

/// Build the deterministic manifest identity for one scheduled worker tick.
pub fn scheduler_manifest(
    worker_id: &str,
    trading_day: &str,
    now: u64,
    code_commit: &str,
    runtime_version: &str,
) -> RunManifest {
    RunManifest {
        run_id: format!("{worker_id}-{trading_day}-{now}"),
        code_commit: code_commit.into(),
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
        runtime_version: runtime_version.into(),
    }
}

/// Build the deterministic job and lease record for one already-closed strategy Bar.
pub fn live_strategy_job(input: LiveStrategyJobInput<'_>) -> (JobSpec, crate::JobRun) {
    let LiveStrategyJobInput {
        job_version,
        worker_id,
        instrument,
        data_fingerprint,
        trading_day,
        lease_now,
        environment,
    } = input;
    let job = JobSpec {
        job_id: format!("live-strategy:{worker_id}"),
        job_version: job_version.into(),
        owner: worker_id.into(),
        enabled: true,
        trigger: Trigger::Manual,
        window: JobWindow::Any,
        depends_on: Vec::new(),
        input_refs: vec![format!("barframe:{data_fingerprint:016x}")],
        output_refs: vec!["strategy-submit-order".into()],
        timeout_seconds: LIVE_STRATEGY_TIMEOUT_SECONDS,
        retry_policy: crate::RetryPolicy::default(),
        concurrency_key: format!("live-strategy:{}", instrument.unwrap_or("")),
        idempotency_key: format!("barframe:{data_fingerprint:016x}"),
        permission_scope: "strategy".into(),
        audit_reason: "live-closed-bar".into(),
        dry_run: environment.eq_ignore_ascii_case("paper"),
    };
    let run_id = job.stable_key(&trading_day);
    let run = crate::JobRun {
        run_id,
        job_id: job.job_id.clone(),
        trading_day,
        attempt: 1,
        status: crate::JobStatus::Running,
        manifest_digest: Some(data_fingerprint),
        error_code: None,
        next_retry_ts: None,
        started_ts: lease_now,
        deadline_ts: lease_now.saturating_add(job.timeout_seconds),
    };
    (job, run)
}
