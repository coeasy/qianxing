use super::*;

#[test]
fn twap_plan_is_identical_across_cli_http_and_python_native() {
    let root = scratch("algo-execution");
    let spec = json!({
        "schema_version": 1,
        "parent_id": "g1-algo-parent",
        "total_quantity_raw": 11,
        "start_ts": 10,
        "end_ts": 40,
        "method": "twap",
        "child_count": 4,
        "volume_profile": []
    })
    .to_string();
    let spec_path = root.join("algo-execution.json");
    std::fs::write(&spec_path, &spec).expect("write algo execution spec");
    let output = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args(["app", "plan-algo-execution"])
        .arg(&spec_path)
        .output()
        .expect("start algo execution CLI");
    assert_eq!(
        output.status.code(),
        Some(0),
        "CLI: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let cli: Value = serde_json::from_slice(&output.stdout).expect("CLI result JSON");
    let (status, http) = via_http("/app/plan-algo-execution", &spec);
    assert_eq!(status, 200, "HTTP algo plan: {http}");
    assert_eq!(cli["children"], http["children"]);
    assert_eq!(cli["parent_id"], "g1-algo-parent");
    assert_eq!(
        cli["children"]
            .as_array()
            .unwrap()
            .iter()
            .map(|child| child["quantity_raw"].as_i64().unwrap())
            .sum::<i64>(),
        11
    );
    match python_native::via_python(&repo_root(), "plan_algo_execution", &spec) {
        None => eprintln!("[skip] Python leg: PyO3 extension not built"),
        Some(Ok(python)) => assert_eq!(python["children"], cli["children"]),
        Some(Err(payload)) => panic!("Python algo execution failed: {payload}"),
    }
}
