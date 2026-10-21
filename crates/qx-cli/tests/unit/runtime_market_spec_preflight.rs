use super::*;

#[test]
fn runtime_reference_preflight_parses_market_specs_for_declared_worker_symbols() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let runtime_path = workspace
        .join("deploy")
        .join("qianxing.runtime.paper-strategy.example.json");
    let mut config = read_runtime_config(&runtime_path).expect("load runtime template");
    let worker = config
        .workers
        .iter_mut()
        .find(|worker| worker.role == WorkerRole::Execution)
        .expect("paper runtime has an execution worker");
    worker.symbols = vec!["BTCUSDT.BINANCE".into()];

    let spec_path = temp_cli_case_dir("runtime-market-spec-preflight").join("market-spec.json");
    worker.instrument_spec_path = Some(spec_path.to_string_lossy().into_owned());
    let worker = worker.clone();
    let mut spec: serde_json::Value = serde_json::from_slice(
        &std::fs::read(workspace.join("deploy/qianxing.binance.spot.spec.json"))
            .expect("read valid product spec"),
    )
    .expect("parse valid product spec");
    std::fs::write(&spec_path, serde_json::to_vec(&spec).unwrap()).unwrap();
    validate_worker_market_specs(&runtime_path, &worker)
        .expect("worker startup preflight accepts a valid spec");
    let (failures, _) = validate_runtime_references(&runtime_path, &config);
    assert!(
        failures
            .iter()
            .all(|failure| !failure.contains("instrument_spec_path 与标的 BTCUSDT.BINANCE")),
        "valid frozen market spec should pass startup preflight: {failures:?}"
    );

    spec["price_tick"] = serde_json::json!(0);
    std::fs::write(&spec_path, serde_json::to_vec(&spec).unwrap()).unwrap();
    assert!(
        validate_worker_market_specs(&runtime_path, &worker).is_err(),
        "worker launch must reject the same invalid market spec"
    );
    let (failures, _) = validate_runtime_references(&runtime_path, &config);
    assert!(
        failures
            .iter()
            .any(|failure| failure.contains("instrument_spec_path 与标的 BTCUSDT.BINANCE")),
        "runtime reference preflight must reject invalid market precision: {failures:?}"
    );
}
