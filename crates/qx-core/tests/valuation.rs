//! 估值单点（P1-6 / WP-19）的行为用例。
//!
//! 收敛的意义在于「该用哪把尺子」只有一处判断。这个文件是它的回归证明面：`Ledger::valuate`
//! 必须与它派发到的那把**原始尺子逐值相等**，`MarginState::valuate` 必须与 `Ledger::valuate`
//! 共用同一个 `ValuationResult` 形状（含 `available = equity − initial_margin`）。
//!
//! 如果派发被改错（比如有杠杆规格却走了乘数尺子），下面的等式会当场不成立；如果
//! `ValuationResult` 的字段关系被复制到别处，`margin_state_valuate_shares_the_result_shape`
//! 会先红。

use qx_core::*;
use std::collections::BTreeMap;

fn perpetual_spec(instrument: &InstrumentId) -> TradingInstrumentSpec {
    TradingInstrumentSpec {
        instrument: instrument.clone(),
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
        max_leverage: 100,
        maintenance_margin_bps: 500,
        valid_from: 1,
        valid_to: None,
    }
}

/// 现货账本：现金 799 USDT + 2 手 BTCUSDT（买价 100、手续费 1），标记价 110。
fn spot_ledger() -> (Ledger, InstrumentId) {
    let instrument = InstrumentId::parse("BTC-USDT.BINANCE").unwrap();
    let mut ledger = Ledger::new();
    ledger
        .deposit("main", "USDT", Money::from_i64(1000), 1)
        .unwrap();
    let order = Order {
        client_id: 7,
        instrument: instrument.clone(),
        side: Side::Buy,
        qty: Quantity::from_i64(2),
        limit: None,
        status: OrderStatus::Accepted,
        filled: Quantity::ZERO,
        account_id: "main".into(),
        trace: None,
        policy: None,
    };
    ledger
        .apply_fill(
            &order,
            &Fill {
                order_id: 7,
                qty: Quantity::from_i64(2),
                price: Price::from_i64(100),
                fee: Money::from_i64(1),
                ts: 2,
                ..Fill::default()
            },
            "USDT",
        )
        .unwrap();
    (ledger, instrument)
}

/// 现货：`valuate` 必须等于乘数尺子（`multiplier = 1` 时就是 `equity_for`）。
#[test]
fn ledger_valuate_equals_the_multiplier_ruler_for_spot() {
    let (ledger, instrument) = spot_ledger();
    let marks = BTreeMap::from([(instrument, Price::from_i64(110))]);
    for multiplier in [1_i128, 7] {
        let expected = ledger
            .equity_for_with_multiplier("main", &marks, "USDT", multiplier)
            .unwrap();
        let context = ValuationContext::spot("main", &marks, "USDT").with_multiplier(multiplier);
        assert_eq!(ledger.valuate(&context).unwrap().equity.raw(), expected);
    }
}

/// 非杠杆规格也必须走乘数尺子（`supports_leverage() == false`），不能被误派发到合约路径。
#[test]
fn ledger_valuate_ignores_a_non_leveraged_spec() {
    let (ledger, instrument) = spot_ledger();
    let marks = BTreeMap::from([(instrument.clone(), Price::from_i64(110))]);
    let mut spec = perpetual_spec(&instrument);
    spec.product = TradingProduct::Spot;
    let expected = ledger
        .equity_for_with_multiplier("main", &marks, "USDT", 1)
        .unwrap();
    let context = ValuationContext::spot("main", &marks, "USDT").with_spec(Some(&spec));
    assert_eq!(ledger.valuate(&context).unwrap().equity.raw(), expected);
}

