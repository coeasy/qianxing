//! 应用层错误契约（T2-0 / 路线图 §X1；方案 §15.5「错误契约唯一转换链」）。
//!
//! 仓里已有两根错误轴，本模块加的是**第三根、也是最外面那根**：
//!
//! - [`qx_core::ErrorCode`]（DD-5 五元契约）答的是「**系统**该怎么反应」——能不能重试、
//!   要不要先对账、安不安全再发一次。它服务的是热路径（下单、成交、对账）。
//! - 本模块的 [`AppErrorCategory`] 答的是「**调用方**该怎么反应」——改输入、去取数据、
//!   降档、申请权限、解决冲突、稍后重试、查存储、报 bug。它服务的是应用层用例的边界。
//!
//! 两根轴**刻意不合并**：`QxError` 是领域事实（"结果未知，必须对账"），
//! `AppError` 是应用层对外的行动指引（"这一条你现在该做什么"）。合并会逼着每一个
//! 用例的调用方先去理解 venue 语义，而那正是应用层要挡在门外的东西。
//!
//! 唯一映射表在本模块：类别 → 行动是 [`AppErrorCategory::action`]，类别 → 重试档是
//! [`AppErrorCategory::retry`]，领域码 → 类别是 [`category_of_domain_code`]。
//! 调用方按 [`AppErrorCategory::as_str`] 分支，**永远不要** `match` 消息文本——
//! 消息是中文展示层，会随文案漂移。
//!
//! ## 两条轴之间唯一必须成立的那条不等式
//!
//! 两根轴各自完整，但有一条不能破：**应用层绝不许把领域层判为"不安全重发"的错误说成
//! 可以原地重发**。形式化就是 `AppError::safe_to_retry() ⟹ QxError::contract().safe_to_retry`。
//! 反向不成立也不该成立——`VenueState` 在领域侧是"退避后可重试"，在调用方侧就是
//! "稍后原样再试"，两边说的本来就不是同一件事。这条不等式由 `AppError` 的边界用例逐码核对。

use qx_core::{ErrorCode, QxError};
use serde::{Deserialize, Serialize};
use std::fmt;

/// 应用层错误类别：调用方**该做什么**。八类，与路线图 §X1 的清单逐条同名同序。
///
/// 闭集：新增一类必须同时补 `tools/check_architecture.py` 的 `qx_app_check`
/// 与 `docs/牵星Qianxing-详细开发计划与实施路线图-2026-10-10.md` §X1 那一行，
/// 否则门禁当场红（它逐名核对本枚举与路线图那张表）。
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AppErrorCategory {
    /// 输入本身不合法（字段缺失、超范围、单位混用、schema 版本不符）。改输入再来。
    InvalidInput,
    /// 要用的数据今天取不到（文件不在、来源不可达、数据集未注册）。先去把数据准备好。
    DataUnavailable,
    /// 数据在，但档位不足以支撑这个用例（Bar 档要 L2、样本过短、复权缺件）。
    /// **不许静默降级成更宽松的假设**——降档是调用方的决定，不是实现的决定。
    FidelityInsufficient,
    /// 权限不足（没有私有凭据、账户不在本部署、operator 未授权）。去申请，别重试。
    PermissionDenied,
    /// 与既有事实冲突（同一 run_id 已有不同内容、乐观锁版本不符、产物路径被占）。
    /// 先去解决冲突，重发只会再撞一次。
    Conflict,
    /// 超时或瞬时故障：**八类里唯一可以原地重发的**。
    Timeout,
    /// 存储层失败（写产物、落快照、读注册表）。先去查存储，别把它当业务拒绝。
    StorageFailure,
    /// 内部一致性错误（不该发生的状态、账不平、断言失败）。**这是 bug**，去报。
    InternalInvariant,
}

/// 用户行动提示：类别 → 调用方下一步该做的事。
///
/// 与 [`AppErrorCategory`] 一一对应，映射表只有 [`AppErrorCategory::action`] 一份。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AppAction {
    /// 修正输入后重发。
    FixInput,
    /// 先把数据准备好（下载/导入/注册数据集）。
    ProvideData,
    /// 换一档数据或降低用例要求——这是调用方的决定。
    LowerFidelity,
    /// 申请凭据或授权。
    RequestPermission,
    /// 解决冲突（换 run_id / 换产物目录 / 先同步既有事实）。
    ResolveConflict,
    /// 稍后原样重试。
    RetryLater,
    /// 检查存储（磁盘、权限、路径、剩余空间）。
    CheckStorage,
    /// 报 bug，附上 correlation id。
    ReportBug,
}

