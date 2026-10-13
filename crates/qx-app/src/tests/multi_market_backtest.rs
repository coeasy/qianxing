use crate::{
    run_multi_market_backtest, AppErrorCategory, BuiltinStrategySpec, CallerCapability,
    MultiMarketBacktestMarket, MultiMarketBacktestSpec, PortfolioStrategySpec, RunContext,
};
use qx_strategy::BuiltinStrategyKind;
use serde_json::Value;

const SCALE: i128 = 1_000_000_000;

fn spec(root: &std::path::Path, run_id: &str) -> MultiMarketBacktestSpec {
    let instruments = ["BTCUSDT.BINANCE", "ETHUSDT.KRAKEN", "SOLUSDT.BYBIT"];
    let markets = instruments
        .iter()
        .enumerate()
        .map(|(index, instrument)| {
            let bars_path =
                super::fixtures::write_frame(root, &format!("market-{index}.json"), instrument, 80);
            let mut strategy =
                BuiltinStrategySpec::new(BuiltinStrategyKind::SmaCross, format!("sma-{index}"));
            strategy.fast_window = 2;
            strategy.slow_window = 3;
            MultiMarketBacktestMarket {
                instrument: (*instrument).into(),
                bars_path,
                priority: index as u32,
                market_spec: None,
                fee_schedule: None,
                strategies: vec![PortfolioStrategySpec {
                    priority: 0,
                    reserved_capital_raw: 20_000 * SCALE,
                    strategy,
                }],
            }
        })
        .collect();
    MultiMarketBacktestSpec {
        schema_version: 1,
        run_id: run_id.into(),
        account_id: "shared-account".into(),
        settlement_currency: "USDT".into(),
        initial_cash_raw: 100_000 * SCALE,
        seed: 7,
        output_dir: root.join("out").to_string_lossy().into_owned(),
        markets,
    }
}

fn research(id: &str) -> RunContext {
    RunContext::new(CallerCapability::Research, id)
}

#[test]
fn three_market_run_shares_ledger_replays_and_is_idempotent() {
    let root = super::fixtures::scratch("multi-market-e2e");
    let spec = spec(&root, "multi-market-e2e");
    let outcome = run_multi_market_backtest(&spec, &research("multi-market-e2e"))
        .expect("three-market shared-account run");
    assert_eq!(outcome.markets_total, 3);
    assert_eq!(outcome.strategies_total, 3);
    assert!(outcome.successful_fills > 0);
    assert_eq!(outcome.equity_points, 240);
    println!(
        "MULTI_MARKET_OBSERVED markets={} strategies={} fills={} equity_points={} return_bps={} replay_verified=true",
        outcome.markets_total,
        outcome.strategies_total,
        outcome.successful_fills,
        outcome.equity_points,
        outcome.return_bps
    );

    let artifact: Value = serde_json::from_str(
        &std::fs::read_to_string(&outcome.artifact_path).expect("artifact exists"),
    )
    .expect("artifact JSON");
    assert_eq!(artifact["replay_verified"], true);
    assert_eq!(artifact["market_data"].as_array().unwrap().len(), 3);
    assert_eq!(artifact["equity"].as_array().unwrap().len(), 240);
    let fills = artifact["per_market_fills"].as_object().unwrap();
    assert_eq!(fills.len(), 3);

    let repeated = run_multi_market_backtest(&spec, &research("multi-market-e2e"))
        .expect("same run is idempotent");
    assert_eq!(outcome.shared_result_hash, repeated.shared_result_hash);
}

#[test]
fn multi_market_validation_rejects_budget_overcommit_and_cancel_handle_completes() {
    let root = super::fixtures::scratch("multi-market-guards");
    let mut invalid = spec(&root, "multi-market-invalid");
    invalid.markets[0].strategies[0].reserved_capital_raw = invalid.initial_cash_raw;
    assert_eq!(
        invalid.validate().unwrap_err().category(),
        AppErrorCategory::InvalidInput
    );

    let mut handle = spec(&root, "multi-market-handle").start(research("multi-market-handle"));
    assert_eq!(handle.wait(), crate::RunStatus::Succeeded);
    let outcome = handle
        .try_take_result()
        .expect("finished result")
        .expect("run handle succeeds");
    assert_eq!(outcome.markets_total, 3);
}
