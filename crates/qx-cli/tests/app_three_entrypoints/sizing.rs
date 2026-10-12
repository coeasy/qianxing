use super::*;

#[test]
fn fractional_kelly_sizing_is_shared_by_cli_http_and_python() {
    let root = scratch("kelly-sizing");
    let output_dir = root.join("out");
    // The repository's generic sample ends with a one-sided rising window. Kelly
    // correctly fails closed without both wins and losses, so make a deterministic
    // mixed-return fixture with a positive estimated edge and an actionable signal.
    let bars_path = root.join("kelly-bars.json");
    let mut bars: Value = serde_json::from_slice(
        &std::fs::read(repo_root().join("deploy/qianxing.bar-frame.example.json"))
            .expect("read example bars"),
    )
    .expect("parse example bars");
    let mut close = 100_000_000_000_i128;
    let closes = (0..70)
        .map(|index| {
            close = if index > 0 && index % 10 == 0 {
                close - close / 1_000
            } else {
                close + close / 100
            };
            close
        })
        .collect::<Vec<_>>();
    for key in ["open_raw", "high_raw", "low_raw", "close_raw"] {
        bars[key] = json!(closes);
    }
    std::fs::write(
        &bars_path,
        serde_json::to_vec(&bars).expect("serialize Kelly bars"),
    )
    .expect("write Kelly bars");
    let mut request: Value = serde_json::from_str(&backtest_spec_json(
        "g1-kelly-sizing",
        &bars_path.to_string_lossy(),
        &output_dir,
    ))
    .expect("backtest spec json");
    request["strategy"]["kind"] = json!("momentum");
    request["strategy"]["period"] = json!(2);
    request["strategy"]["threshold_bps"] = json!(0);
    request["strategy"]["target_kelly_fraction_bps"] = json!(5_000);
    request["strategy"]["kelly_lookback_bars"] = json!(30);
    let spec = request.to_string();

    let cli = via_cli(&spec, &root);
    let (status, http) = via_http("/app/backtest", &spec);
    assert_eq!(status, 200, "HTTP Kelly sizing must succeed: {http}");
    assert_eq!(cli["result_hash"], http["result_hash"]);
    assert!(cli["fills"].as_u64().unwrap_or(0) > 0);

    match python_native::via_python(&repo_root(), "run_backtest", &spec) {
        None => eprintln!("[skip] Python Kelly entrypoint: native extension has not been built"),
        Some(Ok(python)) => assert_eq!(
            python["result_hash"], cli["result_hash"],
            "Python Kelly sizing must use the same Rust result"
        ),
        Some(Err(payload)) => panic!("Python Kelly entrypoint failed: {payload}"),
    }
}
