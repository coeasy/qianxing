use super::*;
use crate::{DepthBarStrategy, NativeOrderBookStrategy, OrderBookActions, RunManifestIdentity};
use qx_core::{
    ExactDecimal, ExactOrderAmounts, ExactTradingRules, InstrumentId, MarginMode, Money, Order,
    OrderPolicy, OrderStatus, PositionMode, PositionSide, Quantity, Side, TradingInstrumentSpec,
    TradingProduct, SCALE,
};
use qx_strategy::{
    Strategy, StrategyContext, StrategyDecision, StrategyHistoryRequirements, StrategyOrderIntent,
    STRATEGY_API_VERSION,
};
use std::sync::{Arc, Mutex};

#[test]
fn accounting_totals_reject_overflow_instead_of_saturating() {
    assert_eq!(checked_raw_total(12, 30, "unused").unwrap(), 42);
    let error = checked_raw_total(i128::MAX, 1, "手续费 raw 投影累计溢出")
        .unwrap_err()
        .to_string();
    assert!(error.contains("手续费 raw 投影累计溢出"), "{error}");
}

#[path = "tests/depth_bar_adapter.rs"]
mod depth_bar_adapter;
#[path = "tests/fill_feedback.rs"]
mod fill_feedback;
#[path = "tests/funding.rs"]
mod funding;
#[path = "tests/pending_risk.rs"]
mod pending_risk;
#[path = "tests/queue_position.rs"]
mod queue_position;

struct NativeBuy;

impl Strategy for NativeBuy {
    fn on_event(
        &mut self,
        context: &StrategyContext,
        event: &qx_strategy::MarketEvent,
    ) -> Result<StrategyDecision, String> {
        let instrument = event.instrument().cloned().ok_or("missing instrument")?;
        Ok(StrategyDecision {
            schema_version: STRATEGY_API_VERSION,
            request_id: format!("native:{}", event.ts()),
            strategy_id: context.strategy_id.clone(),
            signal_id: event.ts(),
            confidence: 1,
            priority: 0,
            expires_at: event.ts(),
            intents: vec![StrategyOrderIntent {
                intent_id: event.ts(),
                instrument,
                side: Side::Buy,
                qty: Quantity::from_i64(1),
                limit: None,
                policy: None,
                reduce_only: false,
                post_only: false,

                qty_exact: None,
                limit_price_exact: None,
                quote_budget: None,
            }],
        })
    }
}
#[path = "tests/margin.rs"]
mod margin;
#[path = "tests/tier.rs"]
mod tier;

struct BuyOnce {
    emitted: bool,
}

struct NoOrders;

struct ExactBuyOnce {
    emitted: bool,
}

#[derive(Default)]
struct ExactDepthBuyer {
    emitted: bool,
    observed_positions: Vec<ExactDecimal>,
}

struct ExactDerivativeBuyer {
    emitted: bool,
    quantity: ExactDecimal,
}

impl OrderBookStrategy for ExactDerivativeBuyer {
    fn supports_exact_orders(&self) -> bool {
        true
    }

    fn on_order_book(
        &mut self,
        snapshot: &OrderBookSnapshot,
        _position: i128,
    ) -> Result<Vec<Order>, String> {
        if self.emitted {
            return Ok(Vec::new());
        }
        self.emitted = true;
        Ok(vec![Order {
            client_id: 780,
            instrument: snapshot.instrument.clone(),
            side: Side::Buy,
            qty: Quantity::ZERO,
            limit: None,
            status: OrderStatus::PendingSubmit,
            filled: Quantity::ZERO,
            account_id: "main".into(),
            trace: None,
            policy: Some(OrderPolicy {
                margin_mode: MarginMode::Cross,
                position_mode: PositionMode::OneWay,
                position_side: PositionSide::Net,
                leverage: 10,
                ..OrderPolicy::default()
            }),
            exact_amounts: Some(ExactOrderAmounts {
                quantity: self.quantity,
                limit_price: None,
                filled: ExactDecimal::ZERO,
                quote_budget: None,
                quote_filled: ExactDecimal::ZERO,
                market_reference_price: None,
            }),
        }])
    }
}

#[derive(Default)]
struct ExactQuoteBudgetBuyer {
    emitted: bool,
}

impl OrderBookStrategy for ExactQuoteBudgetBuyer {
    fn supports_exact_orders(&self) -> bool {
        true
    }

    fn on_order_book(
        &mut self,
        snapshot: &OrderBookSnapshot,
        _position: i128,
    ) -> Result<Vec<Order>, String> {
        if self.emitted {
            return Ok(Vec::new());
        }
        self.emitted = true;
        Ok(vec![Order {
            client_id: 79,
            instrument: snapshot.instrument.clone(),
            side: Side::Buy,
            qty: Quantity::ZERO,
            limit: None,
            status: OrderStatus::PendingSubmit,
            filled: Quantity::ZERO,
            account_id: "main".into(),
            trace: None,
            policy: None,
            exact_amounts: Some(ExactOrderAmounts {
                quantity: ExactDecimal::ZERO,
                limit_price: None,
                filled: ExactDecimal::ZERO,
                quote_budget: Some(ExactDecimal::parse("0.0000000301").unwrap()),
                quote_filled: ExactDecimal::ZERO,
                market_reference_price: None,
            }),
        }])
    }
}

impl OrderBookStrategy for ExactDepthBuyer {
    fn supports_exact_orders(&self) -> bool {
        true
    }

    fn on_order_book(
        &mut self,
        snapshot: &OrderBookSnapshot,
        _position: i128,
    ) -> Result<Vec<Order>, String> {
        if self.emitted {
            return Ok(Vec::new());
        }
        self.emitted = true;
        Ok(vec![Order {
            client_id: 78,
            instrument: snapshot.instrument.clone(),
            side: Side::Buy,
            qty: Quantity::ZERO,
            limit: None,
            status: OrderStatus::PendingSubmit,
            filled: Quantity::ZERO,
            account_id: "main".into(),
            trace: None,
            policy: None,
            exact_amounts: Some(ExactOrderAmounts {
                quantity: ExactDecimal::parse("0.0000000002").unwrap(),
                limit_price: None,
                filled: ExactDecimal::ZERO,
                quote_budget: None,
                quote_filled: ExactDecimal::ZERO,
                market_reference_price: None,
            }),
        }])
    }

    fn on_order_book_actions_with_exact_position(
        &mut self,
        snapshot: &OrderBookSnapshot,
        position_raw: i128,
        position_exact: ExactDecimal,
        _open_order_ids: &[u64],
        _available_margin_raw: Option<i128>,
    ) -> Result<OrderBookActions, String> {
        self.observed_positions.push(position_exact);
        Ok(OrderBookActions {
            orders: self.on_order_book(snapshot, position_raw)?,
            cancel_client_ids: Vec::new(),
        })
    }
}

