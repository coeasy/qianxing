//! 错误码五元契约（DD-5 / P1-11）在 **qx-cli 消费面** 的行为证明。
//!
//! `qx-core` 里那几条用例只证明契约自身自洽；这里证明它真的被用起来：
//! ① 每个变体的五格与契约逐格相等；② CLI 展示层把「码 / 能不能重试 / 要不要先对账」
//! 翻成人话；③ 打开 EventLog 这条真正的 `QxError` 边界产出的诊断文本里**码一定在**——
//! 改前它是 `format!("…: {error:?}")`，落进日志的只有一句中文加 Rust 的 Debug 形状，
//! 机器读不出版本、也读不出该不该重发。

use super::*;
use qx_core::{ErrorCode, QxError, Retryability};

/// 每个变体的五元契约逐格核对。
///
/// 这张表**不是**第二份映射表：它逐个变体调 `contract()` 再与期望比。若有人把
/// `QxError::contract` 里的某一格改错（例如把 `Ambiguous` 的 `safe_to_retry` 置真），
/// 这里当场红——而"结果未知却以为可以安全重发"正是五元契约存在的理由。
#[test]
fn every_variant_exposes_the_five_tuple_contract() {
    let cases: [(QxError, ErrorCode, Retryability, bool, bool); 8] = [
        (
            QxError::Transient("网络抖动".into()),
            ErrorCode::Transient,
            Retryability::Allowed,
            false,
            true,
        ),
        (
            QxError::Permanent("参数非法".into()),
            ErrorCode::Permanent,
            Retryability::Never,
            false,
            false,
        ),
        (
            QxError::Ambiguous("提交超时".into()),
            ErrorCode::Ambiguous,
            Retryability::AfterReconcile,
            true,
            false,
        ),
        (
            QxError::VenueState("停牌".into()),
            ErrorCode::VenueState,
            Retryability::AfterBackoff,
            false,
            true,
        ),
        (
            QxError::ReconcileRequired("账实不符".into()),
            ErrorCode::ReconcileRequired,
            Retryability::Never,
            true,
            false,
        ),
        (
            QxError::ResourceExhausted("配额耗尽".into()),
            ErrorCode::ResourceExhausted,
            Retryability::AfterBackoff,
            false,
            true,
        ),
        (
            QxError::BusinessViolation("超限".into()),
            ErrorCode::BusinessViolation,
            Retryability::Never,
            false,
            false,
        ),
        (
            QxError::Invariant("记账不平".into()),
            ErrorCode::Invariant,
            Retryability::Never,
            false,
            false,
        ),
    ];
    for (error, code, retryability, reconcile_required, safe_to_retry) in cases {
        let contract = error.contract();
        assert_eq!(contract.code, code, "码不符：{error}");
        assert_eq!(contract.retryability, retryability, "重试资格不符：{error}");
        assert_eq!(
            contract.reconcile_required, reconcile_required,
            "对账要求不符：{error}"
        );
        assert_eq!(
            contract.safe_to_retry, safe_to_retry,
            "安全重试不符：{error}"
        );
        // 展示层消息原样带出，且 `code()` 从同一张表派生（不留第二份 match）。
        assert_eq!(contract.user_message, error.message(), "{error}");
        assert_eq!(error.code(), code.as_str(), "{error}");
    }
}

