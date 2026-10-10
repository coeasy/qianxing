//! T2-0 的边界用例：错误类别、映射表、panic 边界、能力闸。
//!
//! 这一族守的是"调用方能不能只按类别分支"这件事。它比"枚举有八个变体"严得多——
//! 变体齐不等于**语义**齐：只要有一处映射写成别的类别，调用方的分支就会把它送错下一步。

use crate::context::{CallerCapability, RunContext};
use crate::error::{category_of_domain_code, AppAction, AppError, AppErrorCategory, AppRetry};
use qx_core::{ErrorCode, QxError};
use std::collections::BTreeSet;

/// 路线图 §X1 逐条列出的八类。**手抄一份是刻意的**：从枚举自己派生这份清单，
/// "少一类"就永远不会红——而这条用例存在的理由正是"少一类/多一类要有人管"。
const ROADMAP_CATEGORIES: [&str; 8] = [
    "InvalidInput",
    "DataUnavailable",
    "FidelityInsufficient",
    "PermissionDenied",
    "Conflict",
    "Timeout",
    "StorageFailure",
    "InternalInvariant",
];

#[test]
fn every_category_has_a_stable_string_and_a_distinct_action() {
    let mut names = BTreeSet::new();
    let mut actions = BTreeSet::new();
    for name in ROADMAP_CATEGORIES {
        let category = category_by_name(name);
        assert!(
            names.insert(category.as_str()),
            "{} 的稳定串与别的类别撞了",
            name
        );
        assert!(
            actions.insert(category.action().as_str()),
            "{} 的行动提示与别的类别撞了——两类给同一句行动指引，调用方就没法靠它决定下一步",
            name
        );
    }
    assert_eq!(names.len(), 8, "八类的稳定串必须是八个不同的值");
    assert_eq!(actions.len(), 8, "八类的行动提示必须是八个不同的值");
}

#[test]
fn only_timeout_is_safe_to_retry_and_it_is_derived_from_the_retry_table() {
    for name in ROADMAP_CATEGORIES {
        let category = category_by_name(name);
        let expected = category.retry() == AppRetry::SafeNow;
        assert_eq!(
            category.safe_to_retry(),
            expected,
            "{name} 的 safe_to_retry 与它的重试档不一致——两个口径分了家，就会有人按其中一个写重试循环"
        );
        assert_eq!(
            category.safe_to_retry(),
            name == "Timeout",
            "{name} 的 safe_to_retry 不是「只有 Timeout 为真」"
        );
    }
}

#[test]
fn every_domain_code_maps_to_a_category_and_never_softens_retry_safety() {
    let mut seen = BTreeSet::new();
    for code in ErrorCode::ALL {
        let domain = domain_error(code);
        let contract = domain.contract();
        assert_eq!(
            contract.code, code,
            "辅助函数 domain_error 造出的实例与循环变量对不上——契约的码必须与领域码一致，\
             否则下面几条断言核的是别的错误"
        );
        let category = category_of_domain_code(code);
        seen.insert(category.as_str());
        let app = AppError::from_qx_error(&domain);
        assert_eq!(app.category(), category, "{code:?} 的类别与映射表不一致");
        assert_eq!(
            app.source_code(),
            Some(code.as_str()),
            "{code:?} 的底层诊断码没有保留——应用层说了什么与领域层为什么必须都能读到"
        );
        assert!(
            !app.safe_to_retry() || contract.safe_to_retry,
            "{code:?}：应用层把领域层判为「不安全重发」的错误说成可以原地重发，\
             这条不等式一旦破掉，调用方会重复下单"
        );
        assert_eq!(
            app.message(),
            contract.user_message,
            "{code:?} 的展示层消息必须原样带出来，不许在应用层改写领域事实"
        );
    }
    assert_eq!(
        seen.len(),
        4,
        "领域八码只映射到 4 个应用层类别是**当前**的事实（Transient/VenueState/ResourceExhausted 都落 \
         Timeout、Permanent/BusinessViolation 都落 InvalidInput、Ambiguous/ReconcileRequired 都落 Conflict、\
         Invariant 落 InternalInvariant）；这张表变了就说明映射被改过，\
         改的人必须同时想清楚那几类调用方下一步是否真的相同"
    );
}

