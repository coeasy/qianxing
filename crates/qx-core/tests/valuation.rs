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
        min_notional_raw: 0,
        max_leverage: 100,
        maintenance_margin_bps: 500,
        valid_from: 1,
        valid_to: None,
        exact_rules: None,
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
        exact_amounts: None,
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

fn spot_spec(instrument: &InstrumentId, base: &str, quote: &str) -> TradingInstrumentSpec {
    TradingInstrumentSpec {
        instrument: instrument.clone(),
        product: TradingProduct::Spot,
        base_currency: base.into(),
        quote_currency: quote.into(),
        settlement_currency: quote.into(),
        contract_size: SCALE,
        linear: false,
        inverse: false,
        price_tick: 1,
        qty_step: 1,
        min_qty: 1,
        min_notional_raw: 0,
        max_leverage: 1,
        maintenance_margin_bps: 0,
        valid_from: 1,
        valid_to: None,
        exact_rules: None,
    }
}

fn subnano_spot_spec(instrument: &InstrumentId) -> TradingInstrumentSpec {
    let mut spec = spot_spec(instrument, "T", "USDT");
    spec.exact_rules = Some(ExactTradingRules {
        contract_size: ExactDecimal::ONE,
        price_tick: ExactDecimal::parse("0.000000001").unwrap(),
        quantity_step: ExactDecimal::parse("0.000000000001").unwrap(),
        minimum_quantity: ExactDecimal::parse("0.000000000001").unwrap(),
        minimum_notional: ExactDecimal::ZERO,
    });
    spec
}

fn buy_position(
    ledger: &mut Ledger,
    instrument: &InstrumentId,
    client_id: u64,
    qty_raw: i128,
    price_raw: i128,
    fee_raw: i128,
    currency: &str,
) {
    let order = Order {
        client_id,
        instrument: instrument.clone(),
        side: Side::Buy,
        qty: Quantity::from_raw(qty_raw),
        limit: None,
        status: OrderStatus::Accepted,
        filled: Quantity::ZERO,
        account_id: "main".into(),
        trace: None,
        policy: None,
        exact_amounts: None,
    };
    ledger
        .apply_fill(
            &order,
            &Fill {
                order_id: client_id,
                qty: Quantity::from_raw(qty_raw),
                price: Price::from_raw(price_raw),
                fee: Money::from_raw(fee_raw),
                ts: client_id,
                ..Fill::default()
            },
            currency,
        )
        .unwrap();
}

#[test]
fn spec_driven_spot_fill_books_cash_in_the_market_quote_currency() {
    let instrument = InstrumentId::parse("ETH-EUR.KRAKEN").unwrap();
    let mut spec = spot_spec(&instrument, "ETH", "EUR");
    spec.quote_currency = "eur".into();
    let order = Order {
        client_id: 90,
        instrument: instrument.clone(),
        side: Side::Buy,
        qty: Quantity::from_i64(2),
        limit: None,
        status: OrderStatus::Accepted,
        filled: Quantity::ZERO,
        account_id: "main".into(),
        trace: None,
        policy: None,

        exact_amounts: None,
    };
    let mut ledger = Ledger::new();
    ledger
        .apply_fill_with_spec(
            &order,
            &Fill {
                order_id: order.client_id,
                qty: Quantity::from_i64(2),
                price: Price::from_i64(100),
                ts: 1,
                ..Fill::default()
            },
            "USDT",
            &spec,
        )
        .unwrap();

    assert_eq!(ledger.cash_for("main", "EUR"), -200 * SCALE);
    assert_eq!(ledger.cash_for("main", "USDT"), 0);
}

