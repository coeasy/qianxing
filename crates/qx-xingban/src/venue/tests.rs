use super::*;
use qx_core::InstrumentId;

struct HalfFill;

impl FillModel for HalfFill {
    fn name(&self) -> &'static str {
        "HalfFill"
    }

    fn tier(&self) -> crate::fill::DataTier {
        crate::fill::DataTier::Bar
    }

    fn assumption(&self) -> &'static str {
        "test fixture fills half of each remaining order"
    }

    fn fill(&self, context: &FillContext<'_>, _rng: &mut DeterministicRng) -> Option<(i128, i128)> {
        Some((context.bar.open, context.qty / 2))
    }
}

fn reserved_order(qty: i64) -> Order {
    Order {
        client_id: 1,
        instrument: InstrumentId::parse("TEST.V").unwrap(),
        side: qx_core::Side::Buy,
        qty: Quantity::from_i64(qty),
        limit: None,
        status: OrderStatus::Submitted,
        filled: Quantity::ZERO,
        account_id: "a".into(),
        trace: None,
        policy: None,
        exact_amounts: None,
    }
}

fn exact_order(client_id: u64, quantity: &str) -> Order {
    Order {
        client_id,
        instrument: InstrumentId::parse("TEST.V").unwrap(),
        side: qx_core::Side::Buy,
        qty: Quantity::ZERO,
        limit: None,
        status: OrderStatus::Submitted,
        filled: Quantity::ZERO,
        account_id: "a".into(),
        trace: None,
        policy: None,
        exact_amounts: Some(qx_core::ExactOrderAmounts {
            quantity: qx_core::ExactDecimal::parse(quantity).unwrap(),
            limit_price: None,
            filled: qx_core::ExactDecimal::ZERO,
            quote_budget: None,
            quote_filled: qx_core::ExactDecimal::ZERO,
            market_reference_price: None,
        }),
    }
}

#[test]
fn bar_matcher_fills_sub_nano_exact_quantity_and_retains_attempt_facts() {
    let mut engine = BarMatchingEngine::new(
        Box::new(crate::fill::NextBarOpenFillModel),
        Box::new(qx_core::ZeroFeeModel),
        1,
    );
    let quantity = qx_core::ExactDecimal::parse("0.0000000001").unwrap();
    engine
        .try_submit_at(exact_order(1, "0.0000000001"), 1)
        .unwrap();

    let bar = Bar::new(2, 100 * SCALE, 101 * SCALE, 99 * SCALE, 100 * SCALE, SCALE);
    let fills = engine.on_bar(&bar, bar.ts);
    assert_eq!(fills.len(), 1);
    assert_eq!(fills[0].qty, Quantity::ZERO);
    assert_eq!(fills[0].price, Price::ZERO);
    assert_eq!(fills[0].exact_amounts.unwrap().quantity, quantity);
    assert_eq!(
        fills[0].exact_amounts.unwrap().price,
        qx_core::ExactDecimal::parse("100").unwrap()
    );
    assert_eq!(engine.pending_count(), 0);

    let attempt = &engine.execution_attempts()[0];
    assert_eq!(attempt.requested_qty_exact, Some(quantity));
    assert_eq!(attempt.filled_qty_exact, Some(quantity));
    assert_eq!(
        attempt.remaining_qty_exact,
        Some(qx_core::ExactDecimal::ZERO)
    );
}

