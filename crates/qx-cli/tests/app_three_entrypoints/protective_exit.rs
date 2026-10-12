use super::*;

#[test]
fn protective_exit_backtest_is_identical_across_cli_http_and_python_native() {
    let root = scratch("protective-exit");
    let output_dir = root.join("out");
    let bars_path = root.join("protective-bars.json");
    let closes = [
        100_000_i128,
        99_000,
        98_000,
        97_000,
        96_000,
        150_000,
        150_000,
        150_000,
    ];
    let bars = json!({
        "schema_version": 1,
        "instrument": "BTCUSDT.BINANCE",
        "source": "protective-exit-contract-test",
        "ts": (1..=closes.len()).map(|i| i as u64 * 1_000).collect::<Vec<_>>(),
        "open_raw": closes,
        "high_raw": closes.iter().map(|close| close + 100).collect::<Vec<_>>(),
        "low_raw": closes.iter().map(|close| close - 100).collect::<Vec<_>>(),
        "close_raw": closes,
        "volume_raw": vec![1_000_000_000; closes.len()],
    });
    std::fs::write(&bars_path, bars.to_string()).expect("write deterministic protective bars");
    let mut request: Value = serde_json::from_str(&backtest_spec_json(
        "g1-protective-exit",
        &bars_path.to_string_lossy(),
        &output_dir,
    ))
    .expect("backtest spec");
    request["strategy"]["kind"] = json!("momentum");
    request["strategy"]["strategy_id"] = json!("g1-protective-momentum");
    request["strategy"]["quantity_raw"] = json!(1_000_000_000_i128);
    request["strategy"]["period"] = json!(2);
    request["strategy"]["threshold_bps"] = json!(0);
    request["protective_exit"] = json!({"stop_loss_bps": 50});
    let spec = request.to_string();

    let cli = via_cli(&spec, &root);
    let (status, http) = via_http("/app/backtest", &spec);
    assert_eq!(status, 200, "HTTP protective-exit backtest: {http}");
    assert_eq!(cli["result_hash"], http["result_hash"]);
    assert_eq!(cli["fills"], http["fills"]);

    let summary_path = cli["artifacts"]["summary"].as_str().unwrap();
    let summary: Value = serde_json::from_slice(
        &std::fs::read(summary_path).expect("read Rust-produced run summary"),
    )
    .expect("summary JSON");
    assert!(summary["protective_exit_fills"].as_array().is_some());
    assert!(!summary["protective_exit_fills"]
        .as_array()
        .unwrap()
        .is_empty());
    for fill in summary["protective_exit_fills"].as_array().unwrap() {
        assert_eq!(fill["reason"], "stop_loss");
        assert!(fill["fill_ts"].as_u64().unwrap() > fill["trigger_ts"].as_u64().unwrap());
        assert!(fill["qty_raw"].as_i64().unwrap() > 0);
    }

    match python_native::via_python(&repo_root(), "run_backtest", &spec) {
        None => eprintln!("[skip] Python leg: PyO3 extension not built"),
        Some(Ok(python)) => {
            assert_eq!(python["result_hash"], cli["result_hash"]);
            assert_eq!(python["fills"], cli["fills"]);
        }
        Some(Err(payload)) => panic!("Python protective-exit backtest failed: {payload}"),
    }
}