#[test]
fn ledger_valuate_marks_spot_positions_and_cash_for_multiple_markets_and_currencies() {
    let eth = InstrumentId::parse("ETH-USDT.BINANCE").unwrap();
    let btc = InstrumentId::parse("BTC-EUR.KRAKEN").unwrap();
    let specs = BTreeMap::from([
        (eth.clone(), spot_spec(&eth, "ETH", "USDT")),
        (btc.clone(), {
            let mut spec = spot_spec(&btc, "BTC", "EUR");
            spec.quote_currency = "eur".into();
            spec
        }),
    ]);
    let mut ledger = Ledger::new();
    ledger
        .deposit("main", "USDT", Money::from_raw(1_000 * SCALE), 1)
        .unwrap();
    ledger
        .deposit("main", "EUR", Money::from_raw(1_000 * SCALE), 1)
        .unwrap();
    buy_position(&mut ledger, &eth, 1, 2 * SCALE, 100 * SCALE, SCALE, "USDT");
    buy_position(
        &mut ledger,
        &btc,
        2,
        3 * SCALE,
        100 * SCALE,
        2 * SCALE,
        "EUR",
    );
    let marks = BTreeMap::from([
        (eth, Price::from_raw(110 * SCALE)),
        (btc, Price::from_raw(120 * SCALE)),
    ]);
    let fx = BTreeMap::from([(String::from("EUR"), Price::from_raw(11 * SCALE / 10))]);

    let equity = ledger
        .equity_for_with_market_specs_and_fx("main", &marks, "USDT", &specs, &fx)
        .unwrap();
    assert_eq!(equity, 2_182_800_000_000);
    let context = ValuationContext::spot("main", &marks, "USDT")
        .with_market_specs(&specs)
        .with_fx(&fx);
    assert_eq!(ledger.valuate(&context).unwrap().equity.raw(), equity);
    let paper = ledger
        .valuate_paper(&PaperValuationContext::new(
            "main", &marks, "USDT", &specs, &fx,
        ))
        .unwrap();
    assert_eq!(paper.equity.raw(), equity);
    assert_eq!(paper.available.raw(), equity);
    assert!(ledger
        .equity_for_with_market_specs_and_fx("main", &marks, "USDT", &specs, &BTreeMap::new())
        .is_err());
    assert!(ledger
        .valuate_paper(&PaperValuationContext::new(
            "main",
            &marks,
            "USDT",
            &specs,
            &BTreeMap::new(),
        ))
        .is_err());
}

#[test]
fn multi_market_exact_valuation_preserves_subnano_cash_and_position_value() {
    let instrument = InstrumentId::parse("T-USDT.BINANCE").unwrap();
    let spec = subnano_spot_spec(&instrument);
    let quantity = ExactDecimal::parse("0.0000000001").unwrap();
    let entry_price = ExactDecimal::ONE;
    let mut ledger = Ledger::new();
    ledger
        .deposit("main", "USDT", Money::from_i64(100), 1)
        .unwrap();
    let order = Order {
        client_id: 21,
        instrument: instrument.clone(),
        side: Side::Buy,
        qty: Quantity::ZERO,
        limit: None,
        status: OrderStatus::Accepted,
        filled: Quantity::ZERO,
        account_id: "main".into(),
        trace: None,
        policy: None,
        exact_amounts: Some(ExactOrderAmounts {
            quantity,
            limit_price: None,
            filled: ExactDecimal::ZERO,
            quote_budget: None,
            quote_filled: ExactDecimal::ZERO,
            market_reference_price: None,
        }),
    };
    ledger
        .apply_fill_with_spec(
            &order,
            &Fill {
                order_id: order.client_id,
                qty: Quantity::ZERO,
                price: Price::ZERO,
                fee: Money::ZERO,
                ts: 2,
                exact_amounts: Some(ExactFillAmounts {
                    quantity,
                    price: entry_price,
                    quote_cost: Some(quantity),
                    quote_budget_complete: false,
                }),
                ..Fill::default()
            },
            "USDT",
            &spec,
        )
        .unwrap();
    let specs = BTreeMap::from([(instrument.clone(), spec.clone())]);
    let marks = BTreeMap::from([(instrument.clone(), Price::from_i64(2))]);

    let exact_equity = ledger
        .equity_for_with_market_specs_and_fx_exact("main", &marks, "USDT", &specs, &BTreeMap::new())
        .unwrap();
    assert_eq!(exact_equity, ExactDecimal::parse("100.0000000001").unwrap());
    let exact_context = ExactValuationContext::new(
        ValuationContext::spot("main", &marks, "USDT").with_market_specs(&specs),
    );
    assert_eq!(ledger.valuate_exact(&exact_context).unwrap(), exact_equity);
    let exact_marks = BTreeMap::from([(
        instrument.clone(),
        ExactDecimal::parse("2.0000001").unwrap(),
    )]);
    let exact_mark_context = ExactValuationContext::new(
        ValuationContext::spot("main", &marks, "USDT").with_market_specs(&specs),
    )
    .with_exact_marks(&exact_marks);
    assert_eq!(
        ledger.valuate_exact(&exact_mark_context).unwrap(),
        ExactDecimal::parse("100.00000000010000001").unwrap()
    );
    let raw_equity = ledger
        .equity_for_with_market_specs_and_fx("main", &marks, "USDT", &specs, &BTreeMap::new())
        .unwrap();
    assert_eq!(raw_equity, 100 * SCALE - 1);

    let attributed_cash = BTreeMap::from([("USDT".into(), quantity.checked_neg().unwrap())]);
    let context = ValuationContext::spot("main", &marks, "USDT").with_spec(Some(&spec));
    assert_eq!(
        ledger
            .instrument_equity_contribution_from_exact_cash(&context, &attributed_cash)
            .unwrap(),
        quantity
    );
}

