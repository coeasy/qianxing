//! CCXT 一轮对账的"发现 → 三面"口径（V11 Q69 / 交易链路 TX3）。
//!
//! 远端-only 与本地-only 两半发现必须同时进对账报告与 worker 健康判定，能对上本地
//! 订单的还要各落一条 `ReconcileRequired`；对不上本地订单的远端孤单不得凭空造句柄。
use super::*;

#[test]
fn ccxt_reconcile_round_routes_both_discovery_halves_to_report_and_fact_stream() {
    let instrument = InstrumentId::parse("BTC/USDT.OKX").unwrap();
    let mut terminal = mk_order(7, &instrument, Side::Buy, 1);
    terminal.status = OrderStatus::Filled;
    let local_orders = vec![terminal, mk_order(8, &instrument, Side::Sell, 1)];
    let known_remote_orders = BTreeMap::from([("remote-known".into(), (7, OrderStatus::Filled))]);
    let value = serde_json::json!({
        "orders": [
            {"order_id": "remote-known", "client_order_id": "7", "symbol": "BTC/USDT", "status": "open"},
            {"order_id": "remote-unmapped", "client_order_id": "8", "symbol": "BTC/USDT", "status": "open"},
            {"order_id": "remote-unknown", "client_order_id": "", "symbol": "ETH/USDT", "status": "open"},
            // 交易所给了本地从未有过的客户号：它是字符串，不是本地订单句柄。
            {"order_id": "remote-foreign", "client_order_id": "99999", "symbol": "ETH/USDT", "status": "open"}
        ]
    });
    let remote =
        ccxt_open_order_issues(&value, &local_orders, &known_remote_orders, "okx").unwrap();
    assert_eq!(remote.len(), 4);
    let local = vec![ccxt_local_order_issue(
        "local_order_missing_remote_id",
        &local_orders[1],
        "okx",
        "CCXT 对账缺少远端订单号",
    )];

    let round = ccxt_reconcile_round(&remote, &local);
    // 报告一面：两半发现都要在，缺任何一半都是"报告说没问题、事实流在喊待对账"。
    assert_eq!(round.order_issues.len(), 5);
    assert_eq!(round.order_issues[3]["kind"], "unknown_remote_open_order");
    assert_eq!(
        round.order_issues[4]["kind"],
        "local_order_missing_remote_id"
    );
    // 事实流一面：只有能对上本地订单的发现才落 ReconcileRequired。
    let facts = round
        .require_reconcile
        .iter()
        .map(|fact| (fact.client_order_id, fact.event_tag))
        .collect::<Vec<_>>();
    assert_eq!(
        facts,
        vec![
            (7, "remote-terminal"),
            (8, "remote-unmapped"),
            (8, "missing-remote")
        ]
    );
    assert!(round
        .require_reconcile
        .iter()
        .all(|fact| !fact.reason.trim().is_empty()));
    assert_eq!(round.require_reconcile[2].reason, "CCXT 对账缺少远端订单号");
}

