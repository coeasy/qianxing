//! Stable JSON contract for same-account strategy baskets.

mod common;

use common::{backtest_spec, scratch};
use qx_app::{
    BacktestSpec, BuiltinStrategySpec, PortfolioBacktestConfig, PortfolioStrategySpec,
    ProtectiveExitSpec,
};
use qx_strategy::BuiltinStrategyKind;
use serde_json::Value;
use std::collections::BTreeSet;

fn keys(value: &Value) -> BTreeSet<String> {
    value
        .as_object()
        .expect("JSON object")
        .keys()
        .cloned()
        .collect()
}

#[test]
fn portfolio_rejects_account_level_protective_exit_without_strategy_attribution() {
    let root = scratch("spec-portfolio-protective-exit");
    let bars = root.join("bars.json").to_string_lossy().into_owned();
    let mut spec = backtest_spec("portfolio-protective-exit", &bars, &root);
    let mut secondary = BuiltinStrategySpec::new(BuiltinStrategyKind::Momentum, "momentum-2");
    secondary.period = 4;
    spec.portfolio = Some(PortfolioBacktestConfig {
        primary_priority: 0,
        primary_reserved_capital_raw: 40_000_000_000_000,
        additional_strategies: vec![PortfolioStrategySpec {
            priority: 1,
            reserved_capital_raw: 40_000_000_000_000,
            strategy: secondary,
        }],
    });
    spec.protective_exit = Some(ProtectiveExitSpec {
        stop_loss_bps: Some(500),
        take_profit_bps: None,
        trailing_stop_bps: None,
    });

    let error = spec
        .validate()
        .expect_err("净账户 protective exit 不能静默破坏各策略的虚拟持仓");
    assert!(error.message().contains("无法归属到策略持仓"));
}

fn expected(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|name| (*name).into()).collect()
}

#[test]
fn portfolio_spec_round_trips_with_a_pinned_nested_schema() {
    let root = scratch("spec-portfolio");
    let bars = root.join("bars.json").to_string_lossy().into_owned();
    let mut spec = backtest_spec("run-portfolio-schema", &bars, &root);
    let mut secondary = BuiltinStrategySpec::new(BuiltinStrategyKind::Momentum, "momentum-2");
    secondary.period = 4;
    spec.portfolio = Some(PortfolioBacktestConfig {
        primary_priority: 1,
        primary_reserved_capital_raw: 40_000_000_000_000,
        additional_strategies: vec![PortfolioStrategySpec {
            priority: 2,
            reserved_capital_raw: 50_000_000_000_000,
            strategy: secondary,
        }],
    });

    let payload = spec.to_json().expect("serialize portfolio spec");
    assert_eq!(BacktestSpec::from_json(&payload).unwrap(), spec);
    let value: Value = serde_json::from_str(&payload).unwrap();
    assert_eq!(
        keys(&value),
        expected(&[
            "schema_version",
            "run_id",
            "instrument",
            "bars_path",
            "settlement_currency",
            "initial_cash_raw",
            "seed",
            "output_dir",
            "strategy",
            "portfolio",
        ])
    );
    let portfolio = &value["portfolio"];
    assert_eq!(
        keys(portfolio),
        expected(&[
            "primary_priority",
            "primary_reserved_capital_raw",
            "additional_strategies",
        ])
    );
    assert_eq!(
        keys(&portfolio["additional_strategies"][0]),
        expected(&["priority", "reserved_capital_raw", "strategy"])
    );
}
