use super::*;
use qx_risk::{MaxNotionalRule, RuleSet};

struct TwoBuysOnce {
    emitted: bool,
}

impl OrderBookStrategy for TwoBuysOnce {
    fn on_order_book(
        &mut self,
        snapshot: &OrderBookSnapshot,
        _position: i128,
    ) -> Result<Vec<Order>, String> {
        if self.emitted {
            return Ok(Vec::new());
        }
        self.emitted = true;
        Ok([101, 102]
            .into_iter()
            .map(|client_id| Order {
                client_id,
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
            })
            .collect())
    }
}

#[test]
fn orderbook_pending_orders_count_toward_max_notional_before_fill() {
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let spec = TradingInstrumentSpec {
        instrument: instrument.clone(),
        product: TradingProduct::Spot,
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
        max_leverage: 1,
        maintenance_margin_bps: 0,
        valid_from: 1,
        valid_to: None,
        exact_rules: None,
    };
    let mut rules = RuleSet::with_version("pending-exposure-test-v1");
    rules.add(Box::new(MaxNotionalRule {
        max_notional: 150 * SCALE,
    }));
    let config = OrderBookBacktestConfig {
        instrument,
        account_id: "main".into(),
        currency: "USDT".into(),
        initial_cash: Money::from_i64(1_000),
        fee_bps: 0,
        instrument_spec: Some(spec),
        risk: RiskGate::from_rule_set(rules),
        data_tier: DataTier::L2L3,
    };
    let mut strategy = TwoBuysOnce { emitted: false };
    let report = OrderBookBacktestEngine::new(config)
        .run(&[snapshot(1, 100), snapshot(2, 100)], &mut strategy)
        .unwrap();

    assert_eq!(
        report.fills.len(),
        1,
        "限额 150 下只允许一笔约 100 的订单成交"
    );
    assert_eq!(
        report
            .ledger
            .position_for("main", &InstrumentId::parse("BTCUSDT.BINANCE").unwrap())
            .quantity
            .raw(),
        Quantity::from_i64(1).raw()
    );
    assert_eq!(
        report
            .event_log
            .events()
            .iter()
            .filter(|event| matches!(event.kind, qx_core::EventKind::Rejected { .. }))
            .count(),
        1,
        "第二个订单须在第一单尚未成交时被拒绝"
    );
    ReplayVerifier::verify(report.event_log.events(), &report.ledger).unwrap();
}
