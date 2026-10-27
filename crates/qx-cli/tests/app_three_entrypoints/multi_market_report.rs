use super::*;

#[test]
fn cli_and_python_read_the_same_replay_verified_multi_market_event_page() {
    let root = scratch("multi-market-event-report-page");
    let deploy = repo_root().join("deploy");
    let source: Value = serde_json::from_slice(
        &std::fs::read(deploy.join("qianxing.bar-frame.example.json"))
            .expect("read sample BarFrame"),
    )
    .expect("decode sample BarFrame");
    let markets = [
        ("BTCUSDT.BINANCE", "mm-report-btc"),
        ("ETHUSDT.BINANCE", "mm-report-eth"),
    ];
    let mut configured_markets = Vec::new();
    for (index, (instrument, strategy_id)) in markets.iter().enumerate() {
        let frame_path = root.join(format!("market-{index}.json"));
        let mut frame = source.clone();
        frame["instrument"] = json!(instrument);
        std::fs::write(
            &frame_path,
            serde_json::to_vec(&frame).expect("encode BarFrame"),
        )
        .expect("persist BarFrame");
        configured_markets.push(json!({
            "instrument": instrument,
            "bars_path": frame_path,
            "priority": index,
            "market_spec": null,
            "fee_schedule": null,
            "strategies": [{
                "priority": 0,
                "reserved_capital_raw": 20_000_000_000_000_i128,
                "strategy": {
                    "kind": "sma_cross",
                    "strategy_id": strategy_id,
                    "quantity_raw": 1_000_000_000_i128,
                    "fast_window": 1,
                    "slow_window": 3
                }
            }]
        }));
    }
    let spec_path = root.join("multi-market.json");
    std::fs::write(
        &spec_path,
        json!({
            "schema_version": 1,
            "run_id": "g1-multi-market-event-report",
            "account_id": "g1-multi-market-account",
            "settlement_currency": "USDT",
            "initial_cash_raw": 100_000_000_000_000_i128,
            "seed": 17,
            "output_dir": root.join("runs"),
            "markets": configured_markets
        })
        .to_string(),
    )
    .expect("write multi-market spec");
    let run = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .arg("app")
        .arg("multi-market-backtest")
        .arg(&spec_path)
        .output()
        .expect("run multi-market backtest");
    assert!(
        run.status.success(),
        "multi-market CLI error: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    let outcome: Value = serde_json::from_slice(&run.stdout).expect("outcome JSON");
    let request = json!({
        "schema_version": 1,
        "outcome": outcome,
        "page": {"offset": 0, "limit": 25}
    });
    let request_path = root.join("event-report-request.json");
    std::fs::write(
        &request_path,
        serde_json::to_vec_pretty(&request).expect("encode request"),
    )
    .expect("write request");
    let cli_report_output = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .current_dir(repo_root())
        .args(["app", "read-multi-market-event-page"])
        .arg(&request_path)
        .output()
        .expect("read event page through qx-cli");
    assert!(
        cli_report_output.status.success(),
        "multi-market report CLI error: {}",
        String::from_utf8_lossy(&cli_report_output.stderr)
    );
    let cli_report: Value =
        serde_json::from_slice(&cli_report_output.stdout).expect("CLI report DTO JSON");
    assert_eq!(cli_report["run_id"], outcome["run_id"]);
    let artifact: Value = serde_json::from_slice(
        &std::fs::read(outcome["artifact_path"].as_str().expect("artifact path"))
            .expect("read multi-market artifact"),
    )
    .expect("decode multi-market artifact");
    assert_eq!(cli_report["total"], artifact["event_count"]);
    assert_eq!(
        cli_report["replayed_ledger_entries"].as_u64().unwrap(),
        artifact["ledger_entries"].as_array().unwrap().len() as u64
    );
    assert!(cli_report["rows"].as_array().unwrap().len() <= 25);

    let complete_report_output = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .current_dir(repo_root())
        .args(["app", "read-multi-market-research-report"])
        .arg(&request_path)
        .output()
        .expect("read complete multi-market report through qx-cli");
    assert!(
        complete_report_output.status.success(),
        "complete multi-market report CLI error: {}",
        String::from_utf8_lossy(&complete_report_output.stderr)
    );
    let complete_report: Value = serde_json::from_slice(&complete_report_output.stdout)
        .expect("complete multi-market report DTO JSON");
    assert_eq!(complete_report["run_id"], outcome["run_id"]);
    assert_eq!(complete_report["markets"].as_array().unwrap().len(), 2);
    assert_eq!(complete_report["verification"]["verified"], true);
    assert!(complete_report["equity"]["rows"].as_array().unwrap().len() <= 1_000);
    assert!(complete_report["fills"]["rows"].as_array().unwrap().len() <= 200);
    assert!(complete_report["events"]["rows"].as_array().unwrap().len() <= 25);
    assert_eq!(complete_report["events"]["total"], artifact["event_count"]);
    assert!(complete_report["summary"]["metrics"]["final_equity_raw"].is_string());
    assert!(
        complete_report["markets"][0]["bars"]["rows"]
            .as_array()
            .unwrap()
            .len()
            <= 1_000
    );
    assert!(complete_report["markets"][0]["bars"]["rows"][0]["close_raw"].is_string());
    assert!(complete_report["markets"]
        .as_array()
        .unwrap()
        .iter()
        .all(|market| market["pnl_contribution"]["rows"]
            .as_array()
            .map_or(true, |rows| rows.len() <= 1_000)));
    assert!(
        complete_report["summary"]["execution_assumptions"]["markets"]
            .as_array()
            .unwrap()
            .iter()
            .all(|market| market["funding_events"]["rows"].as_array().unwrap().len() <= 100)
    );
    assert!(complete_report["events"]["rows"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["market_context"].is_object())
        .all(|row| row["bar_context_status"] == "verified"
            || row["bar_context_status"] == "before_first_visible_bar"));

    match super::python_native::via_python_json_value(
        &repo_root(),
        "read_multi_market_research_report_v1",
        &serde_json::to_string(&request).expect("encode full Python report request"),
    ) {
        None => {
            eprintln!(
                "[skip] Python native multi-market full report reader: PyO3 extension not built"
            )
        }
        Some(Ok(python_report)) => assert_eq!(python_report, complete_report),
        Some(Err(error)) => panic!("Python multi-market full report reader failed: {error}"),
    }
    std::fs::write(
        root.join("complete-report.json"),
        &complete_report_output.stdout,
    )
    .expect("persist complete report for schema validation");

    match super::python_native::via_python_json_value(
        &repo_root(),
        "read_multi_market_event_report_page_v1",
        &serde_json::to_string(&request).expect("encode Python request"),
    ) {
        None => {
            eprintln!("[skip] Python native multi-market report reader: PyO3 extension not built")
        }
        Some(Ok(python_report)) => assert_eq!(python_report, cli_report),
        Some(Err(error)) => panic!("Python multi-market report reader failed: {error}"),
    }

    let mut mismatched_outcome = request.clone();
    mismatched_outcome["outcome"]["run_id"] = json!("another-run");
    std::fs::write(
        &request_path,
        serde_json::to_vec(&mismatched_outcome).expect("encode mismatched outcome"),
    )
    .expect("write mismatched outcome request");
    let mismatch = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .current_dir(repo_root())
        .args(["app", "read-multi-market-event-page"])
        .arg(&request_path)
        .output()
        .expect("reject mismatched multi-market outcome");
    assert!(!mismatch.status.success());
    assert!(
        String::from_utf8_lossy(&mismatch.stderr).contains("身份不一致"),
        "mismatched outcome error: {}",
        String::from_utf8_lossy(&mismatch.stderr)
    );

    let mut out_of_range_page = request;
    out_of_range_page["page"]["offset"] = json!(artifact["event_count"].as_u64().unwrap() + 1);
    std::fs::write(
        &request_path,
        serde_json::to_vec(&out_of_range_page).expect("encode out-of-range page"),
    )
    .expect("write out-of-range page request");
    let out_of_range = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .current_dir(repo_root())
        .args(["app", "read-multi-market-event-page"])
        .arg(&request_path)
        .output()
        .expect("reject out-of-range event page");
    assert!(!out_of_range.status.success());
    assert!(
        String::from_utf8_lossy(&out_of_range.stderr).contains("offset 超出"),
        "out-of-range page error: {}",
        String::from_utf8_lossy(&out_of_range.stderr)
    );
}

#[test]
fn cli_and_python_read_the_same_replay_verified_multi_market_depth_event_page() {
    let root = scratch("multi-market-depth-event-report-page");
    let source: Value = serde_json::from_slice(
        &std::fs::read(repo_root().join("deploy/qianxing.depth-frame.example.json"))
            .expect("read sample DepthFrame"),
    )
    .expect("decode sample DepthFrame");
    let instruments = [
        ("BTCUSDT.BINANCE", "BTC", "BINANCE"),
        ("ETHUSDT.BYBIT", "ETH", "BYBIT"),
    ];
    let mut markets = Vec::new();
    for (index, (instrument, base, venue)) in instruments.iter().enumerate() {
        let (symbol, _) = instrument.split_once('.').expect("canonical instrument");
        let depth_path = root.join(format!("market-{index}.json"));
        let mut frame = source.clone();
        let typed_instrument = json!({"symbol": symbol, "venue": venue});
        frame["instrument"] = typed_instrument.clone();
        for snapshot in frame["snapshots"]
            .as_array_mut()
            .expect("DepthFrame snapshots")
        {
            snapshot["instrument"] = typed_instrument.clone();
        }
        std::fs::write(
            &depth_path,
            serde_json::to_vec(&frame).expect("encode DepthFrame"),
        )
        .expect("persist DepthFrame");
        markets.push(json!({
            "instrument": instrument,
            "depth_path": depth_path,
            "data_tier": "l2",
            "priority": index,
            "market_spec": {
                "instrument": typed_instrument,
                "product": "spot",
                "base_currency": base,
                "quote_currency": "USDT",
                "settlement_currency": "USDT",
                "contract_size": 1_000_000_000_i128,
                "linear": false,
                "inverse": false,
                "price_tick": 1_i128,
                "qty_step": 1_i128,
                "min_qty": 1_i128,
                "max_leverage": 1,
                "maintenance_margin_bps": 0,
                "valid_from": 1_u64,
                "valid_to": null
            },
            "fee_bps": 0,
            "latency_snapshots": 0,
            "queue_position_bps": 0,
            "market_impact_bps": 0,
            "strategy": {
                "kind": "sma_cross",
                "strategy_id": format!("mm-depth-report-{index}"),
                "quantity_raw": 1_000_000_000_i128,
                "fast_window": 2,
                "slow_window": 3
            }
        }));
    }
    let spec_path = root.join("multi-market-depth.json");
    std::fs::write(
        &spec_path,
        json!({
            "schema_version": 1,
            "run_id": "g1-multi-market-depth-event-report",
            "account_id": "g1-multi-market-depth-account",
            "settlement_currency": "USDT",
            "initial_cash_raw": 100_000_000_000_000_i128,
            "output_dir": root.join("runs"),
            "markets": markets
        })
        .to_string(),
    )
    .expect("write multi-market Depth spec");
    let run = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .arg("app")
        .arg("multi-market-depth-backtest")
        .arg(&spec_path)
        .output()
        .expect("run multi-market Depth backtest");
    assert!(
        run.status.success(),
        "multi-market Depth CLI error: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    let outcome: Value = serde_json::from_slice(&run.stdout).expect("Depth outcome JSON");
    let request = json!({
        "schema_version": 1,
        "outcome": outcome,
        "page": {"offset": 0, "limit": 25}
    });
    let request_path = root.join("event-report-request.json");
    std::fs::write(
        &request_path,
        serde_json::to_vec_pretty(&request).expect("encode request"),
    )
    .expect("write request");
    let cli_report_output = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .current_dir(repo_root())
        .args(["app", "read-multi-market-depth-event-page"])
        .arg(&request_path)
        .output()
        .expect("read Depth event page through qx-cli");
    assert!(
        cli_report_output.status.success(),
        "multi-market Depth report CLI error: {}",
        String::from_utf8_lossy(&cli_report_output.stderr)
    );
    let cli_report: Value =
        serde_json::from_slice(&cli_report_output.stdout).expect("CLI Depth report DTO JSON");
    assert_eq!(cli_report["run_id"], outcome["run_id"]);
    assert!(cli_report["total"].as_u64().unwrap() > 0);
    assert!(cli_report["rows"].as_array().unwrap().len() <= 25);

    match super::python_native::via_python_json_value(
        &repo_root(),
        "read_multi_market_depth_event_report_page_v1",
        &serde_json::to_string(&request).expect("encode Python request"),
    ) {
        None => {
            eprintln!("[skip] Python native multi-market Depth reader: PyO3 extension not built")
        }
        Some(Ok(python_report)) => assert_eq!(python_report, cli_report),
        Some(Err(error)) => panic!("Python multi-market Depth reader failed: {error}"),
    }

    let complete_report_output = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .current_dir(repo_root())
        .args(["app", "read-multi-market-depth-research-report"])
        .arg(&request_path)
        .output()
        .expect("read complete multi-market Depth report through qx-cli");
    assert!(
        complete_report_output.status.success(),
        "complete multi-market Depth report CLI error: {}",
        String::from_utf8_lossy(&complete_report_output.stderr)
    );
    let complete_report: Value = serde_json::from_slice(&complete_report_output.stdout)
        .expect("complete multi-market Depth report DTO JSON");
    assert_eq!(complete_report["run_id"], outcome["run_id"]);
    assert_eq!(complete_report["markets"].as_array().unwrap().len(), 2);
    assert_eq!(complete_report["verification"]["verified"], true);
    assert!(complete_report["equity"]["rows"].as_array().unwrap().len() <= 1_000);
    assert!(complete_report["fills"]["rows"].as_array().unwrap().len() <= 200);
    assert!(complete_report["events"]["rows"].as_array().unwrap().len() <= 25);
    assert!(complete_report["summary"]["metrics"]["final_equity_raw"].is_string());
    assert!(complete_report["markets"]
        .as_array()
        .unwrap()
        .iter()
        .all(|market| market["depth"]["rows"]
            .as_array()
            .map_or(false, |snapshots| snapshots.len() <= 1_000)));
    match super::python_native::via_python_json_value(
        &repo_root(),
        "read_multi_market_depth_research_report_v1",
        &serde_json::to_string(&request).expect("encode complete Python Depth report request"),
    ) {
        None => eprintln!(
            "[skip] Python native multi-market Depth full report reader: PyO3 extension not built"
        ),
        Some(Ok(python_report)) => assert_eq!(python_report, complete_report),
        Some(Err(error)) => panic!("Python multi-market Depth full report failed: {error}"),
    }
    std::fs::write(
        root.join("complete-depth-report.json"),
        &complete_report_output.stdout,
    )
    .expect("persist complete Depth report for schema validation");

    let artifact: Value = serde_json::from_slice(
        &std::fs::read(outcome["artifact_path"].as_str().expect("artifact path"))
            .expect("read Depth artifact"),
    )
    .expect("decode Depth artifact");
    let mut mismatched = request.clone();
    mismatched["outcome"]["run_id"] = json!("another-depth-run");
    std::fs::write(&request_path, serde_json::to_vec(&mismatched).unwrap())
        .expect("write mismatched request");
    let mismatch = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .current_dir(repo_root())
        .args(["app", "read-multi-market-depth-event-page"])
        .arg(&request_path)
        .output()
        .expect("reject mismatched Depth outcome");
    assert!(!mismatch.status.success());
    assert!(String::from_utf8_lossy(&mismatch.stderr).contains("身份不一致"));

    let mut out_of_range = request;
    out_of_range["page"]["offset"] = json!(artifact["event_count"].as_u64().unwrap() + 1);
    std::fs::write(&request_path, serde_json::to_vec(&out_of_range).unwrap())
        .expect("write out-of-range request");
    let out_of_range = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .current_dir(repo_root())
        .args(["app", "read-multi-market-depth-event-page"])
        .arg(&request_path)
        .output()
        .expect("reject out-of-range Depth page");
    assert!(!out_of_range.status.success());
    assert!(String::from_utf8_lossy(&out_of_range.stderr).contains("offset 超出"));
}