/// 重试档：**不是布尔**（方案 §15.5 明文要求把重试决定分成四档）。
///
/// 与 [`qx_core::Retryability`] 同形但**不是同一个类型**：那个是领域侧对"系统该不该再发"
/// 的裁决，这个是应用层对"调用方能不能再来一次"的指引。四档只有一处定义
/// （[`AppErrorCategory::retry`]），`safe_to_retry` 也从它派生，所以不存在
/// 「档位说 Never 而布尔说 true」这种自相矛盾。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AppRetry {
    /// 不可重试：重发必然再失败，或必须先去解决别的事。
    Never,
    /// 可以立即原样重发（请求可判定地没有产生副作用）。
    SafeNow,
    /// 先解决冲突/对账，之后可以重发。
    AfterReconcile,
    /// 先查存储/退避，之后可以重发。
    AfterBackoff,
}

impl AppRetry {
    /// 稳定字符串形态（`SCREAMING_SNAKE_CASE`）。
    pub const fn as_str(self) -> &'static str {
        match self {
            AppRetry::Never => "NEVER",
            AppRetry::SafeNow => "SAFE_NOW",
            AppRetry::AfterReconcile => "AFTER_RECONCILE",
            AppRetry::AfterBackoff => "AFTER_BACKOFF",
        }
    }
}

impl AppAction {
    /// 稳定字符串形态（`SCREAMING_SNAKE_CASE`）。序列化、日志、CLI 输出都取它。
    pub const fn as_str(self) -> &'static str {
        match self {
            AppAction::FixInput => "FIX_INPUT",
            AppAction::ProvideData => "PROVIDE_DATA",
            AppAction::LowerFidelity => "LOWER_FIDELITY",
            AppAction::RequestPermission => "REQUEST_PERMISSION",
            AppAction::ResolveConflict => "RESOLVE_CONFLICT",
            AppAction::RetryLater => "RETRY_LATER",
            AppAction::CheckStorage => "CHECK_STORAGE",
            AppAction::ReportBug => "REPORT_BUG",
        }
    }
}

impl fmt::Display for AppAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl AppErrorCategory {
    /// 稳定字符串形态（`SCREAMING_SNAKE_CASE`）。
    pub const fn as_str(self) -> &'static str {
        match self {
            AppErrorCategory::InvalidInput => "INVALID_INPUT",
            AppErrorCategory::DataUnavailable => "DATA_UNAVAILABLE",
            AppErrorCategory::FidelityInsufficient => "FIDELITY_INSUFFICIENT",
            AppErrorCategory::PermissionDenied => "PERMISSION_DENIED",
            AppErrorCategory::Conflict => "CONFLICT",
            AppErrorCategory::Timeout => "TIMEOUT",
            AppErrorCategory::StorageFailure => "STORAGE_FAILURE",
            AppErrorCategory::InternalInvariant => "INTERNAL_INVARIANT",
        }
    }

    /// 用户行动提示。**全仓唯一**一份「类别 → 行动」映射表。
    pub const fn action(self) -> AppAction {
        match self {
            AppErrorCategory::InvalidInput => AppAction::FixInput,
            AppErrorCategory::DataUnavailable => AppAction::ProvideData,
            AppErrorCategory::FidelityInsufficient => AppAction::LowerFidelity,
            AppErrorCategory::PermissionDenied => AppAction::RequestPermission,
            AppErrorCategory::Conflict => AppAction::ResolveConflict,
            AppErrorCategory::Timeout => AppAction::RetryLater,
            AppErrorCategory::StorageFailure => AppAction::CheckStorage,
            AppErrorCategory::InternalInvariant => AppAction::ReportBug,
        }
    }

    /// 重试档。**全仓唯一**一份「类别 → 重试档」映射表，[`AppError::safe_to_retry`] 从它派生。
    pub const fn retry(self) -> AppRetry {
        match self {
            AppErrorCategory::InvalidInput => AppRetry::Never,
            AppErrorCategory::DataUnavailable => AppRetry::Never,
            AppErrorCategory::FidelityInsufficient => AppRetry::Never,
            AppErrorCategory::PermissionDenied => AppRetry::Never,
            AppErrorCategory::Conflict => AppRetry::AfterReconcile,
            AppErrorCategory::Timeout => AppRetry::SafeNow,
            AppErrorCategory::StorageFailure => AppRetry::AfterBackoff,
            AppErrorCategory::InternalInvariant => AppRetry::Never,
        }
    }

    /// 原样重发同一条请求是否**既安全又可能成功**。
    ///
    /// 八类里**只有 [`AppErrorCategory::Timeout`] 为真**：其余七类要么重发必然再失败
    /// （输入错、数据缺、档位不够、没权限），要么必须先去解决冲突、查存储或报 bug。
    /// 这条口径是构造性的——"只有瞬时故障可以原地重试"是应用层唯一站得住的默认。
    pub const fn safe_to_retry(self) -> bool {
        matches!(self.retry(), AppRetry::SafeNow)
    }
}

