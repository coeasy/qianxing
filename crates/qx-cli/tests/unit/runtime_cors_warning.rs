use super::*;

#[test]
fn runtime_check_warns_for_routable_api_without_websocket_origin_allowlist() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let runtime_path = workspace
        .join("deploy")
        .join("qianxing.runtime.production.example.json");
    let mut config = read_runtime_config(&runtime_path).expect("load production runtime template");
    assert_eq!(config.api.bind, "0.0.0.0:8443");
    assert!(config.api.cors_allowed_origins.is_empty());

    let (failures, warnings) = validate_runtime_references(&runtime_path, &config);
    assert!(
        failures
            .iter()
            .all(|failure| !failure.contains("cors_allowed_origins")),
        "empty origin list is a legal neutral configuration: {failures:?}"
    );
    assert_eq!(
        warnings
            .iter()
            .filter(|warning| warning.contains("带 Origin 的 /events/live WebSocket 不受来源限制"))
            .count(),
        1,
        "routable bind without an origin allowlist must produce one actionable warning: {warnings:?}"
    );
    let report = runtime_check_report(&config);
    assert_eq!(report["network_accessed"], false);
    assert!(report["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|warning| {
            warning.as_str().is_some_and(|warning| {
                warning.contains("带 Origin 的 /events/live WebSocket 不受来源限制")
            })
        }));

    config.api.bind = "127.0.0.1:8443".into();
    let (_, loopback_warnings) = validate_runtime_references(&runtime_path, &config);
    assert!(
        loopback_warnings
            .iter()
            .all(|warning| !warning.contains("带 Origin 的 /events/live WebSocket 不受来源限制")),
        "loopback operator configuration should remain quiet: {loopback_warnings:?}"
    );
    let loopback_report = runtime_check_report(&config);
    assert!(loopback_report["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .all(|warning| {
            warning.as_str().is_none_or(|warning| {
                !warning.contains("带 Origin 的 /events/live WebSocket 不受来源限制")
            })
        }));

    config.api.bind = "0.0.0.0:8443".into();
    config.api.cors_allowed_origins = vec!["https://ops.internal.example".into()];
    let (_, allowlisted_warnings) = validate_runtime_references(&runtime_path, &config);
    assert!(
        allowlisted_warnings
            .iter()
            .all(|warning| !warning.contains("带 Origin 的 /events/live WebSocket 不受来源限制")),
        "an explicit source allowlist should satisfy the browser-origin warning: {allowlisted_warnings:?}"
    );
}

fn runtime_check_report(config: &RuntimeConfig) -> serde_json::Value {
    let root = temp_cli_case_dir("runtime-cors-warning");
    let runtime_path = root.join("runtime.json");
    std::fs::write(
        &runtime_path,
        serde_json::to_vec(config).expect("serialize runtime fixture"),
    )
    .expect("write runtime fixture");
    collect_runtime_check_report(&runtime_path).expect("collect runtime-check report")
}