impl OrderBookStrategy for ExactBuyOnce {
    fn on_order_book(
        &mut self,
        snapshot: &OrderBookSnapshot,
        _position: i128,
    ) -> Result<Vec<Order>, String> {
        if self.emitted {
            return Ok(Vec::new());
        }
        self.emitted = true;
        Ok(vec![Order {
            client_id: 77,
            instrument: snapshot.instrument.clone(),
            side: Side::Buy,
            qty: Quantity::ZERO,
            limit: None,
            status: OrderStatus::PendingSubmit,
            filled: Quantity::ZERO,
            account_id: "main".into(),
            trace: None,
            policy: None,
            exact_amounts: Some(ExactOrderAmounts {
                quantity: ExactDecimal::from_parts(2, 10).unwrap(),
                limit_price: None,
                filled: ExactDecimal::ZERO,
                quote_budget: None,
                quote_filled: ExactDecimal::ZERO,
                market_reference_price: None,
            }),
        }])
    }
}

impl OrderBookStrategy for NoOrders {
    fn on_order_book(
        &mut self,
        _snapshot: &OrderBookSnapshot,
        _position: i128,
    ) -> Result<Vec<Order>, String> {
        Ok(Vec::new())
    }
}

impl OrderBookStrategy for BuyOnce {
    fn on_order_book(
        &mut self,
        snapshot: &OrderBookSnapshot,
        _position: i128,
    ) -> Result<Vec<Order>, String> {
        if self.emitted {
            return Ok(Vec::new());
        }
        self.emitted = true;
        Ok(vec![Order {
            client_id: 1,
            instrument: snapshot.instrument.clone(),
            side: Side::Buy,
            qty: Quantity::from_i64(1),
            limit: None,
            status: OrderStatus::PendingSubmit,
            filled: Quantity::ZERO,
            account_id: "main".into(),
            trace: None,
            policy: None,
            exact_amounts: None,
        }])
    }
}

struct DerivativeBuy {
    emitted: bool,
}

impl OrderBookStrategy for DerivativeBuy {
    fn on_order_book(
        &mut self,
        snapshot: &OrderBookSnapshot,
        _position: i128,
    ) -> Result<Vec<Order>, String> {
        if self.emitted {
            return Ok(Vec::new());
        }
        self.emitted = true;
        Ok(vec![Order {
            client_id: 99,
            instrument: snapshot.instrument.clone(),
            side: Side::Buy,
            qty: Quantity::from_i64(1),
            limit: None,
            status: OrderStatus::PendingSubmit,
            filled: Quantity::ZERO,
            account_id: "main".into(),
            trace: None,
            policy: Some(OrderPolicy {
                margin_mode: MarginMode::Cross,
                position_mode: PositionMode::OneWay,
                position_side: PositionSide::Net,
                leverage: 10,
                ..OrderPolicy::default()
            }),
            exact_amounts: None,
        }])
    }
}

struct DerivativeDoubleBuy {
    emitted: u8,
}

impl OrderBookStrategy for DerivativeDoubleBuy {
    fn on_order_book(
        &mut self,
        snapshot: &OrderBookSnapshot,
        _position: i128,
    ) -> Result<Vec<Order>, String> {
        if self.emitted >= 2 {
            return Ok(Vec::new());
        }
        self.emitted += 1;
        let client_id = if self.emitted == 1 { 101 } else { 102 };
        Ok(vec![Order {
            client_id,
            instrument: snapshot.instrument.clone(),
            side: Side::Buy,
            qty: Quantity::from_i64(6),
            limit: None,
            status: OrderStatus::PendingSubmit,
            filled: Quantity::ZERO,
            account_id: "main".into(),
            trace: None,
            policy: Some(OrderPolicy {
                margin_mode: MarginMode::Cross,
                position_mode: PositionMode::OneWay,
                position_side: PositionSide::Net,
                leverage: 10,
                ..OrderPolicy::default()
            }),
            exact_amounts: None,
        }])
    }
}

fn snapshot(ts: u64, ask: i64) -> OrderBookSnapshot {
    OrderBookSnapshot {
        instrument: InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
        ts,
        sequence: ts,
        bids: vec![crate::BookLevel {
            price_exact: None,
            qty_exact: None,
            price: Price::from_i64(99),
            qty: Quantity::from_i64(2),
        }],
        asks: vec![crate::BookLevel {
            price_exact: None,
            qty_exact: None,
            price: Price::from_i64(ask),
            qty: Quantity::from_i64(2),
        }],
    }
}

#[test]
fn midpoint_reference_does_not_overflow_on_large_raw_prices() {
    let price = i128::MAX;
    let level = crate::BookLevel {
        price: qx_core::Price::from_raw(price),
        qty: Quantity::from_raw(1),
        price_exact: None,
        qty_exact: None,
    };
    let snapshot = OrderBookSnapshot {
        instrument: InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
        ts: 1,
        sequence: 1,
        bids: vec![level],
        asks: vec![level],
    };

    assert_eq!(
        super::reference_price(&snapshot),
        Some(qx_core::Price::from_raw(price))
    );
    assert_eq!(
        super::reference_price_exact(&snapshot),
        Some(ExactDecimal::from_parts(price, 9).unwrap())
    );
}

fn exact_snapshot(ts: u64) -> OrderBookSnapshot {
    OrderBookSnapshot {
        instrument: InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
        ts,
        sequence: ts,
        bids: vec![crate::BookLevel::from_exact(
            ExactDecimal::parse("100.0000000001").unwrap(),
            ExactDecimal::parse("0.0000000001").unwrap(),
        )
        .unwrap()],
        asks: vec![crate::BookLevel::from_exact(
            ExactDecimal::parse("100.0000000002").unwrap(),
            ExactDecimal::parse("0.0000000002").unwrap(),
        )
        .unwrap()],
    }
}