/// CLI 展示层把契约的两格翻成人话，且码一定在开头。
#[test]
fn cli_diagnostics_carry_the_code_and_the_next_step() {
    let unknown = QxError::Ambiguous("提交超时，结果未知".into());
    let text = usage_errors::qx_context("打开账户 EventLog 失败", &unknown);
    assert!(text.starts_with("[AMBIGUOUS] "), "缺机器可读码: {text}");
    assert!(text.contains("打开账户 EventLog 失败"), "缺上下文: {text}");
    assert!(text.contains("需先对账"), "结果未知却没提示对账: {text}");
    assert!(
        !text.contains("可重试"),
        "结果未知不得提示可直接重试: {text}"
    );

    let jitter = QxError::Transient("连接被重置".into());
    let text = usage_errors::qx_context("打开账户 EventLog 失败", &jitter);
    assert!(text.starts_with("[TRANSIENT] "), "缺机器可读码: {text}");
    assert!(text.contains("可重试"), "可重试错误却没提示: {text}");
    assert!(!text.contains("需先对账"), "可重试错误不该要求对账: {text}");

    // 有条件的重试（需先退避）与"可直接重试"是两档：混成一档等于让调用方立刻重发。
    let halted = QxError::VenueState("场所维护中".into());
    let text = usage_errors::qx_context("打开账户 EventLog 失败", &halted);
    assert!(text.starts_with("[VENUE_STATE] "), "缺机器可读码: {text}");
    assert!(text.contains("需退避后重试"), "缺退避提示: {text}");
    assert!(
        !text.contains("可重试"),
        "退避后才可重试不得说成可重试: {text}"
    );

    // 业务拒绝：既不可重试也不需对账，两格提示都不该出现（不然调用方会去重发）。
    let rejected = QxError::BusinessViolation("超出名义额上限".into());
    let text = usage_errors::qx_context("下单被拒", &rejected);
    assert!(
        text.starts_with("[BUSINESS_VIOLATION] "),
        "缺机器可读码: {text}"
    );
    assert!(!text.contains("可重试"), "{text}");
    assert!(!text.contains("需先对账"), "{text}");
}

/// 真正的 `QxError` 边界：打开 EventLog 被闸门拒绝时，CLI 交给调用方的文本里码在盘。
///
/// 这是「错误跨 crate 主要靠字符串」的原始症状所在——`PipelineStorage::open_with` 拿到的
/// 是 `QxError`，改前用 `format!("…: {error:?}")` 把它摊成自由文本。若有人把它改回去，
/// 本用例当场红（码不再出现在串首）。
#[test]
fn event_log_boundary_diagnostics_carry_the_machine_readable_code() {
    let root = temp_cli_case_dir("error-code-contract");
    let data_dir = root.join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let log_name = paper_account_log();

    // 先在文件后端写下一件事实：换到数据库后端时闸门才有东西可拒。
    let mut pipeline = LiveEventPipeline::open(&data_dir, &log_name, "USDT").unwrap();
    pipeline
        .ingest(RuntimeEventEnvelope::venue(
            RuntimeExternalEvent::AccountCashflow {
                cashflow: AccountCashflow {
                    account_id: "main".into(),
                    venue_id: "paper".into(),
                    currency: "USDT".into(),
                    kind: CashflowKind::Transfer,
                    amount: Money::from_i64(1_000),
                    external_id: "seed".into(),
                },
            },
            1,
            1,
            1,
            "seed",
        ))
        .unwrap();
    drop(pipeline);

    let mut config = paper_runtime_config(&data_dir);
    config.storage.backend = StorageBackend::Sqlite;
    config.storage.sqlite_path = Some(
        data_dir
            .join("qx-events.sqlite")
            .to_string_lossy()
            .into_owned(),
    );
    let error = match PipelineStorage::from_config(&config)
        .unwrap()
        .open(log_name.clone(), "USDT")
    {
        Ok(_) => panic!("换后端绕过了闸门：账户会从空账本起步并从 seq 0 另起一本"),
        Err(error) => error,
    };
    assert!(
        error.starts_with("[PERMANENT] "),
        "CLI 诊断丢了机器可读码（退回自由文本）: {error}"
    );
    assert!(
        error.contains("后端切换被拒绝") && error.contains(&format!("{log_name}.json")),
        "诊断必须点名拒绝原因与被留下的历史: {error}"
    );
    assert!(
        !error.contains("可重试"),
        "不可重试的错误不得被标成可重试: {error}"
    );
    let _ = std::fs::remove_dir_all(root);
}
