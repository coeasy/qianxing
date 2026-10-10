//! 调用上下文与能力档（路线图 §5 R/P/O/L；方案 §15.5 `RunContext`）。
//!
//! 应用层用例的**每一个**入口都先问一件事：这个调用方够不够格跑这个用例？问完之后才碰磁盘、
//! 才装配引擎。这就是 [`RunContext::require`] 的位置——它在用例的第一行，不在用例的中间某处。
//!
//! 为什么能力档要落在应用层而不是门面：门面各写一份授权判定，就是 §15.2 说的"入口分叉"——
//! CLI 放行的东西 HTTP 拒绝，或者反过来。授权口径只有一份，在用例边界上。

use crate::error::{AppError, AppErrorCategory};

/// 应用层能力档，与路线图 §5 的 R/P/O/L 逐档同名同序。
///
/// 档位是一条**阶梯**（[`CallerCapability::rank`]）：高档覆盖低档，因为"能连真实账户的人"
/// 当然也能跑一次回测。反过来不成立——研究档不许启动 Paper，更不许碰实盘。
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum CallerCapability {
    /// R · 研究：数据检查、回测、报告复核。无外部副作用。
    Research,
    /// P · Paper：模拟运行与 Paper 订单。需要显式本地配置。
    Paper,
    /// O · Operator：worker/调度/恢复/存储运维。需要 operator 身份。
    Operator,
    /// L · Live：真实账户、真实下单、对账。**默认关闭**，独立授权。
    Live,
}

impl CallerCapability {
    /// 稳定字符串形态。展示层与拒绝文案取它——判重试/判权限**不要**按文案。
    pub const fn as_str(self) -> &'static str {
        match self {
            CallerCapability::Research => "RESEARCH",
            CallerCapability::Paper => "PAPER",
            CallerCapability::Operator => "OPERATOR",
            CallerCapability::Live => "LIVE",
        }
    }

    /// 档位高度。阶梯只有这一处定义，[`CallerCapability::allows`] 从它派生。
    pub const fn rank(self) -> u8 {
        match self {
            CallerCapability::Research => 0,
            CallerCapability::Paper => 1,
            CallerCapability::Operator => 2,
            CallerCapability::Live => 3,
        }
    }

    /// 授予本档，是否覆盖 `required` 那一档。
    pub const fn allows(self, required: Self) -> bool {
        self.rank() >= required.rank()
    }
}

/// 本 crate 在**门面没有交代自己的构建身份**时写进 RunManifest 的那一格。
///
/// 它只是包版本，不是提交号——真实的提交号由门面经 [`RunContext::with_code_commit`] 注入
/// （CLI 有 `build_identity::BUILD_REVISION`）。宁可写一个诚实的"版本号"，也不要把
/// `"unknown"` 当成提交号塞进产物：后者看起来像"查过了，没有"。
pub const APP_DEFAULT_CODE_COMMIT: &str = env!("CARGO_PKG_VERSION");

/// 一次调用的上下文：谁在调、拿什么 id 对齐、这次运行的代码身份是什么。
///
/// `correlation_id` 是**报错、产物、日志三处对齐用的那一枚**：用例返回的每条 [`AppError`]
/// 都带着它（[`AppError::with_correlation_id`]），所以同一场失败在 CLI/Python/HTTP 三层
/// 拿到的 id 相同（G1/G6）。
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RunContext {
    capability: CallerCapability,
    correlation_id: String,
    code_commit: String,
}

impl RunContext {
    /// 构造上下文。`correlation_id` 通常就是 `run_id`（或 HTTP 请求 id）。
    pub fn new(capability: CallerCapability, correlation_id: impl Into<String>) -> Self {
        Self {
            capability,
            correlation_id: correlation_id.into(),
            code_commit: APP_DEFAULT_CODE_COMMIT.to_string(),
        }
    }

    /// 让门面把**自己的**构建身份带进来。CLI 传 `build_identity::BUILD_REVISION`；
    /// 不调用时是包版本（见 [`APP_DEFAULT_CODE_COMMIT`]）。
    pub fn with_code_commit(mut self, code_commit: impl Into<String>) -> Self {
        self.code_commit = code_commit.into();
        self
    }

    /// 本次授予的能力档。
    pub fn capability(&self) -> CallerCapability {
        self.capability
    }

    /// 对齐 id（报错、产物、日志三处用它）。
    pub fn correlation_id(&self) -> &str {
        &self.correlation_id
    }

    /// 写进 RunManifest 的代码身份。
    pub fn code_commit(&self) -> &str {
        &self.code_commit
    }

    /// 用例边界的能力闸：本档覆盖不了 `required` 就 [`AppErrorCategory::PermissionDenied`]。
    ///
    /// `use_case` 是展示层用的用例名（中文），不参与分支。
    pub fn require(&self, required: CallerCapability, use_case: &str) -> Result<(), AppError> {
        if self.capability().allows(required) {
            return Ok(());
        }
        Err(AppError::new(
            AppErrorCategory::PermissionDenied,
            format!(
                "{use_case} 需要 {} 档，本次调用只有 {} 档",
                required.as_str(),
                self.capability().as_str()
            ),
        ))
    }
}