impl fmt::Display for AppErrorCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 领域码 → 应用层类别的**唯一**转换表（方案 §15.5「错误契约唯一转换链」的中间那一跳）。
///
/// 逐条理由（为什么不是别的类别）：
///
/// | 领域码 | 类别 | 理由 |
/// |---|---|---|
/// | `Transient` | `Timeout` | 网络抖动/限流：调用方稍后原样再发就是正解 |
/// | `Permanent` | `InvalidInput` | 不可重试的请求级拒绝；调用方唯一能做的是改请求 |
/// | `Ambiguous` | `Conflict` | 结果未知——先解决"这笔到底成没成"，绝不能直接重发 |
/// | `VenueState` | `Timeout` | 停牌/维护/熔断：等场所恢复再发 |
/// | `ReconcileRequired` | `Conflict` | 本地与外部事实冲突，先对账 |
/// | `ResourceExhausted` | `Timeout` | 配额/削峰：退避后重来 |
/// | `BusinessViolation` | `InvalidInput` | 超限/自成交/保证金不足：改参数或改状态 |
/// | `Invariant` | `InternalInvariant` | 账不平/状态机非法迁移：这是 bug |
///
/// `Permanent` 落到 `InvalidInput` 而不是 `PermissionDenied` 是**刻意的**：领域码把
/// "参数非法"和"权限不足"合并成一格，应用层拿不到区分它们的信息，而编一个区分
/// 等于按消息文本猜——那正是本仓明令禁止的分支方式。宁可给一个诚实的"改请求"，
/// 也不给一个可能错的"去申请权限"。
pub const fn category_of_domain_code(code: ErrorCode) -> AppErrorCategory {
    match code {
        ErrorCode::Transient => AppErrorCategory::Timeout,
        ErrorCode::Permanent => AppErrorCategory::InvalidInput,
        ErrorCode::Ambiguous => AppErrorCategory::Conflict,
        ErrorCode::VenueState => AppErrorCategory::Timeout,
        ErrorCode::ReconcileRequired => AppErrorCategory::Conflict,
        ErrorCode::ResourceExhausted => AppErrorCategory::Timeout,
        ErrorCode::BusinessViolation => AppErrorCategory::InvalidInput,
        ErrorCode::Invariant => AppErrorCategory::InternalInvariant,
    }
}

/// 应用层错误：类别 + 展示层消息 + 可定位的 correlation id + 底层诊断码。
///
/// `action` / `retry` / `safe_to_retry` **不存字段**，从类别派生（单源）——存字段就会有人
/// 造出「类别是 INVALID_INPUT 但 action 是 RETRY_LATER」这种自相矛盾的实例。
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct AppError {
    category: AppErrorCategory,
    message: String,
    correlation_id: Option<String>,
    /// 底层诊断证据（领域码 `TRANSIENT` 那类，或 `io:NotFound` 那类）。**不参与分支**。
    source_code: Option<String>,
}

impl AppError {
    /// 构造一条应用层错误。`message` 是展示层（中文），不参与分支。
    pub fn new(category: AppErrorCategory, message: impl Into<String>) -> Self {
        Self {
            category,
            message: message.into(),
            correlation_id: None,
            source_code: None,
        }
    }

