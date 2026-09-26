//! 任务调度契约。
//!
//! Scheduler 只决定“何时、以什么幂等键触发哪个 Job”，不持有交易所客户端，
//! 也不能绕过 Risk/OMS/Ledger。真实执行器可以在控制面或外部 Worker 中实现。

use qx_core::RunManifest;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

mod retry_policy;
pub use retry_policy::RetryPolicy;
mod job_spec;
pub use job_spec::{
    claimable_by, undispatchable_by_registry, JobSpec, JobWindow, Trigger, JOB_OWNER_ANY,
};

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
    /// cron 面里绑定 RunManifest 的唯一入口：先 `manifest.validate()`，再把
    /// `manifest.digest()` 递给 `run_cron_tick_with_calendar`。
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

    /// cron 派发的唯一执行体：到期判据只有 `due_jobs_with_calendar` 一颗（窗口与
    /// 交易日历都算在里面），执行循环与手动/事件入口共用 `execute_job_ids`。
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
        Self::execute_job_ids(
            scheduler,
            job_ids,
            trading_day,
            now,
            manifest_digest,
            executor,
        )
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
        // 一轮执行从"把卡死的运行收掉"开始，与生产派发事务的开场是同一颗（V11 N4b）：
        // 少了这一步，崩溃留下的 `Running` 行会一直占着并发键，这份执行面之后每一轮
        // 都在这条作业上拿 `NotReady`，而生产那条同一时刻已经把它转成 `NeedsIntervention`。
        scheduler.sweep_timed_out(now)?;
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

    /// 到期判据只有这一颗。原先另有一颗只认 `Trigger::Cron`、不看 `JobWindow` 也不看
    /// 交易日历的 `due_jobs`：同一份 JobSpec 走两条路会点出两个不同的到期集合，而窗口
    /// 约束在其中一条上静默失效（V11 N4）。`JobWindow` 与 `Trigger::TradingCalendar`
    /// 只能从这里判定，避免定义了窗口却没有实际约束。
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

    /// 已登记作业的只读视图。运行拓扑装配用它核对 owner 是否有人领取，
    /// 而不是让作业投进一条没人订阅的队列。
    pub fn jobs(&self) -> impl Iterator<Item = &JobSpec> {
        self.jobs.values()
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
    ///
    /// 作业登记只留这一颗，`started_ts` 必须由调用方给：`deadline_ts` 是
    /// `started_ts + timeout_seconds`，登记侧写死一个起点（原来另有 `start_run(...)`
    /// 硬传 `0`）等于让 `timeout_seconds` 在第一次超时收口时就把作业全部判成
    /// `TIMEOUT`。同理，manifest 的校验与摘要取自 `run_*_with_manifest` 那几颗入口，
    /// 它们各自 validate 后把 `manifest.digest()` 递进来（V11 M4、N4b）。
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
            // `started_ts`/`deadline_ts` 与运行时其余时钟一样是毫秒；`timeout_seconds` 是秒。
            // 直接相加会让一条 60 秒的作业在 60 毫秒后"过期"，而这份数字照常外销。
            deadline_ts: started_ts.saturating_add(job.timeout_seconds.saturating_mul(1_000)),
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

    /// 把所有"已过截止时间仍在跑"的运行收成人工接管，并连带释放它们的并发键。
    /// 生产派发每轮先走这一颗：`timeout_seconds` 只有在该收的时候有人收，才真的
    /// 约束什么——否则一条卡死的运行会永久占着并发键，之后每一轮派发都被
    /// `NotReady` 挡掉（V11 N2）。判定复用 `is_timed_out`，改写复用 `mark_timed_out`，
    /// 这里只负责"扫一遍"这件事本身。
    pub fn sweep_timed_out(&mut self, now: u64) -> Result<Vec<JobRun>, SchedulerError> {
        let candidates = self.runs.keys().copied().collect::<Vec<_>>();
        let mut swept = Vec::new();
        for run_id in candidates {
            if self.is_timed_out(run_id, now)? {
                swept.push(self.mark_timed_out(run_id, now)?);
            }
        }
        Ok(swept)
    }

    /// 结束一次运行；`finished_ts` 同时是退避窗口的起点，写死 `0` 会让下一次重试
    /// 时刻落在纪元上、等于随时可重投（原来另有 `finish_run(...)` 就是这么干的）。
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
        // `error_code` 的口径是"这次失败为什么失败"：下面的重试判据按 `retryable_codes`
        // 匹配它，`MANUAL_INTERVENTION` 也认它。成功那一侧往里写东西，读面上就是一个
        // 从没发生过的错误码（V11 O1）。
        run.error_code = if success {
            None
        } else {
            error_code.map(str::to_string)
        };
        run.next_retry_ts = (!success
            && !matches!(run.status, JobStatus::NeedsIntervention)
            && job.retry_policy.should_retry(run.attempt)
            && (job.retry_policy.retryable_codes.is_empty()
                || run
                    .error_code
                    .as_ref()
                    .is_some_and(|code| job.retry_policy.retryable_codes.contains(code))))
        .then(|| job.retry_policy.retry_deadline(finished_ts));
        self.active_keys.remove(&job.concurrency_key);
        if success {
            self.completed.insert(run.job_id.clone());
        }
        self.runs.insert(run_id, run.clone());
        Ok(run)
    }

    /// 把一次已到重试时刻的失败运行推回 Running 并递增 `attempt`。
    ///
    /// 重试只有这一颗入口：`now` 由调用方给，退避判定与并发键占用都在函数里完成。
    /// 原来另有一颗 `retry_run(run_id)`（内部传 `u64::MAX`，等于绕过退避窗口立刻重投），
    /// 仓内没有任何派发者走它——运行时注册表不重试（V11 N4），留着一个"跳过等待"的
    /// 写法只是多一处要让上层记住的口子。
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
        if !matches!(run.status, JobStatus::Failed) || !job.retry_policy.should_retry(run.attempt) {
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
            timeout_seconds: 60,
            retry_policy: RetryPolicy::default(),
            concurrency_key: id.into(),
            idempotency_key: format!("{id}-daily"),
            audit_reason: format!("test {id}"),
            dry_run: true,
        }
    }

    /// R7-4 删掉的三格在磁盘上还活在升级之前写下的调度状态与作业队列信封里。读侧一旦改成
    /// 对未知键严格（`deny_unknown_fields`），升级就会把手里的运行状态读崩：作业队列会整批
    /// 解不开，调度器连自己登记过什么都读不回来。这里钉住"旧文档仍解得回、且形状不变"。
    #[test]
    fn retired_job_spec_keys_are_ignored_by_the_loader() {
        let mut scheduler = Scheduler::default();
        scheduler.register(job("bars", vec![])).unwrap();
        let current = scheduler.to_json().expect("当前形状必须可序列化");
        let retired = current.replace(
            "\"depends_on\":[],",
            "\"depends_on\":[],\"input_refs\":[\"market:BTCUSDT.BINANCE\"],\"output_refs\":[\"strategy:order-intent\"],\"permission_scope\":\"strategy\",",
        );
        assert_ne!(
            current, retired,
            "夹具必须真的带上三格退役键，否则这条用例什么都没测"
        );
        let restored = Scheduler::from_json(&retired)
            .expect("升级之前写下的调度状态必须解得回来")
            .to_json()
            .expect("读回后仍须可序列化");
        assert_eq!(
            restored, current,
            "退役键必须被读侧忽略：解回再印出的形状要与当前形状逐字节相同"
        );
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
            scheduler.start_run_at("factor", "20260910", 1, 1_000),
            Err(SchedulerError::NotReady("factor".into()))
        );
        let first = scheduler
            .start_run_at("load", "20260910", 1, 1_000)
            .unwrap();
        assert_eq!(
            scheduler
                .start_run_at("load", "20260910", 1, 1_000)
                .unwrap(),
            first
        );
        let finished = scheduler
            .finish_run_with_code(first.run_id, true, None, 1_500)
            .unwrap();
        assert_eq!(finished.status, JobStatus::Succeeded);
        let next = scheduler
            .start_run_at("factor", "20260910", 2, 2_000)
            .unwrap();
        assert_eq!(next.status, JobStatus::Running);
    }

    /// `error_code` 只装"这一次失败为什么失败"：一次成功的收尾把结果摘要递进来也不许落
    /// 在那一格里——`/scheduler/runs` 原样外销它，重试判据又拿它匹配 `retryable_codes`，
    /// 于是一条摘要在读面上就是一个从没发生过的错误码（V11 O1）。反向那半同时钉住：失败
    /// 带码必须照常留码，否则判据退化成"这一格永远写不进东西"也能绿。
    #[test]
    fn a_successful_finish_refuses_an_error_code() {
        let mut scheduler = Scheduler::default();
        scheduler.register(job("load", vec![])).unwrap();
        let ok = scheduler
            .start_run_at("load", "20260910", 1, 1_000)
            .unwrap();
        let finished = scheduler
            .finish_run_with_code(ok.run_id, true, Some("2 orders: BTC:1"), 1_500)
            .unwrap();
        assert_eq!(finished.status, JobStatus::Succeeded);
        assert_eq!(
            finished.error_code, None,
            "成功运行不得携带错误码：读面会把它念成一次没发生过的失败"
        );
        let mut failing = job("load", vec![]);
        failing.retry_policy.max_attempts = 2;
        let mut retrying = Scheduler::default();
        retrying.register(failing).unwrap();
        let run = retrying.start_run_at("load", "20260910", 1, 1_000).unwrap();
        let failed = retrying
            .finish_run_with_code(run.run_id, false, Some("TRANSIENT"), 1_500)
            .unwrap();
        assert_eq!(failed.status, JobStatus::Failed);
        assert_eq!(failed.error_code.as_deref(), Some("TRANSIENT"));
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
            .due_jobs_with_calendar(
                &ScheduleTick {
                    minute: 0,
                    hour: 9,
                    day: 10,
                    month: 9,
                    weekday: 4,
                },
                "20260910",
                1_000,
                &TradingCalendar::default(),
                &BTreeSet::new(),
            )
            .unwrap();
        assert_eq!(due.len(), 1);
        let run = scheduler
            .start_run_at("cron", "20260910", 1, 1_000)
            .unwrap();
        scheduler
            .finish_run_with_code(run.run_id, false, None, 1_500)
            .unwrap();
        // 退避窗口从 `finished_ts` 起算：默认 `backoff_seconds` 为 0，所以 1_500 这一刻
        // 就能重投，而更早的时刻不行——写死 `0` 的那颗入口把这条判据抹平了。
        assert!(scheduler.retry_run_at(run.run_id, 1_499).is_err());
        assert_eq!(
            scheduler.retry_run_at(run.run_id, 1_500).unwrap().attempt,
            2
        );
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
            .due_jobs_with_calendar(
                &ScheduleTick {
                    minute: 0,
                    hour: 9,
                    day: 10,
                    month: 9,
                    weekday: 4,
                },
                "20260910",
                1_000,
                &TradingCalendar::default(),
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
        let outcomes = SchedulerWorker::run_cron_tick_with_calendar(
            &mut scheduler,
            &tick,
            "20260910",
            7,
            &TradingCalendar::default(),
            7,
            &mut executor,
        )
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
        let failed = SchedulerWorker::run_cron_tick_with_calendar(
            &mut retry_scheduler,
            &tick,
            "20260910",
            7,
            &TradingCalendar::default(),
            7,
            &mut failing,
        )
        .unwrap();
        assert_eq!(failed[0].run.status, JobStatus::Failed);
        assert_eq!(failed[0].error.as_deref(), Some("WORKER_FAILED"));
        assert_eq!(
            retry_scheduler
                .retry_run_at(failed[0].run.run_id, 7)
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
        let run = scheduler
            .start_run_at("retry-policy", "20260910", 1, 90)
            .unwrap();
        let failed = scheduler
            .finish_run_with_code(run.run_id, false, Some("PERMANENT"), 100)
            .unwrap();
        assert_eq!(failed.next_retry_ts, None);
        assert!(scheduler.retry_run_at(run.run_id, 110).is_err());

        let second = scheduler
            .start_run_at("retry-policy", "20260911", 2, 90)
            .unwrap();
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
        // `started_ts` 与运行时其余时钟同源（毫秒），`timeout_seconds` 是秒：拿 100/130
        // 这类小计数当时间戳，秒与毫秒的差别看不出来，单位分叉就藏在这里（V11 N1）。
        let started = 1_790_176_424_274;
        let run = scheduler
            .start_run_at("timed", "20260910", 1, started)
            .unwrap();
        assert_eq!(run.started_ts, started);
        assert_eq!(run.deadline_ts, started + 30_000);
        assert!(!scheduler
            .is_timed_out(run.run_id, started + 29_999)
            .unwrap());
        assert!(scheduler
            .is_timed_out(run.run_id, started + 30_000)
            .unwrap());
        let timeout = scheduler
            .mark_timed_out(run.run_id, started + 30_000)
            .unwrap();
        assert_eq!(timeout.status, JobStatus::NeedsIntervention);
        assert_eq!(timeout.error_code.as_deref(), Some("TIMEOUT"));
        assert!(!scheduler
            .is_timed_out(run.run_id, started + 999_999)
            .unwrap());
    }

    #[test]
    fn sweeping_a_stalled_run_gives_the_concurrency_key_back() {
        let mut scheduler = Scheduler::default();
        let mut stalled = job("stalled", vec![]);
        stalled.timeout_seconds = 1;
        scheduler.register(stalled).unwrap();
        let started = 1_000;
        scheduler
            .start_run_at("stalled", "20260910", 1, started)
            .unwrap();
        // 没人收这一刀时，卡死的运行会把并发键一直占着：之后任何一天的新运行都拿到
        // NotReady，派发端每一轮都失败一次而状态文件里看不出原因（V11 N2）。
        assert!(matches!(
            scheduler.start_run_at("stalled", "20260911", 1, started),
            Err(SchedulerError::NotReady(_))
        ));
        assert!(scheduler.sweep_timed_out(started + 999).unwrap().is_empty());
        let swept = scheduler.sweep_timed_out(started + 1_000).unwrap();
        assert_eq!(swept.len(), 1);
        assert_eq!(swept[0].error_code.as_deref(), Some("TIMEOUT"));
        let next_day = scheduler
            .start_run_at("stalled", "20260911", 1, started)
            .unwrap();
        assert_eq!(next_day.status, JobStatus::Running);
        // 第二刀只可能落在新的那条上：已经转成人工接管的运行不会被再次改写。
        let second = scheduler.sweep_timed_out(started + 9_999).unwrap();
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].trading_day, "20260911");
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
            input_components: BTreeMap::new(),
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
        let outcomes = SchedulerWorker::run_cron_tick_with_calendar_and_manifest(
            &mut scheduler,
            &tick,
            "20260910",
            7,
            &TradingCalendar::default(),
            &manifest,
            &mut executor,
        )
        .unwrap();
        assert_eq!(outcomes[0].run.manifest_digest, Some(manifest.digest()));

        let mut invalid = manifest;
        invalid.run_id.clear();
        assert!(matches!(
            SchedulerWorker::run_cron_tick_with_calendar_and_manifest(
                &mut scheduler,
                &tick,
                "20260911",
                7,
                &TradingCalendar::default(),
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
            input_components: BTreeMap::new(),
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
            input_components: BTreeMap::new(),
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

    /// 没有这颗收口，库里这份执行面会在一次崩溃之后永久派不动：`start_run_at` 的并发键
    /// 还被那条 `Running` 占着，整轮以 `NotReady` 结束（V11 N4b，与生产 N2 同一口径）。
    #[test]
    fn worker_tick_sweeps_a_run_left_running_by_a_crash() {
        let mut scheduler = Scheduler::default();
        let mut stalled = job("stalled-then-cron", vec![]);
        stalled.trigger = Trigger::Cron("0 9 * * *".into());
        stalled.timeout_seconds = 1;
        scheduler.register(stalled).unwrap();
        let stuck = scheduler
            .start_run_at("stalled-then-cron", "20260910", 1, 1_000)
            .unwrap();
        let mut executor = RecordingExecutor {
            calls: Vec::new(),
            fail: false,
        };
        let outcomes = SchedulerWorker::run_cron_tick_with_calendar(
            &mut scheduler,
            &ScheduleTick {
                minute: 0,
                hour: 9,
                day: 10,
                month: 9,
                weekday: 4,
            },
            "20260911",
            3_000,
            &TradingCalendar::default(),
            3_000,
            &mut executor,
        )
        .unwrap();
        assert_eq!(executor.calls, ["stalled-then-cron#1"]);
        assert_eq!(outcomes.len(), 1);
        let recovered = scheduler.run(stuck.run_id).unwrap();
        assert_eq!(recovered.status, JobStatus::NeedsIntervention);
        assert_eq!(recovered.error_code.as_deref(), Some("TIMEOUT"));
    }
}
