use crate::{
    run_multi_market_depth_backtest_with_strategy_slots, AppErrorCategory, BuiltinStrategySpec,
    CallerCapability, MultiMarketDepthBacktestMarket, MultiMarketDepthBacktestSpec, RunContext,
    StrategyArtifactIdentity,
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
    let spec = spec(&root, "multi-market-depth-e2e");
    let strategies = vec![
        (
            identity("strategy-0"),
            Box::new(BuyOnce(false)) as Box<dyn Strategy>,
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

    let artifact: Value =
        serde_json::from_slice(&std::fs::read(&outcome.artifact_path).expect("artifact exists"))
            .expect("artifact JSON");
    assert_eq!(artifact["replay_verified"], true);
    assert_eq!(artifact["market_data"].as_array().unwrap().len(), 2);
    assert_eq!(artifact["strategy_identities"].as_array().unwrap().len(), 2);
    assert_eq!(artifact["per_market_fills"]["ETHUSDT.BYBIT"], 1);
    assert_eq!(artifact["per_market_fills"]["BTCUSDT.BINANCE"], 0);

    let repeated = run_multi_market_depth_backtest_with_strategy_slots(
        &spec,
        vec![
            (identity("strategy-0"), Box::new(BuyOnce(false))),
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
    let mut handle = spec(&root, "multi-market-depth-handle").start(research("depth-handle"));
    assert_eq!(handle.wait(), crate::RunStatus::Succeeded);
    handle
        .try_take_result()
        .expect("finished result")
        .expect("run succeeds");
}
