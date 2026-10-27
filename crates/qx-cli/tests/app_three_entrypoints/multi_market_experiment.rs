use super::*;

fn spec_json(experiment_id: &str, root: &Path) -> String {
    std::fs::create_dir_all(root).expect("create multi-market experiment fixture root");
    let source: Value =
        serde_json::from_str(&std::fs::read_to_string(sample_bars()).expect("sample BarFrame"))
            .expect("sample BarFrame JSON");
    let instruments = ["BTCUSDT.BINANCE", "ETHUSDT.BINANCE", "SOLUSDT.BINANCE"];
    let markets = instruments
        .iter()
        .enumerate()
        .map(|(index, instrument)| {
            let path = root.join(format!("market-{index}.json"));
            let mut frame = source.clone();
            frame["instrument"] = json!(instrument);
            std::fs::write(&path, frame.to_string()).expect("write market BarFrame");
            json!({
                "instrument": instrument,
                "bars_path": path,
                "priority": index,
                "strategies": [{
                    "priority": 0,
                    "reserved_capital_raw": 20_000_000_000_000_i128,
                    "strategy": {
                        "kind": "sma_cross",
                        "strategy_id": format!("market-slot-{index}"),
                        "quantity_raw": 1_000_000_000_i128,
                        "fast_window": 1,
                        "slow_window": 3,
                    }
                }]
            })
        })
        .collect::<Vec<_>>();
    json!({
        "schema_version": 1,
        "experiment_id": experiment_id,
        "target_strategy_id": "market-slot-0",
        "base": {
            "schema_version": 1,
            "run_id": "unused-multi-market-base",
            "account_id": "g1-shared-account",
            "settlement_currency": "USDT",
            "initial_cash_raw": 100_000_000_000_000_i128,
            "seed": 7,
            "output_dir": root.join("output"),
            "markets": markets,
        },
        "parameter_space": [{ "name": "fast_window", "values": [1, 2] }],
        "strategy_kinds": ["sma_cross", "ema_cross"],
    })
    .to_string()
}

fn comparable(value: &Value) -> Value {
    json!({
        "experiment_id": value["experiment_id"],
        "total_candidates": value["total_candidates"],
        "succeeded_candidates": value["succeeded_candidates"],
        "failed_candidates": value["failed_candidates"],
        "canonical_run_spec_fingerprint": value["canonical_run_spec_fingerprint"],
        "input_data_fingerprint": value["input_data_fingerprint"],
        "result_hash": value["result_hash"],
        "artifact_location_names": value["artifact_locations"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        "has_run_manifest": value["run_manifest"].is_string(),
        "has_run_record": value["run_record"].is_string(),
        "ranking": value["ranking"].as_array().unwrap().iter().map(|item| json!({
            "ordinal": item["ordinal"],
            "return_bps": item["return_bps"],
            "max_drawdown_bps": item["max_drawdown_bps"],
            "shared_result_hash": item["shared_result_hash"],
        })).collect::<Vec<_>>(),
        "candidates": value["candidates"].as_array().unwrap().iter().map(|item| json!({
            "ordinal": item["ordinal"],
            "parameters": item["parameters"],
            "strategy_kind": item["strategy_kind"],
            "shared_result_hash": item["outcome"]["shared_result_hash"],
            "error": item["error"],
        })).collect::<Vec<_>>(),
    })
}

fn assert_experiment_input_is_bound(label: &str, value: &Value) {
    let input_path = value["artifact_locations"]["experiment_input"]
        .as_str()
        .unwrap_or_else(|| panic!("{label} experiment input sidecar location missing: {value}"));
    let input_bytes = std::fs::read(input_path).expect("read experiment input sidecar");
    let record_path = value["run_record"].as_str().expect("RunRecord V2 path");
    let record: Value =
        serde_json::from_slice(&std::fs::read(record_path).expect("read RunRecord V2"))
            .expect("RunRecord V2 JSON");
    let input_ref = record["artifact_refs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|artifact| artifact["name"] == "experiment_input")
        .expect("RunRecord V2 binds experiment input");
    assert_eq!(input_ref["byte_len"], input_bytes.len() as u64);
    assert_eq!(
        input_ref["artifact_id"],
        qx_strategy::sha256_hex(&input_bytes)
    );
}

#[test]
fn multi_market_parameter_experiment_is_shared_by_cli_http_and_python() {
    let root = scratch("multi-market-experiment");
    let spec = spec_json("g1-multi-market-grid", &root);
    let spec_path = root.join("multi-market-experiment.json");
    std::fs::write(&spec_path, &spec).expect("write multi-market experiment spec");
    let output = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args(["app", "run-multi-market-experiment"])
        .arg(&spec_path)
        .output()
        .expect("start multi-market experiment CLI");
    assert_eq!(
        output.status.code(),
        Some(0),
        "CLI error: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let cli: Value = serde_json::from_slice(&output.stdout).expect("CLI experiment JSON");
    let (status, http) = via_http("/app/run-multi-market-experiment", &spec);
    assert_eq!(status, 200, "HTTP experiment failed: {http}");
    assert_eq!(cli["succeeded_candidates"], 4, "{cli}");
    assert_experiment_input_is_bound("CLI", &cli);
    assert_experiment_input_is_bound("HTTP", &http);
    assert_eq!(comparable(&cli), comparable(&http));
    match python_native::via_python(&repo_root(), "run_multi_market_experiment", &spec) {
        None => eprintln!("[skip] Python leg: PyO3 extension not built"),
        Some(Ok(python)) => {
            assert_experiment_input_is_bound("Python", &python);
            assert_eq!(comparable(&python), comparable(&cli));
        }
        Some(Err(payload)) => panic!("Python multi-market experiment failed: {payload}"),
    }
}

#[test]
fn multi_market_experiment_async_route_delivers_terminal_report() {
    let root = scratch("multi-market-experiment-handle");
    let spec = spec_json("g1-multi-market-grid-handle", &root);
    let (sync_status, _) = via_http(
        "/app/run-multi-market-experiment",
        &spec_json("g1-multi-market-grid-root-init", &root.join("sync")),
    );
    assert_eq!(
        sync_status, 200,
        "sync API experiment must initialize roots"
    );
    let service = qx_api::ApiService::new(qx_api::ApiState::default());
    let started = service.handle("POST", "/app/run-multi-market-experiment/start", &spec, 0);
    assert_eq!(started.status, 200, "start failed: {}", started.body);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let response = service.handle("GET", "/app/runs/g1-multi-market-grid-handle", "", 0);
        assert_eq!(response.status, 200, "status failed: {}", response.body);
        let status: Value = serde_json::from_str(&response.body).expect("run status JSON");
        if matches!(
            status["status"].as_str(),
            Some("succeeded" | "failed" | "cancelled")
        ) {
            assert_eq!(status["status"], "succeeded", "terminal result: {status}");
            assert_eq!(status["result"]["completed_candidates"], 4);
            assert_eq!(status["result"]["succeeded_candidates"], 4);
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "experiment did not finish"
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}
