use crate::{
    run_multi_market_depth_backtest_with_strategy_slots, run_multi_market_depth_experiment,
    AppErrorCategory, BuiltinStrategySpec, CallerCapability, ExperimentParameterSpace,
    MultiMarketDepthBacktestMarket, MultiMarketDepthBacktestSpec, RunContext,
    RunMultiMarketDepthExperimentSpec, StrategyArtifactIdentity,
};
use qx_core::{
    InstrumentId, OrderPolicy, Price, Quantity, Side, TradingInstrumentSpec, TradingProduct, SCALE,
};
use qx_strategy::{MarketEvent, Strategy, StrategyContext, StrategyDecision, StrategyOrderIntent};
use qx_xingban::{BookLevel, DepthFrame, OrderBookSnapshot};
use serde_json::Value;

struct BuyOnce(bool);

impl Strategy for BuyOnce {
    fn on_event(
        &mut self,
        context: &StrategyContext,
        event: &MarketEvent,
    ) -> Result<StrategyDecision, String> {
        let MarketEvent::OrderBook { instrument, ts, .. } = event else {
            return Err("expected order-book event".into());
        };
        if self.0 {
            return Ok(StrategyDecision {
                schema_version: 1,
                request_id: format!("{}-{ts}", context.strategy_id),
                strategy_id: context.strategy_id.clone(),
                signal_id: *ts,
                confidence: 10_000,
                priority: 0,
                expires_at: 0,
                intents: Vec::new(),
            });
        }
        self.0 = true;
        Ok(StrategyDecision {
            schema_version: 1,
            request_id: format!("{}-{ts}", context.strategy_id),
            strategy_id: context.strategy_id.clone(),
            signal_id: *ts,
            confidence: 10_000,
            priority: 0,
            expires_at: 0,
            intents: vec![StrategyOrderIntent {
                intent_id: 1,
                instrument: instrument.clone(),
                side: Side::Buy,
                qty: Quantity::from_i64(1),
                limit: None,
                policy: None,
                reduce_only: false,
                post_only: false,
            }],
        })
    }
}

