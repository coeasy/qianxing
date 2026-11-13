mod common;

use common::{depth_spec as spec, scratch};
use qx_app::{
    run_depth_backtest_with_strategy, run_depth_backtest_with_strategy_slots, verify_depth_run,
    BuiltinStrategySpec, CallerCapability, DepthBacktestSpec, PortfolioBacktestConfig,
    PortfolioStrategySpec, RunContext, StrategyArtifactIdentity,
};
use qx_core::{ExactDecimal, ExactTradingRules, Quantity, Side, TradingInstrumentSpec};
use qx_strategy::{
    BuiltinStrategyKind, MarketEvent, Strategy, StrategyContext, StrategyDecision,
    StrategyOrderIntent, STRATEGY_API_VERSION,
};

struct BuyOnFirstEvent(bool, std::sync::Arc<std::sync::Mutex<Option<&'static str>>>);

impl Strategy for BuyOnFirstEvent {
    fn on_event(
        &mut self,
        context: &StrategyContext,
        event: &MarketEvent,
    ) -> Result<StrategyDecision, String> {
        *self.1.lock().expect("event lock") = Some(match event {
            MarketEvent::Tick { .. } => "tick",
            MarketEvent::OrderBook { .. } => "order_book",
            other => return Err(format!("unexpected depth event: {other:?}")),
        });
        let intents = if self.0 {
            Vec::new()
        } else {
            self.0 = true;
            vec![StrategyOrderIntent {
                intent_id: 1,
                instrument: event.instrument().cloned().expect("instrument"),
                side: Side::Buy,
                qty: Quantity::from_raw(1_000_000_000),
                limit: None,
                policy: None,
                reduce_only: false,
                post_only: false,

                qty_exact: None,
                limit_price_exact: None,
                quote_budget: None,
            }]
        };
        Ok(StrategyDecision {
            schema_version: STRATEGY_API_VERSION,
            request_id: format!("{}:{}", context.strategy_id, event.ts()),
            strategy_id: context.strategy_id.clone(),
            signal_id: event.ts(),
            confidence: 10_000,
            priority: 0,
            expires_at: 0,
            intents,
        })
    }
}

struct DepthLifecycleStrategy {
    submitted: bool,
    fill_updates: u8,
    lifecycle: std::sync::Arc<std::sync::Mutex<Vec<&'static str>>>,
}

struct ExactDepthBuyOnce(bool);

struct ExactDerivativeDepthBuyOnce(bool);

impl Strategy for ExactDepthBuyOnce {
    fn on_event(
        &mut self,
        context: &StrategyContext,
        event: &MarketEvent,
    ) -> Result<StrategyDecision, String> {
        if let MarketEvent::OrderBook { asks, .. } = event {
            assert!(asks.iter().all(|level| level.price_exact.is_some()));
        } else {
            return Err("exact DepthFrame v2 must reach the strategy as OrderBook".into());
        }
        let intents = if self.0 {
            Vec::new()
        } else {
            self.0 = true;
            vec![StrategyOrderIntent {
                intent_id: 71,
                instrument: event.instrument().cloned().expect("instrument"),
                side: Side::Buy,
                qty: Quantity::ZERO,
                limit: None,
                policy: None,
                reduce_only: false,
                post_only: false,
                qty_exact: Some(ExactDecimal::parse("0.00000000001").unwrap()),
                limit_price_exact: Some(ExactDecimal::parse("103").unwrap()),
                quote_budget: None,
            }]
        };
        Ok(StrategyDecision {
            schema_version: STRATEGY_API_VERSION,
            request_id: format!("exact-depth:{}", event.ts()),
            strategy_id: context.strategy_id.clone(),
            signal_id: event.ts(),
            confidence: 10_000,
            priority: 0,
            expires_at: 0,
            intents,
        })
    }
}

impl Strategy for ExactDerivativeDepthBuyOnce {
    fn on_event(
        &mut self,
        context: &StrategyContext,
        event: &MarketEvent,
    ) -> Result<StrategyDecision, String> {
        if !matches!(event, MarketEvent::OrderBook { .. }) {
            return Err("exact derivative strategy requires L2 OrderBook events".into());
        }
        let intents = if self.0 {
            Vec::new()
        } else {
            self.0 = true;
            vec![StrategyOrderIntent {
                intent_id: 780,
                instrument: event.instrument().cloned().expect("instrument"),
                side: Side::Buy,
                qty: Quantity::ZERO,
                limit: None,
                policy: Some(qx_core::OrderPolicy {
                    margin_mode: qx_core::MarginMode::Cross,
                    position_mode: qx_core::PositionMode::OneWay,
                    position_side: qx_core::PositionSide::Net,
                    leverage: 10,
                    ..qx_core::OrderPolicy::default()
                }),
                reduce_only: false,
                post_only: false,
                qty_exact: Some(ExactDecimal::parse("0.0000000002").unwrap()),
                limit_price_exact: None,
                quote_budget: None,
            }]
        };
        Ok(StrategyDecision {
            schema_version: STRATEGY_API_VERSION,
            request_id: format!("exact-derivative-depth:{}", event.ts()),
            strategy_id: context.strategy_id.clone(),
            signal_id: event.ts(),
            confidence: 10_000,
            priority: 0,
            expires_at: 0,
            intents,
        })
    }
}

impl Strategy for DepthLifecycleStrategy {
    fn on_init(&mut self, _context: &StrategyContext) -> Result<(), String> {
        self.lifecycle.lock().unwrap().push("init");
        Ok(())
    }

    fn on_start(&mut self, _context: &StrategyContext) -> Result<(), String> {
        self.lifecycle.lock().unwrap().push("start");
        Ok(())
    }

    fn on_warmup_complete(
        &mut self,
        _context: &StrategyContext,
        instrument: Option<&qx_core::InstrumentId>,
        observed_bars: u64,
    ) -> Result<(), String> {
        assert!(instrument.is_none());
        assert_eq!(observed_bars, 0);
        self.lifecycle.lock().unwrap().push("warmup_complete");
        Ok(())
    }

    fn on_event(
        &mut self,
        context: &StrategyContext,
        event: &MarketEvent,
    ) -> Result<StrategyDecision, String> {
        let intents = if self.submitted {
            Vec::new()
        } else {
            self.submitted = true;
            vec![StrategyOrderIntent {
                intent_id: 1,
                instrument: event.instrument().cloned().expect("instrument"),
                side: Side::Buy,
                qty: Quantity::from_i64(1),
                limit: None,
                policy: None,
                reduce_only: false,
                post_only: false,
                qty_exact: None,
                limit_price_exact: None,
                quote_budget: None,
            }]
        };
        Ok(StrategyDecision {
            schema_version: STRATEGY_API_VERSION,
            request_id: format!("lifecycle:{}", event.ts()),
            strategy_id: context.strategy_id.clone(),
            signal_id: event.ts(),
            confidence: 10_000,
            priority: 0,
            expires_at: 0,
            intents,
        })
    }

    fn on_strategy_fill_update_v2(
        &mut self,
        context: &StrategyContext,
        update: &qx_strategy::StrategyFillUpdateV2,
    ) -> Result<Option<StrategyDecision>, String> {
        update.validate_for(context)?;
        self.lifecycle.lock().unwrap().push("fill");
        self.fill_updates += 1;
        let intents = if self.fill_updates == 1 {
            vec![StrategyOrderIntent {
                intent_id: 2,
                instrument: qx_core::InstrumentId::parse(&update.order.instrument)
                    .ok_or_else(|| "invalid fill instrument".to_string())?,
                side: Side::Sell,
                qty: Quantity::from_i64(1),
                limit: None,
                policy: None,
                reduce_only: true,
                post_only: false,
                qty_exact: None,
                limit_price_exact: None,
                quote_budget: None,
            }]
        } else {
            Vec::new()
        };
        Ok((!intents.is_empty()).then(|| StrategyDecision {
            schema_version: STRATEGY_API_VERSION,
            request_id: "close-after-depth-fill".into(),
            strategy_id: context.strategy_id.clone(),
            signal_id: 2,
            confidence: 10_000,
            priority: 0,
            expires_at: 0,
            intents,
        }))
    }

    fn on_stop(&mut self) -> Result<(), String> {
        self.lifecycle.lock().unwrap().push("stop");
        Ok(())
    }
}

#[test]
fn injected_strategy_receives_tick_and_order_book_events_through_shared_depth_engines() {
    let root = scratch("app-injected-depth-strategy");
    let identity = StrategyArtifactIdentity {
        strategy_id: "depth-callback".into(),
        strategy_version: "test-v1".into(),
        source_sha256: "c".repeat(64),
    };
    for (tier, levels, expected_event) in [("l1", 1, "tick"), ("l2", 2, "order_book")] {
        let run_id = format!("custom-{tier}");
        let run_root = root.join(tier);
        std::fs::create_dir_all(&run_root).expect("create run directory");
        let mut request: DepthBacktestSpec = spec(&run_id, &run_root, tier, levels);
        request.strategy =
            BuiltinStrategySpec::new(BuiltinStrategyKind::SmaCross, "depth-callback");
        let seen_event = std::sync::Arc::new(std::sync::Mutex::new(None));
        let outcome = run_depth_backtest_with_strategy(
            &request,
            &identity,
            Box::new(BuyOnFirstEvent(false, seen_event.clone())),
            &RunContext::new(CallerCapability::Research, &run_id),
        )
        .expect("custom strategy runs on shared depth engine");
        assert_eq!(
            *seen_event.lock().expect("event lock"),
            Some(expected_event)
        );
        assert!(
            outcome.fills > 0,
            "custom order must pass shared Risk/OMS/matching"
        );
        assert!(
            verify_depth_run(
                &outcome,
                &RunContext::new(CallerCapability::Research, format!("verify-{tier}"))
            )
            .expect("replay verification")
            .verified
        );
    }
}

#[test]
fn injected_depth_strategy_runs_lifecycle_and_fill_decisions_through_next_boundary() {
    let root = scratch("app-depth-strategy-lifecycle");
    let run_id = "custom-depth-lifecycle";
    let run_root = root.join("l2");
    std::fs::create_dir_all(&run_root).expect("create run directory");
    let mut request = spec(run_id, &run_root, "l2", 2);
    request.strategy = BuiltinStrategySpec::new(BuiltinStrategyKind::SmaCross, "depth-lifecycle");
    let identity = StrategyArtifactIdentity {
        strategy_id: "depth-lifecycle".into(),
        strategy_version: "test-v1".into(),
        source_sha256: "f".repeat(64),
    };
    let lifecycle = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let outcome = run_depth_backtest_with_strategy(
        &request,
        &identity,
        Box::new(DepthLifecycleStrategy {
            submitted: false,
            fill_updates: 0,
            lifecycle: lifecycle.clone(),
        }),
        &RunContext::new(CallerCapability::Research, run_id),
    )
    .expect("depth strategy lifecycle and fill-generated orders run");
    assert!(
        outcome.fills >= 2,
        "fill callback order must reach the next boundary; fills={}, lifecycle={:?}",
        outcome.fills,
        lifecycle.lock().unwrap()
    );
    assert_eq!(
        *lifecycle.lock().unwrap(),
        vec!["init", "start", "warmup_complete", "fill", "fill", "stop"]
    );
    verify_depth_run(
        &outcome,
        &RunContext::new(CallerCapability::Research, "verify-depth-lifecycle"),
    )
    .expect("strategy-generated close is replay verified");
}

#[test]
fn injected_strategy_trades_depth_frame_v2_and_report_keeps_subnano_fees() {
    let root = scratch("app-custom-depth-exact");
    let run_id = "custom-depth-exact";
    let run_root = root.join("l2");
    std::fs::create_dir_all(&run_root).expect("create run directory");
    let mut request = spec(run_id, &run_root, "l2", 2);
    request.strategy = BuiltinStrategySpec::new(BuiltinStrategyKind::SmaCross, "depth-exact");
    request.initial_cash_raw = 2;
    request.fee_bps = Some(100);
    let mut frame: serde_json::Value =
        serde_json::from_str(&common::depth_frame_json(2)).expect("legacy fixture");
    frame["schema_version"] = serde_json::json!(2);
    frame["source"] = serde_json::json!("qx-app-exact-depth-test");
    for snapshot in frame["snapshots"].as_array_mut().unwrap() {
        for side in ["bids", "asks"] {
            for level in snapshot[side].as_array_mut().unwrap() {
                let price = level["price"].as_i64().unwrap();
                let quantity = level["qty"].as_i64().unwrap();
                level["price_exact"] =
                    serde_json::to_value(ExactDecimal::from_parts(i128::from(price), 9).unwrap())
                        .unwrap();
                level["qty_exact"] = serde_json::to_value(
                    ExactDecimal::from_parts(i128::from(quantity), 9).unwrap(),
                )
                .unwrap();
            }
        }
    }
    let depth_path = run_root.join("depth-v2.json");
    std::fs::write(
        &depth_path,
        serde_json::to_vec(&frame).expect("serialize exact depth fixture"),
    )
    .expect("write exact depth fixture");
    request.depth_path = depth_path.to_string_lossy().into_owned();
    let instrument = qx_core::InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let mut market = TradingInstrumentSpec::spot_default(&instrument, "USDT");
    market.base_currency = "BTC".into();
    market.exact_rules = Some(ExactTradingRules {
        contract_size: ExactDecimal::ONE,
        price_tick: ExactDecimal::parse("0.0000000001").unwrap(),
        quantity_step: ExactDecimal::parse("0.00000000001").unwrap(),
        minimum_quantity: ExactDecimal::parse("0.00000000001").unwrap(),
        minimum_notional: ExactDecimal::ZERO,
    });
    request.market_spec = Some(market);
    let identity = StrategyArtifactIdentity {
        strategy_id: "depth-exact".into(),
        strategy_version: "test-exact-v1".into(),
        source_sha256: "a".repeat(64),
    };
    let outcome = run_depth_backtest_with_strategy(
        &request,
        &identity,
        Box::new(ExactDepthBuyOnce(false)),
        &RunContext::new(CallerCapability::Research, run_id),
    )
    .expect("exact custom depth strategy runs through Rust core");
    assert_eq!(outcome.fills, 1);
    let summary: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&outcome.artifacts.summary).unwrap())
            .expect("exact summary");
    assert_eq!(
        summary["metrics_exact"]["fees_by_currency"]["USDT"],
        "0.0000000000103"
    );
    assert_eq!(summary["fees_raw"], 0);
    assert_eq!(
        summary["metrics_exact"]["legacy_raw_projection"]["fees_raw"],
        "0"
    );
    assert_eq!(
        summary["metrics_exact"]["turnover"]["amount"],
        "0.00000000103"
    );
    let fills_csv = std::fs::read_to_string(&outcome.artifacts.fills).expect("fills artifact");
    assert!(fills_csv.contains("0.00000000001"));
    assert!(
        verify_depth_run(
            &outcome,
            &RunContext::new(CallerCapability::Research, "verify-custom-depth-exact")
        )
        .expect("exact depth replay verification")
        .verified
    );
}

