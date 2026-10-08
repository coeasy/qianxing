//! 错误分类：错误类型决定调用方能不能重试、要不要对账、该不该告警。
//!
//! 最关键的是 [`QxError::Ambiguous`]——订单提交超时是量化系统最危险的状态。
//! 绝大多数事故不是"失败了"，而是"不知道成功还是失败"。
//!
//! ## 五元错误契约（DD-5 / P1-11）
//!
//! 错误跨 crate 传递时**只认** [`ErrorContract`] 的五个字段：机器可读
//! [`ErrorCode`]、[`Retryability`]、`reconcile_required`、`safe_to_retry`、以及展示层
//! `user_message`。在此之前，同一个问题（"这条错误能不能重试、要不要先对账"）在各调用方
//! 就地 `match` 变体各写一份，而 `code()` 只是把变体名翻成字符串；现在映射表
//! **只有** [`QxError::contract`] 一份，`code()` 也从它派生。
//!
//! 调用方按 `code` 分支，**永远不要** `match` 消息文本——消息是中文展示层，会随文案漂移。

use serde::{Deserialize, Serialize};
use std::fmt;

/// 机器可读错误码（DD-5 / P1-11）：从"错误跨 crate 主要靠字符串"里抽出来的那个分类。
///
/// **闭集**：一个 [`QxError`] 变体一个码。别处不许自造同形字符串码——要么进这个枚举，
/// 要么它根本不是"错误分类"而是 HTTP/工作流层面的状态（那类留在各自的读面里）。
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum ErrorCode {
    /// 可重试：网络抖动、限流、临时不可用。
    Transient,
    /// 不可重试：参数非法、权限不足、业务规则拒绝。
    Permanent,
    /// 结果未知：超时、连接中断。必须进入 reconcile。
    Ambiguous,
    /// 场所状态：停牌、维护、熔断。应降级或熔断。
    VenueState,
    /// 本地与外部事实需要重新对账，禁止继续自动下单。
    ReconcileRequired,
    /// 资源配额：需要退避或削峰。
    ResourceExhausted,
    /// 违反业务规则：超限、自成交、保证金不足。
    BusinessViolation,
    /// 内部一致性错误：记账不平、状态机非法迁移。必须告警。
    Invariant,
}

impl ErrorCode {
    /// 全部码，顺序与枚举一致。遍历它才能保证"新增变体时映射表不会漏一格"。
    pub const ALL: [ErrorCode; 8] = [
        ErrorCode::Transient,
        ErrorCode::Permanent,
        ErrorCode::Ambiguous,
        ErrorCode::VenueState,
        ErrorCode::ReconcileRequired,
        ErrorCode::ResourceExhausted,
        ErrorCode::BusinessViolation,
        ErrorCode::Invariant,
    ];

    /// 稳定字符串形态（`SCREAMING_SNAKE_CASE`）。序列化、日志、CLI 输出都取它。
    pub const fn as_str(self) -> &'static str {
        match self {
            ErrorCode::Transient => "TRANSIENT",
            ErrorCode::Permanent => "PERMANENT",
            ErrorCode::Ambiguous => "AMBIGUOUS",
            ErrorCode::VenueState => "VENUE_STATE",
            ErrorCode::ReconcileRequired => "RECONCILE_REQUIRED",
            ErrorCode::ResourceExhausted => "RESOURCE_EXHAUSTED",
            ErrorCode::BusinessViolation => "BUSINESS_VIOLATION",
            ErrorCode::Invariant => "INVARIANT",
        }
    }

    /// 由稳定字符串形态还原；未知码返回 `None`（**不**猜测）。
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|code| code.as_str() == value)
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for ErrorCode {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ErrorCode {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).ok_or_else(|| serde::de::Error::custom(format!("未知错误码 {raw}")))
    }
}

/// 重试资格分类（DD-5 五元里的 `retryability`）。
///
/// 刻意**不是布尔**：本仓已有一处明确口径——"是否重试由 [`crate::retry::RetryPolicy`]
/// 按预算裁决"（见 [`QxError::code`] 的历史注释）。这里只给**类别**，次数/退避仍由策略定；
/// 给一个裸布尔等于让每个调用方各抄一份"能不能重试"。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Retryability {
    /// 不允许重试。
    Never,
    /// 允许立即重试：请求可判定地没有产生副作用（网络抖动、限流拒绝）。
    Allowed,
    /// 允许重试，但**必须先对账**：结果未知，盲目重发可能重复下单。
    AfterReconcile,
    /// 允许重试，但要先降级/退避：场所维护、资源配额。
    AfterBackoff,
}

