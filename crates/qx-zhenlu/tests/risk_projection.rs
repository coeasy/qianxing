use qx_core::{
    InstrumentId, MarginMode, Order, OrderPolicy, OrderStatus, PositionMode, PositionSide, Price,
    Quantity, Side, TradingInstrumentSpec, TradingProduct, SCALE,
};
use qx_risk::{MaxNotionalRule, OrderRiskContext, OrderRiskPosition, RiskRule, RuleSet};
use qx_zhenlu::{RiskContext, RiskGate};

fn perpetual_spec() -> TradingInstrumentSpec {
    TradingInstrumentSpec {
        instrument: InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
        product: TradingProduct::Perpetual,
        base_currency: "BTC".into(),
        quote_currency: "USDT".into(),
        settlement_currency: "USDT".into(),
        contract_size: SCALE,
        linear: true,
        inverse: false,
        price_tick: 1,
        qty_step: 1,
        min_qty: 1,
        max_leverage: 20,
        maintenance_margin_bps: 500,
        valid_from: 1,
        valid_to: None,
    }
}

fn order(side: Side, qty: i128, reduce_only: bool) -> Order {
    Order {
        client_id: 1,
        instrument: InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
        side,
        qty: Quantity::from_raw(qty),
        limit: Some(Price::from_raw(100 * SCALE)),
        status: OrderStatus::PendingSubmit,
        filled: Quantity::ZERO,
        account_id: "main".into(),
        trace: None,
        policy: Some(OrderPolicy {
            reduce_only,
            position_side: PositionSide::Net,
            margin_mode: MarginMode::Cross,
            position_mode: PositionMode::OneWay,
            leverage: 10,
            post_only: false,
        }),
    }
}

fn hedge_order(side: Side, qty: i128, position_side: PositionSide) -> Order {
    let mut order = order(side, qty, true);
    order.policy = Some(OrderPolicy {
        reduce_only: true,
        position_side,
        margin_mode: MarginMode::Cross,
        position_mode: PositionMode::Hedge,
        leverage: 10,
        post_only: false,
    });
    order
}

#[test]
fn reduce_only_close_uses_projected_exposure_instead_of_gross_plus_order() {
    let context = RiskContext {
        available_margin_raw: Some(0),
        reference_price: Some(Price::from_raw(100 * SCALE)),
        instrument_spec: Some(perpetual_spec()),
        max_order_notional_raw: None,
        max_position_notional_raw: Some(150 * SCALE),
    };
    let position = OrderRiskPosition::new(2 * SCALE, 200 * SCALE);

    context
        .validate_order(&order(Side::Sell, SCALE, true), &position)
        .expect("a risk-reducing close must be allowed when projected exposure is below the cap");
}

#[test]
fn reduce_only_cannot_increase_or_flip_a_one_way_position() {
    let context = RiskContext {
        available_margin_raw: Some(0),
        reference_price: Some(Price::from_raw(100 * SCALE)),
        instrument_spec: Some(perpetual_spec()),
        max_order_notional_raw: None,
        max_position_notional_raw: Some(500 * SCALE),
    };
    let position = OrderRiskPosition::new(2 * SCALE, 200 * SCALE);

    assert!(context
        .validate_order(&order(Side::Buy, SCALE, true), &position)
        .is_err());
    assert!(context
        .validate_order(&order(Side::Sell, 3 * SCALE, true), &position)
        .is_err());
}

#[test]
fn hedge_reduce_only_uses_the_selected_leg_and_cannot_cross_zero() {
    let context = RiskContext {
        available_margin_raw: Some(0),
        reference_price: Some(Price::from_raw(100 * SCALE)),
        instrument_spec: Some(perpetual_spec()),
        max_order_notional_raw: None,
        max_position_notional_raw: Some(500 * SCALE),
    };
    let position = OrderRiskPosition::new_with_multiplier(0, 400 * SCALE, SCALE)
        .with_hedge_legs(2 * SCALE, -2 * SCALE);

    context
        .validate_order(
            &hedge_order(Side::Sell, SCALE, PositionSide::Long),
            &position,
        )
        .expect("selling a long hedge leg must reduce risk");
    context
        .validate_order(
            &hedge_order(Side::Buy, SCALE, PositionSide::Short),
            &position,
        )
        .expect("buying a short hedge leg must reduce risk");
    assert!(context
        .validate_order(
            &hedge_order(Side::Buy, SCALE, PositionSide::Long),
            &position,
        )
        .is_err());
    assert!(context
        .validate_order(
            &hedge_order(Side::Sell, 3 * SCALE, PositionSide::Long),
            &position,
        )
        .is_err());
}

#[test]
fn empty_risk_gate_still_enforces_reduce_only_account_invariant() {
    let gate = RiskGate::from_rule_set(RuleSet::account_limits_only());
    let position = OrderRiskPosition::new_with_multiplier(0, 400 * SCALE, SCALE)
        .with_hedge_legs(2 * SCALE, -2 * SCALE);

    gate.check_with_price(
        &hedge_order(Side::Sell, SCALE, PositionSide::Long),
        &position,
        Some(Price::from_raw(100 * SCALE)),
    )
    .expect("RiskGate must allow a valid reduce-only close");
    assert!(gate
        .check_with_price(
            &hedge_order(Side::Buy, SCALE, PositionSide::Long),
            &position,
            Some(Price::from_raw(100 * SCALE)),
        )
        .is_err());
}

#[test]
fn max_notional_rule_uses_projected_exposure() {
    // 规则链已上收到 `qx-risk::RuleSet`：名义额折算只走 `OrderRiskContext` 一份
    // 实现，因此规则的输入也必须是规范上下文而不是旧的持仓快照形状。
    let rule = MaxNotionalRule {
        max_notional: 150 * SCALE,
    };
    let context = OrderRiskContext {
        reference_price: Some(Price::from_raw(100 * SCALE)),
        position: OrderRiskPosition {
            net_qty: 2 * SCALE,
            gross_notional: 200 * SCALE,
            multiplier: 1,
            long_qty: 0,
            short_qty: 0,
        },
        ..OrderRiskContext::default()
    };
    rule.check(&context, &order(Side::Sell, SCALE, false))
        .expect("RuleSet semantics must agree with projected exposure");
    // 同一份投影在反向加仓时必须超限。
    assert!(rule
        .check(&context, &order(Side::Buy, 2 * SCALE, false))
        .is_err());
}

#[test]
fn risk_gate_wrapper_delegates_to_rule_set() {
    // deprecated compat：`RiskGate` 只保留旧调用形状，判定来自 `RuleSet`。
    let mut gate = RiskGate::from_rule_set(RuleSet::account_limits_only());
    gate.add(Box::new(MaxNotionalRule {
        max_notional: 150 * SCALE,
    }));
    let position = OrderRiskPosition::new(2 * SCALE, 200 * SCALE);
    gate.check_with_price(
        &order(Side::Sell, SCALE, false),
        &position,
        Some(Price::from_raw(100 * SCALE)),
    )
    .expect("RiskGate wrapper must agree with RuleSet");
    assert!(gate
        .check_with_price(
            &order(Side::Buy, 2 * SCALE, false),
            &position,
            Some(Price::from_raw(100 * SCALE)),
        )
        .is_err());
}
