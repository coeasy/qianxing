use qx_core::{
    ExactDecimal, ExactOrderAmounts, ExactTradingRules, InstrumentId, Money, Order, OrderStatus,
    Price, Quantity, Side, TradingInstrumentSpec, TradingProduct, SCALE,
};
use qx_xingban::{
    BookLevel, DataTier, DepthFrame, OrderBookBacktestConfig, OrderBookBacktestEngine,
    OrderBookExecutionModel, OrderBookPortfolioBacktestEngine, OrderBookPortfolioMarket,
    OrderBookSnapshot, OrderBookStrategy,
};
use qx_zhenlu::RiskGate;

struct NoopStrategy;

impl OrderBookStrategy for NoopStrategy {
    fn on_order_book(
        &mut self,
        _snapshot: &OrderBookSnapshot,
        _position: i128,
    ) -> Result<Vec<qx_core::Order>, String> {
        Ok(Vec::new())
    }
}

struct BuyOnce {
    emitted: bool,
}

struct ExactBuyOnce {
    emitted: bool,
    quantity: ExactDecimal,
}

impl OrderBookStrategy for ExactBuyOnce {
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
            client_id: 1,
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

struct FillIdStrategy {
    emitted: bool,
    fill_ids: Vec<(u64, u64)>,
}

impl OrderBookStrategy for FillIdStrategy {
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