#[test]
fn order_book_backtest_is_causal_and_replays_fill() {
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let config = OrderBookBacktestConfig {
        instrument: instrument.clone(),
        account_id: "main".into(),
        currency: "USDT".into(),
        initial_cash: Money::from_i64(10_000),
        fee_bps: 1,
        instrument_spec: None,
        risk: RiskGate::conservative_default(),
        data_tier: DataTier::L2L3,
    };
    let mut strategy = BuyOnce { emitted: false };
    let report = OrderBookBacktestEngine::new(config)
        .run(&[snapshot(1, 100), snapshot(2, 101)], &mut strategy)
        .unwrap();
    assert_eq!(report.fills.len(), 1);
    assert_eq!(report.fills[0].ts, 2);
    assert_eq!(
        report
            .event_log
            .events()
            .iter()
            .filter(|event| matches!(&event.kind, EventKind::OrderSubmitted { .. }))
            .count(),
        1
    );
    // 深度链同一条纪律（V11 Q62）：事实源要被重新接受一遍并逐条还原出同一本账簿，
    // 而不是把同一段事件切片再哈希一遍。
    let (log, replayed) = ReplayVerifier::replay(report.event_log.events()).unwrap();
    assert_eq!(log.digest(), report.result_hash());
    assert_eq!(replayed.entries(), report.ledger.entries());
    let facts = ReplayVerifier::verify(report.event_log.events(), &report.ledger).unwrap();
    assert_eq!(facts.events, report.event_log.events().len());
    assert_eq!(facts.ledger_entries, report.ledger.entries().len());
    let mut explicit_default_strategy = BuyOnce { emitted: false };
    let explicit_default = OrderBookBacktestEngine::new(l2_config(1))
        .with_execution_model(OrderBookExecutionModel::new(1).unwrap())
        .run(
            &[snapshot(1, 100), snapshot(2, 101)],
            &mut explicit_default_strategy,
        )
        .unwrap();
    assert_eq!(report.result_hash(), explicit_default.result_hash());
}

#[test]
fn depth_backtest_rejects_exact_orders_for_legacy_strategies() {
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let config = OrderBookBacktestConfig {
        instrument: instrument.clone(),
        account_id: "main".into(),
        currency: "USDT".into(),
        initial_cash: Money::from_i64(10_000),
        fee_bps: 1,
        instrument_spec: None,
        risk: RiskGate::conservative_default(),
        data_tier: DataTier::L2L3,
    };
    let report = OrderBookBacktestEngine::new(config)
        .run(
            &[snapshot(1, 100), snapshot(2, 101)],
            &mut ExactBuyOnce { emitted: false },
        )
        .unwrap();

    assert!(report.fills.is_empty());
    assert!(report.event_log.events().iter().any(|event| matches!(
        &event.kind,
        EventKind::Rejected { client_order_id: 77, reason }
            if reason.contains("策略实现 exact hooks")
    )));
}

#[test]
fn depth_backtest_rejects_legacy_orders_on_exact_books_at_admission() {
    let report = OrderBookBacktestEngine::new(l2_config(0))
        .run(
            &[exact_snapshot(1), exact_snapshot(2)],
            &mut BuyOnce { emitted: false },
        )
        .unwrap();
    assert!(report.fills.is_empty());
    assert!(report.event_log.events().iter().any(|event| matches!(
        &event.kind,
        EventKind::Rejected { client_order_id: 1, reason }
            if reason.contains("要求精确数量订单")
    )));
}

#[test]
fn exact_spot_depth_order_runs_through_risk_reservation_fill_ledger_and_strategy_context() {
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let mut spec = TradingInstrumentSpec::spot_default(&instrument, "USDT");
    spec.base_currency = "BTC".into();
    spec.exact_rules = Some(ExactTradingRules {
        contract_size: ExactDecimal::ONE,
        price_tick: ExactDecimal::parse("0.0000000001").unwrap(),
        quantity_step: ExactDecimal::parse("0.0000000001").unwrap(),
        minimum_quantity: ExactDecimal::parse("0.0000000001").unwrap(),
        minimum_notional: ExactDecimal::ZERO,
    });
    let config = OrderBookBacktestConfig {
        instrument: instrument.clone(),
        account_id: "main".into(),
        currency: "USDT".into(),
        initial_cash: Money::from_i64(1),
        fee_bps: 100,
        instrument_spec: Some(spec),
        risk: RiskGate::conservative_default(),
        data_tier: DataTier::L2L3,
    };
    let mut strategy = ExactDepthBuyer::default();
    let report = OrderBookBacktestEngine::new(config)
        .run(&[exact_snapshot(1), exact_snapshot(2)], &mut strategy)
        .unwrap();

    assert_eq!(report.fills.len(), 1);
    assert_eq!(report.fills[0].qty.raw(), 0);
    assert_eq!(
        report.fills[0].exact_amounts.unwrap().quantity,
        ExactDecimal::parse("0.0000000002").unwrap()
    );
    assert_eq!(
        report
            .ledger
            .position_for_side_exact("main", &instrument, PositionSide::Net)
            .quantity,
        ExactDecimal::parse("0.0000000002").unwrap()
    );
    assert_eq!(
        report.ledger.cash_for_exact("main", "USDT"),
        ExactDecimal::parse("0.999999979799999999").unwrap()
    );
    assert_eq!(report.fees_raw, 0);
    assert_eq!(report.turnover_raw, 20);
    let exact_metrics = report.exact_metrics_json().unwrap();
    assert_eq!(exact_metrics["fees_by_currency"]["USDT"], "0.0000000002");
    assert_eq!(exact_metrics["turnover"]["amount"], "0.000000020000000001");
    assert!(exact_metrics["input_precision"]
        .as_str()
        .unwrap()
        .contains("exact risk checks"));
    assert_eq!(
        strategy.observed_positions.last().copied(),
        Some(ExactDecimal::parse("0.0000000002").unwrap())
    );
}

#[test]
fn exact_spot_quote_budget_is_reserved_capped_and_completed_at_current_price_dust() {
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let config = OrderBookBacktestConfig {
        instrument: instrument.clone(),
        account_id: "main".into(),
        currency: "USDT".into(),
        initial_cash: Money::from_i64(1),
        fee_bps: 100,
        instrument_spec: Some(exact_test_spot_spec(&instrument)),
        risk: RiskGate::conservative_default(),
        data_tier: DataTier::L2L3,
    };
    let mut expensive = exact_snapshot(2);
    expensive.asks = vec![crate::BookLevel::from_exact(
        ExactDecimal::parse("200.0000000000").unwrap(),
        ExactDecimal::parse("0.0000000002").unwrap(),
    )
    .unwrap()];
    let mut strategy = ExactQuoteBudgetBuyer::default();
    let report = OrderBookBacktestEngine::new(config)
        .run(&[exact_snapshot(1), expensive], &mut strategy)
        .unwrap();

    assert_eq!(report.fills.len(), 1);
    let fill = report.fills[0].exact_amounts.unwrap();
    assert_eq!(fill.quantity, ExactDecimal::parse("0.0000000001").unwrap());
    assert_eq!(
        fill.quote_cost,
        Some(ExactDecimal::parse("0.00000002").unwrap())
    );
    assert!(fill.quote_budget_complete);
    assert_eq!(
        report.ledger.cash_for_exact("main", "USDT"),
        ExactDecimal::parse("0.9999999798").unwrap()
    );
    assert_eq!(
        report
            .ledger
            .position_for_side_exact("main", &instrument, PositionSide::Net)
            .quantity,
        fill.quantity
    );
    assert!(report.event_log.events().iter().any(|event| matches!(
        &event.kind,
        EventKind::OrderSubmitted { order } if order.client_id == 79
    )));
}

