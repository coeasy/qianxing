use super::*;

#[test]
fn multi_leg_parameter_experiment_is_shared_by_cli_http_and_python() {
    let root = scratch("multi-leg-experiment");
    let spec = multi_leg_experiment_spec_json("g1-multi-leg-grid", &root.join("out"));
    let spec_path = root.join("multi-leg-experiment.json");
    std::fs::write(&spec_path, &spec).expect("write multi-leg experiment spec");
    let output = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args(["app", "run-multi-leg-experiment"])
        .arg(&spec_path)
        .output()
        .expect("start multi-leg experiment CLI");
    assert_eq!(
        output.status.code(),
        Some(0),
        "CLI error: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let cli: Value = serde_json::from_slice(&output.stdout).expect("CLI experiment JSON");
    let (status, http) = via_http("/app/run-multi-leg-experiment", &spec);
    assert_eq!(status, 200, "HTTP experiment failed: {http}");
    let same_results = |value: &Value| {
        json!({
            "experiment_id": value["experiment_id"],
            "total_candidates": value["total_candidates"],
            "completed_candidates": value["completed_candidates"],
            "succeeded_candidates": value["succeeded_candidates"],
            "failed_candidates": value["failed_candidates"],
            "ranking": value["ranking"],
            "candidates": value["candidates"].as_array().unwrap().iter().map(|candidate| {
                json!({
                    "ordinal": candidate["ordinal"],
                    "parameters": candidate["parameters"],
                    "run_id": candidate["outcome"]["run_id"],
                    "combined_return_bps": candidate["outcome"]["combined_return_bps"],
                    "primary_result_hash": candidate["outcome"]["primary_result_hash"],
                    "reference_result_hash": candidate["outcome"]["reference_result_hash"],
                    "error": candidate["error"],
                })
            }).collect::<Vec<_>>(),
        })
    };
    assert_eq!(same_results(&cli), same_results(&http));
    assert_eq!(cli["completed_candidates"], 2);
    assert_eq!(cli["failed_candidates"], 1);
    assert_eq!(cli["succeeded_candidates"], 1);
    match python_native::via_python(&repo_root(), "run_multi_leg_experiment", &spec) {
        None => eprintln!("[skip] Python leg: PyO3 extension not built"),
        Some(Ok(python)) => assert_eq!(same_results(&python), same_results(&cli)),
        Some(Err(payload)) => panic!("Python multi-leg experiment failed: {payload}"),
    }
}

#[test]
fn multi_leg_experiment_run_handle_delivers_the_terminal_report_over_http() {
    let root = scratch("multi-leg-experiment-handle");
    let spec = multi_leg_experiment_spec_json("g1-multi-leg-grid-handle", &root.join("out"));
    // Configure the same server roots used by the synchronous API facade before constructing
    // this service instance; the API intentionally resolves inputs under its configured root.
    let (sync_status, _) = via_http("/app/run-multi-leg-experiment", &spec);
    assert_eq!(
        sync_status, 200,
        "sync API experiment must initialize roots"
    );
    let service = qx_api::ApiService::new(qx_api::ApiState::default());
    let started = service.handle("POST", "/app/run-multi-leg-experiment/start", &spec, 0);
    assert_eq!(started.status, 200, "start failed: {}", started.body);
    let started: Value = serde_json::from_str(&started.body).expect("start run JSON");
    assert_eq!(started["run_id"], "g1-multi-leg-grid-handle");

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let terminal = loop {
        let response = service.handle("GET", "/app/runs/g1-multi-leg-grid-handle", "", 0);
        assert_eq!(response.status, 200, "status failed: {}", response.body);
        let status: Value = serde_json::from_str(&response.body).expect("run status JSON");
        if matches!(
            status["status"].as_str(),
            Some("succeeded" | "failed" | "cancelled")
        ) {
            break status;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "multi-leg experiment did not reach a terminal state"
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
    };
    assert_eq!(
        terminal["status"], "succeeded",
        "terminal report: {terminal}"
    );
    assert_eq!(
        terminal["result"]["experiment_id"],
        "g1-multi-leg-grid-handle"
    );
    assert_eq!(terminal["result"]["completed_candidates"], 2);
    assert!(terminal["result"]["ranking"].as_array().unwrap().len() == 1);
}

#[test]
fn the_same_multi_leg_spec_runs_through_cli_http_and_python_shared_rust_use_cases() {
    let root = scratch("multi-leg");
    let spec = multi_leg_spec_json("g1-multi-leg", &root.join("out"));
    let spec_path = root.join("multi-leg.json");
    std::fs::write(&spec_path, &spec).expect("write MultiLegBacktestSpec");
    let output = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args(["app", "multi-leg-backtest"])
        .arg(&spec_path)
        .output()
        .expect("start multi-leg CLI");
    assert_eq!(
        output.status.code(),
        Some(0),
        "CLI error: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let cli: Value = serde_json::from_slice(&output.stdout).expect("CLI outcome JSON");

    let (status, http) = via_http("/app/multi-leg-backtest", &spec);
    assert_eq!(status, 200, "HTTP outcome: {http}");
    for (name, outcome) in [("CLI", &cli), ("HTTP", &http)] {
        assert_eq!(outcome["run_id"], "g1-multi-leg", "{name} run id");
        assert!(
            outcome["primary_fills"].as_u64().unwrap_or(0) > 0,
            "{name} primary fills"
        );
        assert!(
            outcome["reference_fills"].as_u64().unwrap_or(0) > 0,
            "{name} reference fills"
        );
    }
    for key in [
        "primary_result_hash",
        "reference_result_hash",
        "combined_return_bps",
        "primary_fills",
        "reference_fills",
    ] {
        assert_eq!(cli[key], http[key], "CLI and HTTP differ at {key}");
    }

    match python_native::via_python(&repo_root(), "run_multi_leg_backtest", &spec) {
        None => eprintln!("[skip] Python leg: PyO3 extension not built"),
        Some(Ok(python)) => {
            for key in [
                "primary_result_hash",
                "reference_result_hash",
                "combined_return_bps",
                "primary_fills",
                "reference_fills",
            ] {
                assert_eq!(cli[key], python[key], "CLI and Python differ at {key}");
            }
        }
        Some(Err(payload)) => panic!("Python multi-leg use case failed: {payload}"),
    }
}
