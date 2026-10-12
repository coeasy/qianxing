use super::*;

#[test]
fn same_account_strategy_basket_is_shared_by_cli_http_and_python() {
    let root = scratch("portfolio-backtest");
    let output_dir = root.join("out");
    let mut request: Value = serde_json::from_str(&backtest_spec_json(
        "g1-portfolio-run",
        &sample_bars(),
        &output_dir,
    ))
    .expect("backtest spec json");
    request["portfolio"] = json!({
        "primary_priority": 0,
        "primary_reserved_capital_raw": 50_000_000_000_000_i128,
        "additional_strategies": [{
            "priority": 1,
            "reserved_capital_raw": 50_000_000_000_000_i128,
            "strategy": {
                "kind": "momentum",
                "strategy_id": "g1-momentum",
                "quantity_raw": 1_000_000_000_i128,
                "fast_window": 5,
                "slow_window": 20,
                "period": 4,
                "threshold_bps": 0,
            },
        }],
    });
    let spec = request.to_string();

    let cli = via_cli(&spec, &root);
    let (status, http) = via_http("/app/backtest", &spec);
    assert_eq!(status, 200, "HTTP portfolio backtest: {http}");
    assert!(cli["fills"].as_u64().unwrap_or(0) > 0);
    assert_eq!(cli["result_hash"], http["result_hash"]);
    assert_eq!(cli["fills"], http["fills"]);
    let summary_path = cli["artifacts"]["summary"].as_str().unwrap();
    let summary: Value = serde_json::from_slice(&std::fs::read(summary_path).unwrap()).unwrap();
    assert_eq!(summary["strategy_id"], "portfolio");

    match python_native::via_python(&repo_root(), "run_backtest", &spec) {
        None => eprintln!("[skip] Python portfolio leg: PyO3 extension not built"),
        Some(Ok(python)) => {
            assert_eq!(python["result_hash"], cli["result_hash"]);
            assert_eq!(python["fills"], cli["fills"]);
        }
        Some(Err(payload)) => panic!("Python portfolio backtest failed: {payload}"),
    }
}