#[test]
fn exact_spot_quote_budget_accumulates_cost_across_book_levels() {
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let config = OrderBookBacktestConfig {
        instrument: instrument.clone(),
        account_id: "main".into(),
        currency: "USDT".into(),
        initial_cash: Money::from_i64(1),
        fee_bps: 100,
        instrument_spec: Some(exact_test_spot_spec(&instrument)),
        risk: RiskGate::conservative_default(),
        data_tier: DataTier::L2L3,
    };
    let mut multi_level = exact_snapshot(2);
    multi_level.bids = vec![crate::BookLevel::from_exact(
        ExactDecimal::parse("99.0000000000").unwrap(),
        ExactDecimal::parse("0.0000000002").unwrap(),
    )
    .unwrap()];
    multi_level.asks = vec![
        crate::BookLevel::from_exact(
            ExactDecimal::parse("100.0000000000").unwrap(),
            ExactDecimal::parse("0.0000000002").unwrap(),
        )
        .unwrap(),
        crate::BookLevel::from_exact(
            ExactDecimal::parse("101.0000000000").unwrap(),
            ExactDecimal::parse("0.0000000002").unwrap(),
        )
        .unwrap(),
    ];
    let mut strategy = ExactQuoteBudgetBuyer::default();
    let report = OrderBookBacktestEngine::new(config)
        .run(&[exact_snapshot(1), multi_level], &mut strategy)
        .unwrap();

    assert_eq!(report.fills.len(), 2);
    assert_eq!(
        report.fills[0].exact_amounts.unwrap().quantity,
        ExactDecimal::parse("0.0000000002").unwrap()
    );
    assert!(!report.fills[0].exact_amounts.unwrap().quote_budget_complete);
    assert_eq!(
        report.fills[1].exact_amounts.unwrap().quantity,
        ExactDecimal::parse("0.0000000001").unwrap()
    );
    assert!(report.fills[1].exact_amounts.unwrap().quote_budget_complete);
    assert_eq!(
        report.ledger.cash_for_exact("main", "USDT"),
        ExactDecimal::parse("0.999999969599").unwrap()
    );
}

#[test]
fn exact_spot_quote_budget_keeps_cash_reserved_across_partial_snapshots() {
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let config = OrderBookBacktestConfig {
        instrument: instrument.clone(),
        account_id: "main".into(),
        currency: "USDT".into(),
        initial_cash: Money::from_i64(1),
        fee_bps: 100,
        instrument_spec: Some(exact_test_spot_spec(&instrument)),
        risk: RiskGate::conservative_default(),
        data_tier: DataTier::L2L3,
    };
    let mut strategy = ExactQuoteBudgetBuyer::default();
    let report = OrderBookBacktestEngine::new(config)
        .run(
            &[exact_snapshot(1), exact_snapshot(2), exact_snapshot(3)],
            &mut strategy,
        )
        .unwrap();

    assert_eq!(report.fills.len(), 2);
    assert_eq!(
        report.fills[0].exact_amounts.unwrap().quantity,
        ExactDecimal::parse("0.0000000002").unwrap()
    );
    assert!(!report.fills[0].exact_amounts.unwrap().quote_budget_complete);
    assert_eq!(
        report.fills[1].exact_amounts.unwrap().quantity,
        ExactDecimal::parse("0.0000000001").unwrap()
    );
    assert!(report.fills[1].exact_amounts.unwrap().quote_budget_complete);
    assert_eq!(
        report.ledger.cash_for_exact("main", "USDT"),
        ExactDecimal::parse("0.999999969699999998").unwrap()
    );
}

#[derive(Default)]
struct ExactDoubleBuyer {
    emitted: bool,
}

impl OrderBookStrategy for ExactDoubleBuyer {
    fn supports_exact_orders(&self) -> bool {
        true
    }

    fn on_order_book(
        &mut self,
        snapshot: &OrderBookSnapshot,
        _position: i128,
    ) -> Result<Vec<Order>, String> {
        if self.emitted {
            return Ok(Vec::new());
        }
        self.emitted = true;
        Ok([780_u64, 781_u64]
            .into_iter()
            .map(|client_id| Order {
                client_id,
                instrument: snapshot.instrument.clone(),
                side: Side::Buy,
                qty: Quantity::ZERO,
                limit: None,
                status: OrderStatus::PendingSubmit,
                filled: Quantity::ZERO,
                account_id: "main".into(),
                trace: None,
                policy: None,
                exact_amounts: Some(ExactOrderAmounts {
                    quantity: ExactDecimal::parse("0.006").unwrap(),
                    limit_price: None,
                    filled: ExactDecimal::ZERO,
                    quote_budget: None,
                    quote_filled: ExactDecimal::ZERO,
                    market_reference_price: None,
                }),
            })
            .collect())
    }

    fn on_order_book_actions_with_exact_position(
        &mut self,
        snapshot: &OrderBookSnapshot,
        position_raw: i128,
        _position_exact: ExactDecimal,
        _open_order_ids: &[u64],
        _available_margin_raw: Option<i128>,
    ) -> Result<OrderBookActions, String> {
        Ok(OrderBookActions {
            orders: self.on_order_book(snapshot, position_raw)?,
            cancel_client_ids: Vec::new(),
        })
    }
}

fn exact_test_spot_spec(instrument: &InstrumentId) -> TradingInstrumentSpec {
    let mut spec = TradingInstrumentSpec::spot_default(instrument, "USDT");
    spec.base_currency = "BTC".into();
    spec.exact_rules = Some(ExactTradingRules {
        contract_size: ExactDecimal::ONE,
        price_tick: ExactDecimal::parse("0.0000000001").unwrap(),
        quantity_step: ExactDecimal::parse("0.0000000001").unwrap(),
        minimum_quantity: ExactDecimal::parse("0.0000000001").unwrap(),
        minimum_notional: ExactDecimal::ZERO,
    });
    spec
}

