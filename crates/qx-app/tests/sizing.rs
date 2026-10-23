mod common;

use common::{backtest_spec, scratch};
use qx_app::{
    run_experiment, AppErrorCategory, CallerCapability, ExperimentParameterSpace, RunContext,
    RunExperimentSpec,
};
fn research(id: &str) -> RunContext {
    RunContext::new(CallerCapability::Research, id)
}

#[test]
fn quantity_grid_rejects_sizing_modes_that_make_the_dimension_ineffective() {
    const SCALE: i128 = 1_000_000_000;
    let root = scratch("quantity-grid-sizing-conflict");
    let bars_path = root.join("bars.json");
    std::fs::write(&bars_path, "{}").expect("write placeholder bars path");
    for (field, value) in [
        ("target_notional_raw", 10 * SCALE),
        ("target_available_margin_bps", 5_000),
        ("target_volatility_bps", 500),
        ("target_kelly_fraction_bps", 5_000),
    ] {
        let mut base = backtest_spec(
            "quantity-grid-sizing-conflict",
            &bars_path.to_string_lossy(),
            &root.join("out"),
        );
        match field {
            "target_notional_raw" => base.strategy.target_notional_raw = Some(value),
            "target_available_margin_bps" => {
                base.strategy.target_available_margin_bps = Some(value as u32)
            }
            "target_volatility_bps" => base.strategy.target_volatility_bps = Some(value as u32),
            "target_kelly_fraction_bps" => {
                base.strategy.target_kelly_fraction_bps = Some(value as u32)
            }
            _ => unreachable!(),
        }
        let spec = RunExperimentSpec {
            schema_version: 1,
            experiment_id: format!("quantity-grid-{field}"),
            base,
            parameter_space: vec![ExperimentParameterSpace {
                name: "quantity_raw".into(),
                values: vec![SCALE, 2 * SCALE],
            }],
            strategy_kinds: Vec::new(),
            split_plan: None,
            baselines: Vec::new(),
            search: None,
            cost_sensitivity: Vec::new(),
        };
        let error = spec.validate().expect_err("quantity is shadowed by sizing");
        assert_eq!(error.category(), AppErrorCategory::InvalidInput);
        assert!(error
            .to_string()
            .contains("quantity_raw 参数维度与策略 sizing 模式冲突"));
    }
}

#[test]
fn parameter_experiment_sweeps_kelly_fraction_with_versioned_sizing_identity() {
    const SCALE: i128 = 1_000_000_000;
    let root = scratch("kelly-grid");
    let bars_path = root.join("mixed-returns.json");
    let mut close = 100 * SCALE;
    let closes = (0..70)
        .map(|index| {
            if index > 0 {
                close = if index % 10 == 0 {
                    close - close / 1_000
                } else {
                    close + close / 100
                };
            }
            close
        })
        .collect::<Vec<_>>();
    let ts = (0..70)
        .map(|index| 1_000 + index * 1_000)
        .collect::<Vec<_>>();
    let frame = serde_json::json!({
        "schema_version": 1,
        "instrument": "BTCUSDT.BINANCE",
        "source": "kelly-grid-test-v1",
        "ts": ts,
        "open_raw": closes,
        "high_raw": closes,
        "low_raw": closes,
        "close_raw": closes,
        "volume_raw": vec![SCALE; 70],
    });
    std::fs::write(&bars_path, frame.to_string()).expect("write Kelly grid bars");

    let mut base = backtest_spec(
        "kelly-grid",
        &bars_path.to_string_lossy(),
        &root.join("out"),
    );
    base.strategy.kind = qx_strategy::BuiltinStrategyKind::Momentum;
    base.strategy.period = 2;
    base.strategy.threshold_bps = 0;
    base.strategy.kelly_lookback_bars = Some(30);
    let spec = RunExperimentSpec {
        schema_version: 1,
        experiment_id: "kelly-fractions".into(),
        base,
        parameter_space: vec![ExperimentParameterSpace {
            name: "target_kelly_fraction_bps".into(),
            values: vec![2_500, 5_000],
        }],
        strategy_kinds: Vec::new(),
        split_plan: None,
        baselines: Vec::new(),
        search: None,
        cost_sensitivity: Vec::new(),
    };
    let result =
        run_experiment(&spec, &research("kelly-fractions")).expect("Kelly fraction experiment");
    assert_eq!(result.succeeded_candidates, 2);
    let outcomes = result
        .candidates
        .iter()
        .map(|candidate| candidate.outcome.as_ref().expect("candidate outcome"))
        .collect::<Vec<_>>();
    for outcome in &outcomes {
        let summary: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(&outcome.artifacts.summary).expect("read Kelly summary"),
        )
        .expect("parse Kelly summary");
        assert_eq!(summary["strategy_version"], "builtin-momentum-kelly-v1");
    }
    assert_ne!(outcomes[0].result_hash, outcomes[1].result_hash);
    assert!(outcomes.iter().all(|outcome| outcome.fills > 0));
}