struct BuyOnceOnTick {
    emitted: bool,
    observed: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl Strategy for BuyOnceOnTick {
    fn on_event(
        &mut self,
        context: &StrategyContext,
        event: &MarketEvent,
    ) -> Result<StrategyDecision, String> {
        let MarketEvent::Tick {
            instrument,
            ts,
            bid_raw,
            ask_raw,
            last_raw,
            volume_raw,
        } = event
        else {
            return Err("L1 market must reach the strategy as Tick".into());
        };
        assert_eq!((*bid_raw, *ask_raw), (80 * SCALE, 80 * SCALE));
        assert_eq!(*last_raw, Some(80 * SCALE));
        assert_eq!(*volume_raw, Some(4 * SCALE));
        self.observed
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let intents = if self.emitted {
            Vec::new()
        } else {
            self.emitted = true;
            vec![StrategyOrderIntent {
                intent_id: 1,
                instrument: instrument.clone(),
                side: Side::Buy,
                qty: Quantity::from_i64(1),
                limit: None,
                policy: None,
                reduce_only: false,
                post_only: false,
            }]
        };
        Ok(StrategyDecision {
            schema_version: 1,
            request_id: format!("{}-{ts}", context.strategy_id),
            strategy_id: context.strategy_id.clone(),
            signal_id: *ts,
            confidence: 10_000,
            priority: 0,
            expires_at: 0,
            intents,
        })
    }
}

fn product(instrument: &str, base: &str) -> TradingInstrumentSpec {
    TradingInstrumentSpec {
        instrument: InstrumentId::parse(instrument).unwrap(),
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
        max_leverage: 1,
        maintenance_margin_bps: 0,
        valid_from: 1,
        valid_to: None,
    }
}

fn spec(root: &std::path::Path, run_id: &str) -> MultiMarketDepthBacktestSpec {
    let instruments = [("BTCUSDT.BINANCE", "BTC", 80), ("ETHUSDT.BYBIT", "ETH", 60)];
    let markets = instruments
        .into_iter()
        .enumerate()
        .map(|(index, (instrument, base, ask))| {
            let id = InstrumentId::parse(instrument).unwrap();
            let level = |price| BookLevel {
                price: Price::from_i64(price),
                qty: Quantity::from_i64(2),
            };
            let frame = DepthFrame {
                schema_version: DepthFrame::SUPPORTED_SCHEMA_VERSION,
                source: format!("fixture-{instrument}"),
                instrument: id.clone(),
                snapshots: [1_u64, 2]
                    .map(|ts| OrderBookSnapshot {
                        instrument: id.clone(),
                        ts,
                        sequence: ts,
                        bids: vec![level(ask)],
                        asks: vec![level(ask)],
                    })
                    .to_vec(),
            };
            let path = root.join(format!("depth-{index}.json"));
            std::fs::write(&path, serde_json::to_vec(&frame).unwrap()).unwrap();
            let strategy = BuiltinStrategySpec::new(
                qx_strategy::BuiltinStrategyKind::SmaCross,
                format!("strategy-{index}"),
            );
            MultiMarketDepthBacktestMarket {
                instrument: instrument.into(),
                depth_path: path.to_string_lossy().into_owned(),
                data_tier: None,
                priority: if index == 0 { 1 } else { 0 },
                market_spec: product(instrument, base),
                fee_bps: 0,
                latency_snapshots: 0,
                queue_position_bps: 0,
                market_impact_bps: 0,
                strategy,
                order_policy: Some(OrderPolicy::default()),
            }
        })
        .collect();
    MultiMarketDepthBacktestSpec {
        schema_version: 1,
        run_id: run_id.into(),
        account_id: "shared-account".into(),
        settlement_currency: "USDT".into(),
        initial_cash_raw: 100 * SCALE,
        output_dir: root.join("out").to_string_lossy().into_owned(),
        markets,
    }
}

fn identity(strategy_id: &str) -> StrategyArtifactIdentity {
    StrategyArtifactIdentity {
        strategy_id: strategy_id.into(),
        strategy_version: "test-v1".into(),
        source_sha256: "a".repeat(64),
    }
}

fn research(id: &str) -> RunContext {
    RunContext::new(CallerCapability::Research, id)
}

#[test]
fn multi_market_orderbook_application_shares_cash_and_accepts_language_callbacks() {
    let root = super::fixtures::scratch("multi-market-depth-e2e");
    let mut spec = spec(&root, "multi-market-depth-e2e");
    spec.markets[0].data_tier = Some("l1".into());
    let observed_ticks = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let strategies = vec![
        (
            identity("strategy-0"),
            Box::new(BuyOnceOnTick {
                emitted: false,
                observed: observed_ticks.clone(),
            }) as Box<dyn Strategy>,
        ),
        (
            identity("strategy-1"),
            Box::new(BuyOnce(false)) as Box<dyn Strategy>,
        ),
    ];
    let outcome = run_multi_market_depth_backtest_with_strategy_slots(
        &spec,
        strategies,
        &research("multi-market-depth-e2e"),
    )
    .expect("shared L2 portfolio run");
    assert_eq!(outcome.markets_total, 2);
    assert_eq!(outcome.successful_fills, 1);
    assert_eq!(outcome.final_equity_raw, 100 * SCALE);
    assert_eq!(observed_ticks.load(std::sync::atomic::Ordering::SeqCst), 2);

    let artifact: Value =
        serde_json::from_slice(&std::fs::read(&outcome.artifact_path).expect("artifact exists"))
            .expect("artifact JSON");
    assert_eq!(artifact["replay_verified"], true);
    assert_eq!(artifact["market_data"].as_array().unwrap().len(), 2);
    assert_eq!(artifact["market_data"][0]["data_tier"], "l1");
    assert_eq!(artifact["market_data"][1]["data_tier"], "l2");
    assert_eq!(artifact["strategy_identities"].as_array().unwrap().len(), 2);
    assert_eq!(artifact["per_market_fills"]["ETHUSDT.BYBIT"], 1);
    assert_eq!(artifact["per_market_fills"]["BTCUSDT.BINANCE"], 0);
    println!(
        "MULTI_MARKET_TICK_ORDERBOOK_OBSERVED markets=2 tick_events={} shared_fills={} replay_verified=true",
        observed_ticks.load(std::sync::atomic::Ordering::SeqCst),
        outcome.successful_fills
    );

    let repeated = run_multi_market_depth_backtest_with_strategy_slots(
        &spec,
        vec![
            (
                identity("strategy-0"),
                Box::new(BuyOnceOnTick {
                    emitted: false,
                    observed: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                }),
            ),
            (identity("strategy-1"), Box::new(BuyOnce(false))),
        ],
        &research("multi-market-depth-e2e"),
    )
    .expect("identical run is idempotent");
    assert_eq!(outcome.shared_result_hash, repeated.shared_result_hash);
}

#[test]
fn multi_market_depth_rejects_derivatives_before_reading_market_data() {
    let root = super::fixtures::scratch("multi-market-depth-guards");
    let mut invalid = spec(&root, "multi-market-depth-invalid");
    invalid.markets[0].market_spec.product = TradingProduct::Perpetual;
    assert_eq!(
        invalid.validate().unwrap_err().category(),
        AppErrorCategory::InvalidInput
    );
    let mut invalid_tier = spec(&root, "multi-market-depth-invalid-tier");
    invalid_tier.markets[0].data_tier = Some("l3-only".into());
    assert_eq!(
        invalid_tier.validate().unwrap_err().category(),
        AppErrorCategory::InvalidInput
    );
    let mut handle = spec(&root, "multi-market-depth-handle").start(research("depth-handle"));
    assert_eq!(handle.wait(), crate::RunStatus::Succeeded);
    handle
        .try_take_result()
        .expect("finished result")
        .expect("run succeeds");
}

#[test]
fn multi_market_depth_experiment_sweeps_strategy_and_parameters_deterministically() {
    let root = super::fixtures::scratch("multi-market-depth-experiment");
    let base = spec(&root, "unused-base");
    let experiment = RunMultiMarketDepthExperimentSpec {
        schema_version: 1,
        experiment_id: "multi-market-depth-grid".into(),
        target_strategy_id: "strategy-0".into(),
        base,
        parameter_space: vec![ExperimentParameterSpace {
            name: "quantity_raw".into(),
            values: vec![SCALE / 2, SCALE],
        }],
        callback_parameter_space: vec![],
        strategy_kinds: vec![
            qx_strategy::BuiltinStrategyKind::SmaCross,
            qx_strategy::BuiltinStrategyKind::Momentum,
        ],
    };
    assert_eq!(experiment.validate().unwrap(), 4);
    let result = run_multi_market_depth_experiment(&experiment, &research("depth-grid"))
        .expect("all grid candidates run through the portfolio use case");
    assert_eq!(result.total_candidates, 4);
    assert_eq!(result.completed_candidates, 4);
    assert_eq!(result.failed_candidates, 0);
    assert_eq!(result.ranking.len(), 4);
    assert!(result
        .candidates
        .iter()
        .all(|candidate| candidate.outcome.is_some()));
    let repeated = run_multi_market_depth_experiment(&experiment, &research("depth-grid"))
        .expect("identical experiment is idempotent");
    assert_eq!(result.to_json().unwrap(), repeated.to_json().unwrap());
}

#[test]
fn multi_market_depth_experiment_isolates_callback_panics_per_candidate() {
    let root = super::fixtures::scratch("multi-market-depth-experiment-callback-panic");
    let experiment = RunMultiMarketDepthExperimentSpec {
        schema_version: 1,
        experiment_id: "multi-market-depth-callback-panic".into(),
        target_strategy_id: "strategy-0".into(),
        base: spec(&root, "unused-base"),
        parameter_space: vec![],
        callback_parameter_space: vec![ExperimentParameterSpace {
            name: "mode".into(),
            values: vec![1, 2],
        }],
        strategy_kinds: vec![],
    };
    let result = experiment
        .run_with_strategy_factory(
            &identity("strategy-0"),
            &mut |_candidate, parameters| {
                if parameters["mode"] == 1 {
                    panic!("intentional candidate factory panic");
                }
                Ok(Box::new(BuyOnce(false)))
            },
            &research("depth-callback-panic"),
        )
        .expect("candidate panic should not abort the experiment");

    assert_eq!(result.completed_candidates, 2);
    assert_eq!(result.failed_candidates, 1);
    assert_eq!(result.succeeded_candidates, 1);
    assert!(result.candidates[0].error.is_some());
    assert!(result.candidates[1].outcome.is_some());
}
