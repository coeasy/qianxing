//! End-to-end contract for deterministic same-account strategy baskets.

mod common;

use common::{backtest_spec, scratch, write_frame};
use qx_app::{
    run_backtest, verify_run, AppErrorCategory, CallerCapability, PortfolioBacktestConfig,
    PortfolioStrategySpec, RunContext,
};
const SCALE: i128 = 1_000_000_000;

fn research(id: &str) -> RunContext {
    RunContext::new(CallerCapability::Research, id)
}

#[test]
fn basket_runs_through_shared_engine_and_replay_verification() {
    let root = scratch("portfolio-e2e");
    let bars = write_frame(&root, "bars.json", "BTCUSDT.BINANCE", 80);
    let mut spec = backtest_spec("portfolio-e2e", &bars, &root.join("out"));
    let mut second = spec.strategy.clone();
    second.strategy_id = "sma-secondary".into();
    second.fast_window = 3;
    second.slow_window = 5;
    spec.portfolio = Some(PortfolioBacktestConfig {
        primary_priority: 0,
        primary_reserved_capital_raw: 50_000 * SCALE,
        additional_strategies: vec![PortfolioStrategySpec {
            priority: 1,
            reserved_capital_raw: 50_000 * SCALE,
            strategy: second,
        }],
    });

    let first = run_backtest(&spec, &research("portfolio-e2e")).expect("组合回测");
    let verified = verify_run(&first, &research("portfolio-e2e")).expect("组合结果复核");
    assert!(first.fills > 0, "组合要通过共享撮合链路产生真实成交");
    assert!(verified.verified, "组合结果必须由统一回放核验");
    assert_eq!(verified.result_hash, first.result_hash);

    let repeated = run_backtest(&spec, &research("portfolio-e2e")).expect("幂等重跑");
    assert_eq!(first.result_hash, repeated.result_hash);
    assert_eq!(first.fills, repeated.fills);
}

#[test]
fn basket_validation_rejects_duplicate_ids_and_oversubscribed_budget() {
    let root = scratch("portfolio-invalid");
    let mut spec = backtest_spec("portfolio-invalid", "bars.json", &root);
    let mut duplicate = spec.strategy.clone();
    duplicate.strategy_id = spec.strategy.strategy_id.clone();
    spec.portfolio = Some(PortfolioBacktestConfig {
        primary_priority: 0,
        primary_reserved_capital_raw: 60_000 * SCALE,
        additional_strategies: vec![PortfolioStrategySpec {
            priority: 1,
            reserved_capital_raw: 40_001 * SCALE,
            strategy: duplicate.clone(),
        }],
    });
    assert_eq!(
        spec.validate().unwrap_err().category(),
        AppErrorCategory::InvalidInput
    );

    duplicate.strategy_id = "secondary".into();
    spec.portfolio.as_mut().unwrap().additional_strategies[0].strategy = duplicate;
    assert_eq!(
        spec.validate().unwrap_err().category(),
        AppErrorCategory::InvalidInput
    );
}
