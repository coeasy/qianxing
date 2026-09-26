//! 摘要落盘前的重放闸门（V11 Q62）。
//!
//! 旧口径往产物里写一个 `replay_hash`，而它是把同一段事件切片**再哈希一遍**得来的，
//! 与 `result_hash` 恒等、不可能失败——写在产物里等于声明了一条永远不会红的校验。
//! 现在摘要只在事实源真的重新驱动出同一本账簿之后才落盘，并把重放做过的三件事
//! （重新接受、事件条数、账簿条数）写成读者各自可核对的字段。

use super::*;

/// 一份健康的事实源：两条入金事实 + 与之逐条对应的运行账簿。
fn healthy_fact_source() -> (qx_core::EventLog, qx_core::Ledger) {
    let mut ledger = qx_core::Ledger::new();
    let first = ledger
        .deposit("main", "USD", Money::from_i64(100), 1)
        .unwrap();
    let second = ledger
        .deposit("main", "USD", Money::from_i64(50), 2)
        .unwrap();
    let mut log = qx_core::EventLog::new();
    for id in [first, second] {
        let entry = ledger
            .entries()
            .iter()
            .find(|item| item.id == id)
            .unwrap()
            .clone();
        let seq = log.alloc_seq();
        log.append(qx_core::Event::new(
            seq,
            entry.ts,
            qx_core::Priority::APPLY,
            qx_core::EventKind::LedgerApplied { entry },
        ));
    }
    (log, ledger)
}

/// 重放闸门只关心事实源，输入身份在这里是占位；它会落进摘要的 `input` 块，
/// 由 `backtest_input_provenance` 那组用例按真实文件核对。
fn fixture_input() -> crate::backtests::BacktestInputProvenance {
    crate::backtests::BacktestInputProvenance {
        kind: "barframe",
        path: "q62-fixture-frame.json".into(),
        dataset_id: "strategy-bars:BTC/USDT.BINANCE".into(),
        dataset_version: crate::backtests::BARFRAME_DATASET_VERSION.into(),
        fingerprint: format!("{:016x}", 7u64),
    }
}

/// 本金同样只是占位；它落进摘要的 `account` 块，由 `backtest_account_base` 那组用例核对。
fn account_fixture() -> crate::backtests::BacktestAccountBase {
    crate::backtests::backtest_initial_cash(None).unwrap()
}

/// 用给定事实源拼一份产物输入；除事实源外全是占位口径。
fn artifact_input<'a>(
    instrument: &'a InstrumentId,
    event_log: &'a qx_core::EventLog,
    ledger: &'a qx_core::Ledger,
    result_hash: u64,
) -> crate::backtests::BacktestArtifacts<'a> {
    crate::backtests::BacktestArtifacts {
        strategy_id: "q62-replay-gate",
        instrument,
        sample_unit: "bar",
        samples: 2,
        sample_ts: &[1, 2],
        equity: &[100, 150],
        positions: &[0, 0],
        fills: &[],
        clock_start: 1,
        clock_end: 2,
        input_data_hash: 1,
        result_hash,
        event_log,
        ledger,
        return_bps: 50,
        max_drawdown_bps: 0,
        fees_raw: 0,
        turnover_raw: 0,
        final_equity_raw: 150,
        assumptions: &[],
        model_descriptors: &[],
        risk_rule_set_version: "test",
        risk_rule_source: "conservative-default",
        cost_source: "builtin-default",
        fill_model: None,
        // 重放闸门只关心事实源；本金在这里是占位，由 `backtest_account_base` 那组用例按真实
        // 声明核对（V11 Q72）。
        account_base: crate::backtests::backtest_initial_cash(None).unwrap(),
        matching_kernel: "test",
        rejections: &[],
        input: fixture_input(),
        // 信号口径同理是占位：这里没有内置策略上场，写 None 摘要就不落这个键（V12 R4-j）。
        signal: None,
    }
}

