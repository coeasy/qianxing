use super::*;
use qx_core::{
    ExactDecimal, ExactOrderAmounts, ExactTradingRules, InstrumentId, Ledger, Money, OrderStatus,
    OrderTrace, TradingInstrumentSpec,
};

fn order(side: Side, qty: i128, limit: Option<i128>) -> Order {
    Order {
        client_id: 1,
        instrument: InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
        side,
        qty: Quantity::from_raw(qty),
        limit: limit.map(Price::from_raw),
        status: OrderStatus::PendingSubmit,
        filled: Quantity::ZERO,
        account_id: "main".into(),
        trace: Some(OrderTrace {
            strategy_id: Some("book-test".into()),
            signal_id: Some(1),
            intent_id: Some(1),
            rule_version: Some("v1".into()),
        }),
        policy: None,
        exact_amounts: None,
    }
}

fn book() -> OrderBookSnapshot {
    OrderBookSnapshot {
        instrument: InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
        ts: 10,
        sequence: 1,
        bids: vec![BookLevel {
            price_exact: None,
            qty_exact: None,
            price: Price::from_i64(99),
            qty: Quantity::from_i64(2),
        }],
        asks: vec![
            BookLevel {
                price_exact: None,
                qty_exact: None,
                price: Price::from_i64(100),
                qty: Quantity::from_i64(1),
            },
            BookLevel {
                price_exact: None,
                qty_exact: None,
                price: Price::from_i64(101),
                qty: Quantity::from_i64(2),
            },
        ],
    }
}

fn exact_depth_frame(bid_price: &str, ask_price: &str) -> DepthFrame {
    DepthFrame {
        schema_version: DepthFrame::EXACT_SCHEMA_VERSION,
        source: "exact-depth-fixture".into(),
        instrument: InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
        snapshots: vec![OrderBookSnapshot {
            instrument: InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
            ts: 10,
            sequence: 1,
            bids: vec![BookLevel::from_exact(
                ExactDecimal::parse(bid_price).unwrap(),
                ExactDecimal::parse("0.0000000001").unwrap(),
            )
            .unwrap()],
            asks: vec![BookLevel::from_exact(
                ExactDecimal::parse(ask_price).unwrap(),
                ExactDecimal::parse("0.0000000002").unwrap(),
            )
            .unwrap()],
        }],
    }
}

#[test]
fn depth_v2_preserves_sub_nano_book_levels_and_hashes_the_source_values() {
    let frame = exact_depth_frame("100.0000000001", "100.0000000002");
    frame.validate().unwrap();
    let snapshot = &frame.snapshots[0];
    assert_eq!(snapshot.bids[0].price.raw(), snapshot.asks[0].price.raw());
    assert_eq!(snapshot.bids[0].qty.raw(), 0);
    assert!(snapshot.validate().is_ok());

    let payload = serde_json::to_string(&frame).unwrap();
    let restored = DepthFrame::from_json(&payload).unwrap();
    assert_eq!(restored.input_hash(), frame.input_hash());
    assert_ne!(
        frame.input_hash(),
        exact_depth_frame("100.0000000003", "100.0000000004").input_hash()
    );
    let example = DepthFrame::from_json(include_str!(
        "../../../../docs/examples/depth-frame-v2.example.json"
    ))
    .unwrap();
    assert_eq!(example.schema_version, DepthFrame::EXACT_SCHEMA_VERSION);
}

#[test]
fn depth_v2_rejects_missing_exact_levels_and_mismatched_legacy_projections() {
    let mut frame = exact_depth_frame("100.0000000001", "100.0000000002");
    frame.snapshots[0].asks[0].qty_exact = None;
    assert!(frame.validate().unwrap_err().contains("必须同时提供"));

    let mut frame = exact_depth_frame("100.0000000001", "100.0000000002");
    frame.snapshots[0].bids[0].qty = Quantity::from_raw(1);
    assert!(frame.validate().unwrap_err().contains("投影不一致"));
}

#[test]
fn depth_v1_rejects_exact_fields_instead_of_silently_ignoring_them() {
    let mut frame = exact_depth_frame("100.0000000001", "100.0000000002");
    frame.schema_version = DepthFrame::LEGACY_SCHEMA_VERSION;
    assert!(frame
        .validate()
        .unwrap_err()
        .contains("不能包含精确盘口字段"));
}

