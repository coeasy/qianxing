use super::*;
use std::process::Command;

#[test]
fn cli_and_python_read_the_same_replay_verified_multi_leg_facts() {
    let root = scratch("multi-leg-report-facts");
    let mut spec = serde_json::from_str::<Value>(&multi_leg_spec_json(
        "g1-multi-leg-report-facts",
        &root.join("out"),
    ))
    .expect("multi-leg spec JSON");
    spec["account_id"] = json!("multi-leg-report-account");
    let spec_path = root.join("spec.json");
    std::fs::write(
        &spec_path,
        serde_json::to_vec_pretty(&spec).expect("encode spec"),
    )
    .expect("write multi-leg spec");
    let outcome_output = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args(["app", "multi-leg-backtest"])
        .arg(&spec_path)
        .output()
        .expect("run multi-leg backtest through CLI");
    assert_eq!(
        outcome_output.status.code(),
        Some(0),
        "multi-leg CLI error: {}",
        String::from_utf8_lossy(&outcome_output.stderr)
    );
    let outcome: Value = serde_json::from_slice(&outcome_output.stdout).expect("outcome JSON");
    let outcome_path = root.join("outcome.json");
    std::fs::write(
        &outcome_path,
        serde_json::to_vec_pretty(&outcome).expect("encode outcome"),
    )
    .expect("save outcome for report read");
    let report_output = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .current_dir(repo_root())
        .args(["app", "read-multi-leg-report"])
        .arg(&outcome_path)
        .output()
        .expect("read multi-leg report facts through CLI");
    assert_eq!(
        report_output.status.code(),
        Some(0),
        "multi-leg report CLI error: {}",
        String::from_utf8_lossy(&report_output.stderr)
    );
    let cli_report: Value =
        serde_json::from_slice(&report_output.stdout).expect("CLI multi-leg report facts JSON");
    assert_eq!(cli_report["schema_version"], 1);
    assert_eq!(cli_report["account_id"], outcome["account_id"]);
    assert_eq!(
        cli_report["total_fills"].as_u64().unwrap(),
        outcome["primary_fills"].as_u64().unwrap() + outcome["reference_fills"].as_u64().unwrap()
    );

    match python_native::via_python_json_value(
        &repo_root(),
        "read_multi_leg_report_facts_v1",
        &serde_json::to_string(&outcome).expect("encode outcome for Python reader"),
    ) {
        None => eprintln!("[skip] Python native multi-leg report reader: PyO3 extension not built"),
        Some(Ok(python_report)) => assert_eq!(python_report, cli_report),
        Some(Err(error)) => panic!("Python multi-leg report facts reader failed: {error}"),
    }
}

#[test]
fn cli_and_python_read_the_same_replay_verified_multi_leg_event_page() {
    let root = scratch("multi-leg-event-report-page");
    let mut spec = serde_json::from_str::<Value>(&multi_leg_spec_json(
        "g1-multi-leg-event-report-page",
        &root.join("out"),
    ))
    .expect("multi-leg spec JSON");
    spec["account_id"] = json!("multi-leg-event-report-account");
    let spec_path = root.join("spec.json");
    std::fs::write(
        &spec_path,
        serde_json::to_vec_pretty(&spec).expect("encode spec"),
    )
    .expect("write multi-leg spec");
    let outcome_output = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args(["app", "multi-leg-backtest"])
        .arg(&spec_path)
        .output()
        .expect("run multi-leg backtest through CLI");
    assert_eq!(
        outcome_output.status.code(),
        Some(0),
        "multi-leg CLI error: {}",
        String::from_utf8_lossy(&outcome_output.stderr)
    );
    let outcome: Value = serde_json::from_slice(&outcome_output.stdout).expect("outcome JSON");
    let request = json!({
        "schema_version": 1,
        "outcome": outcome,
        "page": {"offset": 0, "limit": 25}
    });
    let request_path = root.join("event-page-request.json");
    std::fs::write(
        &request_path,
        serde_json::to_vec_pretty(&request).expect("encode read request"),
    )
    .expect("write multi-leg event page request");
    let output = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .current_dir(repo_root())
        .args(["app", "read-multi-leg-event-page"])
        .arg(&request_path)
        .output()
        .expect("read multi-leg EventLog page through CLI");
    assert_eq!(
        output.status.code(),
        Some(0),
        "multi-leg event-page CLI error: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let cli_page: Value = serde_json::from_slice(&output.stdout).expect("CLI page JSON");
    assert_eq!(cli_page["schema_version"], 1);
    assert_eq!(cli_page["run_id"], request["outcome"]["run_id"]);
    assert_eq!(cli_page["limit"], 25);
    assert!(cli_page["rows"].as_array().unwrap().len() <= 25);

    match python_native::via_python_json_value(
        &repo_root(),
        "read_multi_leg_event_report_page_v1",
        &serde_json::to_string(&request).expect("encode Python request"),
    ) {
        None => eprintln!("[skip] Python native multi-leg event reader: PyO3 extension not built"),
        Some(Ok(python_page)) => assert_eq!(python_page, cli_page),
        Some(Err(error)) => panic!("Python multi-leg event reader failed: {error}"),
    }

    let mut invalid_request = request;
    invalid_request["page"]["offset"] = json!(cli_page["total"].as_u64().unwrap() + 1);
    std::fs::write(
        &request_path,
        serde_json::to_vec(&invalid_request).expect("encode invalid page request"),
    )
    .expect("write invalid page request");
    let invalid = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .current_dir(repo_root())
        .args(["app", "read-multi-leg-event-page"])
        .arg(&request_path)
        .output()
        .expect("reject out-of-range multi-leg page");
    assert!(!invalid.status.success());
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("offset 超出"));
}

