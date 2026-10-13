use super::*;

#[test]
fn rust_sdk_composes_multiple_external_strategies_in_one_shared_portfolio() {
    let output_dir = scratch("sdk-callback-portfolio");
    let mut backtest = spec("sdk-callback-portfolio", &output_dir);
    backtest.strategy = BuiltinStrategySpec::new(BuiltinStrategyKind::SmaCross, "custom-primary");
    backtest.portfolio = Some(PortfolioBacktestConfig {
        primary_priority: 0,
        primary_reserved_capital_raw: backtest.initial_cash_raw / 2,
        additional_strategies: vec![PortfolioStrategySpec {
            priority: 1,
            reserved_capital_raw: backtest.initial_cash_raw / 2,
            strategy: BuiltinStrategySpec::new(BuiltinStrategyKind::Momentum, "custom-secondary"),
        }],
    });
    let client = Client::new(CallerCapability::Research, "sdk-callback-portfolio");
    let result = client
        .run_backtest_with_strategy_slots(
            &backtest,
            vec![
                (
                    StrategyArtifactIdentity {
                        strategy_id: "custom-primary".into(),
                        strategy_version: "primary-v1".into(),
                        source_sha256: "b".repeat(64),
                    },
                    Box::new(BuyEachInstrument(Default::default())) as Box<dyn Strategy>,
                ),
                (
                    StrategyArtifactIdentity {
                        strategy_id: "custom-secondary".into(),
                        strategy_version: "secondary-v1".into(),
                        source_sha256: "c".repeat(64),
                    },
                    Box::new(BuyEachInstrument(Default::default())) as Box<dyn Strategy>,
                ),
            ],
        )
        .expect("Rust facade routes custom slots through the shared account engine");
    assert!(result.fills >= 2);
    let fills = std::fs::read_to_string(&result.artifacts.fills).expect("attributed fills");
    assert!(fills.contains("custom-primary"));
    assert!(fills.contains("custom-secondary"));
    assert!(
        client
            .verify_run(&result)
            .expect("shared replay verifier")
            .verified
    );
}

#[test]
fn rust_sdk_runs_custom_multi_market_slots_through_shared_engine_and_run_handle() {
    let root = scratch("sdk-multi-market-custom");
    let source: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(sample_bars()).expect("sample BarFrame"))
            .expect("sample JSON");
    let instruments = ["BTCUSDT.BINANCE", "ETHUSDT.KRAKEN", "SOLUSDT.BYBIT"];
    let markets: Vec<MultiMarketBacktestMarket> = instruments
        .iter()
        .enumerate()
        .map(|(index, instrument)| {
            let path = root.join(format!("market-{index}.json"));
            let mut frame = source.clone();
            frame["instrument"] = serde_json::Value::String((*instrument).into());
            std::fs::write(&path, serde_json::to_vec(&frame).unwrap()).unwrap();
            MultiMarketBacktestMarket {
                instrument: (*instrument).into(),
                bars_path: path.to_string_lossy().into_owned(),
                priority: index as u32,
                market_spec: None,
                fee_schedule: None,
                strategies: vec![PortfolioStrategySpec {
                    priority: 0,
                    reserved_capital_raw: 20_000 * 1_000_000_000,
                    strategy: BuiltinStrategySpec::new(
                        BuiltinStrategyKind::SmaCross,
                        format!("sdk-market-{index}"),
                    ),
                }],
            }
        })
        .collect();
    let build_spec = |run_id: &str| MultiMarketBacktestSpec {
        schema_version: 1,
        run_id: run_id.into(),
        account_id: "sdk-shared-account".into(),
        settlement_currency: "USDT".into(),
        initial_cash_raw: 100_000 * 1_000_000_000,
        seed: 7,
        output_dir: root.join(run_id).to_string_lossy().into_owned(),
        markets: markets.clone(),
    };
    let custom = || {
        vec![(
            StrategyArtifactIdentity {
                strategy_id: "sdk-market-0".into(),
                strategy_version: "consumer-v1".into(),
                source_sha256: "d".repeat(64),
            },
            Box::new(BuyEachInstrument(Default::default())) as Box<dyn Strategy>,
        )]
    };
    let client = Client::new(CallerCapability::Research, "sdk-multi-market-custom");
    let sync = client
        .run_multi_market_backtest_with_strategy_slots(&build_spec("sdk-market-sync"), custom())
        .expect("sync multi-market callback run");
    assert_eq!(sync.markets_total, 3);
    assert!(sync.successful_fills > 0);
    let sync_artifact: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&sync.artifact_path).unwrap()).unwrap();
    assert_eq!(
        sync_artifact["strategy_identities"][0]["strategy_version"],
        "consumer-v1"
    );
    assert_eq!(sync_artifact["replay_verified"], true);
    println!(
        "MULTI_MARKET_RUST_CUSTOM_OBSERVED markets={} fills={} callback_version=consumer-v1 replay_verified=true",
        sync.markets_total, sync.successful_fills
    );

    let mut handle = client
        .start_multi_market_backtest_with_strategy_slots(build_spec("sdk-market-async"), custom())
        .expect("async multi-market callback handle");
    assert_eq!(handle.wait(), RunStatus::Succeeded);
    assert_eq!(handle.try_take_result().unwrap().unwrap().markets_total, 3);
}
