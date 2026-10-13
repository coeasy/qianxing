use super::*;

#[test]
fn derivative_backtest_uses_spec_pnl_instead_of_full_notional_cashflow() {
    let instrument = InstrumentId::parse("BTC/USDT:USDT.SIM").unwrap();
    let mut config = simple_config();
    config.instrument = instrument.clone();
    config.currency = "usdt".into();
    config.instrument_spec = Some(TradingInstrumentSpec {
        instrument,
        product: qx_core::TradingProduct::Perpetual,
        base_currency: "BTC".into(),
        quote_currency: "USDT".into(),
        settlement_currency: "USDT".into(),
        contract_size: qx_core::SCALE,
        linear: true,
        inverse: false,
        price_tick: 1,
        qty_step: 1,
        min_qty: 1,
        max_leverage: 100,
        maintenance_margin_bps: 500,
        valid_from: 1,
        valid_to: None,
    });
    let bars = vec![
        Bar::new(
            1,
            100 * SCALE,
            101 * SCALE,
            99 * SCALE,
            100 * SCALE,
            10 * SCALE,
        ),
        Bar::new(
            2,
            102 * SCALE,
            103 * SCALE,
            101 * SCALE,
            102 * SCALE,
            10 * SCALE,
        ),
        Bar::new(
            3,
            110 * SCALE,
            111 * SCALE,
            109 * SCALE,
            110 * SCALE,
            10 * SCALE,
        ),
    ];
    let report = BacktestEngine::new(config)
        .run(&bars, &mut BuyOnce { done: false })
        .unwrap();
    assert_eq!(report.fills.len(), 1);
    assert_eq!(report.final_equity(), Money::from_i64(1008).raw());
    assert_eq!(
        report.ledger.cash_for("main", "USDT"),
        Money::from_i64(1000).raw()
    );
}

#[test]
fn market_cash_currency_mismatch_is_rejected_before_strategy_execution() {
    let instrument = InstrumentId::parse("BTC/USDT:USDT.SIM").unwrap();
    let mut config = simple_config();
    config.instrument = instrument.clone();
    config.currency = "USD".into();
    config.instrument_spec = Some(TradingInstrumentSpec {
        instrument,
        product: qx_core::TradingProduct::Perpetual,
        base_currency: "BTC".into(),
        quote_currency: "USDT".into(),
        settlement_currency: "USDT".into(),
        contract_size: qx_core::SCALE,
        linear: true,
        inverse: false,
        price_tick: 1,
        qty_step: 1,
        min_qty: 1,
        max_leverage: 100,
        maintenance_margin_bps: 500,
        valid_from: 1,
        valid_to: None,
    });
    let bars = [
        Bar::new(1, 100, 100, 100, 100, 10),
        Bar::new(2, 101, 101, 101, 101, 10),
        Bar::new(3, 102, 102, 102, 102, 10),
    ];
    let mut strategy = BuyOnce { done: false };
    let error = BacktestEngine::new(config)
        .run(&bars, &mut strategy)
        .err()
        .expect("cash and settlement currency mismatch must fail closed");
    assert!(matches!(error, qx_core::QxError::BusinessViolation(_)));
    assert!(
        !strategy.done,
        "configuration mismatch must reject before strategy callbacks"
    );
}
