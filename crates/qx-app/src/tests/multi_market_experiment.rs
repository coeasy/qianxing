use crate::{
    run_multi_market_experiment, AppErrorCategory, BuiltinStrategySpec, CallerCapability,
    ExperimentParameterSpace, MultiMarketBacktestMarket, MultiMarketBacktestSpec,
    PortfolioStrategySpec, RunContext, RunMultiMarketExperimentSpec, RunStatus,
};
use qx_strategy::BuiltinStrategyKind;

const SCALE: i128 = 1_000_000_000;

fn research(id: &str) -> RunContext {
    RunContext::new(CallerCapability::Research, id)
}

fn experiment(root: &std::path::Path) -> RunMultiMarketExperimentSpec {
    let instruments = ["BTCUSDT.BINANCE", "ETHUSDT.KRAKEN", "SOLUSDT.BYBIT"];
    let markets = instruments
        .iter()
        .enumerate()
        .map(|(index, instrument)| {
            let bars_path =
                super::fixtures::write_frame(root, &format!("market-{index}.json"), instrument, 80);
            let mut strategy =
                BuiltinStrategySpec::new(BuiltinStrategyKind::SmaCross, format!("slot-{index}"));
            strategy.fast_window = 2;
            strategy.slow_window = 4;
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
    RunMultiMarketExperimentSpec {
        schema_version: 1,
        experiment_id: "multi-market-grid".into(),
        target_strategy_id: "slot-0".into(),
        base: MultiMarketBacktestSpec {
            schema_version: 1,
            run_id: "unused-grid-base".into(),
            account_id: "grid-account".into(),
            settlement_currency: "USDT".into(),
            initial_cash_raw: 100_000 * SCALE,
            seed: 7,
            output_dir: root.join("output").to_string_lossy().into_owned(),
            markets,
        },
        parameter_space: vec![ExperimentParameterSpace {
            name: "fast_window".into(),
            values: vec![2, 3],
        }],
        strategy_kinds: vec![BuiltinStrategyKind::SmaCross, BuiltinStrategyKind::EmaCross],
    }
}

#[test]
fn multi_market_parameter_grid_runs_shared_engine_ranks_and_reuses_identity() {
    let root = super::fixtures::scratch("multi-market-experiment");
    let spec = experiment(&root);
    assert_eq!(spec.validate().unwrap(), 4);
    let outcome = run_multi_market_experiment(&spec, &research("multi-market-grid"))
        .expect("grid uses shared multi-market backtest");
    assert_eq!(outcome.total_candidates, 4);
    assert_eq!(outcome.completed_candidates, 4);
    assert_eq!(outcome.failed_candidates, 0);
    assert_eq!(outcome.succeeded_candidates, 4);
    assert_eq!(outcome.ranking.len(), 4);
    assert!(outcome.candidates.iter().all(|candidate| candidate
        .outcome
        .as_ref()
        .is_some_and(|run| run.markets_total == 3)));
    assert_eq!(
        outcome
            .candidates
            .iter()
            .map(|candidate| candidate.ordinal)
            .collect::<Vec<_>>(),
        [1, 2, 3, 4]
    );
    assert!(outcome
        .ranking
        .windows(2)
        .all(|pair| pair[0].return_bps > pair[1].return_bps
            || (pair[0].return_bps == pair[1].return_bps
                && (pair[0].max_drawdown_bps < pair[1].max_drawdown_bps
                    || (pair[0].max_drawdown_bps == pair[1].max_drawdown_bps
                        && pair[0].ordinal < pair[1].ordinal)))));
    let repeated = run_multi_market_experiment(&spec, &research("multi-market-grid"))
        .expect("same experiment identity returns prior report");
    assert_eq!(repeated.spec_fingerprint, outcome.spec_fingerprint);
    assert_eq!(repeated.ranking, outcome.ranking);
    assert_eq!(repeated.artifact_path, outcome.artifact_path);

    let mut asynchronous_spec = spec.clone();
    asynchronous_spec.experiment_id = "multi-market-grid-async".into();
    let mut handle = asynchronous_spec.start(research("multi-market-grid-async"));
    assert_eq!(handle.wait(), RunStatus::Succeeded);
    let asynchronous = handle
        .try_take_result()
        .unwrap()
        .expect("async grid succeeds");
    assert_eq!(asynchronous.total_candidates, 4);
    assert_eq!(asynchronous.failed_candidates, 0);
}

#[test]
fn multi_market_experiment_rejects_ambiguous_target_and_caps_cartesian_candidates() {
    let root = super::fixtures::scratch("multi-market-experiment-validation");
    let mut invalid = experiment(&root);
    invalid.target_strategy_id = "missing-slot".into();
    assert_eq!(
        invalid.validate().unwrap_err().category(),
        AppErrorCategory::InvalidInput
    );

    let mut too_many = experiment(&root);
    too_many.strategy_kinds = vec![BuiltinStrategyKind::SmaCross];
    too_many.parameter_space = vec![ExperimentParameterSpace {
        name: "fast_window".into(),
        values: (1..=257).collect(),
    }];
    assert_eq!(
        too_many.validate().unwrap_err().category(),
        AppErrorCategory::InvalidInput
    );
}

#[test]
fn multi_market_experiment_parameter_maps_have_deterministic_order() {
    let root = super::fixtures::scratch("multi-market-experiment-grid-order");
    let mut spec = experiment(&root);
    spec.strategy_kinds.reverse();
    let mut dimensions = spec.parameter_space.clone();
    dimensions.reverse();
    spec.parameter_space = dimensions;
    let json = spec.to_json().unwrap();
    let reparsed = RunMultiMarketExperimentSpec::from_json(&json).unwrap();
    assert_eq!(json, reparsed.to_json().unwrap());
    assert_eq!(
        reparsed
            .strategy_kinds
            .iter()
            .map(|kind| kind.name())
            .collect::<Vec<_>>(),
        ["ema_cross", "sma_cross"]
    );
}
