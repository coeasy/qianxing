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
/// `crates/qx-cli/tests/unit/zero_reader_fields.rs` 的
/// `zero_reader_struct_fields_stay_registered_in_capabilities` 双向钉住：
/// 接上真读者就必须摘掉登记，偷偷删登记同样判红。
///
/// ## 为什么这三格必须容忍缺键（V13 R27）
///
/// `JobSpec` 是**持久化**类型，不是一次性的请求体：它既从 `deploy/qianxing.scheduler.*.json`
/// 读入，又被 `JsonStateStore` 写进调度状态文件（`scheduler.json`）并在下一次启动时读回。
/// 这三格当初按必填加进来，于是**任何**在本构建之前写下的状态文件或作业清单都读不回来：
/// 实测 `qx-cli paper-e2e` 在仓库根跑第二次就死在第 427 列——
/// `加载 Scheduler 状态失败: Io("missing field `input_refs`")`，而报错完全没提"这份状态是旧
/// 版本写的"。更糟的是这三格**零生产读者**（见上面的登记），也就是说"必填"这个选择唯一
/// 能产生的可观测效果就是让旧文件读不回来。
///
/// 处置与同一个结构体里的 `audit_reason` 完全一致（那里已经有先例与注释）：给缺键一个
/// **具名**缺省，让旧文件按"它当年确实没声明"读进来，而不是按"字段不存在"整份拒绝。
/// `Vec` 那两格的缺省是空表（"没声明引用"本来就是合法语义）；`permission_scope` 那格用
/// 具名缺省而不是空串，免得把"旧文件没这一格"与"文件显式声明了空权限"读成同一件事——
/// 这与 V13 R9「金额字段的缺键不许静默变成 0」是同一条纪律：**缺键要有自己的名字**。
///
/// 序列化侧**不受影响**：`#[serde(default)]` 只改反序列化，新写的状态文件逐字节不变，
/// 所以已 bless 的 `config_fingerprint` 与 RunManifest 哈希不会因为这次修复集体失真。
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct JobSpec {
    pub job_id: String,
    pub job_version: String,
    pub owner: String,
    pub enabled: bool,
    pub trigger: Trigger,
    pub window: JobWindow,
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub input_refs: Vec<String>, // 声明式输入引用；仓库内零生产读者，见 capabilities.yaml job_spec_declaration_fields_have_no_production_reader
    #[serde(default)]
    pub output_refs: Vec<String>, // 声明式输出引用，同 input_refs：仓库内零生产读者
    pub timeout_seconds: u64,
    pub retry_policy: RetryPolicy,
    pub concurrency_key: String,
    pub idempotency_key: String,
    #[serde(default = "default_permission_scope")]
    pub permission_scope: String, // 安全形状的声明格；今天零生产读者（没有一处按它做准入判定）
    #[serde(default = "default_audit_reason")]
    pub audit_reason: String,
    pub dry_run: bool,
}

fn default_audit_reason() -> String {
    "legacy-job".into()
}

/// 旧文件里没有 `permission_scope` 那一格时的具名缺省。
///
/// 刻意**不**用空串：空串把"这份文件写于本字段存在之前"和"这份文件显式声明了空权限"读成
/// 同一件事，而这两件事在审计里是两回事。取值与 `default_audit_reason` 的 `"legacy-job"`
/// 同一风格——自描述，且在日志里一眼看得出是迁移路径而不是业务值。
fn default_permission_scope() -> String {
    "legacy-undeclared".into()
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
