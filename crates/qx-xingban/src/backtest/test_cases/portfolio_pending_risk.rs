use super::*;
use crate::{BarStrategy, PortfolioBarStrategy, PortfolioStrategySlot};
use qx_core::{InstrumentId, Order, OrderStatus, TradingInstrumentSpec, TradingProduct};
use qx_guanxing::Bar;
use qx_risk::{MaxNotionalRule, RuleSet};

struct OneShotBuyer {
    submitted: bool,
}

impl BarStrategy for OneShotBuyer {
    fn on_bar_orders_checked(
        &mut self,
        _history: &[Bar],
        instrument: &InstrumentId,
        _ts: u64,
        _position: i128,
    ) -> Result<Vec<Order>, qx_core::QxError> {
        if self.submitted {
            return Ok(Vec::new());
        }
        self.submitted = true;
        Ok(vec![Order {
            client_id: 0,
            instrument: instrument.clone(),
            side: Side::Buy,
            qty: Quantity::from_i64(1),
            limit: None,
            status: OrderStatus::Submitted,
            filled: Quantity::ZERO,
            account_id: "main".into(),
            trace: None,
            policy: None,
        }])
    }
}

fn spot_spec(instrument: InstrumentId) -> TradingInstrumentSpec {
    TradingInstrumentSpec {
        instrument,
        product: TradingProduct::Spot,
        base_currency: "BTC".into(),
        quote_currency: "USD".into(),
        settlement_currency: "USD".into(),
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
    }
}

#[test]
fn portfolio_strategy_orders_count_toward_max_notional_before_fill() {
    let instrument = InstrumentId::parse("BTC/USD.SIM").unwrap();
    let mut config = simple_config();
    config.instrument = instrument.clone();
    config.currency = "USD".into();
    config.initial_cash = Money::from_i64(1_000);
    config.instrument_spec = Some(spot_spec(instrument.clone()));
    let mut rules = RuleSet::with_version("pending-exposure-test-v1");
    rules.add(Box::new(MaxNotionalRule {
        max_notional: 150 * SCALE,
    }));
    config.risk = RiskGate::from_rule_set(rules);
    let mut strategy = PortfolioBarStrategy::new(vec![
        PortfolioStrategySlot::new(0, "alpha", 500 * SCALE, OneShotBuyer { submitted: false }),
        PortfolioStrategySlot::new(1, "beta", 500 * SCALE, OneShotBuyer { submitted: false }),
    ])
    .unwrap();
    let bars = [
        Bar::new(1, 100 * SCALE, 101 * SCALE, 99 * SCALE, 100 * SCALE, SCALE),
        Bar::new(2, 100 * SCALE, 101 * SCALE, 99 * SCALE, 100 * SCALE, SCALE),
        Bar::new(3, 100 * SCALE, 101 * SCALE, 99 * SCALE, 100 * SCALE, SCALE),
    ];

    let report = BacktestEngine::new(config)
        .run(&bars, &mut strategy)
        .unwrap();
    assert_eq!(
        report.fills.len(),
        1,
        "限额 150 下只允许一个名义额 100 的订单成交"
    );
    assert_eq!(
        report
            .ledger
            .position_for("main", &instrument)
            .quantity
            .raw(),
        Quantity::from_i64(1).raw()
    );
    assert_eq!(
        report
            .event_log
            .events()
            .iter()
            .filter(|event| matches!(event.kind, EventKind::Rejected { .. }))
            .count(),
        1,
        "第二个策略订单须在第一单尚未成交时被拒绝"
    );
    assert_replay_matches(&report);
}
