use qx_core::{
    InstrumentId, MarginMode, Order, OrderPolicy, OrderStatus, PositionMode, PositionSide, Price,
    Quantity, Side, TradingInstrumentSpec, TradingProduct, SCALE,
};
use qx_zhenlu::{MaxNotionalRule, PositionSnapshot, RiskContext, RiskRule};

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

#[test]
fn reduce_only_close_uses_projected_exposure_instead_of_gross_plus_order() {
    let context = RiskContext {
        available_margin_raw: Some(0),
        reference_price: Some(Price::from_raw(100 * SCALE)),
        instrument_spec: Some(perpetual_spec()),
        max_order_notional_raw: None,
        max_position_notional_raw: Some(150 * SCALE),
    };
    let position = PositionSnapshot::new(2 * SCALE, 200 * SCALE);

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
    let position = PositionSnapshot::new(2 * SCALE, 200 * SCALE);

    assert!(context
        .validate_order(&order(Side::Buy, SCALE, true), &position)
        .is_err());
    assert!(context
        .validate_order(&order(Side::Sell, 3 * SCALE, true), &position)
        .is_err());
}

#[test]
fn legacy_max_notional_rule_also_uses_projected_exposure() {
    let rule = MaxNotionalRule {
        max_notional: 150 * SCALE,
    };
    let position = PositionSnapshot::new(2 * SCALE, 200 * SCALE);
    rule.check_with_price(
        &order(Side::Sell, SCALE, false),
        &position,
        Some(Price::from_raw(100 * SCALE)),
    )
    .expect("legacy RiskGate semantics must agree with projected exposure");
}