#[test]
fn exact_matcher_consumes_sub_nano_spot_depth_and_keeps_exact_fill_facts() {
    let frame = exact_depth_frame("100.0000000001", "100.0000000002");
    let instrument = frame.instrument.clone();
    let mut spec = TradingInstrumentSpec::spot_default(&instrument, "USDT");
    spec.base_currency = "BTC".into();
    spec.exact_rules = Some(ExactTradingRules {
        contract_size: ExactDecimal::ONE,
        price_tick: ExactDecimal::parse("0.0000000001").unwrap(),
        quantity_step: ExactDecimal::parse("0.0000000001").unwrap(),
        minimum_quantity: ExactDecimal::parse("0.0000000001").unwrap(),
        minimum_notional: ExactDecimal::ZERO,
    });
    let qty = ExactDecimal::parse("0.0000000002").unwrap();
    let mut exact_order = order(Side::Buy, 0, None);
    exact_order.exact_amounts = Some(ExactOrderAmounts {
        quantity: qty,
        limit_price: None,
        filled: ExactDecimal::ZERO,
        quote_budget: None,
        quote_filled: ExactDecimal::ZERO,
        market_reference_price: Some(ExactDecimal::parse("100.0000000002").unwrap()),
    });
    let mut engine = OrderBookMatchingEngine::new(100)
        .unwrap()
        .with_fee_spec(Some(spec.clone()));
    engine.submit(exact_order.clone()).unwrap();

    let fills = engine.on_snapshot(&frame.snapshots[0]).unwrap();
    assert_eq!(fills.len(), 1);
    assert_eq!(fills[0].qty.raw(), 0);
    assert_eq!(fills[0].price.raw(), 0);
    let amounts = fills[0].exact_amounts.unwrap();
    assert_eq!(amounts.quantity, qty);
    assert_eq!(
        amounts.price,
        ExactDecimal::parse("100.0000000002").unwrap()
    );
    assert_eq!(
        amounts.quote_cost,
        Some(ExactDecimal::parse("0.000000020000000001").unwrap())
    );
    assert_eq!(fills[0].exact_fees.len(), 1);
    assert_eq!(
        fills[0].exact_fees[0].amount,
        ExactDecimal::parse("0.0000000002").unwrap()
    );
    assert_eq!(engine.pending_count(), 0);
    let attempt = engine.execution_attempts().last().unwrap();
    assert_eq!(attempt.requested_qty_exact, Some(qty));
    assert_eq!(attempt.filled_qty_exact, Some(qty));
    assert_eq!(attempt.price_exact, Some(amounts.price));

    let mut ledger = Ledger::new();
    ledger
        .deposit("main", "USDT", Money::from_i64(1), 1)
        .unwrap();
    ledger
        .apply_fill_with_spec(&exact_order, &fills[0], "USDT", &spec)
        .unwrap();
    assert_eq!(
        ledger
            .position_for_side_exact(
                &exact_order.account_id,
                &instrument,
                qx_core::PositionSide::Net
            )
            .quantity,
        qty
    );
    assert_eq!(
        ledger.cash_for_exact("main", "USDT"),
        ExactDecimal::parse("0.999999979799999999").unwrap()
    );
}

#[test]
fn market_buy_consumes_multiple_ask_levels_and_keeps_partial_order() {
    let mut engine = OrderBookMatchingEngine::new(10).unwrap();
    engine
        .submit(order(Side::Buy, Quantity::from_i64(2).raw(), None))
        .unwrap();
    let fills = engine.on_snapshot(&book()).unwrap();
    assert_eq!(fills.len(), 2);
    assert_eq!(fills[0].price.raw(), Price::from_i64(100).raw());
    assert_eq!(fills[1].price.raw(), Price::from_i64(101).raw());
    assert_eq!(engine.pending_count(), 0);
}

#[test]
fn limit_order_waits_when_best_price_is_not_executable() {
    let mut engine = OrderBookMatchingEngine::new(0).unwrap();
    engine
        .submit(order(Side::Buy, Quantity::from_i64(1).raw(), Some(99)))
        .unwrap();
    let fills = engine.on_snapshot(&book()).unwrap();
    assert!(fills.is_empty());
    assert_eq!(engine.pending_count(), 1);
}