#[test]
fn exact_bar_participation_caps_share_volume_and_resume_partial_orders() {
    let fill = crate::fill::BarVolumeParticipationLimit::new(
        Box::new(crate::fill::NextBarOpenFillModel),
        5_000,
    )
    .unwrap();
    let mut engine = BarMatchingEngine::new(Box::new(fill), Box::new(qx_core::ZeroFeeModel), 1);
    engine
        .try_submit_at(exact_order(1, "0.0000000004"), 1)
        .unwrap();
    engine
        .try_submit_at(exact_order(2, "0.0000000004"), 1)
        .unwrap();

    // Source volume is slightly above 1e-9 while its backwards-compatible raw projection is 1.
    // The participation limit must use the exact source amount, not the rounded raw column.
    let exact_volume = qx_core::ExactDecimal::parse("0.00000000100000001").unwrap();
    let first_bar = Bar::new(2, SCALE, SCALE, SCALE, SCALE, 1)
        .with_exact_ohlcv(qx_core::ExactOhlcv {
            open: qx_core::ExactDecimal::parse("1").unwrap(),
            high: qx_core::ExactDecimal::parse("1").unwrap(),
            low: qx_core::ExactDecimal::parse("1").unwrap(),
            close: qx_core::ExactDecimal::parse("1").unwrap(),
            volume: exact_volume,
        })
        .unwrap();
    let first = engine.on_bar(&first_bar, first_bar.ts);
    assert_eq!(first.len(), 2);
    assert_eq!(
        first[0].exact_amounts.unwrap().quantity,
        qx_core::ExactDecimal::parse("0.0000000004").unwrap()
    );
    assert_eq!(
        first[1].exact_amounts.unwrap().quantity,
        qx_core::ExactDecimal::parse("0.000000000100000005").unwrap()
    );
    assert_eq!(engine.pending_count(), 1);
    assert_eq!(
        engine.execution_attempts()[1].reason,
        ExecutionAttemptReason::BarParticipationPartial
    );

    let second_bar = Bar::new(3, SCALE, SCALE, SCALE, SCALE, 1)
        .with_exact_ohlcv(qx_core::ExactOhlcv {
            open: qx_core::ExactDecimal::parse("1").unwrap(),
            high: qx_core::ExactDecimal::parse("1").unwrap(),
            low: qx_core::ExactDecimal::parse("1").unwrap(),
            close: qx_core::ExactDecimal::parse("1").unwrap(),
            volume: exact_volume,
        })
        .unwrap();
    let second = engine.on_bar(&second_bar, second_bar.ts);
    assert_eq!(second.len(), 1);
    assert_eq!(
        second[0].exact_amounts.unwrap().quantity,
        qx_core::ExactDecimal::parse("0.000000000299999995").unwrap()
    );
    assert_eq!(engine.pending_count(), 0);
}

#[test]
fn legacy_fill_model_rejects_unrepresentable_exact_quantity_before_queuing() {
    let mut engine = BarMatchingEngine::new(Box::new(HalfFill), Box::new(qx_core::ZeroFeeModel), 1);
    let error = engine
        .try_submit_at(exact_order(1, "0.0000000001"), 1)
        .unwrap_err();
    assert!(error.contains("不支持该精确数量精度"));
    assert_eq!(engine.pending_count(), 0);
}

#[test]
fn pending_orders_hold_funding_until_filled() {
    let mut engine = BarMatchingEngine::new(
        Box::new(crate::fill::NextBarOpenFillModel),
        Box::new(qx_core::ZeroFeeModel),
        1,
    );
    let mut order = reserved_order(1);
    order.limit = Some(Price::from_i64(50));
    engine
        .submit_with_funding_reservation_at(order, 1, 100 * SCALE)
        .unwrap();

    let no_fill = Bar::new(2, 100 * SCALE, 101 * SCALE, 99 * SCALE, 100 * SCALE, SCALE);
    assert!(engine.on_bar(&no_fill, no_fill.ts).is_empty());
    assert_eq!(engine.pending_funding_reservation_raw(), 100 * SCALE);
    assert_eq!(
        engine.execution_attempts()[0].reason,
        ExecutionAttemptReason::LimitNotMarketable
    );

    let fill = Bar::new(3, 50 * SCALE, 51 * SCALE, 49 * SCALE, 50 * SCALE, SCALE);
    assert_eq!(engine.on_bar(&fill, fill.ts).len(), 1);
    assert_eq!(engine.pending_funding_reservation_raw(), 0);
}