/// 健康事实源：摘要必须把重放真正做过的三件事写出来，且旧的恒等键不得复活。
#[test]
fn summary_publishes_the_replay_facts_it_actually_checked() {
    let (log, ledger) = healthy_fact_source();
    let instrument = InstrumentId::parse("BTC/USDT.BINANCE").unwrap();
    let root = temp_cli_case_dir("q62-healthy");
    let (summary_path, _, _) = crate::backtests::persist_backtest_artifacts(
        &root.join("backtest.run.json"),
        &artifact_input(&instrument, &log, &ledger, log.digest()),
    )
    .unwrap();
    let summary: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&summary_path).unwrap()).unwrap();
    assert_eq!(summary["schema_version"], serde_json::json!(4));
    // 摘要必须把期初本金与它的来源一起落盘：只有期末权益的产物算不出收益率的分母。
    assert_eq!(
        summary["account"],
        serde_json::json!({
            "initial_cash_raw": account_fixture().cash.raw().to_string(),
            "source": crate::backtests::BACKTEST_ACCOUNT_BASE_DEFAULT_SOURCE,
        })
    );
    // 摘要必须把递进来的输入身份逐字段写出来：不落盘等于产物对"跑的哪份数据"仍然沉默。
    assert_eq!(
        summary["input"],
        serde_json::json!({
            "kind": "barframe",
            "path": "q62-fixture-frame.json",
            "dataset_id": "strategy-bars:BTC/USDT.BINANCE",
            "dataset_version": crate::backtests::BARFRAME_DATASET_VERSION,
            "fingerprint": format!("{:016x}", 7u64),
        })
    );
    assert_eq!(summary["replay"]["events"], serde_json::json!(2));
    assert_eq!(summary["replay"]["ledger_entries"], serde_json::json!(2));
    assert_eq!(
        summary["replay"]["run_ledger_entries"],
        serde_json::json!(ledger.entries().len())
    );
    assert_eq!(
        summary["replay"]["log_digest"],
        serde_json::json!(format!("{:016x}", log.digest()))
    );
    assert_eq!(summary["result_hash"], summary["replay"]["log_digest"]);
    assert!(
        summary.get("replay_hash").is_none(),
        "恒等的 `replay_hash` 键必须被重放结论取代"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// 反向证据①：运行账簿比事实源多记一条（就地改了账户却没发事实事件）→ 拒绝落盘。
///
/// 反向验证口径：摘掉 `persist_backtest_artifacts` 里的 `ReplayVerifier::verify`，
/// 这条用例立刻拿到 `Ok` 并断言失败；只删事实事件那种"再哈希一遍"式校验对此无感。
#[test]
fn artifacts_refuse_to_land_when_a_ledger_entry_has_no_fact_event() {
    let (log, mut ledger) = healthy_fact_source();
    ledger
        .deposit("main", "USD", Money::from_i64(7), 3)
        .unwrap();
    let root = temp_cli_case_dir("q62-unbacked");
    let instrument = InstrumentId::parse("BTC/USDT.BINANCE").unwrap();
    let error = crate::backtests::persist_backtest_artifacts(
        &root.join("backtest.run.json"),
        &artifact_input(&instrument, &log, &ledger, log.digest()),
    )
    .unwrap_err();
    assert!(
        error.contains("未通过重放校验") && error.contains("replayed=2"),
        "错误应指明重放口径并给出两侧条数: {error}"
    );
    assert!(
        !root.join("backtest.summary.json").exists(),
        "拒落盘不得留下任何摘要工件"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// 反向证据②：事实流不是规范日志（同一时间戳上优先级倒退）→ 拒绝落盘，
/// 并且错误要指出卡在哪一条事件——深度链此前正是这种形状而无人发现。
#[test]
fn artifacts_refuse_to_land_when_the_fact_stream_is_not_canonically_ordered() {
    let (mut log, ledger) = healthy_fact_source();
    let last = log.events().last().unwrap().clone();
    let seq = log.alloc_seq();
    // 与上一条同时戳、优先级却倒退的事实：重放按规范序重新接受，这里必须被拒。
    log.append(qx_core::Event::new(
        seq,
        last.ts,
        last.prio.saturating_sub(1),
        qx_core::EventKind::Settle,
    ));
    let root = temp_cli_case_dir("q62-disordered");
    let instrument = InstrumentId::parse("BTC/USDT.BINANCE").unwrap();
    let error = crate::backtests::persist_backtest_artifacts(
        &root.join("backtest.run.json"),
        &artifact_input(&instrument, &log, &ledger, log.digest()),
    )
    .unwrap_err();
    assert!(
        error.contains("未通过重放校验") && error.contains("事件重放在 seq="),
        "错误应带上卡住的那条事件: {error}"
    );
    let _ = std::fs::remove_dir_all(root);
}
