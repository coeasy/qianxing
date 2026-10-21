use super::*;

struct MarginProbe {
    observed: Arc<Mutex<Vec<Option<i128>>>>,
    emitted: bool,
}

impl OrderBookStrategy for MarginProbe {
    fn on_order_book(
        &mut self,
        _snapshot: &OrderBookSnapshot,
        _position: i128,
    ) -> Result<Vec<Order>, String> {
        Ok(Vec::new())
    }

    fn on_order_book_actions_with_margin(
        &mut self,
        snapshot: &OrderBookSnapshot,
        _position: i128,
        _open_order_ids: &[u64],
        available_margin_raw: Option<i128>,
    ) -> Result<OrderBookActions, String> {
        self.observed.lock().unwrap().push(available_margin_raw);
        if self.emitted {
            return Ok(OrderBookActions::default());
        }
        self.emitted = true;
        Ok(OrderBookActions {
            orders: vec![Order {
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
            }],
            cancel_client_ids: Vec::new(),
        })
    }
}

#[test]
fn spot_margin_snapshot_reaches_strategy_after_each_filled_order() {
    let observed = Arc::new(Mutex::new(Vec::new()));
    let mut strategy = MarginProbe {
        observed: observed.clone(),
        emitted: false,
    };
    let report = OrderBookBacktestEngine::new(l2_config(0))
        .run(
            &[snapshot(1, 100), snapshot(2, 101), snapshot(3, 102)],
            &mut strategy,
        )
        .expect("spot run with margin snapshots");
    assert_eq!(report.fills.len(), 1);
    assert_eq!(
        *observed.lock().unwrap(),
        vec![
            Some(Money::from_i64(10_000).raw()),
            Some(Money::from_i64(9_899).raw()),
            Some(Money::from_i64(9_899).raw()),
        ]
    );
}

struct DerivativeMarginProbe {
    observed: Arc<Mutex<Vec<Option<i128>>>>,
    emitted: bool,
}

impl OrderBookStrategy for DerivativeMarginProbe {
    fn on_order_book(
        &mut self,
        _snapshot: &OrderBookSnapshot,
        _position: i128,
    ) -> Result<Vec<Order>, String> {
        Ok(Vec::new())
    }

    fn on_order_book_actions_with_margin(
        &mut self,
        snapshot: &OrderBookSnapshot,
        _position: i128,
        _open_order_ids: &[u64],
        available_margin_raw: Option<i128>,
    ) -> Result<OrderBookActions, String> {
        self.observed.lock().unwrap().push(available_margin_raw);
        if self.emitted {
            return Ok(OrderBookActions::default());
        }
        self.emitted = true;
        Ok(OrderBookActions {
            orders: vec![Order {
                client_id: 7,
                instrument: snapshot.instrument.clone(),
                side: Side::Buy,
                qty: Quantity::from_i64(1),
                limit: None,
                status: OrderStatus::PendingSubmit,
                filled: Quantity::ZERO,
                account_id: "main".into(),
                trace: None,
                policy: Some(qx_core::OrderPolicy {
                    leverage: 10,
                    ..qx_core::OrderPolicy::default()
                }),
            }],
            cancel_client_ids: Vec::new(),
        })
    }
}

#[test]
fn derivative_margin_snapshot_is_marked_equity_less_position_exposure() {
    let instrument = qx_core::InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let spec = qx_core::TradingInstrumentSpec {
        instrument: instrument.clone(),
        product: qx_core::TradingProduct::Perpetual,
        base_currency: "BTC".into(),
        quote_currency: "USDT".into(),
        settlement_currency: "USDT".into(),
        contract_size: qx_core::SCALE,
        linear: true,
        inverse: false,
        price_tick: 1,
        qty_step: 1,
        min_qty: 1,
        max_leverage: 10,
        maintenance_margin_bps: 500,
        valid_from: 1,
        valid_to: None,
    };
    let config = OrderBookBacktestConfig {
        instrument,
        account_id: "main".into(),
        currency: "USDT".into(),
        initial_cash: qx_core::Money::from_i64(10_000),
        fee_bps: 0,
        instrument_spec: Some(spec),
        risk: qx_zhenlu::RiskGate::conservative_default(),
        data_tier: crate::DataTier::L2L3,
    };
    let observed = Arc::new(Mutex::new(Vec::new()));
    let mut strategy = DerivativeMarginProbe {
        observed: observed.clone(),
        emitted: false,
    };
    let report = OrderBookBacktestEngine::new(config)
        .run(&[snapshot(1, 100), snapshot(2, 100)], &mut strategy)
        .unwrap();

    assert_eq!(report.fills.len(), 1);
    let observed = observed.lock().unwrap();
    assert_eq!(observed[0], Some(qx_core::Money::from_i64(10_000).raw()));
    assert!(observed[1].is_some_and(|available| available < observed[0].unwrap()));
}