#[test]
fn partial_fills_release_only_the_filled_share_of_funding() {
    let mut engine = BarMatchingEngine::new(Box::new(HalfFill), Box::new(qx_core::ZeroFeeModel), 1);
    engine
        .submit_with_funding_reservation_at(reserved_order(2), 1, 100 * SCALE)
        .unwrap();
    let bar = Bar::new(2, 100 * SCALE, 101 * SCALE, 99 * SCALE, 100 * SCALE, SCALE);

    let first = engine.on_bar(&bar, bar.ts);
    assert_eq!(first[0].qty.raw(), SCALE);
    assert_eq!(engine.pending_funding_reservation_raw(), 50 * SCALE);
    let second = engine.on_bar(&bar, bar.ts + 1);
    assert_eq!(second[0].qty.raw(), SCALE / 2);
    assert_eq!(engine.pending_funding_reservation_raw(), 25 * SCALE);
    assert_eq!(engine.execution_attempts()[0].requested_qty_raw, 2 * SCALE);
    assert_eq!(engine.execution_attempts()[0].filled_qty_raw, SCALE);
    assert_eq!(
        engine.execution_attempts()[0].reason,
        ExecutionAttemptReason::ModelPartialFill
    );
}

#[test]
fn order_fills_on_next_bar_open() {
    let mut e = BarMatchingEngine::new(
        Box::new(crate::fill::NextBarOpenFillModel),
        Box::new(qx_core::ZeroFeeModel),
        1,
    );
    let o = Order {
        client_id: 1,
        instrument: InstrumentId::parse("TEST.V").unwrap(),
        side: qx_core::Side::Buy,
        qty: Quantity::from_i64(10),
        limit: None,
        status: OrderStatus::Submitted,
        filled: Quantity::ZERO,
        account_id: "a".into(),
        trace: None,
        policy: None,
        exact_amounts: None,
    };
    e.submit(o);
    let bar = Bar::new(20, 100, 110, 90, 105, 1000);
    let fills = e.on_bar(&bar, 20);
    assert_eq!(fills.len(), 1);
    assert_eq!(fills[0].price.raw(), 100); // open 而非 close(105)
    assert_eq!(e.execution_attempts().len(), 1);
    assert_eq!(
        e.execution_attempts()[0].reason,
        ExecutionAttemptReason::Filled
    );
}

#[test]
fn bar_volume_participation_cap_is_shared_and_resets_for_the_next_bar() {
    let model = crate::fill::BarVolumeParticipationLimit::new(
        Box::new(crate::fill::NextBarOpenFillModel),
        2_500,
    )
    .unwrap();
    let mut engine = BarMatchingEngine::new(Box::new(model), Box::new(qx_core::ZeroFeeModel), 1);
    engine.submit(market_order(100));
    engine.submit(market_order(101));

    let first_bar = Bar::new(
        20,
        100 * SCALE,
        101 * SCALE,
        99 * SCALE,
        100 * SCALE,
        10 * SCALE,
    );
    let first_fills = engine.on_bar(&first_bar, first_bar.ts);
    assert_eq!(first_fills.len(), 1);
    assert_eq!(first_fills[0].order_id, 100);
    assert_eq!(first_fills[0].qty.raw(), 5 * SCALE / 2);

    let second_bar = Bar::new(
        21,
        100 * SCALE,
        101 * SCALE,
        99 * SCALE,
        100 * SCALE,
        10 * SCALE,
    );
    let second_fills = engine.on_bar(&second_bar, second_bar.ts);
    assert_eq!(second_fills.len(), 1);
    assert_eq!(second_fills[0].order_id, 100);
    assert_eq!(second_fills[0].qty.raw(), 5 * SCALE / 2);
}