fn exact_test_perpetual_spec(instrument: &InstrumentId) -> TradingInstrumentSpec {
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
        max_leverage: 10,
        maintenance_margin_bps: 500,
        valid_from: 1,
        valid_to: None,
        exact_rules: Some(ExactTradingRules {
            contract_size: ExactDecimal::ONE,
            price_tick: ExactDecimal::parse("0.0000000001").unwrap(),
            quantity_step: ExactDecimal::parse("0.0000000001").unwrap(),
            minimum_quantity: ExactDecimal::parse("0.0000000001").unwrap(),
            minimum_notional: ExactDecimal::ZERO,
        }),
    }
}

#[test]
fn exact_derivative_depth_uses_sub_nano_margin_settlement_fees_and_ledger() {
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let config = OrderBookBacktestConfig {
        instrument: instrument.clone(),
        account_id: "main".into(),
        currency: "USDT".into(),
        initial_cash: Money::from_raw(10),
        fee_bps: 100,
        instrument_spec: Some(exact_test_perpetual_spec(&instrument)),
        risk: RiskGate::conservative_default(),
        data_tier: DataTier::L2L3,
    };
    let mut strategy = ExactDerivativeBuyer {
        emitted: false,
        quantity: ExactDecimal::parse("0.0000000002").unwrap(),
    };
    let report = OrderBookBacktestEngine::new(config)
        .run(&[exact_snapshot(1), exact_snapshot(2)], &mut strategy)
        .unwrap();

    assert_eq!(report.fills.len(), 1);
    let fill = &report.fills[0];
    assert_eq!(fill.qty, Quantity::ZERO);
    let exact_fill = fill.exact_amounts.unwrap();
    assert_eq!(
        exact_fill.quantity,
        ExactDecimal::parse("0.0000000002").unwrap()
    );
    assert_eq!(exact_fill.quote_cost, None);
    assert_eq!(fill.exact_fees.len(), 1);
    assert_eq!(fill.exact_fees[0].currency.as_deref(), Some("USDT"));
    assert_eq!(
        report
            .ledger
            .position_for_side_exact("main", &instrument, PositionSide::Net)
            .quantity,
        exact_fill.quantity
    );
    assert_eq!(
        report.ledger.cash_for_exact("main", "USDT"),
        ExactDecimal::parse("0.0000000098").unwrap()
    );
    assert_eq!(
        report.exact_metrics_json().unwrap()["fees_by_currency"]["USDT"],
        fill.exact_fees[0].amount.to_string()
    );
}

#[test]
fn exact_derivative_depth_rejects_margin_that_raw_quantity_projection_would_hide() {
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let config = OrderBookBacktestConfig {
        instrument: instrument.clone(),
        account_id: "main".into(),
        currency: "USDT".into(),
        initial_cash: Money::from_raw(1),
        fee_bps: 100,
        instrument_spec: Some(exact_test_perpetual_spec(&instrument)),
        risk: RiskGate::conservative_default(),
        data_tier: DataTier::L2L3,
    };
    let mut strategy = ExactDerivativeBuyer {
        emitted: false,
        quantity: ExactDecimal::parse("0.0000000002").unwrap(),
    };
    let report = OrderBookBacktestEngine::new(config)
        .run(&[exact_snapshot(1), exact_snapshot(2)], &mut strategy)
        .unwrap();

    assert!(report.fills.is_empty());
    assert!(report.event_log.events().iter().any(|event| matches!(
        &event.kind,
        EventKind::Rejected { client_order_id: 780, reason }
            if reason.contains("初始保证金")
    )));
}

#[test]
fn exact_derivative_depth_fails_closed_when_fill_slippage_exceeds_margin() {
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let config = OrderBookBacktestConfig {
        instrument: instrument.clone(),
        account_id: "main".into(),
        currency: "USDT".into(),
        initial_cash: Money::from_raw(10),
        fee_bps: 100,
        instrument_spec: Some(exact_test_perpetual_spec(&instrument)),
        risk: RiskGate::conservative_default(),
        data_tier: DataTier::L2L3,
    };
    let mut slipped_snapshot = exact_snapshot(2);
    slipped_snapshot.asks[0] = crate::BookLevel::from_exact(
        ExactDecimal::parse("1000").unwrap(),
        ExactDecimal::parse("0.0000000002").unwrap(),
    )
    .unwrap();
    let mut strategy = ExactDerivativeBuyer {
        emitted: false,
        quantity: ExactDecimal::parse("0.0000000002").unwrap(),
    };

    let error = match OrderBookBacktestEngine::new(config)
        .run(&[exact_snapshot(1), slipped_snapshot], &mut strategy)
    {
        Ok(_) => panic!("滑点导致保证金不足时应拒绝生成回测报告"),
        Err(error) => error,
    };

    assert!(error.to_string().contains("成交后精确组合保证金不足"));
}

#[test]
fn exact_inverse_derivative_depth_keeps_base_settlement_and_fee_currency() {
    let instrument = InstrumentId::parse("BTCUSD.BINANCE").unwrap();
    let mut spec = exact_test_perpetual_spec(&instrument);
    spec.quote_currency = "USD".into();
    spec.settlement_currency = "BTC".into();
    spec.linear = false;
    spec.inverse = true;
    let config = OrderBookBacktestConfig {
        instrument: instrument.clone(),
        account_id: "main".into(),
        currency: "BTC".into(),
        initial_cash: Money::from_raw(1),
        fee_bps: 100,
        instrument_spec: Some(spec),
        risk: RiskGate::conservative_default(),
        data_tier: DataTier::L2L3,
    };
    let mut snapshots = vec![exact_snapshot(1), exact_snapshot(2)];
    for snapshot in &mut snapshots {
        snapshot.instrument = instrument.clone();
    }
    let mut strategy = ExactDerivativeBuyer {
        emitted: false,
        quantity: ExactDecimal::parse("0.0000000002").unwrap(),
    };
    let report = OrderBookBacktestEngine::new(config)
        .run(&snapshots, &mut strategy)
        .unwrap();

    assert_eq!(report.fills.len(), 1);
    let fill = &report.fills[0];
    assert_eq!(fill.exact_fees[0].currency.as_deref(), Some("BTC"));
    assert_eq!(
        report
            .ledger
            .position_for_side_exact("main", &instrument, PositionSide::Net)
            .quantity,
        ExactDecimal::parse("0.0000000002").unwrap()
    );
    assert_eq!(
        report.ledger.cash_for_exact("main", "BTC"),
        ExactDecimal::parse("0.00000000099998").unwrap()
    );
    assert!(report.exact_metrics_json().unwrap()["fees_by_currency"]
        .get("BTC")
        .is_some());
}

