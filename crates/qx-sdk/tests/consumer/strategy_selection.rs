use super::*;

#[test]
fn the_rust_sdk_facade_quick_selects_multiple_single_leg_strategy_kinds() {
    let output_dir = scratch("strategy-selection");
    let client = Client::new(CallerCapability::Research, "sdk-strategy-selection");
    let kinds = vec![
        BuiltinStrategyKind::SmaCross,
        BuiltinStrategyKind::EmaCross,
        BuiltinStrategyKind::Macd,
        BuiltinStrategyKind::Rsi,
        BuiltinStrategyKind::Bollinger,
        BuiltinStrategyKind::DonchianBreakout,
        BuiltinStrategyKind::Momentum,
        BuiltinStrategyKind::MeanReversion,
        BuiltinStrategyKind::Grid,
        BuiltinStrategyKind::AtrTrend,
    ];
    let result = client
        .run_experiment(&RunExperimentSpec {
            schema_version: 1,
            experiment_id: "sdk-classic-selection".into(),
            base: spec("unused-selection-base", &output_dir),
            parameter_space: Vec::new(),
            strategy_kinds: kinds,
            split_plan: None,
            baselines: Vec::new(),
        })
        .expect("Rust SDK should route strategy selection through qx-app");
    assert_eq!(result.total_candidates, 10);
    assert_eq!(result.succeeded_candidates, 10);
    assert_eq!(result.comparison.expect("shared ranking").runs.len(), 10);
    assert_eq!(
        result
            .candidates
            .iter()
            .map(|candidate| candidate.strategy_kind.as_deref().unwrap())
            .collect::<Vec<_>>(),
        [
            "atr_trend",
            "bollinger",
            "donchian_breakout",
            "ema_cross",
            "grid",
            "macd",
            "mean_reversion",
            "momentum",
            "rsi",
            "sma_cross",
        ]
    );
}