/// 真实尺度下手续费不得截断为 0：10 单位 @100.0 = 1000.0 名义，taker 5bp = 0.5。
/// 回归 `fee_price_multiplier` 被当作普通整数传入、导致基准缩小 1e9 的缺陷。
#[test]
fn default_fee_multiplier_charges_real_scale_fees() {
    let mut e = BarMatchingEngine::new(
        Box::new(crate::fill::NextBarOpenFillModel),
        Box::new(qx_core::MakerTakerFeeModel {
            maker_bp: 2,
            taker_bp: 5,
        }),
        1,
    );
    let o = Order {
        client_id: 3,
        instrument: InstrumentId::parse("TEST.V").unwrap(),
        side: qx_core::Side::Buy,
        qty: Quantity::from_i64(10),
        limit: None,
        status: OrderStatus::Submitted,
        filled: Quantity::ZERO,
        account_id: "a".into(),
        trace: None,
        policy: None,
        exact_amounts: None,
    };
    e.submit(o);
    let bar = Bar::new(20, 100 * SCALE, 101 * SCALE, 99 * SCALE, 100 * SCALE, 1_000);
    let fills = e.on_bar(&bar, 20);
    assert_eq!(fills.len(), 1);
    assert_eq!(fills[0].fee.raw(), SCALE / 2, "taker 5bp on 1000 notional");
}

/// 小数合约乘数（0.001 张）在定点标度下仍须计费。
#[test]
fn fractional_fee_multiplier_stays_in_fixed_scale() {
    let mut e = BarMatchingEngine::new_with_latency_and_fee_multiplier(
        Box::new(crate::fill::NextBarOpenFillModel),
        Box::new(qx_core::MakerTakerFeeModel {
            maker_bp: 0,
            taker_bp: 5,
        }),
        Box::new(crate::cost::ZeroLatency),
        1,
        SCALE / 1000,
    );
    let o = Order {
        client_id: 4,
        instrument: InstrumentId::parse("TEST.V").unwrap(),
        side: qx_core::Side::Buy,
        qty: Quantity::from_i64(10),
        limit: None,
        status: OrderStatus::Submitted,
        filled: Quantity::ZERO,
        account_id: "a".into(),
        trace: None,
        policy: None,
        exact_amounts: None,
    };
    e.submit(o);
    let bar = Bar::new(20, 100 * SCALE, 101 * SCALE, 99 * SCALE, 100 * SCALE, 1_000);
    let fills = e.on_bar(&bar, 20);
    assert_eq!(fills.len(), 1);
    // 0.001 张/合约 → 名义 1.0 → 5bp = 0.0005
    assert_eq!(fills[0].fee.raw(), SCALE / 2000);
}

fn market_order(client_id: u64) -> Order {
    Order {
        client_id,
        instrument: InstrumentId::parse("TEST.V").unwrap(),
        side: qx_core::Side::Buy,
        qty: Quantity::from_i64(10),
        limit: None,
        status: OrderStatus::Submitted,
        filled: Quantity::ZERO,
        account_id: "a".into(),
        trace: None,
        policy: None,
        exact_amounts: None,
    }
}

fn limit_order(client_id: u64, post_only: bool) -> Order {
    Order {
        client_id,
        instrument: InstrumentId::parse("TEST.V").unwrap(),
        side: qx_core::Side::Buy,
        qty: Quantity::from_i64(10),
        // 限价高于开盘价：本引擎判定为可成交，因此普通限价单是吃单方。
        limit: Some(Price::from_i64(101)),
        status: OrderStatus::Submitted,
        filled: Quantity::ZERO,
        account_id: "a".into(),
        trace: None,
        policy: post_only.then(|| qx_core::OrderPolicy {
            post_only: true,
            ..Default::default()
        }),
        exact_amounts: None,
    }
}

/// maker 费率只认 post_only 意图；带限价的吃单成交必须按 taker 计费。
#[test]
fn limit_order_is_taker_unless_post_only() {
    let bar = Bar::new(20, 100 * SCALE, 101 * SCALE, 99 * SCALE, 100 * SCALE, 1_000);
    let fees = |post_only: bool| {
        let mut e = BarMatchingEngine::new(
            Box::new(crate::fill::NextBarOpenFillModel),
            Box::new(qx_core::MakerTakerFeeModel {
                maker_bp: 2,
                taker_bp: 5,
            }),
            1,
        );
        e.submit(limit_order(if post_only { 6 } else { 5 }, post_only));
        e.on_bar(&bar, 20)[0].fee.raw()
    };
    // 名义 1000.0：taker 5bp = 0.5，maker 2bp = 0.2
    assert_eq!(fees(false), SCALE / 2, "marketable limit pays taker");
    assert_eq!(fees(true), SCALE / 5, "post_only pays maker");
}

