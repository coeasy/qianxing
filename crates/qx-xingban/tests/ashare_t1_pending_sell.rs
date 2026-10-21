use qx_core::{InstrumentId, Money, Order, OrderStatus, Quantity, Side, SCALE};
use qx_guanxing::Bar;
use qx_xingban::ashare::AshareRuleConfig;
use qx_xingban::{
    BacktestConfig, BacktestEngine, BarStrategy, DataTier, MakerTakerFeeModel,
    NextBarOpenFillModel, NoMargin, VirtualTradingConfig, ZeroLatency,
};
use qx_zhenlu::RiskGate;

const DAY_MS: u64 = 86_400_000;

struct DuplicateSellStrategy {
    buy_sent: bool,
    sells_sent: bool,
    buy_ts: u64,
    sell_ts: u64,
}

struct PendingSellStrategy {
    buy_sent: bool,
    first_sell_sent: bool,
    second_sell_sent: bool,
    buy_ts: u64,
    first_sell_ts: u64,
    second_sell_ts: u64,
}

impl BarStrategy for PendingSellStrategy {
    fn on_bar_orders(
        &mut self,
        _history: &[Bar],
        instrument: &InstrumentId,
        ts: u64,
        _position: i128,
    ) -> Vec<Order> {
        let order = if ts == self.buy_ts && !self.buy_sent {
            self.buy_sent = true;
            Some((Side::Buy, None))
        } else if ts == self.first_sell_ts && !self.first_sell_sent {
            self.first_sell_sent = true;
            Some((Side::Sell, Some(11 * SCALE)))
        } else if ts == self.second_sell_ts && !self.second_sell_sent {
            self.second_sell_sent = true;
            Some((Side::Sell, None))
        } else {
            None
        };
        order
            .into_iter()
            .map(|(side, limit)| Order {
                client_id: 0,
                instrument: instrument.clone(),
                side,
                qty: Quantity::from_i64(200),
                limit: limit.map(qx_core::Price::from_raw),
                status: OrderStatus::PendingSubmit,
                filled: Quantity::ZERO,
                account_id: "main".into(),
                trace: None,
                policy: None,
            })
            .collect()
    }
}

impl BarStrategy for DuplicateSellStrategy {
    fn on_bar_orders(
        &mut self,
        _history: &[Bar],
        instrument: &InstrumentId,
        ts: u64,
        _position: i128,
    ) -> Vec<Order> {
        let sides = if ts == self.buy_ts && !self.buy_sent {
            self.buy_sent = true;
            vec![(Side::Buy, 200)]
        } else if ts == self.sell_ts && !self.sells_sent {
            self.sells_sent = true;
            vec![(Side::Sell, 200), (Side::Sell, 200)]
        } else {
            Vec::new()
        };
        sides
            .into_iter()
            .map(|(side, qty)| Order {
                client_id: 0,
                instrument: instrument.clone(),
                side,
                qty: Quantity::from_i64(qty),
                limit: None,
                status: OrderStatus::PendingSubmit,
                filled: Quantity::ZERO,
                account_id: "main".into(),
                trace: None,
                policy: None,
            })
            .collect()
    }
}

#[test]
fn same_bar_sell_orders_must_reserve_t1_available_shares() {
    let instrument = InstrumentId::parse("000001.SZSE").unwrap();
    let day_one = 100 * DAY_MS + 9 * 60 * 60 * 1_000;
    let day_two = day_one + DAY_MS;
    let bars = [day_one, day_one + 60_000, day_two, day_two + 60_000]
        .into_iter()
        .map(|ts| Bar::new(ts, 10 * SCALE, 10 * SCALE, 10 * SCALE, 10 * SCALE, 10_000))
        .collect::<Vec<_>>();
    let config = BacktestConfig {
        instrument,
        instrument_spec: None,
        account_id: "main".into(),
        currency: "CNY".into(),
        initial_cash: Money::from_i64(100_000),
        multiplier: 1,
        fill: Box::new(NextBarOpenFillModel),
        fee: Box::new(MakerTakerFeeModel {
            maker_bp: 0,
            taker_bp: 0,
        }),
        data_tier: DataTier::Bar,
        latency: Box::new(ZeroLatency),
        margin: Box::new(NoMargin),
        seed: 1,
        risk: RiskGate::conservative_default(),
        virtual_trading: VirtualTradingConfig {
            ashare_rules: Some(AshareRuleConfig {
                enabled: true,
                ..AshareRuleConfig::default()
            }),
            ..VirtualTradingConfig::default()
        },
    };
    let report = BacktestEngine::new(config)
        .run(
            &bars,
            &mut DuplicateSellStrategy {
                buy_sent: false,
                sells_sent: false,
                buy_ts: bars[1].ts,
                sell_ts: bars[2].ts,
            },
        )
        .expect("A-share backtest should complete");

    assert_eq!(report.positions.last().copied(), Some(0));
}

#[test]
fn an_active_sell_from_a_prior_bar_reserves_t1_available_shares() {
    let instrument = InstrumentId::parse("000001.SZSE").unwrap();
    let day_one = 100 * DAY_MS + 9 * 60 * 60 * 1_000;
    let day_two = day_one + DAY_MS;
    let bars = [
        Bar::new(
            day_one,
            10 * SCALE,
            10 * SCALE,
            10 * SCALE,
            10 * SCALE,
            10_000,
        ),
        Bar::new(
            day_one + 60_000,
            10 * SCALE,
            10 * SCALE,
            10 * SCALE,
            10 * SCALE,
            10_000,
        ),
        Bar::new(
            day_two,
            10 * SCALE,
            10 * SCALE,
            10 * SCALE,
            10 * SCALE,
            10_000,
        ),
        Bar::new(
            day_two + 60_000,
            11 * SCALE,
            11 * SCALE,
            11 * SCALE,
            11 * SCALE,
            10_000,
        ),
    ];
    let config = BacktestConfig {
        instrument,
        instrument_spec: None,
        account_id: "main".into(),
        currency: "CNY".into(),
        initial_cash: Money::from_i64(100_000),
        multiplier: 1,
        fill: Box::new(NextBarOpenFillModel),
        fee: Box::new(MakerTakerFeeModel {
            maker_bp: 0,
            taker_bp: 0,
        }),
        data_tier: DataTier::Bar,
        latency: Box::new(ZeroLatency),
        margin: Box::new(NoMargin),
        seed: 1,
        risk: RiskGate::conservative_default(),
        virtual_trading: VirtualTradingConfig {
            ashare_rules: Some(AshareRuleConfig {
                enabled: true,
                ..AshareRuleConfig::default()
            }),
            ..VirtualTradingConfig::default()
        },
    };
    let report = BacktestEngine::new(config)
        .run(
            &bars,
            &mut PendingSellStrategy {
                buy_sent: false,
                first_sell_sent: false,
                second_sell_sent: false,
                buy_ts: bars[1].ts,
                first_sell_ts: bars[2].ts,
                second_sell_ts: bars[3].ts,
            },
        )
        .expect("A-share backtest should complete");

    assert_eq!(report.positions.last().copied(), Some(0));
    assert_eq!(report.fills.len(), 2, "one buy and one sell must fill");
}
