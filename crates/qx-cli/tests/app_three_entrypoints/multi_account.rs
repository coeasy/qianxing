use super::*;

#[test]
fn isolated_multi_account_backtest_is_shared_by_cli_http_and_python() {
    let root = scratch("multi-account");
    let repo = repo_root();
    let btc_path = sample_bars();
    let eth_path = repo
        .join(".task-tmp")
        .join(format!("eth-bars-{}.json", std::process::id()));
    std::fs::create_dir_all(eth_path.parent().unwrap()).expect("create fixture directory");
    let btc = std::fs::read_to_string(&btc_path).expect("read BTC fixture");
    let eth = btc.replace("BTCUSDT.BINANCE", "ETHUSDT.BINANCE");
    std::fs::write(&eth_path, eth).expect("write ETH fixture");
    let member = |account: &str, run: &str, instrument: &str, bars: &Path, output: &Path| {
        json!({
            "schema_version": 1,
            "run_id": run,
            "account_id": account,
            "instrument": instrument,
            "bars_path": bars,
            "settlement_currency": "USDT",
            "initial_cash_raw": 100_000_000_000_000_i128,
            "seed": 20261013,
            "output_dir": output,
            "strategy": {
                "kind": "sma_cross",
                "strategy_id": format!("{account}-sma"),
                "quantity_raw": 1_000_000_000_i128,
                "fast_window": 2,
                "slow_window": 3,
            },
        })
    };
    let spec = json!({
        "schema_version": 1,
        "batch_id": "g1-multi-account",
        "runs": [
            member("acct-eth", "g1-eth", "ETHUSDT.BINANCE", &eth_path, &root.join("eth-out")),
            member("acct-btc", "g1-btc", "BTCUSDT.BINANCE", Path::new(&btc_path), &root.join("btc-out")),
        ],
    })
    .to_string();
    let spec_path = root.join("multi-account.json");
    std::fs::write(&spec_path, &spec).expect("write batch spec");
    let output = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args(["app", "multi-account-backtest"])
        .arg(&spec_path)
        .output()
        .expect("start multi-account CLI");
    assert_eq!(
        output.status.code(),
        Some(0),
        "CLI error: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let cli: Value = serde_json::from_slice(&output.stdout).expect("CLI batch JSON");
    let (status, http) = via_http("/app/multi-account-backtest", &spec);
    assert_eq!(status, 200, "HTTP batch failed: {http}");
    for value in [&cli, &http] {
        assert!(value["summary"]["complete"].as_bool().unwrap());
        assert_eq!(value["summary"]["accounts_succeeded"], 2);
        assert_eq!(value["summary"]["accounts_failed"], 0);
        assert!(value["summary"]["successful_fills"].as_u64().unwrap() > 0);
        assert_eq!(value["runs"][0]["account_id"], "acct-btc");
        assert_eq!(value["runs"][1]["account_id"], "acct-eth");
    }
    assert_eq!(cli["spec_fingerprint"], http["spec_fingerprint"]);
    for index in 0..2 {
        assert_eq!(
            cli["runs"][index]["outcome"]["result_hash"],
            http["runs"][index]["outcome"]["result_hash"]
        );
    }
    match python_native::via_python(&repo, "run_multi_account_backtest", &spec) {
        None => eprintln!("[skip] Python leg: PyO3 extension not built"),
        Some(Ok(python)) => {
            assert_eq!(python["spec_fingerprint"], cli["spec_fingerprint"]);
            for index in 0..2 {
                assert_eq!(
                    python["runs"][index]["outcome"]["result_hash"],
                    cli["runs"][index]["outcome"]["result_hash"]
                );
            }
        }
        Some(Err(payload)) => panic!("Python multi-account use case failed: {payload}"),
    }
}