#[derive(Default)]
struct ExactBuyThenCompetingSells {
    stage: u8,
}

impl OrderBookStrategy for ExactBuyThenCompetingSells {
    fn supports_exact_orders(&self) -> bool {
        true
    }

    fn on_order_book(
        &mut self,
        _snapshot: &OrderBookSnapshot,
        _position: i128,
    ) -> Result<Vec<Order>, String> {
        Ok(Vec::new())
    }

    fn on_order_book_actions_with_exact_position(
        &mut self,
        snapshot: &OrderBookSnapshot,
        _position_raw: i128,
        position_exact: ExactDecimal,
        _open_order_ids: &[u64],
        _available_margin_raw: Option<i128>,
    ) -> Result<OrderBookActions, String> {
        let order = |client_id, side| Order {
            client_id,
            instrument: snapshot.instrument.clone(),
            side,
            qty: Quantity::ZERO,
            limit: None,
            status: OrderStatus::PendingSubmit,
            filled: Quantity::ZERO,
            account_id: "main".into(),
            trace: None,
            policy: None,
            exact_amounts: Some(ExactOrderAmounts {
                quantity: ExactDecimal::parse("0.0000000002").unwrap(),
                limit_price: None,
                filled: ExactDecimal::ZERO,
                quote_budget: None,
                quote_filled: ExactDecimal::ZERO,
                market_reference_price: None,
            }),
        };
        let orders = if self.stage == 0 {
            self.stage = 1;
            vec![order(880, Side::Buy)]
        } else if self.stage == 1 && position_exact.is_positive() {
            self.stage = 2;
            vec![order(900, Side::Sell), order(901, Side::Sell)]
        } else {
            Vec::new()
        };
        Ok(OrderBookActions {
            orders,
            cancel_client_ids: Vec::new(),
        })
    }
}

#[test]
fn exact_spot_sell_reserves_inventory_across_competing_orders() {
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let config = OrderBookBacktestConfig {
        instrument: instrument.clone(),
        account_id: "main".into(),
        currency: "USDT".into(),
        initial_cash: Money::from_i64(1),
        fee_bps: 100,
        instrument_spec: Some(exact_test_spot_spec(&instrument)),
        risk: RiskGate::conservative_default(),
        data_tier: DataTier::L2L3,
    };
    let make_snapshot = |ts| OrderBookSnapshot {
        instrument: instrument.clone(),
        ts,
        sequence: ts,
        bids: vec![crate::BookLevel::from_exact(
            ExactDecimal::parse("100.0000000001").unwrap(),
            ExactDecimal::parse("0.0000000002").unwrap(),
        )
        .unwrap()],
        asks: vec![crate::BookLevel::from_exact(
            ExactDecimal::parse("100.0000000002").unwrap(),
            ExactDecimal::parse("0.0000000002").unwrap(),
        )
        .unwrap()],
    };
    let report = OrderBookBacktestEngine::new(config)
        .run(
            &[make_snapshot(1), make_snapshot(2), make_snapshot(3)],
            &mut ExactBuyThenCompetingSells::default(),
        )
        .unwrap();
    assert_eq!(report.fills.len(), 2);
    assert_eq!(report.fills[0].order_id, 880);
    assert_eq!(report.fills[1].order_id, 900);
    assert!(report.event_log.events().iter().any(|event| matches!(
        &event.kind,
        EventKind::Rejected { client_order_id: 901, reason }
            if reason.contains("扣除待成交卖单后的可用持仓")
    )));
    assert_eq!(
        report
            .ledger
            .position_for_side_exact("main", &instrument, PositionSide::Net)
            .quantity,
        ExactDecimal::ZERO
    );
    let metrics = report.exact_metrics_json().unwrap();
    assert_eq!(metrics["fees_by_currency"]["USDT"], "0.0000000004");
    assert_eq!(metrics["turnover"]["amount"], "0.000000040000000002");
}

#[test]
fn exact_spot_pending_orders_reserve_shared_cash_before_acceptance() {
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let config = OrderBookBacktestConfig {
        instrument: instrument.clone(),
        account_id: "main".into(),
        currency: "USDT".into(),
        initial_cash: Money::from_i64(1),
        fee_bps: 100,
        instrument_spec: Some(exact_test_spot_spec(&instrument)),
        risk: RiskGate::conservative_default(),
        data_tier: DataTier::L2L3,
    };
    let make_snapshot = |ts| OrderBookSnapshot {
        instrument: instrument.clone(),
        ts,
        sequence: ts,
        bids: vec![crate::BookLevel::from_exact(
            ExactDecimal::parse("100").unwrap(),
            ExactDecimal::parse("0.01").unwrap(),
        )
        .unwrap()],
        asks: vec![crate::BookLevel::from_exact(
            ExactDecimal::parse("100.0000000001").unwrap(),
            ExactDecimal::parse("0.01").unwrap(),
        )
        .unwrap()],
    };
    let report = OrderBookBacktestEngine::new(config)
        .run(
            &[make_snapshot(1), make_snapshot(2)],
            &mut ExactDoubleBuyer::default(),
        )
        .unwrap();
    let submitted = report
        .event_log
        .events()
        .iter()
        .filter(|event| matches!(&event.kind, EventKind::OrderSubmitted { .. }))
        .count();
    let rejected_ids = report
        .event_log
        .events()
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::Rejected {
                client_order_id,
                reason,
            } if reason.contains("超过订单簿回测可用现金") => Some(*client_order_id),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(submitted, 1);
    assert_eq!(rejected_ids, vec![781]);
    assert_eq!(report.fills.len(), 1);
    assert_eq!(
        report.ledger.cash_for_exact("main", "USDT"),
        ExactDecimal::parse("0.393999999999394").unwrap()
    );
}

#[test]
fn exact_spot_order_rejects_queue_position_estimation() {
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let config = OrderBookBacktestConfig {
        instrument: instrument.clone(),
        account_id: "main".into(),
        currency: "USDT".into(),
        initial_cash: Money::from_i64(1),
        fee_bps: 100,
        instrument_spec: Some(exact_test_spot_spec(&instrument)),
        risk: RiskGate::conservative_default(),
        data_tier: DataTier::L2L3,
    };
    let report = OrderBookBacktestEngine::new(config)
        .with_execution_model(crate::OrderBookExecutionModel {
            fee_bps: 100,
            latency_snapshots: 0,
            queue_position_bps: 100,
            queue_position_model: crate::QueuePositionModel::Disabled,
            market_impact_bps: 0,
        })
        .run(
            &[exact_snapshot(1), exact_snapshot(2)],
            &mut ExactDepthBuyer::default(),
        )
        .unwrap();
    assert!(report.fills.is_empty());
    assert!(report.event_log.events().iter().any(|event| matches!(
        &event.kind,
        EventKind::Rejected { client_order_id: 78, reason }
            if reason.contains("暂不支持队列位置估算模型")
    )));
}

