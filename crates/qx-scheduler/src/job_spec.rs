//! 作业声明模型：触发器、交易窗口与 JobSpec，以及 owner 路由判据。
//!
//! Scheduler 只决定"何时触发哪个 Job"，这里放的是它的输入契约。`owner` 是路由键，
//! `JobSpec::validate` 只保证字段合法；"有没有启用中的 Strategy worker 会领取它"
//! 不在调度器视野内，由运行拓扑装配处（`qx-cli` 的 `load_scheduler_state`）把关——
//! 但**判据本身**放在这里，领取端与装配端共用同一处（V11 §41 E7）。

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

/// owner 路由判据：只有 id 与 owner 逐字相等的 worker，或 owner 是通配值时的任意
/// 启用 worker 会领取作业。领取端（`qx-cli` 的 `workers.rs`）与装配端
/// （`qx-cli` 的 `load_scheduler_state`）共用这一处判定——判据抄成两份时，
/// 装配端放行而领取端不认（或反过来）都不会有人发现，作业就这么静死在队列里。
pub fn claimable_by(owner: &str, worker_id: &str) -> bool {
    owner == JOB_OWNER_ANY || owner == worker_id
}

/// 作业声明。字段名单里的每一格都必须有读者（V11 R7-4）：`depends_on` 被 `ready_jobs`
/// 与环检测认得，`owner` 被领取端与装配处认得，`timeout_seconds`/`retry_policy` 被执行面
/// 认得。
///
/// `input_refs`/`output_refs`/`permission_scope` 三格今天**只有写侧没有读侧**：依赖由
/// `depends_on` 承载，权限判定在控制面（`Permission` + operator 映射）而不是这里。
/// 按仓库先例（#118/#170/#171）与 2026-10-06 方案 §13.2 的裁定，这三格**保留不删**
/// （删面等于把缺口藏起来），改为在 `maturity/capabilities.yaml` 逐条登记
/// （`job_spec_declaration_fields_have_no_production_reader`），并由
/// `crates/qx-cli/src/tests/zero_reader_fields.rs` 的
/// `zero_reader_struct_fields_stay_registered_in_capabilities` 双向钉住：
/// 接上真读者就必须摘掉登记，偷偷删登记同样判红。
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct JobSpec {
    pub job_id: String,
    pub job_version: String,
    pub owner: String,
    pub enabled: bool,
    pub trigger: Trigger,
    pub window: JobWindow,
    pub depends_on: Vec<String>,
    pub input_refs: Vec<String>, // 声明式输入引用；仓库内零生产读者，见 capabilities.yaml job_spec_declaration_fields_have_no_production_reader
    pub output_refs: Vec<String>, // 声明式输出引用，同 input_refs：仓库内零生产读者
    pub timeout_seconds: u64,
    pub retry_policy: RetryPolicy,
    pub concurrency_key: String,
    pub idempotency_key: String,
    pub permission_scope: String, // 安全形状的声明格；今天零生产读者（没有一处按它做准入判定）
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