#[test]
fn injected_strategy_trades_exact_perpetual_depth_and_report_reader_round_trips() {
    let root = scratch("app-custom-depth-exact-perpetual");
    let run_id = "custom-depth-exact-perpetual";
    let run_root = root.join("l2");
    std::fs::create_dir_all(&run_root).expect("create run directory");
    let mut request = spec(run_id, &run_root, "l2", 2);
    request.strategy =
        BuiltinStrategySpec::new(BuiltinStrategyKind::SmaCross, "depth-exact-perpetual");
    request.initial_cash_raw = 10;
    request.fee_bps = Some(100);
    let mut frame: serde_json::Value =
        serde_json::from_str(&common::depth_frame_json(2)).expect("legacy fixture");
    frame["schema_version"] = serde_json::json!(2);
    frame["source"] = serde_json::json!("qx-app-exact-perpetual-depth-test");
    for snapshot in frame["snapshots"].as_array_mut().unwrap() {
        for side in ["bids", "asks"] {
            for level in snapshot[side].as_array_mut().unwrap() {
                let price = level["price"].as_i64().unwrap();
                let quantity = level["qty"].as_i64().unwrap();
                level["price_exact"] =
                    serde_json::to_value(ExactDecimal::from_parts(i128::from(price), 9).unwrap())
                        .unwrap();
                level["qty_exact"] = serde_json::to_value(
                    ExactDecimal::from_parts(i128::from(quantity), 9).unwrap(),
                )
                .unwrap();
            }
        }
    }
    let depth_path = run_root.join("depth-v2-perpetual.json");
    std::fs::write(
        &depth_path,
        serde_json::to_vec(&frame).expect("serialize exact derivative depth fixture"),
    )
    .expect("write exact derivative depth fixture");
    request.depth_path = depth_path.to_string_lossy().into_owned();
    request.settlement_currency = "USDT".into();
    let instrument = qx_core::InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let mut market = TradingInstrumentSpec {
        instrument,
        product: qx_core::TradingProduct::Perpetual,
        base_currency: "BTC".into(),
        quote_currency: "USDT".into(),
        settlement_currency: "USDT".into(),
        contract_size: 1_000_000_000,
        linear: true,
        inverse: false,
        price_tick: 1,
        qty_step: 1,
        min_qty: 1,
        min_notional_raw: 0,
        max_leverage: 10,
        maintenance_margin_bps: 500,
        valid_from: 0,
        valid_to: None,
        exact_rules: Some(ExactTradingRules {
            contract_size: ExactDecimal::ONE,
            price_tick: ExactDecimal::parse("0.0000000001").unwrap(),
            quantity_step: ExactDecimal::parse("0.0000000001").unwrap(),
            minimum_quantity: ExactDecimal::parse("0.0000000001").unwrap(),
            minimum_notional: ExactDecimal::ZERO,
        }),
    };
    market.contract_size = 1_000_000_000;
    request.market_spec = Some(market);
    request.order_policy = Some(qx_core::OrderPolicy {
        margin_mode: qx_core::MarginMode::Cross,
        position_mode: qx_core::PositionMode::OneWay,
        position_side: qx_core::PositionSide::Net,
        leverage: 10,
        ..qx_core::OrderPolicy::default()
    });
    let identity = StrategyArtifactIdentity {
        strategy_id: "depth-exact-perpetual".into(),
        strategy_version: "test-exact-perpetual-v1".into(),
        source_sha256: "b".repeat(64),
    };
    let outcome = run_depth_backtest_with_strategy(
        &request,
        &identity,
        Box::new(ExactDerivativeDepthBuyOnce(false)),
        &RunContext::new(CallerCapability::Research, run_id),
    )
    .expect("exact derivative depth strategy runs through the shared Rust application path");

    assert_eq!(outcome.fills, 1);
    let summary: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&outcome.artifacts.summary).unwrap())
            .expect("exact derivative summary");
    assert_eq!(
        summary["metrics_exact"]["fees_by_currency"]["USDT"],
        "0.000000000206"
    );
    assert_eq!(summary["fees_raw"], 0);
    assert_eq!(
        summary["metrics_exact"]["turnover"]["amount"],
        "0.0000000206"
    );
    let report = qx_app::read_depth_run_report_v1(
        &outcome,
        &RunContext::new(CallerCapability::Research, "read-exact-derivative-depth"),
    )
    .expect("exact derivative report reader validates the committed artifacts");
    assert_eq!(
        report.summary["metrics_exact"]["fees_by_currency"]["USDT"],
        "0.000000000206"
    );
}