#[test]
fn cli_and_python_read_the_same_complete_bounded_multi_leg_report() {
    let root = scratch("multi-leg-research-report");
    let mut spec = serde_json::from_str::<Value>(&multi_leg_spec_json(
        "g1-multi-leg-research-report",
        &root.join("out"),
    ))
    .expect("multi-leg spec JSON");
    spec["account_id"] = json!("multi-leg-research-report-account");
    let spec_path = root.join("spec.json");
    std::fs::write(
        &spec_path,
        serde_json::to_vec_pretty(&spec).expect("encode spec"),
    )
    .expect("write multi-leg spec");
    let outcome_output = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args(["app", "multi-leg-backtest"])
        .arg(&spec_path)
        .output()
        .expect("run multi-leg backtest through CLI");
    assert_eq!(
        outcome_output.status.code(),
        Some(0),
        "multi-leg CLI error: {}",
        String::from_utf8_lossy(&outcome_output.stderr)
    );
    let outcome: Value = serde_json::from_slice(&outcome_output.stdout).expect("outcome JSON");
    let request = json!({
        "schema_version": 1,
        "outcome": outcome,
        "page": {"offset": 0, "limit": 25}
    });
    let request_path = root.join("research-report-request.json");
    std::fs::write(
        &request_path,
        serde_json::to_vec_pretty(&request).expect("encode report request"),
    )
    .expect("write report request");
    let cli_output = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .current_dir(repo_root())
        .args(["app", "read-multi-leg-research-report"])
        .arg(&request_path)
        .output()
        .expect("read full multi-leg report through CLI");
    assert_eq!(
        cli_output.status.code(),
        Some(0),
        "multi-leg research report CLI error: {}",
        String::from_utf8_lossy(&cli_output.stderr)
    );
    let cli_report: Value = serde_json::from_slice(&cli_output.stdout).expect("report JSON");
    assert_eq!(cli_report["schema_version"], 1);
    assert_eq!(cli_report["run_id"], request["outcome"]["run_id"]);
    assert_eq!(
        cli_report["summary"]["account_id"],
        request["outcome"]["account_id"]
    );
    assert_eq!(cli_report["markets"].as_array().unwrap().len(), 2);
    assert_eq!(
        cli_report["equity"]["total"],
        cli_report["summary"]["equity"]["points"]
    );
    assert!(cli_report["equity"]["rows"].as_array().unwrap().len() <= 1_000);
    assert_eq!(
        cli_report["fills"]["total"],
        cli_report["summary"]["total_fills"]
    );
    assert!(cli_report["fills"]["rows"].as_array().unwrap().len() <= 200);
    assert_eq!(cli_report["events"]["limit"], 25);
    assert!(cli_report["execution"]["market_specs_json"].is_string());

    match python_native::via_python_json_value(
        &repo_root(),
        "read_multi_leg_research_report_v1",
        &serde_json::to_string(&request).expect("encode Python report request"),
    ) {
        None => eprintln!("[skip] Python native full multi-leg report: PyO3 extension not built"),
        Some(Ok(python_report)) => assert_eq!(python_report, cli_report),
        Some(Err(error)) => panic!("Python full multi-leg report reader failed: {error}"),
    }

    let mut invalid_request = request;
    invalid_request["page"]["offset"] = json!(cli_report["events"]["total"].as_u64().unwrap() + 1);
    std::fs::write(
        &request_path,
        serde_json::to_vec(&invalid_request).expect("encode invalid report request"),
    )
    .expect("write invalid report request");
    let invalid = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .current_dir(repo_root())
        .args(["app", "read-multi-leg-research-report"])
        .arg(&request_path)
        .output()
        .expect("reject out-of-range report page");
    assert!(!invalid.status.success());
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("offset 超出"));
}