/// A 股三件套必须经撮合引擎按真实尺度计费：佣金有下限、印花税与过户费无下限，
/// 因此费用基准被错误缩小会让后两者归零、前者退化为固定 5 元。
#[test]
fn ashare_fees_charge_commission_stamp_and_transfer() {
    let model = || qx_core::AShareFeeModel {
        commission_bp: 3,
        min_commission: 5 * SCALE,
        stamp_duty_bp: 5,
        transfer_fee_bp: 1,
    };
    let px = 10 * SCALE;
    let bar = Bar::new(20, px, px, px, px, 1_000);
    let charge = |side: qx_core::Side, id: u64| {
        let mut e = BarMatchingEngine::new(
            Box::new(crate::fill::NextBarOpenFillModel),
            Box::new(model()),
            1,
        );
        e.submit(Order {
            client_id: id,
            instrument: InstrumentId::parse("600000.SSE").unwrap(),
            side,
            qty: Quantity::from_i64(100),
            limit: None,
            status: OrderStatus::Submitted,
            filled: Quantity::ZERO,
            account_id: "a".into(),
            trace: None,
            policy: None,
            exact_amounts: None,
        });
        e.on_bar(&bar, 20)[0].fee.raw()
    };
    // 100 股 @10.0 = 1000 元：佣金 3bp=0.3 → 取 5 元下限，过户费 1bp=0.1
    assert_eq!(charge(qx_core::Side::Buy, 7), 51 * SCALE / 10);
    // 卖出另征印花税 5bp=0.5
    assert_eq!(charge(qx_core::Side::Sell, 8), 56 * SCALE / 10);
}

#[test]
fn latency_delays_order_eligibility() {
    let mut e = BarMatchingEngine::new_with_latency(
        Box::new(crate::fill::NextBarOpenFillModel),
        Box::new(qx_core::ZeroFeeModel),
        Box::new(crate::cost::StaticLatency {
            base_ns: 5_000_000,
            insert_ns: 0,
        }),
        1,
    );
    let o = Order {
        client_id: 2,
        instrument: InstrumentId::parse("TEST.V").unwrap(),
        side: qx_core::Side::Buy,
        qty: Quantity::from_i64(1),
        limit: None,
        status: OrderStatus::Submitted,
        filled: Quantity::ZERO,
        account_id: "a".into(),
        trace: None,
        policy: None,
        exact_amounts: None,
    };
    e.submit_at(o, 0);
    assert!(e.on_bar(&Bar::new(4, 100, 100, 100, 100, 1), 4).is_empty());
    assert_eq!(e.on_bar(&Bar::new(5, 101, 101, 101, 101, 1), 5).len(), 1);
}

/// 停牌闸门位于引擎，而不是各 `FillModel`：除 `NextBarOpen` 外的模型都只读
/// `ctx.bar`，忽略 `ctx.halted` 的第三方实现同样不得在停牌期间成交。
#[test]
fn halted_blocks_every_fill_model() {
    let models: Vec<Box<dyn FillModel>> = vec![
        Box::new(crate::fill::NextBarOpenFillModel),
        Box::new(crate::fill::BestPriceFillModel),
        Box::new(crate::fill::OneTickSlippageFillModel { tick: 1 }),
        Box::new(crate::fill::ProbabilisticFillModel {
            prob_fill_on_limit: SCALE,
            tick: 1,
        }),
        Box::new(crate::fill::VolumeSensitiveFillModel { frac_bp: 1_000 }),
    ];
    for model in models {
        let model_name = model.name();
        let mut e = BarMatchingEngine::new(model, Box::new(qx_core::ZeroFeeModel), 1);
        e.submit(market_order(20));
        e.set_halted(true);
        let bar = Bar::new(20, 100 * SCALE, 101 * SCALE, 99 * SCALE, 100 * SCALE, 1_000);
        assert!(
            e.on_bar(&bar, 20).is_empty(),
            "{model_name} 不应在停牌期间成交"
        );
        assert_eq!(e.pending_count(), 1, "停牌只推迟成交，不得丢弃挂单");
    }
}