/// 名义额远超可用现金的现货买单：用来验证深度链不会把账簿现金透支成负数。
struct BigBuyOnce {
    emitted: bool,
}

impl OrderBookStrategy for BigBuyOnce {
    fn on_order_book(
        &mut self,
        snapshot: &OrderBookSnapshot,
        _position: i128,
    ) -> Result<Vec<Order>, String> {
        if self.emitted {
            return Ok(Vec::new());
        }
        self.emitted = true;
        Ok(vec![Order {
            client_id: 7,
            instrument: snapshot.instrument.clone(),
            side: Side::Buy,
            qty: Quantity::from_i64(1_000),
            limit: None,
            status: OrderStatus::PendingSubmit,
            filled: Quantity::ZERO,
            account_id: "main".into(),
            trace: None,
            policy: None,
            exact_amounts: None,
        }])
    }
}

/// 没有 market spec 的深度回测同样是"现货买入只能用可用现金"。Bar 链的
/// `cash_funded_spot_buy` 分支一直这么挡；这条用例钉住两链同口径，否则一次超买
/// 会静默把账簿现金透支成负数，同一份信号在两条链上给出两种结论。
#[test]
fn specless_spot_buy_beyond_available_cash_is_rejected_not_overdrawn() {
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let config = OrderBookBacktestConfig {
        instrument,
        account_id: "main".into(),
        currency: "USDT".into(),
        initial_cash: Money::from_i64(10_000),
        fee_bps: 50,
        instrument_spec: None,
        risk: RiskGate::conservative_default(),
        data_tier: DataTier::L2L3,
    };
    let mut strategy = BigBuyOnce { emitted: false };
    let report = OrderBookBacktestEngine::new(config)
        .run(&[snapshot(1, 100), snapshot(2, 101)], &mut strategy)
        .unwrap();
    assert!(
        report.fills.is_empty(),
        "1 万现金成交了 10 万名义额: {:?}",
        report.fills
    );
    assert_eq!(report.turnover_raw, 0);
    assert_eq!(report.fees_raw, 0);
    assert_eq!(
        report.ledger.cash_for("main", "USDT"),
        Money::from_i64(10_000).raw(),
        "现金被透支，说明买入没有过现金门"
    );
    let reasons: Vec<String> = report
        .event_log
        .events()
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::Rejected { reason, .. } => Some(reason.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        reasons,
        vec!["买入成本（含预估手续费）超过订单簿回测可用现金".to_string()],
        "拒单事实没有进入事件日志"
    );
}

#[test]
fn derivative_order_book_uses_margin_ledger_semantics() {
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let spec = TradingInstrumentSpec {
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
    };
    let config = OrderBookBacktestConfig {
        instrument,
        account_id: "main".into(),
        currency: "USDT".into(),
        initial_cash: Money::from_i64(10_000),
        fee_bps: 0,
        instrument_spec: Some(spec),
        risk: RiskGate::conservative_default(),
        data_tier: DataTier::L2L3,
    };
    let mut strategy = DerivativeBuy { emitted: false };
    let report = OrderBookBacktestEngine::new(config)
        .run(&[snapshot(1, 100), snapshot(2, 101)], &mut strategy)
        .unwrap();
    assert_eq!(report.fills.len(), 1);
    assert_eq!(
        report.ledger.cash_for("main", "USDT"),
        Money::from_i64(10_000).raw()
    );
    assert_eq!(
        report
            .ledger
            .position_for("main", &InstrumentId::parse("BTCUSDT.BINANCE").unwrap())
            .quantity
            .raw(),
        Quantity::from_i64(1).raw()
    );
}

#[test]
fn derivative_orders_share_one_projected_portfolio_margin_budget() {
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let spec = TradingInstrumentSpec {
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
    };
    let config = OrderBookBacktestConfig {
        instrument,
        account_id: "main".into(),
        currency: "USDT".into(),
        initial_cash: Money::from_i64(100),
        fee_bps: 0,
        instrument_spec: Some(spec),
        risk: RiskGate::conservative_default(),
        data_tier: DataTier::L2L3,
    };
    let mut strategy = DerivativeDoubleBuy { emitted: 0 };
    let report = OrderBookBacktestEngine::new(config)
        .run(&[snapshot(1, 100), snapshot(2, 101)], &mut strategy)
        .unwrap();
    let submitted = report
        .event_log
        .events()
        .iter()
        .filter(|event| matches!(&event.kind, EventKind::OrderSubmitted { .. }))
        .count();
    assert_eq!(submitted, 1, "只应提交账户可承受的第一笔订单");
    let rejected = report
        .event_log
        .events()
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::Rejected {
                client_order_id: 102,
                reason,
            } => Some(reason),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(rejected.len(), 1);
    assert!(rejected[0].contains("初始保证金超过账户可用保证金"));
}

/// 小额合约乘数的盘口回测必须按真实名义额计费：Ledger 用
/// `spec.notional` 记账，若费用仍按 `qty * price` 计，0.001 合约的
/// 手续费会被高估 1000 倍。
#[test]
fn fractional_contract_sizes_fee_on_spec_notional() {
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let spec = TradingInstrumentSpec {
        instrument: instrument.clone(),
        product: TradingProduct::Perpetual,
        base_currency: "BTC".into(),
        quote_currency: "USDT".into(),
        settlement_currency: "USDT".into(),
        contract_size: SCALE / 1000,
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
    };
    let config = OrderBookBacktestConfig {
        instrument,
        account_id: "main".into(),
        currency: "USDT".into(),
        initial_cash: Money::from_i64(10_000),
        fee_bps: 100,
        instrument_spec: Some(spec),
        risk: RiskGate::conservative_default(),
        data_tier: DataTier::L2L3,
    };
    let mut strategy = DerivativeBuy { emitted: false };
    let report = OrderBookBacktestEngine::new(config)
        .run(&[snapshot(1, 100), snapshot(2, 100)], &mut strategy)
        .unwrap();
    assert_eq!(report.fills.len(), 1);
    // 1 张 * 0.001 BTC * 100 USDT = 0.1 USDT 名义额，1% 费率 = 0.001
    assert_eq!(report.fills[0].fee.raw(), SCALE / 1000);
}