#[test]
fn multi_market_valuation_fails_closed_on_missing_position_spec_or_mark() {
    let eth = InstrumentId::parse("ETH-USDT.BINANCE").unwrap();
    let btc = InstrumentId::parse("BTC-EUR.KRAKEN").unwrap();
    let specs = BTreeMap::from([
        (eth.clone(), spot_spec(&eth, "ETH", "USDT")),
        (btc.clone(), spot_spec(&btc, "BTC", "EUR")),
    ]);
    let mut ledger = Ledger::new();
    ledger
        .deposit("main", "USDT", Money::from_raw(1_000 * SCALE), 1)
        .unwrap();
    ledger
        .deposit("main", "EUR", Money::from_raw(1_000 * SCALE), 1)
        .unwrap();
    buy_position(&mut ledger, &eth, 1, SCALE, 100 * SCALE, 0, "USDT");
    buy_position(&mut ledger, &btc, 2, SCALE, 100 * SCALE, 0, "EUR");

    let complete_marks = BTreeMap::from([
        (eth.clone(), Price::from_raw(110 * SCALE)),
        (btc.clone(), Price::from_raw(120 * SCALE)),
    ]);
    let fx = BTreeMap::from([(String::from("EUR"), Price::from_raw(11 * SCALE / 10))]);
    let without_btc_spec = BTreeMap::from([(eth.clone(), specs[&eth].clone())]);
    assert!(ledger
        .equity_for_with_market_specs_and_fx(
            "main",
            &complete_marks,
            "USDT",
            &without_btc_spec,
            &fx,
        )
        .is_err());

    let without_btc_mark = BTreeMap::from([(eth, Price::from_raw(110 * SCALE))]);
    assert!(ledger
        .equity_for_with_market_specs_and_fx("main", &without_btc_mark, "USDT", &specs, &fx,)
        .is_err());
}