    fn on_fill(&mut self, order: &Order, fill: &qx_core::Fill) -> Result<(), qx_core::QxError> {
        self.fill_ids.push((order.client_id, fill.order_id));
        Ok(())
    }
}

#[test]
fn depth_frame_hash_includes_bid_ask_boundaries() {
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let level = |price| BookLevel {
        price_exact: None,
        qty_exact: None,
        price: Price::from_i64(price),
        qty: Quantity::from_i64(1),
    };
    let frame = |bids, asks| DepthFrame {
        schema_version: DepthFrame::LEGACY_SCHEMA_VERSION,
        source: "fixture".into(),
        instrument: instrument.clone(),
        snapshots: vec![OrderBookSnapshot {
            instrument: instrument.clone(),
            ts: 1,
            sequence: 1,
            bids,
            asks,
        }],
    };
    let bid_ask = frame(vec![level(99)], vec![level(100)]);
    let ask_only = frame(vec![], vec![level(99), level(100)]);

    assert!(bid_ask.validate().is_ok());
    assert!(ask_only.validate().is_ok());
    assert_ne!(bid_ask.input_hash(), ask_only.input_hash());
}

#[test]
fn order_book_report_hash_includes_bid_ask_boundaries() {
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let level = |price| BookLevel {
        price_exact: None,
        qty_exact: None,
        price: Price::from_i64(price),
        qty: Quantity::from_i64(1),
    };
    let book = |bids: Vec<BookLevel>, asks: Vec<BookLevel>| -> Vec<OrderBookSnapshot> {
        [1, 2]
            .map(|ts| OrderBookSnapshot {
                instrument: instrument.clone(),
                ts,
                sequence: ts,
                bids: bids.clone(),
                asks: asks.clone(),
            })
            .to_vec()
    };
    let report = |snapshots: Vec<OrderBookSnapshot>| {
        OrderBookBacktestEngine::new(OrderBookBacktestConfig {
            instrument: instrument.clone(),
            account_id: "main".into(),
            currency: "USDT".into(),
            initial_cash: Money::from_i64(1_000),
            fee_bps: 0,
            instrument_spec: None,
            risk: RiskGate::conservative_default(),
            data_tier: DataTier::L2L3,
        })
        .run(&snapshots, &mut NoopStrategy)
        .unwrap()
    };

    let bid_ask = report(book(vec![level(99)], vec![level(100)]));
    let ask_only = report(book(vec![], vec![level(99), level(100)]));

    assert_ne!(bid_ask.input_data_hash, ask_only.input_data_hash);
}

#[test]
fn multi_market_orderbook_shares_cash_oms_and_replay() {
    let btc = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let eth = InstrumentId::parse("ETHUSDT.BYBIT").unwrap();
    let spec = |instrument: InstrumentId, base: &str| TradingInstrumentSpec {
        instrument,
        product: TradingProduct::Spot,
        base_currency: base.into(),
        quote_currency: "USDT".into(),
        settlement_currency: "USDT".into(),
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
    };
    let books = |instrument: &InstrumentId, ask: i64| {
        [1, 2].map(|ts| OrderBookSnapshot {
            instrument: instrument.clone(),
            ts,
            sequence: ts,
            bids: vec![BookLevel {
                price_exact: None,
                qty_exact: None,
                price: Price::from_i64(ask - 1),
                qty: Quantity::from_i64(2),
            }],
            asks: vec![BookLevel {
                price_exact: None,
                qty_exact: None,
                price: Price::from_i64(ask),
                qty: Quantity::from_i64(2),
            }],
        })
    };
    let btc_books = books(&btc, 80);
    let eth_books = books(&eth, 60);
    let mut btc_strategy = BuyOnce { emitted: false };
    let mut eth_strategy = BuyOnce { emitted: false };
    let markets = vec![
        OrderBookPortfolioMarket {
            instrument_spec: spec(btc.clone(), "BTC"),
            data_tier: DataTier::L2L3,
            priority: 1,
            snapshots: &btc_books,
            execution_model: OrderBookExecutionModel::new(0).unwrap(),
            strategy: &mut btc_strategy,
        },
        OrderBookPortfolioMarket {
            instrument_spec: spec(eth.clone(), "ETH"),
            data_tier: DataTier::L2L3,
            priority: 0,
            snapshots: &eth_books,
            execution_model: OrderBookExecutionModel::new(0).unwrap(),
            strategy: &mut eth_strategy,
        },
    ];
    let report = OrderBookPortfolioBacktestEngine::new(
        "main",
        "USDT",
        Money::from_i64(100),
        RiskGate::conservative_default(),
        markets,
    )
    .unwrap()
    .run()
    .unwrap();

    assert_eq!(report.fills.len(), 1);
    assert_eq!(report.fills_by_instrument[&eth].len(), 1);
    assert!(report.fills_by_instrument[&btc].is_empty());
    assert_eq!(
        report.ledger.cash_for("main", "USDT"),
        Money::from_i64(40).raw()
    );
    assert_eq!(report.pending_orders, 0);
    assert!(report.event_log.events().iter().any(|event| matches!(
        &event.kind,
        qx_core::EventKind::Rejected { reason, .. }
            if reason.contains("可用现金")
    )));
    report.verify().unwrap();
}

#[test]
fn multi_market_depth_v2_exact_spot_shares_cash_reservations_and_keeps_fees() {
    let btc = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let eth = InstrumentId::parse("ETHUSDT.BYBIT").unwrap();
    let spec = |instrument: InstrumentId, base: &str| {
        let mut spec = TradingInstrumentSpec {
            instrument,
            product: TradingProduct::Spot,
            base_currency: base.into(),
            quote_currency: "USDT".into(),
            settlement_currency: "USDT".into(),
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
        };
        spec.exact_rules = Some(ExactTradingRules {
            contract_size: ExactDecimal::ONE,
            price_tick: ExactDecimal::parse("0.1").unwrap(),
            quantity_step: ExactDecimal::parse("0.000000000001").unwrap(),
            minimum_quantity: ExactDecimal::parse("0.000000000001").unwrap(),
            minimum_notional: ExactDecimal::ZERO,
        });
        spec
    };
    let books = |instrument: &InstrumentId| {
        [1, 2]
            .map(|ts| OrderBookSnapshot {
                instrument: instrument.clone(),
                ts,
                sequence: ts,
                bids: vec![BookLevel::from_exact(
                    ExactDecimal::parse("99.9").unwrap(),
                    ExactDecimal::parse("0.00000000001").unwrap(),
                )
                .unwrap()],
                asks: vec![BookLevel::from_exact(
                    ExactDecimal::parse("100").unwrap(),
                    ExactDecimal::parse("0.00000000001").unwrap(),
                )
                .unwrap()],
            })
            .to_vec()
    };
    let btc_books = books(&btc);
    let eth_books = books(&eth);
    let mut btc_strategy = ExactBuyOnce {
        emitted: false,
        quantity: ExactDecimal::parse("0.000000000006").unwrap(),
    };
    let mut eth_strategy = ExactBuyOnce {
        emitted: false,
        quantity: ExactDecimal::parse("0.000000000006").unwrap(),
    };
    let markets = vec![
        OrderBookPortfolioMarket {
            instrument_spec: spec(btc.clone(), "BTC"),
            data_tier: DataTier::L2L3,
            priority: 1,
            snapshots: &btc_books,
            execution_model: OrderBookExecutionModel::new(100).unwrap(),
            strategy: &mut btc_strategy,
        },
        OrderBookPortfolioMarket {
            instrument_spec: spec(eth.clone(), "ETH"),
            data_tier: DataTier::L2L3,
            priority: 0,
            snapshots: &eth_books,
            execution_model: OrderBookExecutionModel::new(100).unwrap(),
            strategy: &mut eth_strategy,
        },
    ];
    let report = OrderBookPortfolioBacktestEngine::new(
        "main",
        "USDT",
        Money::from_raw(1),
        RiskGate::conservative_default(),
        markets,
    )
    .unwrap()
    .run()
    .unwrap();

    assert_eq!(report.fills.len(), 1);
    assert_eq!(
        report.fills[0].exact_amounts.unwrap().quantity.to_string(),
        "0.000000000006"
    );
    assert!(report.event_log.events().iter().any(|event| matches!(
        &event.kind,
        qx_core::EventKind::Rejected { reason, .. }
            if reason.contains("精确买入成本")
    )));
    assert_eq!(
        report.ledger.cash_for_exact("main", "USDT").to_string(),
        "0.000000000394"
    );
    assert_eq!(report.turnover_exact.to_string(), "0.0000000006");
    assert_eq!(
        report.fees_exact_by_currency["USDT"].to_string(),
        "0.000000000006"
    );
    assert_eq!(report.fees_raw, 0);
    assert!(report.exact_metrics_json().unwrap()["turnover"]["amount"] == "0.0000000006");
    report.verify().unwrap();
}

#[test]
fn multi_market_orderbook_restores_strategy_local_fill_ids() {
    let btc = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let eth = InstrumentId::parse("ETHUSDT.BYBIT").unwrap();
    let spec = |instrument: InstrumentId, base: &str| TradingInstrumentSpec {
        instrument,
        product: TradingProduct::Spot,
        base_currency: base.into(),
        quote_currency: "USDT".into(),
        settlement_currency: "USDT".into(),
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
    };
    let books = |instrument: &InstrumentId, ask: i64| {
        [1, 2].map(|ts| OrderBookSnapshot {
            instrument: instrument.clone(),
            ts,
            sequence: ts,
            bids: vec![BookLevel {
                price_exact: None,
                qty_exact: None,
                price: Price::from_i64(ask - 1),
                qty: Quantity::from_i64(2),
            }],
            asks: vec![BookLevel {
                price_exact: None,
                qty_exact: None,
                price: Price::from_i64(ask),
                qty: Quantity::from_i64(2),
            }],
        })
    };
    let btc_books = books(&btc, 80);
    let eth_books = books(&eth, 60);
    let mut btc_strategy = FillIdStrategy {
        emitted: false,
        fill_ids: Vec::new(),
    };
    let mut eth_strategy = FillIdStrategy {
        emitted: false,
        fill_ids: Vec::new(),
    };
    let markets = vec![
        OrderBookPortfolioMarket {
            instrument_spec: spec(btc.clone(), "BTC"),
            data_tier: DataTier::L2L3,
            priority: 1,
            snapshots: &btc_books,
            execution_model: OrderBookExecutionModel::new(0).unwrap(),
            strategy: &mut btc_strategy,
        },
        OrderBookPortfolioMarket {
            instrument_spec: spec(eth.clone(), "ETH"),
            data_tier: DataTier::L2L3,
            priority: 0,
            snapshots: &eth_books,
            execution_model: OrderBookExecutionModel::new(0).unwrap(),
            strategy: &mut eth_strategy,
        },
    ];
    let report = OrderBookPortfolioBacktestEngine::new(
        "main",
        "USDT",
        Money::from_i64(200),
        RiskGate::conservative_default(),
        markets,
    )
    .unwrap()
    .run()
    .unwrap();

    assert_eq!(
        report
            .fills
            .iter()
            .map(|fill| fill.order_id)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    assert_eq!(btc_strategy.fill_ids, vec![(1, 1)]);
    assert_eq!(eth_strategy.fill_ids, vec![(1, 1)]);
    report.verify().unwrap();
}
