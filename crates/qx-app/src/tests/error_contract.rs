//! 只需 crate 内入口的错误映射与 panic 边界用例。

use crate::cases::guard::guard_panics;
use crate::error::{category_of_domain_code, AppAction, AppError, AppErrorCategory};
use qx_core::{ErrorCode, QxError};
use std::collections::BTreeSet;

#[test]
fn every_domain_code_maps_to_a_category_and_never_softens_retry_safety() {
    let mut seen = BTreeSet::new();
    for code in ErrorCode::ALL {
        let domain = domain_error(code);
        let contract = domain.contract();
        assert_eq!(contract.code, code);
        let category = category_of_domain_code(code);
        seen.insert(category.as_str());
        let app = AppError::from_qx_error(&domain);
        assert_eq!(app.category(), category);
        assert_eq!(app.source_code(), Some(code.as_str()));
        assert!(!app.safe_to_retry() || contract.safe_to_retry);
        assert_eq!(app.message(), contract.user_message);
    }
    assert_eq!(
        seen.len(),
        4,
        "领域八码当前归并为四类；类别映射变动时必须重新审视调用方分支"
    );
}

/// 写成穷尽 match，新增领域错误码时用例必须在编译期要求补齐。
fn domain_error(code: ErrorCode) -> QxError {
    let message = format!("{} 的领域诊断", code.as_str());
    match code {
        ErrorCode::Transient => QxError::Transient(message),
        ErrorCode::Permanent => QxError::Permanent(message),
        ErrorCode::Ambiguous => QxError::Ambiguous(message),
        ErrorCode::VenueState => QxError::VenueState(message),
        ErrorCode::ReconcileRequired => QxError::ReconcileRequired(message),
        ErrorCode::ResourceExhausted => QxError::ResourceExhausted(message),
        ErrorCode::BusinessViolation => QxError::BusinessViolation(message),
        ErrorCode::Invariant => QxError::Invariant(message),
    }
}

#[test]
fn panic_at_the_use_case_boundary_becomes_an_internal_invariant_with_the_run_id() {
    let error = guard_panics("run-7", || -> Result<(), AppError> {
        panic!("下游 unwrap 炸了")
    })
    .expect_err("panic 必须被翻成错误");
    assert_eq!(error.category(), AppErrorCategory::InternalInvariant);
    assert_eq!(error.action(), AppAction::ReportBug);
    assert!(!error.safe_to_retry());
    assert_eq!(error.correlation_id(), Some("run-7"));
    assert_eq!(error.source_code(), Some("panic"));
    assert!(error.message().contains("下游 unwrap 炸了"));
}

#[test]
fn a_non_string_panic_payload_is_reported_without_inventing_a_message() {
    let error = guard_panics("run-8", || -> Result<(), AppError> {
        std::panic::panic_any(42_u32)
    })
    .expect_err("panic 必须被翻成错误");
    assert_eq!(error.category(), AppErrorCategory::InternalInvariant);
    assert!(error.message().contains("非字符串 panic 载荷"));
}