impl Retryability {
    /// 是否**无条件**允许立即再发起一次尝试。
    ///
    /// 只有 [`Retryability::Allowed`] 为真：`AfterReconcile` 要先对账、`AfterBackoff`
    /// 要先降级，两者都不是"直接重发"。热路径的重试循环用这个谓词，避免把
    /// "结果未知"当成"可以直接重发"。
    pub const fn allows_retry(self) -> bool {
        matches!(self, Retryability::Allowed)
    }

    /// 这类错误是否**原则上**有重试的可能（含需对账/需退避两种有条件的）。
    pub const fn may_retry_eventually(self) -> bool {
        !matches!(self, Retryability::Never)
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Retryability::Never => "NEVER",
            Retryability::Allowed => "ALLOWED",
            Retryability::AfterReconcile => "AFTER_RECONCILE",
            Retryability::AfterBackoff => "AFTER_BACKOFF",
        }
    }
}

/// 错误码五元契约（DD-5 / P1-11）：错误跨 crate 传递时只认这五个字段。
///
/// `user_message` 是展示层（中文），`code` 是机器可读层——调用方按码分支，
/// 永远不要 `match` 消息文本。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ErrorContract<'a> {
    /// 机器可读错误码。
    pub code: ErrorCode,
    /// 重试资格分类（不是布尔：见 [`Retryability`]）。
    pub retryability: Retryability,
    /// 是否必须进入对账（`Ambiguous` / `ReconcileRequired` 为真）。
    pub reconcile_required: bool,
    /// 是否**安全**再发起一次同样的请求：与"能不能重试"不是一回事。
    /// 结果未知的错误即使可重试也不安全（可能已经成交）。
    pub safe_to_retry: bool,
    /// 展示层消息（中文）。给人和日志看，不参与分支。
    pub user_message: &'a str,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum QxError {
    /// 可重试：网络抖动、限流、临时不可用。
    Transient(String),
    /// 不可重试：参数非法、权限不足、业务规则拒绝。
    Permanent(String),
    /// 结果未知：超时、连接中断。**必须进入 reconcile，绝不能假设成功或失败。**
    Ambiguous(String),
    /// 场所状态：停牌、维护、熔断。应降级或熔断。
    VenueState(String),
    /// 本地与外部事实需要重新对账，禁止继续自动下单。
    ReconcileRequired(String),
    /// 资源配额：需要退避或削峰，不能当作风控拒绝重试。
    ResourceExhausted(String),
    /// 违反业务规则：超限、自成交、保证金不足。
    BusinessViolation(String),
    /// 内部一致性错误：记账不平、状态机非法迁移。必须告警。
    Invariant(String),
}

impl QxError {
    /// 这条错误的展示层消息（中文）。给人和日志看，**不要**按它分支。
    pub fn message(&self) -> &str {
        match self {
            QxError::Transient(message)
            | QxError::Permanent(message)
            | QxError::Ambiguous(message)
            | QxError::VenueState(message)
            | QxError::ReconcileRequired(message)
            | QxError::ResourceExhausted(message)
            | QxError::BusinessViolation(message)
            | QxError::Invariant(message) => message,
        }
    }

