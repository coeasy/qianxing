mod common;

use common::scratch;
use qx_app::{
    run_multi_market_depth_backtest, run_multi_market_depth_backtest_with_strategy_slots,
    run_multi_market_depth_experiment, validate_depth_dataset_manifest, AppErrorCategory,
    BuiltinStrategySpec, CallerCapability, DepthDatasetManifestValidationSpec,
    ExperimentParameterSpace, MultiMarketDepthBacktestMarket, MultiMarketDepthBacktestSpec,
    RunContext, RunMultiMarketDepthExperimentSpec, RunStatus, StrategyArtifactIdentity,
};
use qx_core::{
    ExactDecimal, ExactTradingRules, InstrumentId, MarginMode, OrderPolicy, PositionMode,
    PositionSide, Price, Quantity, Side, TradingInstrumentSpec, TradingProduct, SCALE,
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

                qty_exact: None,
                limit_price_exact: None,
                quote_budget: None,
            }],
        })
    }
}

struct ExactQuoteBudgetOnce(bool);

impl Strategy for ExactQuoteBudgetOnce {
    fn on_event(
        &mut self,
        context: &StrategyContext,
        event: &MarketEvent,
    ) -> Result<StrategyDecision, String> {
        let MarketEvent::OrderBook { instrument, ts, .. } = event else {
            return Err("expected exact order-book event".into());
        };
        let intents = if self.0 {
            Vec::new()
        } else {
            self.0 = true;
            vec![StrategyOrderIntent {
                intent_id: 1,
                instrument: instrument.clone(),
                side: Side::Buy,
                qty: Quantity::ZERO,
                limit: None,
                policy: None,
                reduce_only: false,
                post_only: false,
                qty_exact: None,
                limit_price_exact: None,
                quote_budget: Some(ExactDecimal::parse("0.0000000006")?),
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

struct ExactDerivativeBuyOnce(bool);

impl Strategy for ExactDerivativeBuyOnce {
    fn on_event(
        &mut self,
        context: &StrategyContext,
        event: &MarketEvent,
    ) -> Result<StrategyDecision, String> {
        let MarketEvent::OrderBook { instrument, ts, .. } = event else {
            return Err("expected derivative order-book event".into());
        };
        let intents = if self.0 {
            Vec::new()
        } else {
            self.0 = true;
            vec![StrategyOrderIntent {
                intent_id: 1,
                instrument: instrument.clone(),
                side: Side::Buy,
                qty: Quantity::ZERO,
                limit: None,
                policy: Some(OrderPolicy {
                    margin_mode: MarginMode::Cross,
                    position_mode: PositionMode::OneWay,
                    position_side: PositionSide::Net,
                    leverage: 2,
                    ..OrderPolicy::default()
                }),
                reduce_only: false,
                post_only: false,
                qty_exact: Some(ExactDecimal::ONE),
                limit_price_exact: None,
                quote_budget: None,
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

                qty_exact: None,
                limit_price_exact: None,
                quote_budget: None,
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
        min_notional_raw: 0,
        max_leverage: 1,
        maintenance_margin_bps: 0,
        valid_from: 1,
        valid_to: None,
        exact_rules: None,
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
                price_exact: None,
                qty_exact: None,
                price: Price::from_i64(price),
                qty: Quantity::from_i64(2),
            };
            let frame = DepthFrame {
                schema_version: DepthFrame::LEGACY_SCHEMA_VERSION,
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
                dataset_manifest_path: None,
                data_tier: None,
                priority: if index == 0 { 1 } else { 0 },
                market_spec: product(instrument, base),
                fee_bps: 0,
                funding_bps: 0,
                funding_events: Vec::new(),
                latency_snapshots: 0,
                queue_position_bps: 0,
                queue_position_model: Default::default(),
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
        initial_cash_exact: None,
        output_dir: root.join("out").to_string_lossy().into_owned(),
        markets,
    }
}

#[test]
fn multi_market_depth_binds_quality_manifest_to_run_identity_and_rejects_drift() {
    let root = scratch("multi-market-depth-quality");
    let mut run = spec(&root, "multi-market-depth-quality");
    let market = &mut run.markets[0];
    let manifest_path = root.join("depth-quality.json");
    let context = RunContext::new(CallerCapability::Research, run.run_id.clone());
    validate_depth_dataset_manifest(
        &DepthDatasetManifestValidationSpec::new(
            market.depth_path.clone(),
            qx_data::DatasetTier::L2,
            1,
            "UTC",
            manifest_path.to_string_lossy().to_string(),
        ),
        &context,
    )
    .unwrap();
    market.dataset_manifest_path = Some(manifest_path.to_string_lossy().to_string());
    let outcome = run_multi_market_depth_backtest(&run, &context).unwrap();
    let artifact: Value =
        serde_json::from_slice(&std::fs::read(&outcome.artifact_path).unwrap()).unwrap();
    let quality = &artifact["market_data"][0]["dataset_quality"];
    assert!(quality["manifest_fingerprint"].as_str().is_some());
    let quality_report = qx_app::read_multi_market_depth_research_report_v1(
        &qx_app::MultiMarketDepthResearchReportReadRequestV1 {
            schema_version: qx_app::MULTI_MARKET_DEPTH_RESEARCH_REPORT_SCHEMA_VERSION_V1,
            outcome: outcome.clone(),
            page: None,
        },
        &context,
    )
    .expect("complete report preserves verified dataset quality details");
    let quality_panel = quality_report.markets[0]
        .dataset_quality
        .as_ref()
        .expect("quality manifest is surfaced in the complete report");
    assert_eq!(
        quality_panel["manifest_fingerprint"],
        quality["manifest_fingerprint"]
    );
    assert_eq!(quality_panel["manifest_sha256"], quality["manifest_sha256"]);
    assert!(quality_panel["quality_report"].is_object());
    let run_manifest_path = artifact["run_manifest_path"].as_str().unwrap();
    let run_manifest: qx_core::RunManifest =
        qx_core::RunManifest::from_json(&std::fs::read_to_string(run_manifest_path).unwrap())
            .unwrap();
    assert!(run_manifest
        .input_components
        .contains_key(&format!("dataset-quality:{}", run.markets[0].instrument)));
    let canonical = qx_spec::CanonicalRunSpec::from_json(
        run_manifest.canonical_run_spec_json.as_deref().unwrap(),
    )
    .unwrap();
    assert_eq!(
        canonical.extensions[&format!("dataset_quality:{}", run.markets[0].instrument)],
        quality["manifest_fingerprint"]
    );
    qx_app::artifact_store::verify_canonical_run_spec_for_manifest(&run_manifest, &artifact)
        .unwrap();

    std::fs::write(&manifest_path, "tampered").unwrap();
    let mut drifted = run.clone();
    drifted.run_id = "multi-market-depth-quality-drift".into();
    drifted.output_dir = root.join("drifted-out").to_string_lossy().to_string();
    let error = run_multi_market_depth_backtest(
        &drifted,
        &RunContext::new(CallerCapability::Research, drifted.run_id.clone()),
    )
    .unwrap_err();
    assert_eq!(error.category(), AppErrorCategory::InvalidInput);
    assert!(!std::path::Path::new(&drifted.output_dir).exists());
}

#[test]
fn multi_market_depth_canonical_identity_is_path_independent_and_read_verified() {
    let first_root = scratch("multi-market-depth-canonical-first");
    let first_spec = spec(&first_root, "multi-market-depth-canonical-first");
    let first_outcome = run_multi_market_depth_backtest(
        &first_spec,
        &research("multi-market-depth-canonical-first"),
    )
    .expect("first portfolio run");
    let report = qx_app::read_multi_market_depth_research_report_v1(
        &qx_app::MultiMarketDepthResearchReportReadRequestV1 {
            schema_version: qx_app::MULTI_MARKET_DEPTH_RESEARCH_REPORT_SCHEMA_VERSION_V1,
            outcome: first_outcome.clone(),
            page: Some(qx_app::MultiMarketEventReportPageRequestV1 {
                offset: 0,
                limit: 1,
            }),
        },
        &research(&first_outcome.run_id),
    )
    .expect("complete application report");
    assert_eq!(report.run_id, first_outcome.run_id);
    assert_eq!(report.markets.len(), 2);
    assert!(report.markets.iter().all(|market| {
        market.depth.total == 2
            && !market.source.is_empty()
            && market.content_hash.len() == 16
            && market.dataset_quality.is_none()
    }));
    assert_eq!(report.equity.total, first_outcome.equity_points);
    assert!(report
        .equity
        .rows
        .iter()
        .all(|row| row.get("equity_exact").and_then(Value::as_str).is_some()));
    assert!(report
        .summary
        .pointer("/metrics_exact/equity/final")
        .is_some());
    assert_eq!(report.fills.total, first_outcome.successful_fills);
    assert_eq!(
        Some(report.events.total),
        report.summary["event_count"].as_u64()
    );
    assert_eq!(report.events.rows.len(), 1);

    let first_artifact: Value =
        serde_json::from_slice(&std::fs::read(&first_outcome.artifact_path).unwrap()).unwrap();
    let first_manifest_path = first_artifact["run_manifest_path"].as_str().unwrap();
    let first_manifest =
        qx_core::RunManifest::from_json(&std::fs::read_to_string(first_manifest_path).unwrap())
            .unwrap();
    let canonical_json = first_manifest.canonical_run_spec_json.as_deref().unwrap();
    let canonical = qx_spec::CanonicalRunSpec::from_json(canonical_json).unwrap();
    assert_eq!(canonical.run_type, "multi_market_depth_backtest");
    assert_eq!(canonical.datasets.len(), 2);
    assert!(canonical.datasets.iter().all(|dataset| {
        dataset.role == "depth_snapshots" && dataset.start_ts_ms == 1 && dataset.end_ts_ms == 2
    }));
    assert!(!canonical_json.contains(&first_root.to_string_lossy().to_string()));
    assert!(!canonical_json.contains("fixture-BTCUSDT"));
    assert!(canonical_json.contains("\"seed\":\"0\""));
    assert_eq!(canonical.behavior["initial_cash_raw"], "100000000000");
    assert!(canonical.behavior["markets"][0].get("depth_path").is_none());
    assert!(canonical.behavior["markets"][0]
        .get("dataset_manifest_path")
        .is_none());
    qx_app::artifact_store::verify_canonical_run_spec_for_manifest(
        &first_manifest,
        &first_artifact,
    )
    .unwrap();

    let second_root = scratch("multi-market-depth-canonical-second");
    let second_spec = spec(&second_root, "multi-market-depth-canonical-second");
    let second_outcome = run_multi_market_depth_backtest(
        &second_spec,
        &research("multi-market-depth-canonical-second"),
    )
    .expect("relocated portfolio run");
    let second_artifact: Value =
        serde_json::from_slice(&std::fs::read(&second_outcome.artifact_path).unwrap()).unwrap();
    let second_manifest = qx_core::RunManifest::from_json(
        &std::fs::read_to_string(second_artifact["run_manifest_path"].as_str().unwrap()).unwrap(),
    )
    .unwrap();
    assert_eq!(
        first_manifest.canonical_run_spec_fingerprint,
        second_manifest.canonical_run_spec_fingerprint
    );
    qx_app::artifact_store::verify_canonical_run_spec_for_manifest(
        &second_manifest,
        &second_artifact,
    )
    .unwrap();

    let mut tampered = first_artifact;
    tampered["market_data"][0]["end_ts_ms"] = serde_json::json!(99);
    assert!(
        qx_app::artifact_store::verify_canonical_run_spec_for_manifest(&first_manifest, &tampered)
            .is_err()
    );
}

#[test]
fn multi_market_depth_complete_report_rejects_equity_sidecar_drift() {
    let root = scratch("multi-market-depth-complete-report-drift");
    let run = spec(&root, "multi-market-depth-complete-report-drift");
    let outcome = run_multi_market_depth_backtest(&run, &research(&run.run_id))
        .expect("multi-market depth run");
    let artifact: Value =
        serde_json::from_slice(&std::fs::read(&outcome.artifact_path).unwrap()).unwrap();
    let equity_path = artifact["equity_path"]
        .as_str()
        .expect("equity sidecar path");
    let mut equity: Value =
        serde_json::from_slice(&std::fs::read(equity_path).unwrap()).expect("equity sidecar");
    let last = equity
        .as_array_mut()
        .and_then(|rows| rows.last_mut())
        .expect("final equity point");
    let final_raw = last[1]
        .as_i64()
        .map(i128::from)
        .or_else(|| last[1].as_str().and_then(|raw| raw.parse::<i128>().ok()))
        .expect("raw equity integer");
    last[1] = Value::String((final_raw + 1).to_string());
    std::fs::write(equity_path, serde_json::to_vec(&equity).unwrap()).unwrap();

    let error = qx_app::read_multi_market_depth_research_report_v1(
        &qx_app::MultiMarketDepthResearchReportReadRequestV1 {
            schema_version: qx_app::MULTI_MARKET_DEPTH_RESEARCH_REPORT_SCHEMA_VERSION_V1,
            outcome: outcome.clone(),
            page: None,
        },
        &research(&outcome.run_id),
    )
    .expect_err("equity sidecar drift must not be reported as verified");
    assert_eq!(error.category(), AppErrorCategory::Conflict);
}

#[test]
fn multi_market_depth_complete_report_rejects_exact_equity_sidecar_drift() {
    let root = scratch("multi-market-depth-exact-equity-drift");
    let run = spec(&root, "multi-market-depth-exact-equity-drift");
    let outcome = run_multi_market_depth_backtest(&run, &research(&run.run_id))
        .expect("multi-market depth run");
    let artifact: Value =
        serde_json::from_slice(&std::fs::read(&outcome.artifact_path).unwrap()).unwrap();
    let exact_path = artifact["equity_exact_path"]
        .as_str()
        .expect("exact equity sidecar path");
    let mut exact: Value =
        serde_json::from_slice(&std::fs::read(exact_path).unwrap()).expect("exact equity sidecar");
    let last = exact
        .as_array_mut()
        .and_then(|rows| rows.last_mut())
        .expect("final exact equity point");
    last["equity_exact"] = Value::String("999999999".into());
    std::fs::write(exact_path, serde_json::to_vec(&exact).unwrap()).unwrap();

    let error = qx_app::read_multi_market_depth_research_report_v1(
        &qx_app::MultiMarketDepthResearchReportReadRequestV1 {
            schema_version: qx_app::MULTI_MARKET_DEPTH_RESEARCH_REPORT_SCHEMA_VERSION_V1,
            outcome: outcome.clone(),
            page: None,
        },
        &research(&outcome.run_id),
    )
    .expect_err("exact equity sidecar drift must not be reported as verified");
    assert_eq!(error.category(), AppErrorCategory::Conflict);
}

fn write_oscillating_depth(root: &std::path::Path, slot: usize, instrument: &str) -> String {
    let id = InstrumentId::parse(instrument).unwrap();
    let snapshots = (0..48)
        .map(|index| {
            let phase = index % 20;
            let mid = if phase < 10 {
                100 + phase * 2
            } else {
                100 + (20 - phase) * 2
            };
            OrderBookSnapshot {
                instrument: id.clone(),
                ts: 1_000 + index as u64 * 1_000,
                sequence: index as u64 + 1,
                bids: vec![BookLevel {
                    price_exact: None,
                    qty_exact: None,
                    price: Price::from_i64(mid - 1),
                    qty: Quantity::from_i64(100),
                }],
                asks: vec![BookLevel {
                    price_exact: None,
                    qty_exact: None,
                    price: Price::from_i64(mid + 1),
                    qty: Quantity::from_i64(100),
                }],
            }
        })
        .collect();
    let frame = DepthFrame {
        schema_version: DepthFrame::LEGACY_SCHEMA_VERSION,
        source: format!("oscillating-{instrument}"),
        instrument: id,
        snapshots,
    };
    let path = root.join(format!("oscillating-{slot}.json"));
    std::fs::write(&path, serde_json::to_vec(&frame).unwrap()).unwrap();
    path.to_string_lossy().into_owned()
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
    let root = scratch("multi-market-depth-e2e");
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
    let manifest = qx_core::RunManifest::from_json(
        &std::fs::read_to_string(artifact["run_manifest_path"].as_str().unwrap()).unwrap(),
    )
    .unwrap();
    let canonical =
        qx_spec::CanonicalRunSpec::from_json(manifest.canonical_run_spec_json.as_deref().unwrap())
            .unwrap();
    assert!(canonical
        .strategies
        .iter()
        .all(|strategy| strategy.source_sha256.as_deref() == Some(&"a".repeat(64))));
    qx_app::artifact_store::verify_canonical_run_spec_for_manifest(&manifest, &artifact).unwrap();
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
fn multi_market_depth_v2_exact_spot_report_verifies_shared_cash_fees_and_turnover() {
    let root = scratch("multi-market-depth-v2-exact-financial-report");
    let run_id = "multi-market-depth-v2-exact-financial-report";
    let mut run = spec(&root, run_id);
    run.schema_version = qx_app::MULTI_MARKET_DEPTH_BACKTEST_EXACT_SCHEMA_VERSION;
    run.initial_cash_raw = 1;
    run.initial_cash_exact = Some(ExactDecimal::parse("0.0000000013").unwrap());
    for (index, market) in run.markets.iter_mut().enumerate() {
        let id = InstrumentId::parse(&market.instrument).unwrap();
        let bid = BookLevel::from_exact(
            ExactDecimal::from_parts(100, 0).unwrap(),
            ExactDecimal::from_parts(1, 0).unwrap(),
        )
        .unwrap();
        let ask = BookLevel::from_exact(
            ExactDecimal::from_parts(100, 0).unwrap(),
            ExactDecimal::from_parts(1, 0).unwrap(),
        )
        .unwrap();
        let frame = DepthFrame {
            schema_version: DepthFrame::EXACT_SCHEMA_VERSION,
            source: format!("exact-{instrument}", instrument = market.instrument),
            instrument: id.clone(),
            snapshots: [1_u64, 2]
                .map(|ts| OrderBookSnapshot {
                    instrument: id.clone(),
                    ts,
                    sequence: ts,
                    bids: vec![bid],
                    asks: vec![ask],
                })
                .to_vec(),
        };
        let path = root.join(format!("exact-depth-{index}.json"));
        std::fs::write(&path, serde_json::to_vec(&frame).unwrap()).unwrap();
        market.depth_path = path.to_string_lossy().into_owned();
        market.priority = index as u32;
        market.fee_bps = 100;
        market.market_spec.exact_rules = Some(ExactTradingRules {
            contract_size: ExactDecimal::ONE,
            price_tick: ExactDecimal::from_parts(1, 2).unwrap(),
            quantity_step: ExactDecimal::parse("0.000000000001").unwrap(),
            minimum_quantity: ExactDecimal::parse("0.000000000001").unwrap(),
            minimum_notional: ExactDecimal::ZERO,
        });
    }
    let strategies = run
        .markets
        .iter()
        .map(|market| {
            (
                identity(&market.strategy.strategy_id),
                Box::new(ExactQuoteBudgetOnce(false)) as Box<dyn Strategy>,
            )
        })
        .collect();
    let outcome =
        run_multi_market_depth_backtest_with_strategy_slots(&run, strategies, &research(run_id))
            .expect("shared exact-spot portfolio run");
    assert_eq!(outcome.successful_fills, 2);

    let report = qx_app::read_multi_market_depth_research_report_v1(
        &qx_app::MultiMarketDepthResearchReportReadRequestV1 {
            schema_version: qx_app::MULTI_MARKET_DEPTH_RESEARCH_REPORT_SCHEMA_VERSION_V1,
            outcome: outcome.clone(),
            page: None,
        },
        &research(run_id),
    )
    .expect("exact metrics are recomputed from fills and ledger");
    assert_eq!(report.summary["markets_total"], 2);
    assert_eq!(report.summary["successful_fills"], 2);
    assert_eq!(
        report.summary.pointer("/metrics_exact/equity/initial"),
        Some(&Value::String("0.0000000013".into()))
    );
    assert_eq!(
        report.summary.pointer("/account/initial_cash_exact"),
        Some(&Value::String("0.0000000013".into()))
    );
    assert_eq!(
        report.summary.pointer("/metrics_exact/turnover/amount"),
        Some(&Value::String("0.0000000012".into()))
    );
    assert_eq!(
        report
            .summary
            .pointer("/metrics_exact/fees_by_currency/USDT"),
        Some(&Value::String("0.000000000012".into()))
    );

    let mut artifact: Value =
        serde_json::from_slice(&std::fs::read(&outcome.artifact_path).unwrap()).unwrap();
    assert_eq!(artifact["schema_version"], 4);
    artifact["metrics_exact"]["turnover"]["amount"] = Value::String("1".into());
    std::fs::write(
        &outcome.artifact_path,
        serde_json::to_vec(&artifact).unwrap(),
    )
    .unwrap();
    let error = qx_app::read_multi_market_depth_research_report_v1(
        &qx_app::MultiMarketDepthResearchReportReadRequestV1 {
            schema_version: qx_app::MULTI_MARKET_DEPTH_RESEARCH_REPORT_SCHEMA_VERSION_V1,
            outcome,
            page: None,
        },
        &research(run_id),
    )
    .expect_err("tampered exact turnover must not be reported as verified");
    assert_eq!(error.category(), AppErrorCategory::Conflict);
}

#[test]
fn multi_market_depth_v2_accepts_positive_subnano_principal_with_zero_raw_projection() {
    let root = scratch("multi-market-depth-subnano-principal");
    let run_id = "multi-market-depth-subnano-principal";
    let mut run = spec(&root, run_id);
    run.schema_version = qx_app::MULTI_MARKET_DEPTH_BACKTEST_EXACT_SCHEMA_VERSION;
    run.initial_cash_raw = 0;
    run.initial_cash_exact = Some(ExactDecimal::parse("0.0000000005").unwrap());
    let context = research(run_id);

    let outcome = run_multi_market_depth_backtest(&run, &context)
        .expect("positive exact principal is valid even when its raw projection is zero");
    assert_eq!(outcome.successful_fills, 0);
    assert_eq!(outcome.final_equity_raw, 0);

    let report = qx_app::read_multi_market_depth_research_report_v1(
        &qx_app::MultiMarketDepthResearchReportReadRequestV1 {
            schema_version: qx_app::MULTI_MARKET_DEPTH_RESEARCH_REPORT_SCHEMA_VERSION_V1,
            outcome,
            page: None,
        },
        &context,
    )
    .expect("complete report verifies the subnano principal and exact curve");
    assert_eq!(
        report.summary.pointer("/metrics_exact/equity/initial"),
        Some(&Value::String("0.0000000005".into()))
    );
    assert_eq!(
        report.summary.pointer("/metrics_exact/equity/final"),
        Some(&Value::String("0.0000000005".into()))
    );
}

#[test]
fn multi_market_depth_exact_linear_derivatives_share_one_margin_budget_and_report() {
    let root = scratch("multi-market-depth-exact-linear-margin");
    let run_id = "multi-market-depth-exact-linear-margin";
    let mut run = spec(&root, run_id);
    run.initial_cash_raw = 60 * SCALE;
    for (index, market) in run.markets.iter_mut().enumerate() {
        market.market_spec.product = TradingProduct::Perpetual;
        market.market_spec.linear = true;
        market.market_spec.inverse = false;
        market.market_spec.max_leverage = 10;
        market.market_spec.exact_rules = Some(ExactTradingRules {
            contract_size: ExactDecimal::ONE,
            price_tick: ExactDecimal::ONE,
            quantity_step: ExactDecimal::ONE,
            minimum_quantity: ExactDecimal::ONE,
            minimum_notional: ExactDecimal::ZERO,
        });
        market.order_policy = Some(OrderPolicy {
            margin_mode: MarginMode::Cross,
            position_mode: PositionMode::OneWay,
            position_side: PositionSide::Net,
            leverage: 2,
            ..OrderPolicy::default()
        });
        market.fee_bps = 0;
        market.priority = index as u32;
        let instrument = InstrumentId::parse(&market.instrument).unwrap();
        let level = BookLevel::from_exact(
            ExactDecimal::from_parts(100, 0).unwrap(),
            ExactDecimal::from_parts(10, 0).unwrap(),
        )
        .unwrap();
        let frame = DepthFrame {
            schema_version: DepthFrame::EXACT_SCHEMA_VERSION,
            source: format!("linear-{instrument}"),
            instrument: instrument.clone(),
            snapshots: [1_u64, 2]
                .map(|ts| OrderBookSnapshot {
                    instrument: instrument.clone(),
                    ts,
                    sequence: ts,
                    bids: vec![level],
                    asks: vec![level],
                })
                .to_vec(),
        };
        let path = root.join(format!("linear-depth-{index}.json"));
        std::fs::write(&path, serde_json::to_vec(&frame).unwrap()).unwrap();
        market.depth_path = path.to_string_lossy().into_owned();
    }
    let strategies = run
        .markets
        .iter()
        .map(|market| {
            (
                identity(&market.strategy.strategy_id),
                Box::new(ExactDerivativeBuyOnce(false)) as Box<dyn Strategy>,
            )
        })
        .collect();

    let outcome =
        run_multi_market_depth_backtest_with_strategy_slots(&run, strategies, &research(run_id))
            .expect("same-settlement derivative portfolio passes the app path");

    assert_eq!(outcome.successful_fills, 1);
    let report = qx_app::read_multi_market_depth_research_report_v1(
        &qx_app::MultiMarketDepthResearchReportReadRequestV1 {
            schema_version: qx_app::MULTI_MARKET_DEPTH_RESEARCH_REPORT_SCHEMA_VERSION_V1,
            outcome,
            page: None,
        },
        &research(run_id),
    )
    .expect("reader verifies shared exact derivative margin run");
    assert_eq!(report.summary["markets_total"], 2);
    assert_eq!(report.summary["successful_fills"], 1);
    assert_eq!(
        report.summary.pointer("/metrics_exact/turnover/amount"),
        Some(&Value::String("100".into()))
    );
    assert_eq!(
        report.summary.pointer("/metrics_exact/input_precision"),
        Some(&Value::String("Shared-account multi-market DepthFrame v2 supports either spec-bound Spot base-quantity portfolios with exact cash/inventory reservations or homogeneous linear/inverse derivative portfolios with one settlement currency and aggregate exact margin/fee projection; derivative fills recheck actual-price margin, funding cashflows preserve exact quantity and mark values, queue estimation is disabled, and mixed Spot/derivative or cross-currency portfolios fail closed. v1 inputs keep the 1e-9 projection.".into()))
    );
}

#[test]
fn multi_market_depth_exact_derivatives_fail_closed_after_fill_slippage_exceeds_margin() {
    let root = scratch("multi-market-depth-exact-slippage-margin");
    let run_id = "multi-market-depth-exact-slippage-margin";
    let mut run = spec(&root, run_id);
    run.initial_cash_raw = 60 * SCALE;
    for (index, market) in run.markets.iter_mut().enumerate() {
        market.market_spec.product = TradingProduct::Perpetual;
        market.market_spec.linear = true;
        market.market_spec.inverse = false;
        market.market_spec.max_leverage = 10;
        market.market_spec.exact_rules = Some(ExactTradingRules {
            contract_size: ExactDecimal::ONE,
            price_tick: ExactDecimal::ONE,
            quantity_step: ExactDecimal::ONE,
            minimum_quantity: ExactDecimal::ONE,
            minimum_notional: ExactDecimal::ZERO,
        });
        market.order_policy = Some(OrderPolicy {
            margin_mode: MarginMode::Cross,
            position_mode: PositionMode::OneWay,
            position_side: PositionSide::Net,
            leverage: 2,
            ..OrderPolicy::default()
        });
        market.fee_bps = 0;
        market.priority = index as u32;
        let instrument = InstrumentId::parse(&market.instrument).unwrap();
        let snapshots = [(1_u64, 100_i128), (2_u64, 200_i128)]
            .map(|(ts, price)| {
                let level = BookLevel::from_exact(
                    ExactDecimal::from_parts(price, 0).unwrap(),
                    ExactDecimal::from_parts(10, 0).unwrap(),
                )
                .unwrap();
                OrderBookSnapshot {
                    instrument: instrument.clone(),
                    ts,
                    sequence: ts,
                    bids: vec![level],
                    asks: vec![level],
                }
            })
            .to_vec();
        let frame = DepthFrame {
            schema_version: DepthFrame::EXACT_SCHEMA_VERSION,
            source: format!("slippage-{instrument}"),
            instrument: instrument.clone(),
            snapshots,
        };
        let path = root.join(format!("slippage-depth-{index}.json"));
        std::fs::write(&path, serde_json::to_vec(&frame).unwrap()).unwrap();
        market.depth_path = path.to_string_lossy().into_owned();
    }
    let strategies = run
        .markets
        .iter()
        .enumerate()
        .map(|(index, market)| {
            (
                identity(&market.strategy.strategy_id),
                Box::new(ExactDerivativeBuyOnce(index != 0)) as Box<dyn Strategy>,
            )
        })
        .collect();

    let result =
        run_multi_market_depth_backtest_with_strategy_slots(&run, strategies, &research(run_id));

    assert!(
        result.is_err(),
        "a fill at twice the reference price must not produce a successful under-margined report"
    );
}

#[test]
fn multi_market_depth_exact_inverse_derivatives_share_one_margin_budget_and_report() {
    let root = scratch("multi-market-depth-exact-inverse-margin");
    let run_id = "multi-market-depth-exact-inverse-margin";
    let mut run = spec(&root, run_id);
    run.settlement_currency = "BTC".into();
    run.initial_cash_raw = 750_000_000; // 0.75 BTC: enough for one, not both, at 2x.
    for (index, market) in run.markets.iter_mut().enumerate() {
        market.market_spec.product = TradingProduct::Perpetual;
        market.market_spec.linear = false;
        market.market_spec.inverse = true;
        market.market_spec.settlement_currency = "BTC".into();
        market.market_spec.max_leverage = 10;
        market.market_spec.exact_rules = Some(ExactTradingRules {
            contract_size: ExactDecimal::from_parts(100, 0).unwrap(),
            price_tick: ExactDecimal::ONE,
            quantity_step: ExactDecimal::ONE,
            minimum_quantity: ExactDecimal::ONE,
            minimum_notional: ExactDecimal::ZERO,
        });
        market.order_policy = Some(OrderPolicy {
            margin_mode: MarginMode::Cross,
            position_mode: PositionMode::OneWay,
            position_side: PositionSide::Net,
            leverage: 2,
            ..OrderPolicy::default()
        });
        market.fee_bps = 0;
        market.priority = index as u32;
        let instrument = InstrumentId::parse(&market.instrument).unwrap();
        let level = BookLevel::from_exact(
            ExactDecimal::from_parts(100, 0).unwrap(),
            ExactDecimal::from_parts(10, 0).unwrap(),
        )
        .unwrap();
        let frame = DepthFrame {
            schema_version: DepthFrame::EXACT_SCHEMA_VERSION,
            source: format!("inverse-{instrument}"),
            instrument: instrument.clone(),
            snapshots: [1_u64, 2]
                .map(|ts| OrderBookSnapshot {
                    instrument: instrument.clone(),
                    ts,
                    sequence: ts,
                    bids: vec![level],
                    asks: vec![level],
                })
                .to_vec(),
        };
        let path = root.join(format!("inverse-depth-{index}.json"));
        std::fs::write(&path, serde_json::to_vec(&frame).unwrap()).unwrap();
        market.depth_path = path.to_string_lossy().into_owned();
    }
    let strategies = run
        .markets
        .iter()
        .map(|market| {
            (
                identity(&market.strategy.strategy_id),
                Box::new(ExactDerivativeBuyOnce(false)) as Box<dyn Strategy>,
            )
        })
        .collect();

    let outcome =
        run_multi_market_depth_backtest_with_strategy_slots(&run, strategies, &research(run_id))
            .expect("same-settlement inverse portfolio passes the app path");

    assert_eq!(outcome.successful_fills, 1);
    let report = qx_app::read_multi_market_depth_research_report_v1(
        &qx_app::MultiMarketDepthResearchReportReadRequestV1 {
            schema_version: qx_app::MULTI_MARKET_DEPTH_RESEARCH_REPORT_SCHEMA_VERSION_V1,
            outcome,
            page: None,
        },
        &research(run_id),
    )
    .expect("reader verifies shared exact inverse derivative margin run");
    assert_eq!(report.summary["markets_total"], 2);
    assert_eq!(report.summary["successful_fills"], 1);
    assert_eq!(
        report.summary.pointer("/metrics_exact/input_precision"),
        Some(&Value::String("Shared-account multi-market DepthFrame v2 supports either spec-bound Spot base-quantity portfolios with exact cash/inventory reservations or homogeneous linear/inverse derivative portfolios with one settlement currency and aggregate exact margin/fee projection; derivative fills recheck actual-price margin, funding cashflows preserve exact quantity and mark values, queue estimation is disabled, and mixed Spot/derivative or cross-currency portfolios fail closed. v1 inputs keep the 1e-9 projection.".into()))
    );
}

#[test]
fn multi_market_depth_requires_exact_derivative_rules_and_explicit_policy() {
    let root = scratch("multi-market-depth-guards");
    let mut invalid = spec(&root, "multi-market-depth-invalid");
    invalid.markets[0].market_spec.product = TradingProduct::Perpetual;
    invalid.markets[0].market_spec.linear = true;
    invalid.markets[0].market_spec.max_leverage = 10;
    invalid.markets[0].order_policy = Some(OrderPolicy::default());
    assert_eq!(
        invalid.validate().unwrap_err().category(),
        AppErrorCategory::InvalidInput
    );

    let exact_rules = || {
        Some(ExactTradingRules {
            contract_size: ExactDecimal::ONE,
            price_tick: ExactDecimal::ONE,
            quantity_step: ExactDecimal::ONE,
            minimum_quantity: ExactDecimal::ONE,
            minimum_notional: ExactDecimal::ZERO,
        })
    };
    let mut mixed_products = spec(&root, "multi-market-depth-mixed-products");
    mixed_products.markets[0].market_spec.product = TradingProduct::Perpetual;
    mixed_products.markets[0].market_spec.linear = true;
    mixed_products.markets[0].market_spec.max_leverage = 10;
    mixed_products.markets[0].market_spec.exact_rules = exact_rules();
    mixed_products.markets[0].order_policy = Some(OrderPolicy::default());
    assert_eq!(
        mixed_products.validate().unwrap_err().category(),
        AppErrorCategory::InvalidInput,
        "Spot and derivative collateral must not be mixed implicitly"
    );

    let mut cross_currency = spec(&root, "multi-market-depth-cross-currency");
    for market in &mut cross_currency.markets {
        market.market_spec.product = TradingProduct::Perpetual;
        market.market_spec.linear = true;
        market.market_spec.max_leverage = 10;
        market.market_spec.exact_rules = exact_rules();
        market.order_policy = Some(OrderPolicy::default());
    }
    cross_currency.markets[1].market_spec.settlement_currency = "BTC".into();
    assert_eq!(
        cross_currency.validate().unwrap_err().category(),
        AppErrorCategory::InvalidInput,
        "Cross-currency collateral must remain fail closed"
    );

    let mut invalid_tier = spec(&root, "multi-market-depth-invalid-tier");
    invalid_tier.markets[0].data_tier = Some("l3-only".into());
    assert_eq!(
        invalid_tier.validate().unwrap_err().category(),
        AppErrorCategory::InvalidInput
    );
    let mut handle = spec(&root, "multi-market-depth-handle").start(research("depth-handle"));
    assert_eq!(handle.wait(), RunStatus::Succeeded);
    handle
        .try_take_result()
        .expect("finished result")
        .expect("run succeeds");
}

#[test]
fn multi_market_depth_experiment_sweeps_strategy_and_parameters_deterministically() {
    let root = scratch("multi-market-depth-experiment");
    let mut base = spec(&root, "unused-base");
    base.initial_cash_raw = 100_000 * SCALE;
    base.schema_version = qx_app::MULTI_MARKET_DEPTH_BACKTEST_EXACT_SCHEMA_VERSION;
    base.initial_cash_exact = Some(ExactDecimal::from_parts(base.initial_cash_raw, 9).unwrap());
    base.markets[0].depth_path = write_oscillating_depth(&root, 0, "BTCUSDT.BINANCE");
    base.markets[0].data_tier = Some("l1".into());
    base.markets[0].strategy.fast_window = 2;
    base.markets[0].strategy.slow_window = 3;
    base.markets[1].depth_path = write_oscillating_depth(&root, 1, "ETHUSDT.BYBIT");
    base.markets[1].strategy.fast_window = 2;
    base.markets[1].strategy.slow_window = 3;
    let experiment = RunMultiMarketDepthExperimentSpec {
        schema_version: 1,
        experiment_id: "multi-market-depth-grid".into(),
        target_strategy_id: "strategy-0".into(),
        base: base.clone(),
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
    assert!(result.candidates.iter().all(|candidate| {
        candidate
            .outcome
            .as_ref()
            .is_some_and(|outcome| outcome.successful_fills > 0)
    }));
    assert!(result.canonical_run_spec_json.is_some());
    assert!(result.canonical_run_spec_fingerprint.is_some());
    assert!(result.input_data_fingerprint.is_some());
    assert!(result.result_hash.is_some());
    assert!(result
        .run_manifest
        .as_ref()
        .is_some_and(|path| std::path::Path::new(path).is_file()));
    assert!(result
        .run_record
        .as_ref()
        .is_some_and(|path| std::path::Path::new(path).is_file()));
    let artifact_locations = result.artifact_locations.as_ref().unwrap();
    let experiment_input_path = artifact_locations
        .get("experiment_input")
        .expect("experiment input spec sidecar locator");
    let experiment_input_bytes =
        std::fs::read(experiment_input_path).expect("experiment input spec sidecar is persisted");
    let run_record = qx_spec::RunRecordV2::from_json(
        &std::fs::read_to_string(result.run_record.as_ref().unwrap()).unwrap(),
    )
    .expect("experiment RunRecord V2 parses");
    let experiment_input_ref = run_record
        .artifact_refs
        .iter()
        .find(|artifact| artifact.name == "experiment_input")
        .expect("RunRecord binds experiment input sidecar");
    assert_eq!(
        experiment_input_ref.artifact_id,
        qx_strategy::sha256_hex(&experiment_input_bytes)
    );
    assert_eq!(
        experiment_input_ref.byte_len,
        experiment_input_bytes.len() as u64
    );
    let manifest = qx_core::RunManifest::from_json(
        &std::fs::read_to_string(result.run_manifest.as_ref().unwrap()).unwrap(),
    )
    .unwrap();
    let parent_identity =
        qx_spec::CanonicalRunSpec::from_json(result.canonical_run_spec_json.as_deref().unwrap())
            .unwrap();
    assert_eq!(parent_identity.run_type, "multi_market_depth_experiment");
    qx_app::artifact_store::verify_canonical_run_spec_for_manifest(
        &manifest,
        &serde_json::to_value(&result).unwrap(),
    )
    .expect("the experiment manifest binds canonical inputs and candidates");
    let base_outcome =
        run_multi_market_depth_backtest(&base, &research("multi-market-depth-builtin-backtest"))
            .expect("built-in strategies consume L1 and L2 snapshots through the shared adapter");
    assert!(base_outcome.successful_fills > 0);
    println!(
        "MULTI_MARKET_DEPTH_BUILTIN_OBSERVED tiers=l1+l2 candidates=4 positive_fill_candidates=4 base_fills={} replay_verified=true",
        base_outcome.successful_fills
    );
    let repeated = run_multi_market_depth_experiment(&experiment, &research("depth-grid"))
        .expect("identical experiment is idempotent");
    assert_eq!(result.to_json().unwrap(), repeated.to_json().unwrap());

    let mut relocated = experiment.clone();
    for (index, market) in relocated.base.markets.iter_mut().enumerate() {
        let copied = root.join(format!("relocated-{index}.depth.json"));
        std::fs::copy(&market.depth_path, &copied).expect("copy same depth frame");
        market.depth_path = copied.to_string_lossy().into_owned();
    }
    let path_independent = run_multi_market_depth_experiment(&relocated, &research("depth-grid"))
        .expect("content-identical frames at new paths reuse the verified experiment");
    assert_eq!(
        result.canonical_run_spec_fingerprint,
        path_independent.canonical_run_spec_fingerprint
    );
    assert_eq!(
        result.to_json().unwrap(),
        path_independent.to_json().unwrap()
    );

    let report_index_path = result.artifact_path.clone();
    let mut legacy_report = result.clone();
    legacy_report.canonical_run_spec_json = None;
    legacy_report.canonical_run_spec_fingerprint = None;
    legacy_report.input_data_fingerprint = None;
    legacy_report.result_hash = None;
    legacy_report.run_manifest = None;
    legacy_report.run_record = None;
    legacy_report.artifact_locations = None;
    std::fs::write(&report_index_path, legacy_report.to_json().unwrap())
        .expect("write legacy report shape");
    let upgraded = run_multi_market_depth_experiment(&experiment, &research("depth-grid"))
        .expect("verified legacy report upgrades");
    assert!(upgraded.run_record.is_some());
    let upgraded_summary = serde_json::to_value(&upgraded).unwrap();
    qx_app::artifact_store::verify_declared_run_manifest(&upgraded_summary)
        .expect("upgraded report has verified RunManifest and RunRecord");

    let upgraded_input_path = upgraded
        .artifact_locations
        .as_ref()
        .unwrap()
        .get("experiment_input")
        .expect("upgraded input sidecar locator");
    let upgraded_input_bytes =
        std::fs::read(upgraded_input_path).expect("read upgraded input sidecar");
    std::fs::write(upgraded_input_path, b"tampered")
        .expect("tamper immutable experiment input sidecar");
    assert!(qx_app::artifact_store::verify_declared_run_manifest(&upgraded_summary).is_err());
    std::fs::write(upgraded_input_path, upgraded_input_bytes)
        .expect("restore immutable experiment input sidecar");

    let immutable_report = upgraded
        .artifact_locations
        .as_ref()
        .unwrap()
        .get("experiment_report")
        .expect("immutable experiment report locator");
    std::fs::write(immutable_report, b"tampered").expect("tamper immutable report");
    assert!(qx_app::artifact_store::verify_declared_run_manifest(&upgraded_summary).is_err());
    assert_eq!(
        run_multi_market_depth_experiment(&experiment, &research("depth-grid"))
            .unwrap_err()
            .category(),
        AppErrorCategory::StorageFailure
    );
}

#[test]
fn multi_market_depth_experiment_isolates_callback_panics_per_candidate() {
    let root = scratch("multi-market-depth-experiment-callback-panic");
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