#[test]
fn one_snapshot_liquidity_is_not_reused_by_multiple_orders() {
    let mut engine = OrderBookMatchingEngine::new(0).unwrap();
    let mut first = order(Side::Buy, Quantity::from_i64(2).raw(), None);
    first.client_id = 1;
    let mut second = order(Side::Buy, Quantity::from_i64(2).raw(), None);
    second.client_id = 2;
    engine.submit(first).unwrap();
    engine.submit(second).unwrap();
    let fills = engine.on_snapshot(&book()).unwrap();
    assert_eq!(
        fills.iter().map(|fill| fill.qty.raw()).sum::<i128>(),
        Quantity::from_i64(3).raw()
    );
    assert_eq!(engine.pending_count(), 1);
}

#[test]
fn execution_model_applies_latency_queue_and_market_impact() {
    let model = OrderBookExecutionModel {
        fee_bps: 0,
        latency_snapshots: 1,
        queue_position_bps: 5_000,
        queue_position_model: QueuePositionModel::Disabled,
        market_impact_bps: 100,
    };
    let mut engine = OrderBookMatchingEngine::with_model(model).unwrap();
    engine
        .submit(order(Side::Buy, Quantity::from_i64(1).raw(), None))
        .unwrap();
    let first = book();
    assert!(engine.on_snapshot(&first).unwrap().is_empty());
    let second = OrderBookSnapshot {
        ts: 11,
        sequence: 2,
        ..first
    };
    let fills = engine.on_snapshot(&second).unwrap();
    assert_eq!(fills.len(), 1);
    assert_eq!(fills[0].price, Price::from_i64(101));
    assert_eq!(fills[0].qty, Quantity::from_i64(1));
}

/// 队列优先级不能因为"部分成交"与"尚未到期"两组拼接而颠倒：早提交且已部分
/// 成交的订单，必须继续排在场次更晚、延迟更长的订单之前。
#[test]
fn partially_filled_order_keeps_queue_priority_over_later_order() {
    let model = OrderBookExecutionModel {
        fee_bps: 0,
        latency_snapshots: 1,
        queue_position_bps: 0,
        queue_position_model: QueuePositionModel::Disabled,
        market_impact_bps: 0,
    };
    let mut engine = OrderBookMatchingEngine::with_model(model).unwrap();
    let limit = Some(Price::from_i64(100).raw());
    let mut first = order(Side::Buy, Quantity::from_i64(2).raw(), limit);
    first.client_id = 1;
    let mut second = order(Side::Buy, Quantity::from_i64(2).raw(), limit);
    second.client_id = 2;
    engine.submit(first).unwrap();
    // 快照 1：first 的第 2 个快照才到期，本轮无成交。
    assert!(engine.on_snapshot(&book()).unwrap().is_empty());
    // 此后提交的 second 要到第 3 个快照才到期。
    engine.submit(second).unwrap();
    let second_snapshot = OrderBookSnapshot {
        ts: 11,
        sequence: 2,
        ..book()
    };
    let fills = engine.on_snapshot(&second_snapshot).unwrap();
    // 买一只挂 1 单位，first 吃到 1 单位后仍是挂单；second 仍在等待到期。
    assert_eq!(
        fills.iter().map(|fill| fill.order_id).collect::<Vec<_>>(),
        vec![1]
    );
    assert_eq!(engine.pending_count(), 2);
    // 买一的最后流动性应回到先提交的 first，而不是后提交、先轮到执行的 second。
    let third_snapshot = OrderBookSnapshot {
        ts: 12,
        sequence: 3,
        ..book()
    };
    let fills = engine.on_snapshot(&third_snapshot).unwrap();
    assert_eq!(
        fills.iter().map(|fill| fill.order_id).collect::<Vec<_>>(),
        vec![1]
    );
    assert_eq!(engine.pending_count(), 1);
}