/// 有杠杆规格且无汇率：走合约规格尺子，与 `equity_for_with_spec` 逐值相等。
#[test]
fn ledger_valuate_uses_the_contract_spec_ruler_without_fx() {
    let instrument = InstrumentId::parse("BTC/USDT:USDT.SIM").unwrap();
    let spec = perpetual_spec(&instrument);
    let mut ledger = Ledger::new();
    ledger
        .deposit("main", "USDT", Money::from_i64(1000), 1)
        .unwrap();
    let order = Order {
        client_id: 1,
        instrument: instrument.clone(),
        side: Side::Buy,
        qty: Quantity::from_i64(1),
        limit: None,
        status: OrderStatus::Accepted,
        filled: Quantity::ZERO,
        account_id: "main".into(),
        trace: None,
        policy: None,
    };
    ledger
        .apply_fill_with_spec(
            &order,
            &Fill {
                order_id: 1,
                qty: Quantity::from_i64(1),
                price: Price::from_i64(100),
                ts: 2,
                ..Fill::default()
            },
            "USDT",
            &spec,
        )
        .unwrap();
    let marks = BTreeMap::from([(instrument, Price::from_i64(110))]);
    let expected = ledger
        .equity_for_with_spec("main", &marks, "USDT", &spec)
        .unwrap();
    let context = ValuationContext::spot("main", &marks, "USDT").with_spec(Some(&spec));
    assert_eq!(ledger.valuate(&context).unwrap().equity.raw(), expected);
}

/// 有杠杆规格 + 有汇率：必须切到跨币种尺子，而不是继续用合约规格尺子。
#[test]
fn ledger_valuate_switches_to_the_fx_ruler_only_when_rates_are_given() {
    let instrument = InstrumentId::parse("BTC/USDT:USDT.SIM").unwrap();
    let spec = perpetual_spec(&instrument);
    let mut ledger = Ledger::new();
    ledger
        .deposit("main", "BTC", Money::from_i64(1), 1)
        .unwrap();
    ledger
        .deposit("main", "USDT", Money::from_i64(100), 1)
        .unwrap();
    let marks = BTreeMap::new();
    let fx = BTreeMap::from([(String::from("BTC"), Price::from_i64(60_000))]);

    let spec_only = ledger
        .equity_for_with_spec("main", &marks, "USDT", &spec)
        .unwrap();
    let with_fx = ledger
        .equity_for_with_spec_and_fx("main", &marks, "USDT", &spec, &fx)
        .unwrap();
    // 两把尺子在这本账上给出不同的数，等式才真的在验派发。
    assert_ne!(spec_only, with_fx);

    let base = ValuationContext::spot("main", &marks, "USDT").with_spec(Some(&spec));
    assert_eq!(ledger.valuate(&base).unwrap().equity.raw(), spec_only);
    let converted = base.with_fx(&fx);
    assert_eq!(ledger.valuate(&converted).unwrap().equity.raw(), with_fx);
}

/// `available = equity − initial_margin` 由上下文里的保证金占用推出，币种随结果走。
#[test]
fn ledger_valuate_derives_available_from_the_context_margin() {
    let (ledger, instrument) = spot_ledger();
    let marks = BTreeMap::from([(instrument, Price::from_i64(110))]);
    let mut context = ValuationContext::spot("main", &marks, "USDT");
    context.margin_required = Money::from_i64(19);
    context.maintenance_required = Money::from_i64(5);
    let result = ledger.valuate(&context).unwrap();
    assert_eq!(result.initial_margin, Money::from_i64(19));
    assert_eq!(result.maintenance_margin, Money::from_i64(5));
    assert_eq!(
        result.available.raw(),
        result.equity.raw() - Money::from_i64(19).raw()
    );
    assert_eq!(result.currency, "USDT");
}

/// `MarginState` 与 `Ledger` 必须共用同一个 `ValuationResult` 形状：字段关系一致，
/// `equity()` / `available()` 只是它的两个字段视图。
#[test]
fn margin_state_valuate_shares_the_result_shape() {
    let state = MarginState {
        collateral: Money::from_i64(1000),
        unrealized_pnl: Money::from_i64(50),
        funding: Money::from_i64(2),
        interest: Money::from_i64(3),
        initial_margin: Money::from_i64(100),
        maintenance_margin: Money::from_i64(50),
    };
    let result = state.valuate("USDT").unwrap();
    assert_eq!(result.equity, Money::from_i64(1045));
    assert_eq!(result.available, Money::from_i64(945));
    assert_eq!(result.currency, "USDT");
    assert_eq!(state.equity(), Some(result.equity));
    assert_eq!(state.available(), Some(result.available));
}
