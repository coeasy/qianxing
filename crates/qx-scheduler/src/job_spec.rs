//! 作业声明模型：触发器、交易窗口与 JobSpec。
//!
//! Scheduler 只决定"何时触发哪个 Job"，这里放的是它的输入契约。`owner` 是路由键，
//! `JobSpec::validate` 只保证字段合法；"有没有启用中的 Strategy worker 会领取它"
//! 不在调度器视野内，由运行拓扑装配处把关。

use crate::{CronSpec, RetryPolicy, SchedulerError, TradingCalendar};
use qx_core::Fnv1a;
use serde::{Deserialize, Serialize};

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Trigger {
    Manual,
    Cron(String),
    TradingCalendar { session: String },
    Event(String),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum JobWindow {
    Any,
    PreOpen,
    Session,
    PostClose,
}

impl JobWindow {
    pub(crate) fn allows(self, calendar: &TradingCalendar, trading_day: &str, ts: u64) -> bool {
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

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Any => "any",
            Self::PreOpen => "pre_open",
            Self::Session => "session",
            Self::PostClose => "post_close",
        }
    }
}

/// 作业 owner 的通配值：任何启用的 Strategy worker 都能领取。一份作业清单服务多套
/// 拓扑时用它，但拓扑装配处必须确认"至少有一个可领取的 worker"（V11 §41 E7）。
pub const JOB_OWNER_ANY: &str = "*";

/// 由 Scheduler 登记的作业定义。`owner` 是路由键：只有 id 与它相等、或它等于
/// `JOB_OWNER_ANY` 的启用 Strategy worker 会领取，`JobSpec::validate` 只看非空，
/// 真正的可领取性由 `qx-cli` 的运行拓扑装配把关。
/// owner 路由判据：只有 id 与 owner 相等的 worker，或 owner 是通配值时的任意
/// 启用 worker 会领取作业。领取端与装配端共用这一处判定。
pub fn claimable_by(owner: &str, worker_id: &str) -> bool {
    owner == JOB_OWNER_ANY || owner == worker_id
}

/// 运行时的注册表派发器（`qx-cli` 的 `dispatch_scheduled_jobs`）今天只走得到的形状：
/// `Trigger::Cron` + `JobWindow::Any` + 一次尝试。返回 `Some(原因)` 表示"这份声明
/// 写在作业文件里也不会生效"——装配处必须把它挡在启动之前，而不是让它静留在
/// 一个永远不会触发的事件名上（V11 N4，与 E7 的 owner 无人领取同一形状）。
///
/// 为什么不在这里补齐语义而不是放行：非 `Any` 窗口要 `TradingCalendar` 的会话边界，
/// 而运行时没有任何日历写入者；重试要有人重新投递新 `attempt` 的 JobRun，而
/// `FileJobQueue` 按 `run_id` 落盘且信封不携带 `attempt`，重投会撞 `Conflict` 而不是
/// 重来一次（V11 N3 开放项）。两者都不是装配处顺手能改的事。
pub fn undispatchable_by_registry(job: &JobSpec) -> Option<&'static str> {
    if !matches!(job.trigger, Trigger::Cron(_)) {
        return Some(
            "注册表派发只认 `Trigger::Cron`：`Manual`/`Event` 各有自己的派发端，\
             `TradingCalendar` 在没有日历的运行时里只剩 `session: \"any\"` 一条路，\
             而那条路的语义是每分钟都到期",
        );
    }
    if !matches!(job.window, JobWindow::Any) {
        return Some("非 `Any` 窗口要交易日历，而运行时派发没有日历写入者");
    }
    if job.retry_policy.max_attempts > 1 {
        return Some(
            "重试没有派发者：作业队列按 `run_id` 落盘、不携带 `attempt`，重投会撞已结算信封",
        );
    }
    None
}

/// 作业声明。字段名单里的每一格都必须有读者（V11 R7-4）：`depends_on` 被 `ready_jobs`
/// 与环检测认得，`owner` 被领取端与装配处认得，`timeout_seconds`/`retry_policy` 被执行面
/// 认得。此前这里还有 `input_refs`/`output_refs`/`permission_scope` 三格只有写侧没有读侧，
/// 而它们是作业 JSON 的**必填**格——运行者必须手抄三份没人执行的声明，
/// 其中 `permission_scope` 的名字还替一种并不存在的授权作保（权限判定在控制面那边）。
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct JobSpec {
    pub job_id: String,
    pub job_version: String,
    pub owner: String,
    pub enabled: bool,
    pub trigger: Trigger,
    pub window: JobWindow,
    pub depends_on: Vec<String>,
    pub timeout_seconds: u64,
    pub retry_policy: RetryPolicy,
    pub concurrency_key: String,
    pub idempotency_key: String,
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