#[test]
fn multiple_custom_strategies_share_tick_and_order_book_portfolio_slots() {
    let root = scratch("app-custom-depth-portfolio");
    for (tier, levels, event_name) in [("l1", 1, "tick"), ("l2", 2, "order_book")] {
        let run_id = format!("custom-depth-portfolio-{tier}");
        let run_root = root.join(tier);
        std::fs::create_dir_all(&run_root).expect("create output");
        let mut request = spec(&run_id, &run_root, tier, levels);
        request.strategy = BuiltinStrategySpec::new(BuiltinStrategyKind::SmaCross, "depth-sma");
        request.initial_cash_raw = 1_000_000 * 1_000_000_000;
        request.portfolio = Some(PortfolioBacktestConfig {
            primary_priority: 0,
            primary_reserved_capital_raw: 500_000 * 1_000_000_000,
            primary_reserved_capital_exact: None,
            additional_strategies: vec![PortfolioStrategySpec {
                priority: 1,
                reserved_capital_raw: 500_000 * 1_000_000_000,
                reserved_capital_exact: None,
                strategy: BuiltinStrategySpec::new(BuiltinStrategyKind::Momentum, "depth-momentum"),
            }],
        });
        let primary_event = std::sync::Arc::new(std::sync::Mutex::new(None));
        let child_event = std::sync::Arc::new(std::sync::Mutex::new(None));
        let identity = |strategy_id: &str, source: char| StrategyArtifactIdentity {
            strategy_id: strategy_id.into(),
            strategy_version: format!("{strategy_id}-v1"),
            source_sha256: source.to_string().repeat(64),
        };
        let outcome = run_depth_backtest_with_strategy_slots(
            &request,
            vec![
                (
                    identity("depth-sma", 'd'),
                    Box::new(BuyOnFirstEvent(false, primary_event.clone())) as Box<dyn Strategy>,
                ),
                (
                    identity("depth-momentum", 'e'),
                    Box::new(BuyOnFirstEvent(false, child_event.clone())) as Box<dyn Strategy>,
                ),
            ],
            &RunContext::new(CallerCapability::Research, &run_id),
        )
        .expect("multiple caller callbacks share the Rust portfolio");
        assert_eq!(
            *primary_event.lock().expect("primary event"),
            Some(event_name)
        );
        assert_eq!(*child_event.lock().expect("child event"), Some(event_name));
        assert!(outcome.fills >= 2, "both callback strategies must fill");
        let fills = std::fs::read_to_string(&outcome.artifacts.fills).expect("fills");
        let owners = fills
            .lines()
            .skip(1)
            .filter_map(|line| line.split(',').nth(6))
            .collect::<std::collections::BTreeSet<_>>();
        assert!(owners.contains("depth-sma"));
        assert!(owners.contains("depth-momentum"));
        verify_depth_run(
            &outcome,
            &RunContext::new(CallerCapability::Research, format!("verify-{run_id}")),
        )
        .expect("multi-callback Tick/OrderBook run replays");
        let rerun = run_depth_backtest_with_strategy_slots(
            &request,
            vec![
                (
                    identity("depth-momentum", 'e'),
                    Box::new(BuyOnFirstEvent(false, child_event.clone())) as Box<dyn Strategy>,
                ),
                (
                    identity("depth-sma", 'd'),
                    Box::new(BuyOnFirstEvent(false, primary_event.clone())) as Box<dyn Strategy>,
                ),
            ],
            &RunContext::new(CallerCapability::Research, format!("rerun-{run_id}")),
        )
        .expect("binding order does not affect deterministic portfolio identity");
        assert_eq!(outcome.result_hash, rerun.result_hash);
        let changed_identity = run_depth_backtest_with_strategy_slots(
            &request,
            vec![
                (
                    identity("depth-sma", '3'),
                    Box::new(BuyOnFirstEvent(false, primary_event.clone())) as Box<dyn Strategy>,
                ),
                (
                    identity("depth-momentum", 'e'),
                    Box::new(BuyOnFirstEvent(false, child_event.clone())) as Box<dyn Strategy>,
                ),
            ],
            &RunContext::new(CallerCapability::Research, format!("changed-{run_id}")),
        )
        .expect("changed strategy source is a distinct content-addressed run");
        assert_ne!(
            changed_identity.artifacts.run_manifest,
            outcome.artifacts.run_manifest
        );
    }
}
