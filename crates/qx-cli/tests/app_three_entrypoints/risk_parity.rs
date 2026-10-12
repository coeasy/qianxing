use super::*;

#[test]
fn risk_parity_plan_is_identical_across_cli_http_and_python_native() {
    let root = scratch("risk-parity");
    let spec = json!({
        "schema_version": 1,
        "portfolio_id": "g1-risk-parity",
        "available_cash_raw": 1_000_000_000_000_i128,
        "target_gross_notional_raw": 600_000_000_000_i128,
        "current_positions": {"BTCUSDT.BINANCE": 10_i128},
        "assets": [
            {"instrument": "BTCUSDT.BINANCE", "mark_price_raw": 20_000_000_000_i128,
             "returns_bps": [-200, 100, -100, 200, -100, 100]},
            {"instrument": "ETHUSDT.BINANCE", "mark_price_raw": 1_000_000_000_i128,
             "returns_bps": [-100, 50, -50, 100, -50, 50]}
        ],
        "max_turnover_bps": 10_000,
        "min_trade_notional_raw": 0
    })
    .to_string();
    let spec_path = root.join("risk-parity.json");
    std::fs::write(&spec_path, &spec).expect("write risk parity spec");

    let output = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args(["app", "plan-risk-parity"])
        .arg(&spec_path)
        .output()
        .expect("start risk parity CLI");
    assert_eq!(
        output.status.code(),
        Some(0),
        "CLI: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let cli: Value = serde_json::from_slice(&output.stdout).expect("CLI result JSON");
    let (status, http) = via_http("/app/plan-risk-parity", &spec);
    assert_eq!(status, 200, "HTTP risk parity: {http}");
    assert_eq!(cli["plan_hash"], http["plan_hash"]);
    assert_eq!(cli["plan"], http["plan"]);
    assert_eq!(cli["plan"]["allocations"].as_array().unwrap().len(), 2);
    assert_eq!(
        cli["plan"]["allocations"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["weight_bps"].as_u64().unwrap())
            .sum::<u64>(),
        10_000
    );
    match python_native::via_python(&repo_root(), "plan_risk_parity", &spec) {
        None => eprintln!("[skip] Python leg: PyO3 extension not built"),
        Some(Ok(python)) => {
            assert_eq!(python["plan_hash"], cli["plan_hash"]);
            assert_eq!(python["plan"], cli["plan"]);
        }
        Some(Err(payload)) => panic!("Python risk parity failed: {payload}"),
    }
}