    /// 挂上可定位的 run/request id。用例边界必须在**返回之前**把它挂上，
    /// 否则用户拿到的错误无从与日志、产物对齐（G6 要的是"一次故障四层同一个 id"）。
    pub fn with_correlation_id(mut self, correlation_id: impl Into<String>) -> Self {
        self.correlation_id = Some(correlation_id.into());
        self
    }

    /// 记下底层诊断码（领域码或 `io:<kind>`）。它只进 JSON 的 `source_code` 一格，
    /// 供排障与告警关联；**调用方不要按它分支**——分支按 [`AppError::category`]。
    pub fn with_source_code(mut self, source_code: impl Into<String>) -> Self {
        self.source_code = Some(source_code.into());
        self
    }

    /// 领域错误 → 应用层错误（方案 §15.5 那条转换链的中间一跳）。
    ///
    /// 类别走 [`category_of_domain_code`] 这张唯一表；底层码原样保留在 `source_code`，
    /// 所以"应用层说了什么"与"领域层为什么"两件事都能被读到。
    pub fn from_qx_error(error: &QxError) -> Self {
        let contract = error.contract();
        Self::new(
            category_of_domain_code(contract.code),
            contract.user_message.to_string(),
        )
        .with_source_code(contract.code.as_str())
    }

    /// `std::io::Error` → 应用层错误。文件读写的四类落点各不相同，**不许一律算存储故障**：
    /// "文件不在"是数据问题（去取数据），"没权限"是授权问题（去申请），
    /// "已存在"是冲突（先解决），其余才是存储故障。
    pub fn from_io(context: &str, error: &std::io::Error) -> Self {
        let category = match error.kind() {
            std::io::ErrorKind::NotFound => AppErrorCategory::DataUnavailable,
            std::io::ErrorKind::PermissionDenied => AppErrorCategory::PermissionDenied,
            std::io::ErrorKind::AlreadyExists => AppErrorCategory::Conflict,
            std::io::ErrorKind::TimedOut => AppErrorCategory::Timeout,
            _ => AppErrorCategory::StorageFailure,
        };
        Self::new(category, format!("{context}: {error}"))
            .with_source_code(format!("io:{:?}", error.kind()))
    }

    /// 类别（机器可读）。调用方按它分支。
    pub fn category(&self) -> AppErrorCategory {
        self.category
    }

    /// 用户行动提示（从类别派生）。
    pub fn action(&self) -> AppAction {
        self.category().action()
    }

    /// 重试档（从类别派生）。
    pub fn retry(&self) -> AppRetry {
        self.category().retry()
    }

    /// 是否安全原地重试（从重试档派生）。
    pub fn safe_to_retry(&self) -> bool {
        self.category().safe_to_retry()
    }

    /// 可定位的 run/request id（没挂就是 `None`，**不**编一个假的）。
    pub fn correlation_id(&self) -> Option<&str> {
        self.correlation_id.as_deref()
    }

    /// 底层诊断码（没挂就是 `None`）。
    pub fn source_code(&self) -> Option<&str> {
        self.source_code.as_deref()
    }

    /// 展示层消息（中文）。给人和日志看，**不要**按它分支。
    pub fn message(&self) -> &str {
        &self.message
    }

    /// 稳定 JSON 形态。三个入口（CLI / Python / HTTP）报错时都走它，
    /// 所以同一场失败在三处逐字节可比——这是 G1「错误 code 相同」的载体。
    pub fn to_json(&self) -> String {
        serde_json::json!({
            "category": self.category().as_str(),
            "action": self.action().as_str(),
            "retry": self.retry().as_str(),
            "safe_to_retry": self.safe_to_retry(),
            "correlation_id": self.correlation_id(),
            "source_code": self.source_code(),
            "message": self.message(),
        })
        .to_string()
    }
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.correlation_id() {
            Some(id) => write!(
                f,
                "[{} · {}] id={id} {}",
                self.category(),
                self.action(),
                self.message()
            ),
            None => write!(
                f,
                "[{} · {}] {}",
                self.category(),
                self.action(),
                self.message()
            ),
        }
    }
}

impl std::error::Error for AppError {}