/// 每个领域码造一个代表实例。写成 `match` 而不是"从码名拼变体"：`QxError` 新增变体时
/// 这里编译不过，用例不会静默漏掉新码。上面那条 `contract().code == code` 的断言反过来
/// 校验这个辅助函数与领域码一一对应。
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
fn io_errors_are_classified_by_kind_not_lumped_into_storage_failure() {
    let cases = [
        (
            std::io::ErrorKind::NotFound,
            AppErrorCategory::DataUnavailable,
            AppAction::ProvideData,
        ),
        (
            std::io::ErrorKind::PermissionDenied,
            AppErrorCategory::PermissionDenied,
            AppAction::RequestPermission,
        ),
        (
            std::io::ErrorKind::AlreadyExists,
            AppErrorCategory::Conflict,
            AppAction::ResolveConflict,
        ),
        (
            std::io::ErrorKind::TimedOut,
            AppErrorCategory::Timeout,
            AppAction::RetryLater,
        ),
        (
            std::io::ErrorKind::WriteZero,
            AppErrorCategory::StorageFailure,
            AppAction::CheckStorage,
        ),
    ];
    for (kind, category, action) in cases {
        let error = AppError::from_io("读取产物", &std::io::Error::from(kind));
        assert_eq!(error.category(), category, "{kind:?} 的类别不对");
        assert_eq!(error.action(), action, "{kind:?} 的行动提示不对");
        assert_eq!(
            error.source_code(),
            Some(format!("io:{kind:?}").as_str()),
            "{kind:?} 的底层诊断码没有保留"
        );
    }
}

#[test]
fn app_error_json_carries_every_field_the_four_entrypoints_compare() {
    let error = AppError::new(AppErrorCategory::Conflict, "同一 run_id 已有不同内容")
        .with_correlation_id("run-42")
        .with_source_code("AMBIGUOUS");
    let value: serde_json::Value =
        serde_json::from_str(&error.to_json()).expect("错误 JSON 可解析");
    let keys = value
        .as_object()
        .expect("错误 JSON 是对象")
        .keys()
        .cloned()
        .collect::<BTreeSet<_>>();
    let expected = [
        "action",
        "category",
        "correlation_id",
        "message",
        "retry",
        "safe_to_retry",
        "source_code",
    ]
    .into_iter()
    .map(str::to_string)
    .collect::<BTreeSet<_>>();
    assert_eq!(
        keys, expected,
        "错误 JSON 的键集就是三个入口逐字节比对的载体，加键删键都要同时改三侧"
    );
    assert_eq!(value["category"], "CONFLICT");
    assert_eq!(value["action"], "RESOLVE_CONFLICT");
    assert_eq!(value["retry"], "AFTER_RECONCILE");
    assert_eq!(value["safe_to_retry"], false);
    assert_eq!(value["correlation_id"], "run-42");
}

#[test]
fn an_unset_correlation_id_is_null_and_never_an_invented_placeholder() {
    let error = AppError::new(AppErrorCategory::InvalidInput, "缺字段");
    assert_eq!(error.correlation_id(), None);
    let value: serde_json::Value =
        serde_json::from_str(&error.to_json()).expect("错误 JSON 可解析");
    assert!(
        value["correlation_id"].is_null(),
        "没挂 id 就必须是 null——编一个占位串会让「这一层没有 id」与「这一层有 id」长得一样"
    );
    assert!(value["source_code"].is_null());
}

