//! 任务调度契约。
//!
//! Scheduler 只决定“何时、以什么幂等键触发哪个 Job”，不持有交易所客户端，
//! 也不能绕过 Risk/OMS/Ledger。真实执行器可以在控制面或外部 Worker 中实现。

use qx_core::{Fnv1a, RunManifest};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Trigger {
    Manual,
    Cron(String),
    TradingCalendar { session: String },
    Event(String),
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ScheduleTick {
    pub minute: u8,
    pub hour: u8,
    pub day: u8,
    pub month: u8,
    pub weekday: u8,
}

impl ScheduleTick {
    pub fn validate(&self) -> Result<(), SchedulerError> {
        if self.minute >= 60
            || self.hour >= 24
            || self.day == 0
            || self.day > 31
            || self.month == 0
            || self.month > 12
            || self.weekday > 6
        {
            return Err(SchedulerError::Invalid("Cron tick 超出日历范围".into()));
        }
        Ok(())
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CronSpec {
    minutes: BTreeSet<u8>,
    hours: BTreeSet<u8>,
    days: BTreeSet<u8>,
    months: BTreeSet<u8>,
    weekdays: BTreeSet<u8>,
}

impl CronSpec {
    pub fn parse(expression: &str) -> Result<Self, SchedulerError> {
        let fields = expression.split_whitespace().collect::<Vec<_>>();
        if fields.len() != 5 {
            return Err(SchedulerError::Invalid(
                "Cron 必须是五字段 minute hour day month weekday".into(),
            ));
        }
        Ok(Self {
            minutes: parse_cron_field(fields[0], 0, 59)?,
            hours: parse_cron_field(fields[1], 0, 23)?,
            days: parse_cron_field(fields[2], 1, 31)?,
            months: parse_cron_field(fields[3], 1, 12)?,
            weekdays: parse_cron_field(fields[4], 0, 6)?,
        })
    }

    pub fn matches(&self, tick: &ScheduleTick) -> bool {
        self.minutes.contains(&tick.minute)
            && self.hours.contains(&tick.hour)
            && self.days.contains(&tick.day)
            && self.months.contains(&tick.month)
            && self.weekdays.contains(&tick.weekday)
    }
}

fn parse_cron_field(field: &str, min: u8, max: u8) -> Result<BTreeSet<u8>, SchedulerError> {
    let mut values = BTreeSet::new();
    for part in field.split(',') {
        let (base, step) = part.split_once('/').map_or((part, 1_u8), |(base, step)| {
            (base, step.parse::<u8>().unwrap_or(0))
        });
        if step == 0 {
            return Err(SchedulerError::Invalid(format!("Cron step 非法: {part}")));
        }
        let (left, right) = if base == "*" {
            (min.to_string(), max.to_string())
        } else {
            base.split_once('-')
                .map_or((base.to_string(), base.to_string()), |(a, b)| {
                    (a.to_string(), b.to_string())
                })
        };
        let start = left
            .parse::<u8>()
            .map_err(|_| SchedulerError::Invalid(format!("Cron 数字非法: {part}")))?;
        let end = right
            .parse::<u8>()
            .map_err(|_| SchedulerError::Invalid(format!("Cron 数字非法: {part}")))?;
        if start < min || end > max || start > end {
            return Err(SchedulerError::Invalid(format!("Cron 范围非法: {part}")));
        }
        values.extend((start..=end).step_by(step as usize));
    }
    if values.is_empty() {
        return Err(SchedulerError::Invalid("Cron 字段不能为空".into()));
    }
    Ok(values)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum JobWindow {
    Any,
    PreOpen,
    Session,
    PostClose,
}

impl JobWindow {
    fn allows(self, calendar: &TradingCalendar, trading_day: &str, ts: u64) -> bool {
        match self {
            Self::Any => true,
            Self::PreOpen => calendar
                .session(trading_day)
                .is_some_and(|session| ts < session.open),
            Self::Session => calendar.is_open(trading_day, ts),
            Self::PostClose => calendar
                .session(trading_day)
                .is_some_and(|session| ts >= session.close),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Any => "any",
            Self::PreOpen => "pre_open",
            Self::Session => "session",
            Self::PostClose => "post_close",
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub backoff_seconds: u64,
    pub retryable_codes: BTreeSet<String>,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 1,
            backoff_seconds: 0,
            retryable_codes: BTreeSet::new(),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct JobSpec {
    pub job_id: String,
    pub job_version: String,
    pub owner: String,
    pub enabled: bool,
    pub trigger: Trigger,
    pub window: JobWindow,
    pub depends_on: Vec<String>,
    pub input_refs: Vec<String>,
    pub output_refs: Vec<String>,
    pub timeout_seconds: u64,
    pub retry_policy: RetryPolicy,
    pub concurrency_key: String,
    pub idempotency_key: String,
    pub permission_scope: String,
    #[serde(default = "default_audit_reason")]
    pub audit_reason: String,
    pub dry_run: bool,
}

fn default_audit_reason() -> String {
    "legacy-job".into()
}

impl JobSpec {
    pub fn validate(&self) -> Result<(), SchedulerError> {
        if self.job_id.trim().is_empty()
            || self.job_version.trim().is_empty()
            || self.owner.trim().is_empty()
            || self.timeout_seconds == 0
            || self.concurrency_key.trim().is_empty()
            || self.idempotency_key.trim().is_empty()
            || self.audit_reason.trim().is_empty()
        {
            return Err(SchedulerError::Invalid("JobSpec 必填字段非法".into()));
        }
        if self.retry_policy.max_attempts == 0 {
            return Err(SchedulerError::Invalid("max_attempts 不能为 0".into()));
        }
        if let Trigger::Cron(expression) = &self.trigger {
            CronSpec::parse(expression)?;
        }
        if self
            .depends_on
            .iter()
            .any(|dependency| dependency == &self.job_id)
        {
            return Err(SchedulerError::Cycle(self.job_id.clone()));
        }
        Ok(())
    }

    pub fn stable_key(&self, trading_day: &str) -> u64 {
        let mut h = Fnv1a::new();
        h.write_text(&self.job_id);
        h.write_text(&self.job_version);
        h.write_text(&self.idempotency_key);
        h.write_text(trading_day);
        h.finish()
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum JobStatus {
    Pending,
    Running,
    Succeeded,
    Failed,
    Paused,
    NeedsIntervention,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct JobRun {
    pub run_id: u64,
    pub job_id: String,
    pub trading_day: String,
    pub attempt: u32,
    pub status: JobStatus,
    pub manifest_digest: Option<u64>,
    #[serde(default)]
    pub error_code: Option<String>,
    #[serde(default)]
    pub next_retry_ts: Option<u64>,
    #[serde(default)]
    pub started_ts: u64,
    #[serde(default)]
    pub deadline_ts: u64,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Session {
    pub trading_day: String,
    pub open: u64,
    pub close: u64,
}

#[derive(Default, Serialize, Deserialize)]
pub struct TradingCalendar {
    sessions: BTreeMap<String, Session>,
}

impl TradingCalendar {
    pub fn insert(&mut self, session: Session) -> Result<(), SchedulerError> {
        if session.trading_day.trim().is_empty() || session.open >= session.close {
            return Err(SchedulerError::Invalid("交易时段非法".into()));
        }
        self.sessions.insert(session.trading_day.clone(), session);
        Ok(())
    }

    pub fn session(&self, day: &str) -> Option<&Session> {
        self.sessions.get(day)
    }

    pub fn to_json(&self) -> Result<String, SchedulerError> {
        serde_json::to_string(self).map_err(|error| SchedulerError::Invalid(error.to_string()))
    }

    pub fn from_json(input: &str) -> Result<Self, SchedulerError> {
        let calendar: Self = serde_json::from_str(input)
            .map_err(|error| SchedulerError::Invalid(error.to_string()))?;
        for session in calendar.sessions.values() {
            if session.trading_day.trim().is_empty() || session.open >= session.close {
                return Err(SchedulerError::Invalid("交易日历包含非法时段".into()));
            }
        }
        Ok(calendar)
    }

    pub fn is_open(&self, day: &str, ts: u64) -> bool {
        self.session(day)
            .map(|session| ts >= session.open && ts < session.close)
            .unwrap_or(false)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum SchedulerError {
    Duplicate(String),
    MissingDependency(String),
    Cycle(String),
    Invalid(String),
    NotReady(String),
    UnknownRun(u64),
}

#[derive(Default, Serialize, Deserialize)]
pub struct Scheduler {
    jobs: BTreeMap<String, JobSpec>,
    runs: BTreeMap<u64, JobRun>,
    active_keys: BTreeSet<String>,
    completed: BTreeSet<String>,
}

/// Scheduler 不执行交易逻辑；执行器只能通过这个 trait 接收经过依赖、幂等和
/// 并发校验的 JobRun。执行器应自行使用 `timeout_seconds` 实现超时隔离。
pub trait JobExecutor {
    fn execute(&mut self, job: &JobSpec, run: &JobRun) -> Result<String, String>;
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct WorkerOutcome {
    pub run: JobRun,
    pub result_code: Option<String>,
    pub error: Option<String>,
}

pub struct SchedulerWorker;

impl SchedulerWorker {
    pub fn run_cron_tick_with_manifest<E: JobExecutor>(
        scheduler: &mut Scheduler,
        tick: &ScheduleTick,
        trading_day: &str,
        manifest: &RunManifest,
        executor: &mut E,
    ) -> Result<Vec<WorkerOutcome>, SchedulerError> {
        manifest.validate().map_err(SchedulerError::Invalid)?;
        Self::run_cron_tick(scheduler, tick, trading_day, manifest.digest(), executor)
    }

    /// 生产入口：同时绑定 RunManifest、交易日历和 JobWindow。
    /// 旧的 `run_cron_tick_with_manifest` 保留给无交易日历的研究任务；交易任务必须
    /// 使用本入口，避免 manifest 已绑定但窗口约束被绕过。
    pub fn run_cron_tick_with_calendar_and_manifest<E: JobExecutor>(
        scheduler: &mut Scheduler,
        tick: &ScheduleTick,
        trading_day: &str,
        now: u64,
        calendar: &TradingCalendar,
        manifest: &RunManifest,
        executor: &mut E,
    ) -> Result<Vec<WorkerOutcome>, SchedulerError> {
        manifest.validate().map_err(SchedulerError::Invalid)?;
        Self::run_cron_tick_with_calendar(
            scheduler,
            tick,
            trading_day,
            now,
            calendar,
            manifest.digest(),
            executor,
        )
    }

    pub fn run_cron_tick<E: JobExecutor>(
        scheduler: &mut Scheduler,
        tick: &ScheduleTick,
        trading_day: &str,
        manifest_digest: u64,
        executor: &mut E,
    ) -> Result<Vec<WorkerOutcome>, SchedulerError> {
        let completed = scheduler.completed.clone();
        let job_ids = scheduler
            .due_jobs(tick, &completed)?
            .into_iter()
            .map(|job| job.job_id.clone())
            .collect::<Vec<_>>();
        let mut outcomes = Vec::with_capacity(job_ids.len());
        for job_id in job_ids {
            let run = scheduler.start_run(&job_id, trading_day, manifest_digest)?;
            let job = scheduler
                .job(&job_id)
                .cloned()
                .ok_or_else(|| SchedulerError::MissingDependency(job_id.clone()))?;
            let execution = executor.execute(&job, &run);
            let (success, error_code) = match &execution {
                Ok(_) => (true, None),
                Err(error) => (false, Some(error.as_str())),
            };
            let final_run = scheduler.finish_run_with_code(run.run_id, success, error_code, 0)?;
            outcomes.push(match execution {
                Ok(result_code) => WorkerOutcome {
                    run: final_run,
                    result_code: Some(result_code),
                    error: None,
                },
                Err(error) => WorkerOutcome {
                    run: final_run,
                    result_code: None,
                    error: Some(error),
                },
            });
        }
        Ok(outcomes)
    }

    /// 生产调度入口：同时执行 cron、交易日历和 JobWindow 约束。
    pub fn run_cron_tick_with_calendar<E: JobExecutor>(
        scheduler: &mut Scheduler,
        tick: &ScheduleTick,
        trading_day: &str,
        now: u64,
        calendar: &TradingCalendar,
        manifest_digest: u64,
        executor: &mut E,
    ) -> Result<Vec<WorkerOutcome>, SchedulerError> {
        let completed = scheduler.completed.clone();
        let job_ids = scheduler
            .due_jobs_with_calendar(tick, trading_day, now, calendar, &completed)?
            .into_iter()
            .map(|job| job.job_id.clone())
            .collect::<Vec<_>>();
        let mut outcomes = Vec::with_capacity(job_ids.len());
        for job_id in job_ids {
            let run = scheduler.start_run_at(&job_id, trading_day, manifest_digest, now)?;
            let job = scheduler
                .job(&job_id)
                .cloned()
                .ok_or_else(|| SchedulerError::MissingDependency(job_id.clone()))?;
            let execution = executor.execute(&job, &run);
            let (success, error_code) = match &execution {
                Ok(_) => (true, None),
                Err(error) => (false, Some(error.as_str())),
            };
            let final_run = scheduler.finish_run_with_code(run.run_id, success, error_code, now)?;
            outcomes.push(match execution {
                Ok(result_code) => WorkerOutcome {
                    run: final_run,
                    result_code: Some(result_code),
                    error: None,
                },
                Err(error) => WorkerOutcome {
                    run: final_run,
                    result_code: None,
                    error: Some(error),
                },
            });
        }
        Ok(outcomes)
    }

    pub fn run_event_with_manifest<E: JobExecutor>(
        scheduler: &mut Scheduler,
        event: &str,
        trading_day: &str,
        now: u64,
        manifest: &RunManifest,
        executor: &mut E,
    ) -> Result<Vec<WorkerOutcome>, SchedulerError> {
        if event.trim().is_empty() {
            return Err(SchedulerError::Invalid("事件名称不能为空".into()));
        }
        manifest.validate().map_err(SchedulerError::Invalid)?;
        let completed = scheduler.completed.clone();
        let job_ids = scheduler
            .due_event_jobs(event, &completed)?
            .into_iter()
            .map(|job| job.job_id.clone())
            .collect::<Vec<_>>();
        Self::execute_job_ids(
            scheduler,
            job_ids,
            trading_day,
            now,
            manifest.digest(),
            executor,
        )
    }

    pub fn run_manual_with_manifest<E: JobExecutor>(
        scheduler: &mut Scheduler,
        job_id: &str,
        trading_day: &str,
        now: u64,
        manifest: &RunManifest,
        executor: &mut E,
    ) -> Result<Vec<WorkerOutcome>, SchedulerError> {
        manifest.validate().map_err(SchedulerError::Invalid)?;
        let completed = scheduler.completed.clone();
        let due = scheduler.due_manual_jobs(&completed)?;
        if !due.iter().any(|job| job.job_id == job_id) {
            return Err(SchedulerError::NotReady(job_id.into()));
        }
        Self::execute_job_ids(
            scheduler,
            vec![job_id.into()],
            trading_day,
            now,
            manifest.digest(),
            executor,
        )
    }

    fn execute_job_ids<E: JobExecutor>(
        scheduler: &mut Scheduler,
        job_ids: Vec<String>,
        trading_day: &str,
        now: u64,
        manifest_digest: u64,
        executor: &mut E,
    ) -> Result<Vec<WorkerOutcome>, SchedulerError> {
        let mut outcomes = Vec::with_capacity(job_ids.len());
        for job_id in job_ids {
            let run = scheduler.start_run_at(&job_id, trading_day, manifest_digest, now)?;
            let job = scheduler
                .job(&job_id)
                .cloned()
                .ok_or_else(|| SchedulerError::MissingDependency(job_id.clone()))?;
            let execution = executor.execute(&job, &run);
            let (success, error_code) = match &execution {
                Ok(_) => (true, None),
                Err(error) => (false, Some(error.as_str())),
            };
            let final_run = scheduler.finish_run_with_code(run.run_id, success, error_code, now)?;
            outcomes.push(match execution {
                Ok(result_code) => WorkerOutcome {
                    run: final_run,
                    result_code: Some(result_code),
                    error: None,
                },
                Err(error) => WorkerOutcome {
                    run: final_run,
                    result_code: None,
                    error: Some(error),
                },
            });
        }
        Ok(outcomes)
    }
}

impl Scheduler {
    pub fn register(&mut self, job: JobSpec) -> Result<(), SchedulerError> {
        job.validate()?;
        if self.jobs.contains_key(&job.job_id) {
            return Err(SchedulerError::Duplicate(job.job_id));
        }
        for dependency in &job.depends_on {
            if dependency == &job.job_id {
                return Err(SchedulerError::Cycle(job.job_id));
            }
        }
        let job_id = job.job_id.clone();
        self.jobs.insert(job_id.clone(), job);
        if let Err(error) = self.validate_dependencies() {
            self.jobs.remove(&job_id);
            return Err(error);
        }
        Ok(())
    }

    pub fn ready_jobs(&self, completed: &BTreeSet<String>) -> Vec<&JobSpec> {
        self.jobs
            .values()
            .filter(|job| {
                job.enabled
                    && job
                        .depends_on
                        .iter()
                        .all(|dependency| completed.contains(dependency))
            })
            .collect()
    }

    pub fn due_jobs(
        &self,
        tick: &ScheduleTick,
        completed: &BTreeSet<String>,
    ) -> Result<Vec<&JobSpec>, SchedulerError> {
        tick.validate()?;
        let mut jobs = Vec::new();
        for job in self.jobs.values() {
            if !job.enabled
                || !job
                    .depends_on
                    .iter()
                    .all(|dependency| completed.contains(dependency))
            {
                continue;
            }
            if let Trigger::Cron(expression) = &job.trigger {
                if CronSpec::parse(expression)?.matches(tick) {
                    jobs.push(job);
                }
            }
        }
        Ok(jobs)
    }

    /// 带交易日历和业务时间的触发入口。`JobWindow` 与
    /// `Trigger::TradingCalendar` 只能通过此入口判定，避免定义了窗口却没有实际约束。
    pub fn due_jobs_with_calendar(
        &self,
        tick: &ScheduleTick,
        trading_day: &str,
        now: u64,
        calendar: &TradingCalendar,
        completed: &BTreeSet<String>,
    ) -> Result<Vec<&JobSpec>, SchedulerError> {
        tick.validate()?;
        let mut jobs = Vec::new();
        for job in self.jobs.values() {
            if !job.enabled
                || !job
                    .depends_on
                    .iter()
                    .all(|dependency| completed.contains(dependency))
                || !job.window.allows(calendar, trading_day, now)
            {
                continue;
            }
            let triggered = match &job.trigger {
                Trigger::Cron(expression) => CronSpec::parse(expression)?.matches(tick),
                Trigger::TradingCalendar { session } => {
                    session == job.window.name()
                        || (session == "any" && matches!(job.window, JobWindow::Any))
                }
                Trigger::Manual | Trigger::Event(_) => false,
            };
            if triggered {
                jobs.push(job);
            }
        }
        Ok(jobs)
    }

    pub fn due_event_jobs(
        &self,
        event: &str,
        completed: &BTreeSet<String>,
    ) -> Result<Vec<&JobSpec>, SchedulerError> {
        if event.trim().is_empty() {
            return Err(SchedulerError::Invalid("事件名称不能为空".into()));
        }
        Ok(self
            .jobs
            .values()
            .filter(|job| {
                job.enabled
                    && job
                        .depends_on
                        .iter()
                        .all(|dependency| completed.contains(dependency))
                    && matches!(&job.trigger, Trigger::Event(name) if name == event)
            })
            .collect())
    }

    pub fn due_manual_jobs(
        &self,
        completed: &BTreeSet<String>,
    ) -> Result<Vec<&JobSpec>, SchedulerError> {
        Ok(self
            .jobs
            .values()
            .filter(|job| {
                job.enabled
                    && job
                        .depends_on
                        .iter()
                        .all(|dependency| completed.contains(dependency))
                    && matches!(job.trigger, Trigger::Manual)
            })
            .collect())
    }

    pub fn job(&self, id: &str) -> Option<&JobSpec> {
        self.jobs.get(id)
    }

    pub fn len(&self) -> usize {
        self.jobs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.jobs.is_empty()
    }

    /// 返回已经成功完成的 Job id；运行时调度器用它计算依赖，而不暴露内部状态表。
    pub fn completed_jobs(&self) -> BTreeSet<String> {
        self.completed.clone()
    }

    /// 创建一次幂等运行。真正的 worker 只接收这里返回的 JobRun，不能自行绕过依赖。
    pub fn start_run(
        &mut self,
        job_id: &str,
        trading_day: &str,
        manifest_digest: u64,
    ) -> Result<JobRun, SchedulerError> {
        self.start_run_at(job_id, trading_day, manifest_digest, 0)
    }

    pub fn start_run_at(
        &mut self,
        job_id: &str,
        trading_day: &str,
        manifest_digest: u64,
        started_ts: u64,
    ) -> Result<JobRun, SchedulerError> {
        let job = self
            .jobs
            .get(job_id)
            .ok_or_else(|| SchedulerError::MissingDependency(job_id.into()))?;
        if !job.enabled || !job.depends_on.iter().all(|id| self.completed.contains(id)) {
            return Err(SchedulerError::NotReady(job_id.into()));
        }
        let run_id = job.stable_key(trading_day);
        if let Some(existing) = self.runs.get(&run_id) {
            return Ok(existing.clone());
        }
        if !self.active_keys.insert(job.concurrency_key.clone()) {
            return Err(SchedulerError::NotReady(format!(
                "并发键正在运行: {}",
                job.concurrency_key
            )));
        }
        let run = JobRun {
            run_id,
            job_id: job_id.into(),
            trading_day: trading_day.into(),
            attempt: 1,
            status: JobStatus::Running,
            manifest_digest: Some(manifest_digest),
            error_code: None,
            next_retry_ts: None,
            started_ts,
            deadline_ts: started_ts.saturating_add(job.timeout_seconds),
        };
        self.runs.insert(run_id, run.clone());
        Ok(run)
    }

    pub fn is_timed_out(&self, run_id: u64, now: u64) -> Result<bool, SchedulerError> {
        let run = self
            .runs
            .get(&run_id)
            .ok_or(SchedulerError::UnknownRun(run_id))?;
        Ok(matches!(run.status, JobStatus::Running) && now >= run.deadline_ts)
    }

    /// 将已经超过截止时间的运行置为人工接管，释放并发键，禁止隐式重试/重复执行。
    pub fn mark_timed_out(&mut self, run_id: u64, now: u64) -> Result<JobRun, SchedulerError> {
        let mut run = self
            .runs
            .get(&run_id)
            .cloned()
            .ok_or(SchedulerError::UnknownRun(run_id))?;
        if !matches!(run.status, JobStatus::Running) || now < run.deadline_ts {
            return Ok(run);
        }
        let job = self
            .jobs
            .get(&run.job_id)
            .ok_or_else(|| SchedulerError::MissingDependency(run.job_id.clone()))?;
        run.status = JobStatus::NeedsIntervention;
        run.error_code = Some("TIMEOUT".into());
        run.next_retry_ts = None;
        self.active_keys.remove(&job.concurrency_key);
        self.runs.insert(run_id, run.clone());
        Ok(run)
    }

    pub fn start_run_with_manifest(
        &mut self,
        job_id: &str,
        trading_day: &str,
        manifest: &RunManifest,
    ) -> Result<JobRun, SchedulerError> {
        manifest.validate().map_err(SchedulerError::Invalid)?;
        self.start_run(job_id, trading_day, manifest.digest())
    }

    pub fn finish_run(&mut self, run_id: u64, success: bool) -> Result<JobRun, SchedulerError> {
        self.finish_run_with_code(run_id, success, None, 0)
    }

    pub fn finish_run_with_code(
        &mut self,
        run_id: u64,
        success: bool,
        error_code: Option<&str>,
        finished_ts: u64,
    ) -> Result<JobRun, SchedulerError> {
        let mut run = self
            .runs
            .get(&run_id)
            .cloned()
            .ok_or(SchedulerError::UnknownRun(run_id))?;
        if !matches!(run.status, JobStatus::Running) {
            return Ok(run);
        }
        let job = self
            .jobs
            .get(&run.job_id)
            .ok_or_else(|| SchedulerError::MissingDependency(run.job_id.clone()))?;
        run.status = if success {
            JobStatus::Succeeded
        } else if matches!(
            error_code,
            Some("MANUAL_INTERVENTION" | "NEEDS_INTERVENTION")
        ) {
            JobStatus::NeedsIntervention
        } else {
            JobStatus::Failed
        };
        run.error_code = error_code.map(str::to_string);
        run.next_retry_ts = (!success
            && !matches!(run.status, JobStatus::NeedsIntervention)
            && run.attempt < job.retry_policy.max_attempts
            && (job.retry_policy.retryable_codes.is_empty()
                || run
                    .error_code
                    .as_ref()
                    .is_some_and(|code| job.retry_policy.retryable_codes.contains(code))))
        .then(|| finished_ts.saturating_add(job.retry_policy.backoff_seconds));
        self.active_keys.remove(&job.concurrency_key);
        if success {
            self.completed.insert(run.job_id.clone());
        }
        self.runs.insert(run_id, run.clone());
        Ok(run)
    }

    pub fn retry_run(&mut self, run_id: u64) -> Result<JobRun, SchedulerError> {
        self.retry_run_at(run_id, u64::MAX)
    }

    pub fn retry_run_at(&mut self, run_id: u64, now: u64) -> Result<JobRun, SchedulerError> {
        let mut run = self
            .runs
            .get(&run_id)
            .cloned()
            .ok_or(SchedulerError::UnknownRun(run_id))?;
        let job = self
            .jobs
            .get(&run.job_id)
            .ok_or_else(|| SchedulerError::MissingDependency(run.job_id.clone()))?;
        if !matches!(run.status, JobStatus::Failed) || run.attempt >= job.retry_policy.max_attempts
        {
            return Err(SchedulerError::NotReady(format!("run {} 不可重试", run_id)));
        }
        if run
            .next_retry_ts
            .is_none_or(|next_retry_ts| now < next_retry_ts)
        {
            return Err(SchedulerError::NotReady(format!(
                "run {} 尚未到达重试时间",
                run_id
            )));
        }
        if !self.active_keys.insert(job.concurrency_key.clone()) {
            return Err(SchedulerError::NotReady("并发键正在运行".into()));
        }
        run.attempt += 1;
        run.status = JobStatus::Running;
        run.error_code = None;
        run.next_retry_ts = None;
        self.runs.insert(run_id, run.clone());
        Ok(run)
    }

    pub fn run(&self, run_id: u64) -> Option<&JobRun> {
        self.runs.get(&run_id)
    }

    /// 返回可供 QueryPort/运维读模型使用的确定性 JobRun 列表。
    ///
    /// Scheduler 仍然是唯一的运行状态拥有者；调用方拿到的是克隆快照，
    /// 不能通过查询接口修改调度状态。
    pub fn runs(&self) -> Vec<JobRun> {
        self.runs.values().cloned().collect()
    }

    pub fn to_json(&self) -> Result<String, String> {
        serde_json::to_string(self).map_err(|error| error.to_string())
    }

    pub fn from_json(input: &str) -> Result<Self, String> {
        let scheduler: Self = serde_json::from_str(input).map_err(|error| error.to_string())?;
        for job in scheduler.jobs.values() {
            job.validate()
                .map_err(|error| format!("非法 JobSpec: {error:?}"))?;
        }
        scheduler
            .validate_dependencies()
            .map_err(|error| format!("非法调度状态: {error:?}"))?;
        scheduler
            .validate_state()
            .map_err(|error| format!("非法调度运行状态: {error:?}"))?;
        Ok(scheduler)
    }

    fn validate_state(&self) -> Result<(), SchedulerError> {
        let mut expected_active = BTreeSet::new();
        let mut expected_completed = BTreeSet::new();
        for (run_id, run) in &self.runs {
            let job = self
                .jobs
                .get(&run.job_id)
                .ok_or_else(|| SchedulerError::MissingDependency(run.job_id.clone()))?;
            if *run_id != run.run_id
                || run.run_id != job.stable_key(&run.trading_day)
                || run.attempt == 0
                || run.started_ts > run.deadline_ts
            {
                return Err(SchedulerError::Invalid(format!(
                    "run {} 身份、尝试次数或截止时间非法",
                    run.run_id
                )));
            }
            match run.status {
                JobStatus::Running => {
                    if !expected_active.insert(job.concurrency_key.clone()) {
                        return Err(SchedulerError::Invalid("运行中的并发键重复".into()));
                    }
                }
                JobStatus::Succeeded => {
                    expected_completed.insert(run.job_id.clone());
                }
                JobStatus::Failed
                | JobStatus::Pending
                | JobStatus::Paused
                | JobStatus::NeedsIntervention => {}
            }
            if run.status != JobStatus::Failed && run.next_retry_ts.is_some() {
                return Err(SchedulerError::Invalid(
                    "非 Failed 运行不能带重试时间".into(),
                ));
            }
        }
        if self.active_keys != expected_active || self.completed != expected_completed {
            return Err(SchedulerError::Invalid(
                "active_keys/completed 与 runs 不一致".into(),
            ));
        }
        Ok(())
    }

    fn validate_dependencies(&self) -> Result<(), SchedulerError> {
        for job in self.jobs.values() {
            for dependency in &job.depends_on {
                if !self.jobs.contains_key(dependency) {
                    return Err(SchedulerError::MissingDependency(dependency.clone()));
                }
            }
        }
        let mut visiting = BTreeSet::new();
        let mut visited = BTreeSet::new();
        for id in self.jobs.keys() {
            self.visit(id, &mut visiting, &mut visited)?;
        }
        Ok(())
    }

    fn visit(
        &self,
        id: &str,
        visiting: &mut BTreeSet<String>,
        visited: &mut BTreeSet<String>,
    ) -> Result<(), SchedulerError> {
        if visited.contains(id) {
            return Ok(());
        }
        if !visiting.insert(id.to_string()) {
            return Err(SchedulerError::Cycle(id.into()));
        }
        let job = self.jobs.get(id).expect("dependency validated");
        for dependency in &job.depends_on {
            self.visit(dependency, visiting, visited)?;
        }
        visiting.remove(id);
        visited.insert(id.into());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(id: &str, depends_on: Vec<&str>) -> JobSpec {
        JobSpec {
            job_id: id.into(),
            job_version: "v1".into(),
            owner: "research".into(),
            enabled: true,
            trigger: Trigger::Manual,
            window: JobWindow::Any,
            depends_on: depends_on.into_iter().map(str::to_string).collect(),
            input_refs: vec!["bars".into()],
            output_refs: vec![format!("out-{id}")],
            timeout_seconds: 60,
            retry_policy: RetryPolicy::default(),
            concurrency_key: id.into(),
            idempotency_key: format!("{id}-daily"),
            permission_scope: "research".into(),
            audit_reason: format!("test {id}"),
            dry_run: true,
        }
    }

    #[test]
    fn dependencies_are_deterministic() {
        let mut scheduler = Scheduler::default();
        scheduler.register(job("load", vec![])).unwrap();
        scheduler.register(job("factor", vec!["load"])).unwrap();
        assert_eq!(scheduler.ready_jobs(&BTreeSet::new())[0].job_id, "load");
        assert_eq!(
            scheduler.ready_jobs(&["load".into()].into_iter().collect())[0].job_id,
            "factor"
        );
    }

    #[test]
    fn missing_dependency_and_cycle_are_rejected() {
        let mut scheduler = Scheduler::default();
        assert_eq!(
            scheduler.register(job("factor", vec!["missing"])),
            Err(SchedulerError::MissingDependency("missing".into()))
        );
        scheduler.register(job("a", vec![])).unwrap();
        let b = job("b", vec!["a"]);
        scheduler.register(b.clone()).unwrap();
        assert_eq!(
            scheduler.register(b),
            Err(SchedulerError::Duplicate("b".into()))
        );
        assert_eq!(scheduler.len(), 2);
    }

    #[test]
    fn calendar_is_explicit() {
        let mut calendar = TradingCalendar::default();
        calendar
            .insert(Session {
                trading_day: "20260910".into(),
                open: 10,
                close: 20,
            })
            .unwrap();
        assert!(calendar.is_open("20260910", 10));
        assert!(!calendar.is_open("20260910", 20));
        let json = calendar.to_json().unwrap();
        assert!(TradingCalendar::from_json(&json)
            .unwrap()
            .is_open("20260910", 10));
    }

    #[test]
    fn runs_are_idempotent_and_respect_dependencies() {
        let mut scheduler = Scheduler::default();
        scheduler.register(job("load", vec![])).unwrap();
        scheduler.register(job("factor", vec!["load"])).unwrap();
        assert_eq!(
            scheduler.start_run("factor", "20260910", 1),
            Err(SchedulerError::NotReady("factor".into()))
        );
        let first = scheduler.start_run("load", "20260910", 1).unwrap();
        assert_eq!(scheduler.start_run("load", "20260910", 1).unwrap(), first);
        let finished = scheduler.finish_run(first.run_id, true).unwrap();
        assert_eq!(finished.status, JobStatus::Succeeded);
        let next = scheduler.start_run("factor", "20260910", 2).unwrap();
        assert_eq!(next.status, JobStatus::Running);
    }

    #[test]
    fn cron_trigger_and_retry_are_deterministic() {
        let cron = CronSpec::parse("0 9 * * 1-5").unwrap();
        assert!(cron.matches(&ScheduleTick {
            minute: 0,
            hour: 9,
            day: 10,
            month: 9,
            weekday: 4,
        }));
        assert!(!cron.matches(&ScheduleTick {
            minute: 1,
            hour: 9,
            day: 10,
            month: 9,
            weekday: 4,
        }));
        assert!(CronSpec::parse("*/5 * * * *")
            .unwrap()
            .matches(&ScheduleTick {
                minute: 10,
                hour: 1,
                day: 1,
                month: 1,
                weekday: 0,
            }));
        let mut scheduler = Scheduler::default();
        let mut cron_job = job("cron", vec![]);
        cron_job.trigger = Trigger::Cron("0 9 * * 1-5".into());
        cron_job.retry_policy.max_attempts = 2;
        scheduler.register(cron_job).unwrap();
        let due = scheduler
            .due_jobs(
                &ScheduleTick {
                    minute: 0,
                    hour: 9,
                    day: 10,
                    month: 9,
                    weekday: 4,
                },
                &BTreeSet::new(),
            )
            .unwrap();
        assert_eq!(due.len(), 1);
        let run = scheduler.start_run("cron", "20260910", 1).unwrap();
        scheduler.finish_run(run.run_id, false).unwrap();
        assert_eq!(scheduler.retry_run(run.run_id).unwrap().attempt, 2);
    }

    #[test]
    fn invalid_cron_is_rejected_at_registration_and_query() {
        let mut scheduler = Scheduler::default();
        let mut invalid = job("invalid-cron", vec![]);
        invalid.trigger = Trigger::Cron("0 25 * * *".into());
        assert!(matches!(
            scheduler.register(invalid),
            Err(SchedulerError::Invalid(_))
        ));

        let mut valid = job("valid-cron", vec![]);
        valid.trigger = Trigger::Cron("0 9 * * *".into());
        scheduler.register(valid).unwrap();
        let mut restored = scheduler.to_json().unwrap();
        restored = restored.replace("0 9 * * *", "0 25 * * *");
        assert!(Scheduler::from_json(&restored).is_err());
        assert!(scheduler
            .due_jobs(
                &ScheduleTick {
                    minute: 0,
                    hour: 9,
                    day: 10,
                    month: 9,
                    weekday: 4,
                },
                &BTreeSet::new(),
            )
            .is_ok());
    }

    struct RecordingExecutor {
        calls: Vec<String>,
        fail: bool,
    }

    impl JobExecutor for RecordingExecutor {
        fn execute(&mut self, job: &JobSpec, run: &JobRun) -> Result<String, String> {
            self.calls.push(format!("{}#{}", job.job_id, run.attempt));
            if self.fail {
                Err("WORKER_FAILED".into())
            } else {
                Ok("WORKER_OK".into())
            }
        }
    }

    #[test]
    fn scheduler_worker_executes_due_jobs_and_records_failure() {
        let mut scheduler = Scheduler::default();
        let mut cron_job = job("worker", vec![]);
        cron_job.trigger = Trigger::Cron("0 9 * * *".into());
        scheduler.register(cron_job).unwrap();
        let tick = ScheduleTick {
            minute: 0,
            hour: 9,
            day: 10,
            month: 9,
            weekday: 4,
        };
        let mut executor = RecordingExecutor {
            calls: Vec::new(),
            fail: false,
        };
        let outcomes =
            SchedulerWorker::run_cron_tick(&mut scheduler, &tick, "20260910", 7, &mut executor)
                .unwrap();
        assert_eq!(executor.calls, ["worker#1"]);
        assert_eq!(outcomes[0].run.status, JobStatus::Succeeded);
        assert_eq!(outcomes[0].result_code.as_deref(), Some("WORKER_OK"));

        let mut retry_scheduler = Scheduler::default();
        let mut retry_job = job("retry-worker", vec![]);
        retry_job.trigger = Trigger::Cron("0 9 * * *".into());
        retry_job.retry_policy.max_attempts = 2;
        retry_scheduler.register(retry_job).unwrap();
        let mut failing = RecordingExecutor {
            calls: Vec::new(),
            fail: true,
        };
        let failed = SchedulerWorker::run_cron_tick(
            &mut retry_scheduler,
            &tick,
            "20260910",
            7,
            &mut failing,
        )
        .unwrap();
        assert_eq!(failed[0].run.status, JobStatus::Failed);
        assert_eq!(failed[0].error.as_deref(), Some("WORKER_FAILED"));
        assert_eq!(
            retry_scheduler
                .retry_run(failed[0].run.run_id)
                .unwrap()
                .attempt,
            2
        );
    }

    #[test]
    fn retry_policy_enforces_error_code_and_backoff() {
        let mut scheduler = Scheduler::default();
        let mut retry_job = job("retry-policy", vec![]);
        retry_job.retry_policy.max_attempts = 3;
        retry_job.retry_policy.backoff_seconds = 10;
        retry_job
            .retry_policy
            .retryable_codes
            .insert("TRANSIENT".into());
        scheduler.register(retry_job).unwrap();
        let run = scheduler.start_run("retry-policy", "20260910", 1).unwrap();
        let failed = scheduler
            .finish_run_with_code(run.run_id, false, Some("PERMANENT"), 100)
            .unwrap();
        assert_eq!(failed.next_retry_ts, None);
        assert!(scheduler.retry_run_at(run.run_id, 110).is_err());

        let second = scheduler.start_run("retry-policy", "20260911", 2).unwrap();
        scheduler
            .finish_run_with_code(second.run_id, false, Some("TRANSIENT"), 100)
            .unwrap();
        assert!(scheduler.retry_run_at(second.run_id, 109).is_err());
        assert_eq!(
            scheduler.retry_run_at(second.run_id, 110).unwrap().attempt,
            2
        );
    }

    #[test]
    fn run_records_deadline_for_timeout_monitoring() {
        let mut scheduler = Scheduler::default();
        let mut timed = job("timed", vec![]);
        timed.timeout_seconds = 30;
        scheduler.register(timed).unwrap();
        let run = scheduler.start_run_at("timed", "20260910", 1, 100).unwrap();
        assert_eq!(run.started_ts, 100);
        assert_eq!(run.deadline_ts, 130);
        assert!(!scheduler.is_timed_out(run.run_id, 129).unwrap());
        assert!(scheduler.is_timed_out(run.run_id, 130).unwrap());
        let timeout = scheduler.mark_timed_out(run.run_id, 130).unwrap();
        assert_eq!(timeout.status, JobStatus::NeedsIntervention);
        assert_eq!(timeout.error_code.as_deref(), Some("TIMEOUT"));
        assert!(!scheduler.is_timed_out(run.run_id, 999).unwrap());
    }

    #[test]
    fn trading_calendar_and_job_window_are_enforced() {
        let mut calendar = TradingCalendar::default();
        calendar
            .insert(Session {
                trading_day: "20260910".into(),
                open: 100,
                close: 200,
            })
            .unwrap();
        let tick = ScheduleTick {
            minute: 0,
            hour: 9,
            day: 10,
            month: 9,
            weekday: 4,
        };
        let mut scheduler = Scheduler::default();
        let mut pre_open = job("pre-open", vec![]);
        pre_open.trigger = Trigger::Cron("0 9 * * *".into());
        pre_open.window = JobWindow::PreOpen;
        scheduler.register(pre_open).unwrap();
        let mut session = job("session", vec![]);
        session.trigger = Trigger::TradingCalendar {
            session: "session".into(),
        };
        session.window = JobWindow::Session;
        scheduler.register(session).unwrap();
        let completed = BTreeSet::new();
        assert_eq!(
            scheduler
                .due_jobs_with_calendar(&tick, "20260910", 50, &calendar, &completed)
                .unwrap()
                .iter()
                .map(|job| job.job_id.as_str())
                .collect::<Vec<_>>(),
            vec!["pre-open"]
        );
        assert_eq!(
            scheduler
                .due_jobs_with_calendar(&tick, "20260910", 150, &calendar, &completed)
                .unwrap()
                .iter()
                .map(|job| job.job_id.as_str())
                .collect::<Vec<_>>(),
            vec!["session"]
        );
        let mut executor = RecordingExecutor {
            calls: Vec::new(),
            fail: false,
        };
        let outcomes = SchedulerWorker::run_cron_tick_with_calendar(
            &mut scheduler,
            &tick,
            "20260910",
            50,
            &calendar,
            7,
            &mut executor,
        )
        .unwrap();
        assert_eq!(executor.calls, ["pre-open#1"]);
        assert_eq!(outcomes.len(), 1);
    }

    #[test]
    fn restored_scheduler_rejects_inconsistent_run_indexes() {
        let mut scheduler = Scheduler::default();
        scheduler.register(job("restore", vec![])).unwrap();
        let run = scheduler
            .start_run_at("restore", "20260910", 1, 10)
            .unwrap();
        let json = scheduler.to_json().unwrap();
        assert_eq!(
            Scheduler::from_json(&json).unwrap().run(run.run_id),
            Some(&run)
        );
        let forged = json.replace("\"active_keys\":[\"restore\"]", "\"active_keys\":[]");
        assert!(Scheduler::from_json(&forged).is_err());
    }

    #[test]
    fn worker_accepts_only_a_valid_run_manifest() {
        let mut scheduler = Scheduler::default();
        let mut cron_job = job("manifest-worker", vec![]);
        cron_job.trigger = Trigger::Cron("0 9 * * *".into());
        scheduler.register(cron_job).unwrap();
        let tick = ScheduleTick {
            minute: 0,
            hour: 9,
            day: 10,
            month: 9,
            weekday: 4,
        };
        let mut executor = RecordingExecutor {
            calls: Vec::new(),
            fail: false,
        };
        let manifest = RunManifest {
            run_id: "run-1".into(),
            code_commit: "commit".into(),
            config_hash: "config".into(),
            data_fingerprint: "data".into(),
            clock_start: 1,
            clock_end: 2,
            global_seed: 3,
            determinism_mode: true,
            result_hash: "result".into(),
            strategy_version: "strategy".into(),
            instrument_spec_version: "instrument".into(),
            model_fingerprint: "model".into(),
            input_event_hash: "input".into(),
            output_event_hash: "output".into(),
            runtime_version: "runtime".into(),
        };
        let outcomes = SchedulerWorker::run_cron_tick_with_manifest(
            &mut scheduler,
            &tick,
            "20260910",
            &manifest,
            &mut executor,
        )
        .unwrap();
        assert_eq!(outcomes[0].run.manifest_digest, Some(manifest.digest()));

        let mut invalid = manifest;
        invalid.run_id.clear();
        assert!(matches!(
            SchedulerWorker::run_cron_tick_with_manifest(
                &mut scheduler,
                &tick,
                "20260911",
                &invalid,
                &mut executor,
            ),
            Err(SchedulerError::Invalid(_))
        ));
    }

    #[test]
    fn event_and_manual_triggers_are_executable_and_manifest_bound() {
        let manifest = RunManifest {
            run_id: "run-event".into(),
            code_commit: "commit".into(),
            config_hash: "config".into(),
            data_fingerprint: "data".into(),
            clock_start: 1,
            clock_end: 2,
            global_seed: 3,
            determinism_mode: true,
            result_hash: "result".into(),
            strategy_version: "strategy".into(),
            instrument_spec_version: "instrument".into(),
            model_fingerprint: "model".into(),
            input_event_hash: "input".into(),
            output_event_hash: "output".into(),
            runtime_version: "runtime".into(),
        };
        let mut scheduler = Scheduler::default();
        let mut event_job = job("on-fill", vec![]);
        event_job.trigger = Trigger::Event("fill.received".into());
        scheduler.register(event_job).unwrap();
        scheduler.register(job("manual-report", vec![])).unwrap();
        assert_eq!(
            scheduler
                .due_event_jobs("fill.received", &BTreeSet::new())
                .unwrap()
                .len(),
            1
        );
        let mut executor = RecordingExecutor {
            calls: Vec::new(),
            fail: false,
        };
        let event_outcomes = SchedulerWorker::run_event_with_manifest(
            &mut scheduler,
            "fill.received",
            "20260910",
            100,
            &manifest,
            &mut executor,
        )
        .unwrap();
        assert_eq!(
            event_outcomes[0].run.manifest_digest,
            Some(manifest.digest())
        );
        let manual_outcomes = SchedulerWorker::run_manual_with_manifest(
            &mut scheduler,
            "manual-report",
            "20260910",
            101,
            &manifest,
            &mut executor,
        )
        .unwrap();
        assert_eq!(manual_outcomes[0].run.status, JobStatus::Succeeded);
        assert_eq!(executor.calls, ["on-fill#1", "manual-report#1"]);
    }

    #[test]
    fn calendar_worker_manifest_path_enforces_window() {
        let mut scheduler = Scheduler::default();
        let mut session = job("session-manifest", vec![]);
        session.trigger = Trigger::TradingCalendar {
            session: "session".into(),
        };
        session.window = JobWindow::Session;
        scheduler.register(session).unwrap();
        let mut calendar = TradingCalendar::default();
        calendar
            .insert(Session {
                trading_day: "20260910".into(),
                open: 100,
                close: 200,
            })
            .unwrap();
        let manifest = RunManifest {
            run_id: "run-calendar".into(),
            code_commit: "commit".into(),
            config_hash: "config".into(),
            data_fingerprint: "data".into(),
            clock_start: 1,
            clock_end: 2,
            global_seed: 3,
            determinism_mode: true,
            result_hash: "result".into(),
            strategy_version: "strategy".into(),
            instrument_spec_version: "instrument".into(),
            model_fingerprint: "model".into(),
            input_event_hash: "input".into(),
            output_event_hash: "output".into(),
            runtime_version: "runtime".into(),
        };
        let mut executor = RecordingExecutor {
            calls: Vec::new(),
            fail: false,
        };
        let tick = ScheduleTick {
            minute: 0,
            hour: 9,
            day: 10,
            month: 9,
            weekday: 4,
        };
        assert!(SchedulerWorker::run_cron_tick_with_calendar_and_manifest(
            &mut scheduler,
            &tick,
            "20260910",
            50,
            &calendar,
            &manifest,
            &mut executor,
        )
        .unwrap()
        .is_empty());
        let outcomes = SchedulerWorker::run_cron_tick_with_calendar_and_manifest(
            &mut scheduler,
            &tick,
            "20260910",
            150,
            &calendar,
            &manifest,
            &mut executor,
        )
        .unwrap();
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].run.manifest_digest, Some(manifest.digest()));
    }
}