/// 越界 `FillModel` 的成交量与价格由引擎兜住：现货记账路径不校验超额成交，
/// 而滑点模型在低价标的上可以报出非正成交价。
#[test]
fn out_of_bounds_fill_model_is_clamped_by_engine() {
    struct OverFill;
    impl FillModel for OverFill {
        fn name(&self) -> &'static str {
            "OverFill"
        }
        fn tier(&self) -> crate::fill::DataTier {
            crate::fill::DataTier::Bar
        }
        fn assumption(&self) -> &'static str {
            "测试用：报出两倍于剩余量的成交"
        }
        fn fill(&self, ctx: &FillContext, _rng: &mut DeterministicRng) -> Option<(i128, i128)> {
            Some((ctx.bar.close, ctx.qty * 2))
        }
    }
    let mut e = BarMatchingEngine::new(Box::new(OverFill), Box::new(qx_core::ZeroFeeModel), 1);
    e.submit(market_order(21));
    let fills = e.on_bar(&Bar::new(20, 100, 100, 100, 100, 1), 20);
    assert_eq!(fills.len(), 1);
    assert_eq!(fills[0].qty.raw(), Quantity::from_i64(10).raw());
    assert_eq!(e.pending_count(), 0, "夹到剩余量后订单应已全成");

    struct BadPrice;
    impl FillModel for BadPrice {
        fn name(&self) -> &'static str {
            "BadPrice"
        }
        fn tier(&self) -> crate::fill::DataTier {
            crate::fill::DataTier::Bar
        }
        fn assumption(&self) -> &'static str {
            "测试用：报出负成交价"
        }
        fn fill(&self, ctx: &FillContext, _rng: &mut DeterministicRng) -> Option<(i128, i128)> {
            Some((-1, ctx.qty))
        }
    }
    let mut e = BarMatchingEngine::new(Box::new(BadPrice), Box::new(qx_core::ZeroFeeModel), 1);
    e.submit(market_order(22));
    assert!(e
        .on_bar(&Bar::new(20, 100, 100, 100, 100, 1), 20)
        .is_empty());
    assert_eq!(e.pending_count(), 1);
}

/// 反向（币本位）合约的费用基准是 `qty × contract_size / price`：与线性口径
/// 相差 price² 量级，且必须与账本 `TradingInstrumentSpec::notional` 同源。
#[test]
fn inverse_contract_uses_coin_notional_for_fees() {
    let price = 20_000_i128 * SCALE;
    let qty = SCALE; // 1 张
    let contract_size = SCALE; // 1 张 = 1 币
    let bar = Bar::new(20, price, price, price, price, 1_000);
    let engine = |inverse: bool| {
        let mut e = BarMatchingEngine::new_with_latency_and_fee_multiplier(
            Box::new(crate::fill::NextBarOpenFillModel),
            Box::new(qx_core::MakerTakerFeeModel {
                maker_bp: 0,
                taker_bp: 5,
            }),
            Box::new(crate::cost::ZeroLatency),
            1,
            contract_size,
        );
        e.set_inverse_fee_basis(inverse);
        let mut one_contract = market_order(23);
        one_contract.qty = Quantity::from_raw(qty);
        e.submit(one_contract);
        e.on_bar(&bar, 20)[0].fee.raw()
    };
    // 反向：名义 1 × 1 / 20000 币 = 5e-5，5bp = 2.5e-8 → 定点原始值 25。
    assert_eq!(engine(true), 25);
    // 线性：名义 1 × 20000 = 2e4，5bp = 10 → 定点原始值 10 × SCALE。
    assert_eq!(engine(false), 10 * SCALE);
}
