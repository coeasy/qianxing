use crate::cases::{run_experiment, ExperimentParameterSpace, RunExperimentSpec};
use crate::context::{CallerCapability, RunContext};
use crate::tests::fixtures::{backtest_spec, scratch};
fn research(id: &str) -> RunContext {
    RunContext::new(CallerCapability::Research, id)
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
