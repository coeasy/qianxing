use super::*;

const CLASSIC_KINDS: [qx_strategy::BuiltinStrategyKind; 10] = [
    qx_strategy::BuiltinStrategyKind::SmaCross,
    qx_strategy::BuiltinStrategyKind::EmaCross,
    qx_strategy::BuiltinStrategyKind::Macd,
    qx_strategy::BuiltinStrategyKind::Rsi,
    qx_strategy::BuiltinStrategyKind::Bollinger,
    qx_strategy::BuiltinStrategyKind::DonchianBreakout,
    qx_strategy::BuiltinStrategyKind::Momentum,
    qx_strategy::BuiltinStrategyKind::MeanReversion,
    qx_strategy::BuiltinStrategyKind::Grid,
    qx_strategy::BuiltinStrategyKind::AtrTrend,
];

/// The ten quick-pick strategies must traverse the same Rust app use case from
/// CLI, HTTP, and the Python native facade, with stable results per spec.
#[test]
fn ten_classic_strategies_backtest_identically_across_entrypoints() {
    let root = scratch("classic-strategies");
    let html = std::fs::read_to_string(repo_root().join("web/console/index.html"))
        .expect("read console strategy picker");
    let picker = html
        .split_once("<select id=\"research-strategy\">")
        .and_then(|(_, rest)| rest.split_once("</select>"))
        .map(|(picker, _)| picker)
        .expect("console strategy picker exists");
    let picker_kinds: Vec<&str> = picker
        .split("<option value=\"")
        .skip(1)
        .filter_map(|option| option.split_once('"').map(|(kind, _)| kind))
        .collect();
    let classic_kinds: Vec<&str> = CLASSIC_KINDS.iter().map(|kind| kind.name()).collect();
    assert_eq!(
        picker_kinds, classic_kinds,
        "picker must mirror the Rust catalog"
    );

    for strategy in CLASSIC_KINDS {
        assert!(qx_strategy::BuiltinStrategyKind::ALL.contains(&strategy));
        let kind = strategy.name();
        let run_id = format!("g1-classic-{kind}");
        let mut request: Value = serde_json::from_str(&backtest_spec_json(
            &run_id,
            &sample_bars(),
            &root.join(kind),
        ))
        .expect("backtest spec json");
        request["strategy"]["kind"] = json!(kind);
        request["strategy"]["strategy_id"] = json!(format!("classic-{kind}"));
        let spec = request.to_string();

        let cli = via_cli(&spec, &root);
        let (status, http) = via_http("/app/backtest", &spec);
        assert_eq!(status, 200, "HTTP {kind} backtest: {http}");
        assert_eq!(cli["result_hash"], http["result_hash"], "{kind} CLI/HTTP");
        assert_eq!(cli["fills"], http["fills"], "{kind} CLI/HTTP fills");
        assert_eq!(cli["equity_points"], 70, "{kind} completed the backtest");

        match python_native::via_python(&repo_root(), "run_backtest", &spec) {
            None => eprintln!("[skip] Python {kind}: PyO3 extension not built"),
            Some(Ok(python)) => {
                assert_eq!(python["result_hash"], cli["result_hash"], "{kind} Python");
                assert_eq!(python["fills"], cli["fills"], "{kind} Python fills");
            }
            Some(Err(payload)) => panic!("Python {kind} backtest failed: {payload}"),
        }
    }
}
