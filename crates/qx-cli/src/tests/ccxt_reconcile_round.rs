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
