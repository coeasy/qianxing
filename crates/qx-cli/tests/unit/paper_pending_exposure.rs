use super::*;
use qx_risk::{MaxNotionalRule, RuleSet};

#[test]
fn paper_position_limit_reserves_same_account_nonterminal_orders() {
    let root = temp_cli_case_dir("paper-pending-exposure");
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let spec =
        super::paper_margin_valuation::margin_valuation_spec(&instrument, TradingProduct::Spot);
    let spec_path = root.join("spot.spec.json");
    std::fs::write(&spec_path, serde_json::to_vec(&spec).unwrap()).unwrap();
    let mut worker = mk_worker(
        "paper-execution",
        WorkerRole::Execution,
        "paper",
        Some(&spec_path.to_string_lossy()),
    );
    worker.max_position_notional_raw = Some(150 * SCALE);

    let mut pipeline = LiveEventPipeline::open(&root, paper_account_log(), "USDT").unwrap();
    let mut pending = mk_order(9_801, &instrument, Side::Buy, 1);
    pending.limit = Some(Price::from_i64(100));
    pipeline.register_order(pending, 1).unwrap();
    let mut other_account = mk_order(9_803, &instrument, Side::Buy, 1);
    other_account.account_id = "other-account".into();
    other_account.limit = Some(Price::from_i64(500));
    pipeline.register_order(other_account, 1).unwrap();

    let mut candidate = mk_order(9_802, &instrument, Side::Buy, 1);
    candidate.limit = Some(Price::from_i64(100));
    let (_, position) = worker_risk_context(&worker, &candidate, &pipeline, None)
        .unwrap()
        .expect("配置了产品规格的 Paper worker 必须构造风险上下文");

    assert_eq!(
        position.gross_notional,
        100 * SCALE,
        "只计入本账户未成交订单；其他账户的 500 名义额必须隔离"
    );
    let mut risk = RiskGate::from_rule_set(RuleSet::account_limits_only());
    risk.add(Box::new(MaxNotionalRule {
        max_notional: 150 * SCALE,
    }));
    assert!(
        risk.check_with_spec(&candidate, &position, candidate.limit, Some(&spec))
            .is_err(),
        "已有 100 名义额在途订单后，再提交 100 应超过 150 限额"
    );

    assert!(
        risk.check_with_spec(
            &candidate,
            &OrderRiskPosition::default(),
            candidate.limit,
            Some(&spec)
        )
        .is_ok(),
        "没有在途订单时，本单 100 名义额应处于 150 限额内"
    );
}