/// 对账进程重启不得让它自己的余额事实看起来像重复（V11 R2）：seq 从日志尾端接上、
/// correlation 带本轮时间戳，两者由 `venue_balance_fact_identity` 单点构造，两条对账链共用。
#[test]
fn balance_facts_stay_unique_across_a_reconciler_restart() {
    let root = temp_cli_case_dir("reconcile-fact-identity");
    let balance = |usdt: i64| RuntimeExternalEvent::AccountBalanceSnapshot {
        account_id: "main".into(),
        venue_id: "okx".into(),
        balances: vec![AccountBalance {
            asset: "USDT".into(),
            free: Money::from_i64(usdt),
            locked: Money::ZERO,
            borrowed: Money::ZERO,
        }],
    };
    let log_name = "ccxt-main-okx-events";
    let mut before = LiveEventPipeline::open(&root, log_name, "USDT").unwrap();
    // 一轮只有一个身份：重试沿用同一份 (seq, correlation)，才不会被记成"另一条新事实"。
    let round_fact = |seq: u64, correlation: String| {
        RuntimeEventEnvelope::venue(balance(10), 1_000, 1_000, seq, correlation)
    };
    let (seq, correlation) = venue_balance_fact_identity(&before, "ccxt-reconciler", 1_000);
    before.ingest(round_fact(seq, correlation.clone())).unwrap();
    assert!(
        before
            .ingest(round_fact(seq, correlation))
            .unwrap()
            .deduplicated,
        "同一轮重投必须仍被去重"
    );
    drop(before);
    // 重启后的下一轮：新句柄、新时间戳，余额事实得是新的一格。
    let mut after_restart = LiveEventPipeline::open(&root, log_name, "USDT").unwrap();
    let (next_seq, next_correlation) =
        venue_balance_fact_identity(&after_restart, "ccxt-reconciler", 1_001);
    assert!(
        !after_restart
            .ingest(RuntimeEventEnvelope::venue(
                balance(11),
                1_001,
                1_001,
                next_seq,
                next_correlation
            ))
            .unwrap()
            .deduplicated,
        "重启后的余额事实被当成已应用的旧事实静默吞掉了"
    );
    assert_eq!(after_restart.log().events().len(), 2);

    // 修复前的形状（seq 每进程从 0 重来、correlation 由 seq 拼出）必须真的会被吞掉——
    // 它不成立就说明上面那条断言问不出问题。
    let legacy_root = root.join("legacy");
    std::fs::create_dir_all(&legacy_root).unwrap();
    let mut legacy_before = LiveEventPipeline::open(&legacy_root, log_name, "USDT").unwrap();
    legacy_before
        .ingest(RuntimeEventEnvelope::venue(
            balance(10),
            1_000,
            1_000,
            1,
            "ccxt-reconciler:balances:1",
        ))
        .unwrap();
    drop(legacy_before);
    let mut legacy_restart = LiveEventPipeline::open(&legacy_root, log_name, "USDT").unwrap();
    assert!(
        legacy_restart
            .ingest(RuntimeEventEnvelope::venue(
                balance(11),
                1_001,
                1_001,
                1,
                "ccxt-reconciler:balances:1"
            ))
            .unwrap()
            .deduplicated,
        "旧形状必须仍会被去重，否则这一族缺陷根本没被这条用例描述"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// 健康结论必须由两半发现共同决定（V11 Q69）。写在 worker 循环里时这一条只能靠真实运行
/// 触发，`HEAD` 因此在"只有一张本地单子查不到远端结果"的那一轮继续报 `Ready`。
#[test]
fn ccxt_reconcile_service_status_degrades_on_a_local_only_finding() {
    let instrument = InstrumentId::parse("BTC/USDT.OKX").unwrap();
    let local_only = ccxt_local_order_issue(
        "local_order_missing_remote_id",
        &mk_order(9, &instrument, Side::Buy, 1),
        "okx",
        "CCXT 对账缺少远端订单号",
    );
    let round = ccxt_reconcile_round(&[], &[local_only]);
    assert_eq!(round.order_issues.len(), 1);
    assert_eq!(
        ccxt_reconcile_service_status(&round, &[]),
        qx_runtime::ServiceStatus::Degraded
    );

    let discrepancy = RuntimeBalanceDiscrepancy {
        account_id: "main".into(),
        venue_id: "okx".into(),
        asset: "USDT".into(),
        ledger_raw: 0,
        venue_raw: qx_core::Money::from_i64(10).raw(),
    };
    let clean = ccxt_reconcile_round(&[], &[]);
    assert_eq!(
        ccxt_reconcile_service_status(&clean, &[discrepancy]),
        qx_runtime::ServiceStatus::Degraded
    );
    assert_eq!(
        ccxt_reconcile_service_status(&clean, &[]),
        qx_runtime::ServiceStatus::Ready
    );
}
