use super::*;

fn market_spec(price_tick: i128) -> Value {
    json!({
        "instrument": {"symbol": "BTCUSDT", "venue": "BINANCE"},
        "product": "spot",
        "base_currency": "BTC",
        "quote_currency": "USDT",
        "settlement_currency": "USDT",
        "contract_size": 1_000_000_000_i128,
        "linear": true,
        "inverse": false,
        "price_tick": price_tick,
        "qty_step": 1_i128,
        "min_qty": 1_i128,
        "max_leverage": 1,
        "maintenance_margin_bps": 0,
        "valid_from": 0,
        "valid_to": null,
    })
}

#[test]
fn frozen_market_spec_is_used_by_cli_http_and_python_bar_runs() {
    let root = scratch("market-spec");
    let mut request: Value = serde_json::from_str(&backtest_spec_json(
        "g1-market-spec",
        &sample_bars(),
        &root.join("out"),
    ))
    .expect("backtest spec json");
    request["market_spec"] = market_spec(1_000);
    request["fee_schedule"] = json!({"maker_fee_bps": 11, "taker_fee_bps": 37});
    let spec = request.to_string();
    let cli = via_cli(&spec, &root);
    let (status, http) = via_http("/app/backtest", &spec);
    assert_eq!(status, 200, "HTTP market-aware Bar run failed: {http}");
    assert_eq!(cli["result_hash"], http["result_hash"]);

    let manifest_path = cli["artifacts"]["run_manifest"].as_str().unwrap();
    let manifest: Value = serde_json::from_slice(&std::fs::read(manifest_path).unwrap()).unwrap();
    assert!(manifest["instrument_spec_version"]
        .as_str()
        .unwrap()
        .starts_with("inline-market-spec:"));
    let summary: Value = serde_json::from_slice(
        &std::fs::read(cli["artifacts"]["summary"].as_str().unwrap()).unwrap(),
    )
    .unwrap();
    assert_eq!(
        summary["fee_model"],
        "MakerTaker@v1[params=maker_bp=11;taker_bp=37]"
    );
    assert!(summary["fees_raw"].as_i64().unwrap() > 0);

    match python_native::via_python(&repo_root(), "run_backtest", &spec) {
        None => eprintln!("[skip] Python leg: PyO3 extension is not built"),
        Some(Ok(python)) => assert_eq!(python["result_hash"], cli["result_hash"]),
        Some(Err(payload)) => panic!("Python market-aware Bar run failed: {payload}"),
    }
}

#[test]
fn backtest_refuses_simulated_fills_outside_the_frozen_market_tick() {
    let root = scratch("market-spec-bad-tick");
    let output_dir = root.join("out");
    let mut request: Value = serde_json::from_str(&backtest_spec_json(
        "g1-market-bad-tick",
        &sample_bars(),
        &output_dir,
    ))
    .expect("backtest spec json");
    request["market_spec"] = market_spec(999_999_937);
    request["fee_schedule"] = json!({"maker_fee_bps": 2, "taker_fee_bps": 5});
    let spec = request.to_string();

    let spec_path = root.join("spec.json");
    std::fs::write(&spec_path, &spec).expect("write bad-tick spec");
    let cli = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args(["app", "backtest"])
        .arg(&spec_path)
        .output()
        .expect("start qx-cli");
    assert_eq!(cli.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&cli.stderr);
    assert!(
        stderr.contains("成交价格不是正数或不在价格 tick 上"),
        "{stderr}"
    );

    let (status, http) = via_http("/app/backtest", &spec);
    assert_eq!(status, 400, "{http}");
    assert!(http["message"].as_str().unwrap().contains("价格 tick"));
    assert!(!output_dir.join("g1-market-bad-tick.run.json").exists());

    match python_native::via_python(&repo_root(), "run_backtest", &spec) {
        None => eprintln!("[skip] Python leg: PyO3 extension is not built"),
        Some(Err(payload)) => assert!(payload.contains("价格 tick"), "{payload}"),
        Some(Ok(value)) => panic!("out-of-tick fill should fail: {value}"),
    }
}