/// 同一快照的流动性不能被第二张限价单重复吃掉：扣减前置队列时不夹到 0，
/// 第一单的成交量就会以"负队列"的形式重新变成可吃数量。
#[test]
fn limit_orders_do_not_resell_the_same_snapshot_liquidity() {
    let limit = Some(Price::from_i64(100).raw());
    let mut first = order(Side::Buy, Quantity::from_i64(1).raw(), limit);
    first.client_id = 1;
    let mut second = order(Side::Buy, Quantity::from_i64(1).raw(), limit);
    second.client_id = 2;
    let mut engine = OrderBookMatchingEngine::new(0).unwrap();
    engine.submit(first).unwrap();
    engine.submit(second).unwrap();
    let fills = engine.on_snapshot(&book()).unwrap();
    assert_eq!(
        fills.iter().map(|fill| fill.order_id).collect::<Vec<_>>(),
        vec![1],
        "买一只有 1 单位，second 不该拿到同一份流动性"
    );
    assert_eq!(
        fills.iter().map(|fill| fill.qty.raw()).sum::<i128>(),
        Quantity::from_i64(1).raw()
    );
}

#[test]
fn fifo_volume_estimate_advances_queue_before_filling_passive_limit() {
    let model = OrderBookExecutionModel {
        fee_bps: 0,
        latency_snapshots: 0,
        queue_position_bps: 5_000,
        queue_position_model: QueuePositionModel::FifoVolumeEstimate {
            volume_advance_bps: 10_000,
        },
        market_impact_bps: 0,
    };
    let mut engine = OrderBookMatchingEngine::with_model(model).unwrap();
    let mut legacy_engine = OrderBookMatchingEngine::new(0).unwrap();
    engine
        .submit(order(
            Side::Buy,
            Quantity::from_i64(2).raw(),
            Some(Price::from_i64(99).raw()),
        ))
        .unwrap();
    legacy_engine
        .submit(order(
            Side::Buy,
            Quantity::from_i64(2).raw(),
            Some(Price::from_i64(99).raw()),
        ))
        .unwrap();
    let snapshot = |ts, sequence, bid_qty| OrderBookSnapshot {
        ts,
        sequence,
        bids: if bid_qty == 0 {
            vec![]
        } else {
            vec![BookLevel {
                price_exact: None,
                qty_exact: None,
                price: Price::from_i64(99),
                qty: Quantity::from_i64(bid_qty),
            }]
        },
        ..book()
    };
    assert!(engine.on_snapshot(&snapshot(10, 1, 8)).unwrap().is_empty());
    assert!(legacy_engine
        .on_snapshot(&snapshot(10, 1, 4))
        .unwrap()
        .is_empty());
    assert!(engine.on_snapshot(&snapshot(11, 2, 4)).unwrap().is_empty());
    assert!(legacy_engine
        .on_snapshot(&snapshot(11, 2, 2))
        .unwrap()
        .is_empty());
    let fills = engine.on_snapshot(&snapshot(12, 3, 2)).unwrap();
    assert!(legacy_engine
        .on_snapshot(&snapshot(12, 3, 0))
        .unwrap()
        .is_empty());
    assert_eq!(fills.len(), 1);
    assert_eq!(fills[0].qty, Quantity::from_i64(2));
    assert_eq!(fills[0].price, Price::from_i64(99));
}

#[test]
fn cancellation_before_snapshot_prevents_fill_and_preserves_event_order() {
    let mut engine = OrderBookMatchingEngine::new(0).unwrap();
    engine
        .submit(order(Side::Buy, Quantity::from_i64(1).raw(), None))
        .unwrap();
    let fills = engine
        .on_snapshot_with_cancellations(&book(), &[1])
        .unwrap();
    assert!(fills.is_empty());
    assert_eq!(engine.pending_count(), 0);
    assert!(engine
        .on_snapshot(&OrderBookSnapshot {
            ts: 9,
            sequence: 2,
            ..book()
        })
        .is_err());
}

#[test]
fn duplicate_order_and_non_monotonic_snapshot_are_rejected() {
    let mut engine = OrderBookMatchingEngine::new(0).unwrap();
    engine
        .submit(order(Side::Buy, Quantity::from_i64(1).raw(), None))
        .unwrap();
    assert!(engine
        .submit(order(Side::Buy, Quantity::from_i64(1).raw(), None))
        .is_err());
    engine.on_snapshot(&book()).unwrap();
    assert!(engine.on_snapshot(&book()).is_err());
}
