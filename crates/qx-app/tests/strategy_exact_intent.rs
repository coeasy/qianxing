use qx_app::{build_strategy_order_from_intent, StrategyOrderBindings, StrategyOrderInput};
use qx_core::{
    ExactDecimal, ExactTradingRules, InstrumentId, MarginMode, PositionMode, TradingInstrumentSpec,
    TradingProduct, SCALE,
};

fn spec() -> TradingInstrumentSpec {
    TradingInstrumentSpec {
        instrument: InstrumentId::parse("PEPEUSDT.BINANCE").unwrap(),
        product: TradingProduct::Spot,
        base_currency: "PEPE".into(),
        quote_currency: "USDT".into(),
        settlement_currency: "USDT".into(),
        contract_size: SCALE,
        linear: false,
        inverse: false,
        price_tick: 1,
        qty_step: 1,
        min_qty: 1,
        min_notional_raw: 0,
        max_leverage: 1,
        maintenance_margin_bps: 0,
        valid_from: 0,
        valid_to: None,
        exact_rules: Some(ExactTradingRules {
            contract_size: ExactDecimal::parse("1").unwrap(),
            price_tick: ExactDecimal::parse("0.00000001").unwrap(),
            quantity_step: ExactDecimal::parse("0.0000000001").unwrap(),
            minimum_quantity: ExactDecimal::parse("0.0000000001").unwrap(),
            minimum_notional: ExactDecimal::ZERO,
        }),
    }
}

fn input() -> StrategyOrderInput {
    StrategyOrderInput {
        intent_id: 7,
        instrument: "PEPEUSDT.BINANCE".into(),
        side: "buy".into(),
        qty_raw: 0,
        qty_exact: Some("1e-10".into()),
        quote_budget_exact: None,
        limit_price_raw: None,
        limit_price_exact: Some("0.00000005".into()),
        reduce_only: false,
        post_only: false,
        position_side: None,
        margin_mode: None,
        position_mode: None,
        leverage: None,
    }
}

#[test]
fn exact_strategy_intent_becomes_scale_aware_order_after_frozen_rule_validation() {
    let spec = spec();
    let intent = input();
    let built = build_strategy_order_from_intent(
        &intent,
        &StrategyOrderBindings {
            account_id: "account-1",
            strategy_id: "pepe-maker",
            signal_id: 9,
            round_scope: "run-1",
            rule_version: "v1",
            product: Some(TradingProduct::Spot),
            position_mode: Some(PositionMode::OneWay),
            margin_mode: Some(MarginMode::Cash),
            leverage: Some(1),
            market_spec: Some(&spec),
        },
    )
    .unwrap();

    let exact = built.order.exact_amounts.unwrap();
    assert_eq!(built.order.qty.raw(), 0);
    assert_eq!(built.order.limit, None);
    assert_eq!(exact.quantity, ExactDecimal::parse("0.0000000001").unwrap());
    assert_eq!(
        exact.limit_price,
        Some(ExactDecimal::parse("0.00000005").unwrap())
    );
}

#[test]
fn bitget_quote_budget_intent_builds_a_distinct_exact_market_order() {
    let mut spec = spec();
    spec.instrument = InstrumentId::parse("PEPE/USDT.BITGET").unwrap();
    spec.base_currency = "PEPE".into();
    spec.quote_currency = "USDT".into();
    spec.settlement_currency = "USDT".into();
    spec.exact_rules.as_mut().unwrap().minimum_notional = ExactDecimal::parse("5").unwrap();
    let intent = StrategyOrderInput {
        intent_id: 12,
        instrument: "PEPE/USDT.BITGET".into(),
        side: "buy".into(),
        qty_raw: 0,
        qty_exact: None,
        quote_budget_exact: Some("12.34000001".into()),
        limit_price_raw: None,
        limit_price_exact: None,
        reduce_only: false,
        post_only: false,
        position_side: None,
        margin_mode: None,
        position_mode: None,
        leverage: None,
    };
    let built = build_strategy_order_from_intent(
        &intent,
        &StrategyOrderBindings {
            account_id: "bitget-account",
            strategy_id: "budget-buy",
            signal_id: 2,
            round_scope: "run-1",
            rule_version: "v1",
            product: Some(TradingProduct::Spot),
            position_mode: Some(PositionMode::OneWay),
            margin_mode: Some(MarginMode::Cash),
            leverage: Some(1),
            market_spec: Some(&spec),
        },
    )
    .unwrap();
    let order = built.order;
    order.validate().unwrap();
    let amounts = order.exact_amounts.unwrap();
    assert_eq!(amounts.quantity, ExactDecimal::ZERO);
    assert_eq!(
        amounts.quote_budget,
        Some(ExactDecimal::parse("12.34000001").unwrap())
    );
    assert_eq!(order.limit_price_decimal(), None);
}

#[test]
fn exact_strategy_intent_requires_market_rules_and_rejects_off_step_values() {
    let spec = spec();
    let intent = input();
    let mut missing_spec = StrategyOrderBindings {
        account_id: "account-1",
        strategy_id: "pepe-maker",
        signal_id: 9,
        round_scope: "run-1",
        rule_version: "v1",
        product: Some(TradingProduct::Spot),
        position_mode: Some(PositionMode::OneWay),
        margin_mode: Some(MarginMode::Cash),
        leverage: Some(1),
        market_spec: None,
    };
    assert!(build_strategy_order_from_intent(&intent, &missing_spec).is_err());

    missing_spec.market_spec = Some(&spec);
    let mut off_step = intent;
    off_step.qty_exact = Some("0.00000000015".into());
    assert!(build_strategy_order_from_intent(&off_step, &missing_spec).is_err());
}