    /// 错误码五元契约（DD-5 / P1-11）。
    ///
    /// 这是全仓**唯一**一份「变体 → 码 / 重试资格 / 对账 / 安全重试」映射表：调用方要
    /// 判断"能不能重试、要不要对账"就取这里，不要再 `match` 变体各写一份。
    /// 映射依据逐条来自各变体的文档注释（`Transient` 是网络抖动、`Ambiguous` 是结果未知…）。
    pub fn contract(&self) -> ErrorContract<'_> {
        let code = match self {
            QxError::Transient(_) => ErrorCode::Transient,
            QxError::Permanent(_) => ErrorCode::Permanent,
            QxError::Ambiguous(_) => ErrorCode::Ambiguous,
            QxError::VenueState(_) => ErrorCode::VenueState,
            QxError::ReconcileRequired(_) => ErrorCode::ReconcileRequired,
            QxError::ResourceExhausted(_) => ErrorCode::ResourceExhausted,
            QxError::BusinessViolation(_) => ErrorCode::BusinessViolation,
            QxError::Invariant(_) => ErrorCode::Invariant,
        };
        let (retryability, reconcile_required, safe_to_retry) = match code {
            ErrorCode::Transient => (Retryability::Allowed, false, true),
            ErrorCode::Permanent => (Retryability::Never, false, false),
            ErrorCode::Ambiguous => (Retryability::AfterReconcile, true, false),
            ErrorCode::VenueState => (Retryability::AfterBackoff, false, true),
            ErrorCode::ReconcileRequired => (Retryability::Never, true, false),
            ErrorCode::ResourceExhausted => (Retryability::AfterBackoff, false, true),
            ErrorCode::BusinessViolation => (Retryability::Never, false, false),
            ErrorCode::Invariant => (Retryability::Never, false, false),
        };
        ErrorContract {
            code,
            retryability,
            reconcile_required,
            safe_to_retry,
            user_message: self.message(),
        }
    }

    /// 错误的机器可读分类。是否重试由 [`crate::retry::RetryPolicy`] 按预算裁决，
    /// 不在此处再给一个布尔口径；码本身从 [`QxError::contract`] 派生（单源）。
    pub fn code(&self) -> &'static str {
        self.contract().code.as_str()
    }
}

impl fmt::Display for QxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}", self.code(), self.message())
    }
}

impl std::error::Error for QxError {}

pub type QxResult<T> = Result<T, QxError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_variant_has_one_machine_readable_code() {
        // 「能不能重试」由 [`crate::retry::RetryPolicy`] 按预算裁决，错误类型只对外
        // 暴露这一个分类字典；重复的布尔口径会在调用方各自漂移。
        assert_eq!(QxError::Transient("t".into()).code(), "TRANSIENT");
        assert_eq!(QxError::Ambiguous("t".into()).code(), "AMBIGUOUS");
        assert_eq!(
            QxError::ReconcileRequired("r".into()).code(),
            "RECONCILE_REQUIRED"
        );
        assert_eq!(QxError::Invariant("i".into()).code(), "INVARIANT");
        assert_eq!(
            QxError::Ambiguous("timeout".into()).to_string(),
            "[AMBIGUOUS] timeout"
        );
    }

    #[test]
    fn code_and_contract_come_from_one_mapping_table() {
        // 每个码都要能从字符串还原（序列化/日志/CLI 输出的都是它）。
        for code in ErrorCode::ALL {
            assert_eq!(ErrorCode::parse(code.as_str()), Some(code));
            assert_eq!(code.to_string(), code.as_str());
        }
        assert_eq!(ErrorCode::parse("NOT_A_CODE"), None);
    }

    #[test]
    fn contract_separates_retryable_from_safe_to_retry() {
        // 结果未知：**可重试但不安全**——盲目重发可能重复下单。这是五元契约存在的理由。
        let unknown = QxError::Ambiguous("timeout".into());
        let ambiguous = unknown.contract();
        assert_eq!(ambiguous.retryability, Retryability::AfterReconcile);
        assert!(ambiguous.retryability.may_retry_eventually());
        assert!(!ambiguous.retryability.allows_retry());
        assert!(ambiguous.reconcile_required);
        assert!(!ambiguous.safe_to_retry);

        // 网络抖动：可以立即重试，也安全。
        let jitter = QxError::Transient("jitter".into());
        let transient = jitter.contract();
        assert!(transient.retryability.allows_retry());
        assert!(!transient.reconcile_required);
        assert!(transient.safe_to_retry);

        // 业务拒绝：既不重试也不对账。
        let limit = QxError::BusinessViolation("limit".into());
        let rejected = limit.contract();
        assert_eq!(rejected.retryability, Retryability::Never);
        assert!(!rejected.safe_to_retry);

        // 展示层消息原样带出来。
        assert_eq!(ambiguous.user_message, "timeout");
    }
}
