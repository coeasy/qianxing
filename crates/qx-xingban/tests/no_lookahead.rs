use qx_core::{InstrumentId, Money, Order, OrderStatus, Quantity, Side};
use qx_guanxing::Bar;
use qx_risk::MaxNotionalRule;
use qx_xingban::{
    BacktestConfig, BacktestEngine, BarStrategy, DataTier, MakerTakerFeeModel,
    NextBarOpenFillModel, NoMargin, VirtualTradingConfig, ZeroLatency,
};
use qx_zhenlu::RiskGate;

struct BuyOnce {
    done: bool,
}

impl BarStrategy for BuyOnce {
    fn on_bar(
        &mut self,
        _history: &[Bar],
        instrument: &InstrumentId,
        _ts: u64,
        position: i128,
    ) -> Option<Order> {
        if self.done || position != 0 {
            return None;
        }
        self.done = true;
        Some(Order {
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
        })
    }
}

#[test]
fn pre_trade_risk_uses_last_visible_close_not_current_bar_close() {
    let instrument = InstrumentId::parse("T.SIM").unwrap();
    let mut risk = RiskGate::from_rule_set(qx_risk::RuleSet::account_limits_only());
    risk.add(Box::new(MaxNotionalRule { max_notional: 150 }));
    let config = BacktestConfig {
        instrument,
        instrument_spec: None,
        account_id: "main".into(),
        currency: "USD".into(),
        initial_cash: Money::from_i64(1_000),
        multiplier: 1,
        fill: Box::new(NextBarOpenFillModel),
        fee: Box::new(MakerTakerFeeModel {
            maker_bp: 0,
            taker_bp: 0,
        }),
        data_tier: DataTier::Bar,
        latency: Box::new(ZeroLatency),
        margin: Box::new(NoMargin),
        seed: 7,
        risk,
        virtual_trading: VirtualTradingConfig::default(),
    };

    // At t=2 the strategy is only allowed to observe t=1, whose close is 100.
    // The t=2 close jumps to 1000. If pre-trade risk incorrectly reads that
    // future close, MaxNotional(150) rejects the order. Correct causal risk
    // uses the last visible close=100 and the order can fill at t=2 open=110.
    let bars = vec![
        Bar::new(1, 100, 110, 90, 100, 10),
        Bar::new(2, 110, 1_000, 100, 1_000, 10),
    ];
    let report = BacktestEngine::new(config)
        .run(&bars, &mut BuyOnce { done: false })
        .expect("causal backtest should complete");

    assert_eq!(
        report.fills.len(),
        1,
        "future close must not reject the order"
    );
    assert_eq!(report.fills[0].price.raw(), 110);
}