/// 执行模型不能静默覆盖配置里的费率，否则成本口径与声明不一致。
#[test]
fn conflicting_execution_model_fee_is_rejected_not_silently_applied() {
    let config = OrderBookBacktestConfig {
        instrument: InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
        account_id: "main".into(),
        currency: "USDT".into(),
        initial_cash: Money::from_i64(10_000),
        fee_bps: 5,
        instrument_spec: None,
        risk: RiskGate::conservative_default(),
        data_tier: DataTier::L2L3,
    };
    let mut strategy = DerivativeBuy { emitted: false };
    let error = OrderBookBacktestEngine::new(config)
        .with_execution_model(OrderBookExecutionModel::new(7).unwrap())
        .run(&[snapshot(1, 100), snapshot(2, 100)], &mut strategy)
        .err()
        .expect("费率冲突必须报错");
    assert!(
        matches!(error, qx_core::QxError::BusinessViolation(_))
            && error.to_string().contains("fee_bps"),
        "错误信息应指出冲突字段: {error}"
    );
}

fn l2_config(fee_bps: i128) -> OrderBookBacktestConfig {
    OrderBookBacktestConfig {
        instrument: InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
        account_id: "main".into(),
        currency: "USDT".into(),
        initial_cash: Money::from_i64(10_000),
        fee_bps,
        instrument_spec: None,
        risk: RiskGate::conservative_default(),
        data_tier: DataTier::L2L3,
    }
}

fn book_context() -> qx_strategy::StrategyContext {
    qx_strategy::StrategyContext {
        strategy_id: "depth-book".into(),
        strategy_version: "v1".into(),
        account_id: "main".into(),
        venue_id: "paper".into(),
        data_fingerprint: "book-1".into(),
        as_of: 1,
        positions_exact: std::collections::BTreeMap::new(),
        cash_exact: std::collections::BTreeMap::new(),
        available_margin_exact: None,
        positions: std::collections::BTreeMap::new(),
        cash: std::collections::BTreeMap::new(),
        available_margin_raw: Some(10_000),
        risk_state: "ready".into(),
    }
}

#[test]
fn order_book_report_is_deterministic_and_carries_unified_metrics() {
    let snapshots = [snapshot(1, 100), snapshot(2, 101), snapshot(3, 102)];
    let first = OrderBookBacktestEngine::new(l2_config(1))
        .run(&snapshots, &mut BuyOnce { emitted: false })
        .unwrap();
    let second = OrderBookBacktestEngine::new(l2_config(1))
        .run(&snapshots, &mut BuyOnce { emitted: false })
        .unwrap();
    assert_eq!(first.result_hash(), second.result_hash());
    assert_eq!(first.equity, second.equity);
    assert_eq!(first.snapshot_ts, vec![1, 2, 3]);
    assert_eq!(first.positions.len(), first.equity.len());
    assert_eq!(
        first.fees_raw,
        first.fills.iter().map(|fill| fill.fee.raw()).sum::<i128>()
    );
    assert!(first.turnover_raw > 0);
    assert_eq!(first.clock_start, 1);
    assert_eq!(first.clock_end, 3);
    assert_eq!(first.final_equity(), *first.equity.last().unwrap());
    assert!(first
        .model_descriptors
        .iter()
        .any(|descriptor| descriptor == "data_tier=L2L3"));
    // 缺省口径要逐字写进描述子，非缺省口径要同时改变描述子与结果哈希：命令面那条
    // 断言挡住"配了却不生效"，这一条挡住"生效了却没写进产物"。
    assert!(first.model_descriptors.iter().any(|descriptor| descriptor
        .starts_with("orderbook-matching@v2 fee_bps=1 latency_snapshots=0 queue_position_bps=0 queue_position=disabled market_impact_bps=0")));
    let modeled = OrderBookBacktestEngine::new(l2_config(1))
        .with_execution_model(OrderBookExecutionModel {
            fee_bps: 1,
            latency_snapshots: 2,
            queue_position_bps: 500,
            queue_position_model: crate::QueuePositionModel::Disabled,
            market_impact_bps: 10,
        })
        .run(&snapshots, &mut BuyOnce { emitted: false })
        .unwrap();
    assert!(modeled.model_descriptors.iter().any(|descriptor| descriptor
        .starts_with("orderbook-matching@v2 fee_bps=1 latency_snapshots=2 queue_position_bps=500 queue_position=disabled market_impact_bps=10")));
    assert_ne!(
        modeled.result_hash(),
        first.result_hash(),
        "延迟与冲击参数没有改变撮合结果"
    );
    let manifest = first
        .run_manifest(
            RunManifestIdentity {
                run_id: "book-test:run",
                code_commit: "workspace",
                config_hash: "config-hash",
                strategy_version: "v1",
                instrument_spec_version: "default-instrument-spec-v1",
                runtime_version: "runtime-schema-1",
            },
            "depth:test",
        )
        .unwrap();
    assert_eq!(manifest.clock_start, 1);
    assert_eq!(manifest.clock_end, 3);
    assert_eq!(
        manifest.input_event_hash,
        format!("{:016x}", first.input_data_hash)
    );
    assert_eq!(
        manifest.result_hash,
        format!("{:016x}", first.result_hash())
    );
    let changed = OrderBookBacktestEngine::new(l2_config(50))
        .run(&snapshots, &mut BuyOnce { emitted: false })
        .unwrap();
    assert_ne!(changed.result_hash(), first.result_hash());
    assert!(changed.fees_raw > first.fees_raw);
}

#[test]
fn depth_backtest_input_hash_binds_exact_book_values_beyond_raw_projection() {
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let run = |bid: &str, ask: &str| {
        let make_snapshot = |ts| OrderBookSnapshot {
            instrument: instrument.clone(),
            ts,
            sequence: ts,
            bids: vec![crate::BookLevel::from_exact(
                ExactDecimal::parse(bid).unwrap(),
                ExactDecimal::parse("0.0000000001").unwrap(),
            )
            .unwrap()],
            asks: vec![crate::BookLevel::from_exact(
                ExactDecimal::parse(ask).unwrap(),
                ExactDecimal::parse("0.0000000002").unwrap(),
            )
            .unwrap()],
        };
        let snapshots = [make_snapshot(1), make_snapshot(2)];
        assert_eq!(snapshots[0].bids[0].price.raw(), 99 * SCALE);
        assert_eq!(snapshots[0].bids[0].qty.raw(), 0);
        OrderBookBacktestEngine::new(l2_config(0))
            .run(&snapshots, &mut NoOrders)
            .unwrap()
    };
    let first = run("99.0000000001", "100.0000000001");
    let changed = run("99.0000000002", "100.0000000002");
    assert_ne!(first.input_data_hash, changed.input_data_hash);
}
