use qx_core::{
    InstrumentId, MakerTakerFeeModel, Money, Order, OrderStatus, Quantity, Side,
    TradingInstrumentSpec, TradingProduct, SCALE,
};
use qx_guanxing::Bar;
use qx_xingban::{
    BacktestConfig, BacktestEngine, BarStrategy, DataTier, NextBarOpenFillModel, NoMargin,
    VirtualTradingConfig, ZeroLatency,
};
use qx_zhenlu::RiskGate;

struct SellWithoutInventory {
    sent: bool,
}

struct PendingSpotSellStrategy {
    buy_sent: bool,
    first_sell_sent: bool,
    second_sell_sent: bool,
}

struct SameBarSpotSellStrategy {
    buy_sent: bool,
    sells_sent: bool,
}

impl BarStrategy for SellWithoutInventory {
    fn on_bar(
        &mut self,
        _history: &[Bar],
        instrument: &InstrumentId,
        _ts: u64,
        _position: i128,
    ) -> Option<Order> {
        if self.sent {
            return None;
        }
        self.sent = true;
        Some(Order {
            client_id: 0,
            instrument: instrument.clone(),
            side: Side::Sell,
            qty: Quantity::from_i64(1),
            limit: None,
            status: OrderStatus::PendingSubmit,
            filled: Quantity::ZERO,
            account_id: "main".into(),
            trace: None,
            policy: None,
        })
    }
}

impl BarStrategy for PendingSpotSellStrategy {
    fn on_bar_orders(
        &mut self,
        _history: &[Bar],
        instrument: &InstrumentId,
        ts: u64,
        _position: i128,
    ) -> Vec<Order> {
        let order = match ts {
            2 if !self.buy_sent => {
                self.buy_sent = true;
                Some((Side::Buy, None))
            }
            3 if !self.first_sell_sent => {
                self.first_sell_sent = true;
                Some((Side::Sell, Some(11 * SCALE)))
            }
            4 if !self.second_sell_sent => {
                self.second_sell_sent = true;
                Some((Side::Sell, None))
            }
            _ => None,
        };
        order
            .into_iter()
            .map(|(side, limit)| Order {
                client_id: 0,
                instrument: instrument.clone(),
                side,
                qty: Quantity::from_i64(1),
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

impl BarStrategy for SameBarSpotSellStrategy {
    fn on_bar_orders(
        &mut self,
        _history: &[Bar],
        instrument: &InstrumentId,
        ts: u64,
        _position: i128,
    ) -> Vec<Order> {
        let sides = match ts {
            2 if !self.buy_sent => {
                self.buy_sent = true;
                vec![Side::Buy]
            }
            3 if !self.sells_sent => {
                self.sells_sent = true;
                vec![Side::Sell, Side::Sell]
            }
            _ => Vec::new(),
        };
        sides
            .into_iter()
            .map(|side| Order {
                client_id: 0,
                instrument: instrument.clone(),
                side,
                qty: Quantity::from_i64(1),
                limit: if side == Side::Sell {
                    Some(qx_core::Price::from_raw(11 * SCALE))
                } else {
                    None
                },
                status: OrderStatus::PendingSubmit,
                filled: Quantity::ZERO,
                account_id: "main".into(),
                trace: None,
                policy: None,
            })
            .collect()
    }
}

fn spot_spec(instrument: InstrumentId) -> TradingInstrumentSpec {
    TradingInstrumentSpec {
        instrument,
        product: TradingProduct::Spot,
        base_currency: "BTC".into(),
        quote_currency: "USDT".into(),
        settlement_currency: "USDT".into(),
        contract_size: SCALE,
        linear: false,
        inverse: false,
        price_tick: 1,
        qty_step: 1,
        min_qty: 1,
        max_leverage: 1,
        maintenance_margin_bps: 0,
        valid_from: 1,
        valid_to: None,
    }
}

fn backtest_config(instrument: InstrumentId) -> BacktestConfig {
    BacktestConfig {
        instrument: instrument.clone(),
        instrument_spec: Some(spot_spec(instrument)),
        account_id: "main".into(),
        currency: "USDT".into(),
        initial_cash: Money::from_i64(10_000),
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
        virtual_trading: VirtualTradingConfig::default(),
    }
}

#[test]
fn cash_spot_sell_without_inventory_is_rejected() {
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let config = backtest_config(instrument);
    let bars = [
        Bar::new(1, 10, 10, 10, 10, 1),
        Bar::new(2, 10, 10, 10, 10, 1),
    ];
    let report = BacktestEngine::new(config)
        .run(&bars, &mut SellWithoutInventory { sent: false })
        .expect("spot backtest should complete");

    assert!(report.fills.is_empty());
    assert_eq!(report.positions.last().copied(), Some(0));
}

#[test]
fn pending_cash_spot_sell_reserves_inventory_across_bars() {
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let bars = [
        Bar::new(1, 10 * SCALE, 10 * SCALE, 10 * SCALE, 10 * SCALE, SCALE),
        Bar::new(2, 10 * SCALE, 10 * SCALE, 10 * SCALE, 10 * SCALE, SCALE),
        Bar::new(3, 10 * SCALE, 10 * SCALE, 10 * SCALE, 10 * SCALE, SCALE),
        Bar::new(4, 10 * SCALE, 10 * SCALE, 10 * SCALE, 10 * SCALE, SCALE),
        Bar::new(5, 11 * SCALE, 11 * SCALE, 11 * SCALE, 11 * SCALE, SCALE),
    ];
    let report = BacktestEngine::new(backtest_config(instrument))
        .run(
            &bars,
            &mut PendingSpotSellStrategy {
                buy_sent: false,
                first_sell_sent: false,
                second_sell_sent: false,
            },
        )
        .expect("spot backtest should complete");

    assert_eq!(report.fills.len(), 2, "only the buy and reserved sell fill");
    assert_eq!(report.positions.last().copied(), Some(0));
}

#[test]
fn same_bar_cash_spot_sells_reserve_inventory_before_accepting_the_next_order() {
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let bars = [
        Bar::new(1, 10 * SCALE, 10 * SCALE, 10 * SCALE, 10 * SCALE, SCALE),
        Bar::new(2, 10 * SCALE, 10 * SCALE, 10 * SCALE, 10 * SCALE, SCALE),
        Bar::new(3, 10 * SCALE, 10 * SCALE, 10 * SCALE, 10 * SCALE, SCALE),
        Bar::new(4, 11 * SCALE, 11 * SCALE, 11 * SCALE, 11 * SCALE, SCALE),
    ];
    let report = BacktestEngine::new(backtest_config(instrument))
        .run(
            &bars,
            &mut SameBarSpotSellStrategy {
                buy_sent: false,
                sells_sent: false,
            },
        )
        .expect("spot backtest should complete");

    assert_eq!(report.fills.len(), 2, "only the buy and one sell can fill");
    assert_eq!(report.positions.last().copied(), Some(0));
    assert_eq!(
        report
            .event_log
            .events()
            .iter()
            .filter(|event| matches!(event.kind, qx_core::EventKind::Rejected { .. }))
            .count(),
        1,
        "the second same-bar sell must be rejected against the first sell's inventory reservation"
    );
}
