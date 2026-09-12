//! 错误分类：错误类型决定调用方能不能重试、要不要对账、该不该告警。
//!
//! 最关键的是 [`QxError::Ambiguous`]——订单提交超时是量化系统最危险的状态。
//! 绝大多数事故不是"失败了"，而是"不知道成功还是失败"。

use std::fmt;

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
    /// 是否允许自动重试。
    pub fn retryable(&self) -> bool {
        matches!(self, QxError::Transient(_))
    }

    /// 是否需要对账。
    pub fn needs_reconcile(&self) -> bool {
        matches!(self, QxError::Ambiguous(_) | QxError::ReconcileRequired(_))
    }

    pub fn code(&self) -> &'static str {
        match self {
            QxError::Transient(_) => "TRANSIENT",
            QxError::Permanent(_) => "PERMANENT",
            QxError::Ambiguous(_) => "AMBIGUOUS",
            QxError::VenueState(_) => "VENUE_STATE",
            QxError::ReconcileRequired(_) => "RECONCILE_REQUIRED",
            QxError::ResourceExhausted(_) => "RESOURCE_EXHAUSTED",
            QxError::BusinessViolation(_) => "BUSINESS_VIOLATION",
            QxError::Invariant(_) => "INVARIANT",
        }
    }
}

impl fmt::Display for QxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            QxError::Transient(s)
            | QxError::Permanent(s)
            | QxError::Ambiguous(s)
            | QxError::VenueState(s)
            | QxError::ReconcileRequired(s)
            | QxError::ResourceExhausted(s)
            | QxError::BusinessViolation(s)
            | QxError::Invariant(s) => write!(f, "[{}] {}", self.code(), s),
        }
    }
}

impl std::error::Error for QxError {}

pub type QxResult<T> = Result<T, QxError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_transient_is_retryable() {
        assert!(QxError::Transient("t".into()).retryable());
        assert!(!QxError::Ambiguous("t".into()).retryable());
        assert!(QxError::Ambiguous("t".into()).needs_reconcile());
    }
}