#[test]
fn panic_at_the_use_case_boundary_becomes_an_internal_invariant_with_the_run_id() {
    let error = crate::cases::guard::guard_panics("run-7", || -> Result<(), AppError> {
        panic!("下游 unwrap 炸了")
    })
    .expect_err("panic 必须被翻成错误");
    assert_eq!(error.category(), AppErrorCategory::InternalInvariant);
    assert_eq!(error.action(), AppAction::ReportBug);
    assert!(!error.safe_to_retry(), "内部不一致绝不可以说「重发就好」");
    assert_eq!(error.correlation_id(), Some("run-7"));
    assert_eq!(error.source_code(), Some("panic"));
    assert!(
        error.message().contains("下游 unwrap 炸了"),
        "panic 载荷的文本必须带出来，否则排障的人失去唯一线索：{}",
        error.message()
    );
}

#[test]
fn a_non_string_panic_payload_is_reported_without_inventing_a_message() {
    let error = crate::cases::guard::guard_panics("run-8", || -> Result<(), AppError> {
        std::panic::panic_any(42_u32)
    })
    .expect_err("panic 必须被翻成错误");
    assert_eq!(error.category(), AppErrorCategory::InternalInvariant);
    assert!(
        error.message().contains("非字符串 panic 载荷"),
        "非字符串载荷没有稳定文本，只能说清楚这一点，不许编一个：{}",
        error.message()
    );
}

#[test]
fn the_capability_ladder_covers_lower_ranks_and_denies_higher_ones() {
    let research = RunContext::new(CallerCapability::Research, "run-9");
    assert!(research
        .require(CallerCapability::Research, "数据集校验")
        .is_ok());
    let denied = research
        .require(CallerCapability::Paper, "Paper 启动")
        .expect_err("研究档不许启动 Paper");
    assert_eq!(denied.category(), AppErrorCategory::PermissionDenied);
    assert_eq!(denied.action(), AppAction::RequestPermission);
    assert!(!denied.safe_to_retry(), "没权限时重发必然再失败");

    let live = RunContext::new(CallerCapability::Live, "run-10");
    assert!(
        live.require(CallerCapability::Research, "Bar 回测").is_ok(),
        "高档必须覆盖低档：能连真实账户的人当然也能跑一次回测"
    );
    assert!(live.require(CallerCapability::Live, "实盘下单").is_ok());
    for capability in [
        CallerCapability::Research,
        CallerCapability::Paper,
        CallerCapability::Operator,
        CallerCapability::Live,
    ] {
        assert!(
            capability.allows(capability),
            "{} 档必须覆盖自己",
            capability.as_str()
        );
    }
}

#[test]
fn the_context_carries_the_callers_build_identity_into_the_manifest() {
    let default = RunContext::new(CallerCapability::Research, "run-11");
    assert_eq!(
        default.code_commit(),
        crate::context::APP_DEFAULT_CODE_COMMIT
    );
    let overridden = RunContext::new(CallerCapability::Research, "run-11")
        .with_code_commit("0123456789abcdef0123456789abcdef01234567");
    assert_eq!(
        overridden.code_commit(),
        "0123456789abcdef0123456789abcdef01234567",
        "门面必须能把自己的构建身份带进来，否则产物里的 code_commit 只是一个包版本"
    );
    assert_eq!(overridden.correlation_id(), "run-11");
    assert_eq!(overridden.capability(), CallerCapability::Research);
}

/// 按名字取类别。写成一个 `match` 而不是数组下标：`ROADMAP_CATEGORIES` 与枚举之间
/// 少一个变体时，这条用例在**编译期**就红，而不是运行时数错个数。
fn category_by_name(name: &str) -> AppErrorCategory {
    match name {
        "InvalidInput" => AppErrorCategory::InvalidInput,
        "DataUnavailable" => AppErrorCategory::DataUnavailable,
        "FidelityInsufficient" => AppErrorCategory::FidelityInsufficient,
        "PermissionDenied" => AppErrorCategory::PermissionDenied,
        "Conflict" => AppErrorCategory::Conflict,
        "Timeout" => AppErrorCategory::Timeout,
        "StorageFailure" => AppErrorCategory::StorageFailure,
        "InternalInvariant" => AppErrorCategory::InternalInvariant,
        other => panic!("路线图 §X1 里的类别 {other} 在本枚举里不存在"),
    }
}