#[test]
fn single_market_derivative_valuation_refuses_to_omit_other_account_positions() {
    let btc = InstrumentId::parse("BTC/USDT:USDT.SIM").unwrap();
    let eth = InstrumentId::parse("ETH/USDT:USDT.SIM").unwrap();
    let btc_spec = perpetual_spec(&btc);
    let mut eth_spec = perpetual_spec(&eth);
    eth_spec.product = TradingProduct::Margin;
    eth_spec.base_currency = "ETH".into();
    let mut ledger = Ledger::new();
    ledger
        .deposit("main", "USDT", Money::from_i64(1_000), 1)
        .unwrap();
    for (client_id, instrument, spec) in [
        (1, btc.clone(), btc_spec.clone()),
        (2, eth.clone(), eth_spec.clone()),
    ] {
        let order = Order {
            client_id,
            instrument,
            side: Side::Buy,
            qty: Quantity::from_i64(1),
            limit: None,
            status: OrderStatus::Accepted,
            filled: Quantity::ZERO,
            account_id: "main".into(),
            trace: None,
            policy: None,
            exact_amounts: None,
        };
        ledger
            .apply_fill_with_spec(
                &order,
                &Fill {
                    order_id: client_id,
                    qty: Quantity::from_i64(1),
                    price: Price::from_i64(100),
                    ts: 2,
                    ..Fill::default()
                },
                "USDT",
                &spec,
            )
            .unwrap();
    }
    let marks = BTreeMap::from([
        (btc.clone(), Price::from_i64(110)),
        (eth.clone(), Price::from_i64(120)),
    ]);
    assert!(ledger
        .equity_for_with_spec("main", &marks, "USDT", &btc_spec)
        .is_err());

    let specs = BTreeMap::from([(btc.clone(), btc_spec), (eth.clone(), eth_spec)]);
    assert_eq!(
        ledger
            .equity_for_with_market_specs_and_fx("main", &marks, "USDT", &specs, &BTreeMap::new())
            .unwrap(),
        1_030 * SCALE,
        "多规格估值必须把保证金产品按未实现盈亏估值"
    );
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
        let exact_expected = ledger
            .equity_for_with_multiplier_exact("main", &marks, "USDT", multiplier, &BTreeMap::new())
            .unwrap();
        assert_eq!(
            ledger
                .valuate_exact(&ExactValuationContext::new(context))
                .unwrap(),
            exact_expected
        );
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

        exact_amounts: None,
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

#[test]
fn paper_valuation_uses_persisted_blended_position_leverage_and_fails_closed_when_unknown() {
    let instrument = InstrumentId::parse("BTC-USDT.BINANCE").unwrap();
    let spec = perpetual_spec(&instrument);
    let specs = BTreeMap::from([(instrument.clone(), spec.clone())]);
    let marks = BTreeMap::from([(instrument.clone(), Price::from_i64(100))]);
    let mut ledger = Ledger::new();
    ledger
        .deposit("main", "USDT", Money::from_i64(1000), 1)
        .unwrap();

    for (client_id, leverage) in [(1, 10), (2, 20)] {
        let order = Order {
            client_id,
            instrument: instrument.clone(),
            side: Side::Buy,
            qty: Quantity::from_i64(1),
            limit: None,
            status: OrderStatus::Accepted,
            filled: Quantity::ZERO,
            account_id: "main".into(),
            trace: None,
            policy: Some(OrderPolicy {
                margin_mode: MarginMode::Cross,
                position_mode: PositionMode::OneWay,
                position_side: PositionSide::Net,
                leverage,
                ..OrderPolicy::default()
            }),
            exact_amounts: None,
        };
        ledger
            .apply_fill_with_spec(
                &order,
                &Fill {
                    order_id: client_id,
                    qty: Quantity::from_i64(1),
                    price: Price::from_i64(100),
                    ts: client_id,
                    ..Fill::default()
                },
                "USDT",
                &spec,
            )
            .unwrap();
    }

    let fx_rates = BTreeMap::new();
    let paper = PaperValuationContext::new("main", &marks, "USDT", &specs, &fx_rates);
    let result = ledger.valuate_paper(&paper).unwrap();
    assert_eq!(result.initial_margin.raw(), Money::from_i64(15).raw());
    assert_eq!(result.available.raw(), Money::from_i64(985).raw());

    let mut replayed = Ledger::new();
    for entry in ledger.entries().iter().cloned() {
        replayed.apply_entry(entry).unwrap();
    }
    assert_eq!(
        replayed.position_for("main", &instrument),
        ledger.position_for("main", &instrument)
    );
    assert_eq!(replayed.valuate_paper(&paper).unwrap(), result);

    let legacy_order = Order {
        client_id: 3,
        instrument: instrument.clone(),
        side: Side::Buy,
        qty: Quantity::from_i64(1),
        limit: None,
        status: OrderStatus::Accepted,
        filled: Quantity::ZERO,
        account_id: "legacy".into(),
        trace: None,
        policy: None,

        exact_amounts: None,
    };
    let mut legacy = Ledger::new();
    legacy
        .apply_fill(
            &legacy_order,
            &Fill {
                order_id: 3,
                qty: Quantity::from_i64(1),
                price: Price::from_i64(100),
                ts: 3,
                ..Fill::default()
            },
            "USDT",
        )
        .unwrap();
    let legacy_paper = PaperValuationContext::new("legacy", &marks, "USDT", &specs, &fx_rates);
    assert!(legacy
        .valuate_paper(&legacy_paper)
        .unwrap_err()
        .to_string()
        .contains("缺少可回放的开仓杠杆"));
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
