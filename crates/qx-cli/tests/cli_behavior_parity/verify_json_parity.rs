use super::observe;

#[test]
fn verify_json_replays_the_kernel_only_report_without_side_effects() {
    let first = observe("verify-json", &["verify", "--json"], 1);
    let second = observe("verify-json", &["verify", "--json"], 2);
    assert_eq!(first.exit_code, 0, "verify --json failed: {first:?}");
    assert_eq!(first, second, "verify --json changed across clean replays");

    let report: serde_json::Value =
        serde_json::from_str(&first.stdout).expect("verify --json emits one JSON object");
    assert_eq!(report["schema_version"], 2);
    assert_eq!(report["command"], "verify");
    assert_eq!(report["ok"], true);
    assert_eq!(report["checks"].as_array().map(Vec::len), Some(1));
    assert_eq!(report["checks"][0]["name"], "selfcheck_chain");
    assert_eq!(report["checks"][0]["scope"], "kernel_only");
    assert_eq!(report["readiness_layers"]["R"]["status"], "pass");
    for layer in ["P", "O", "L"] {
        assert_eq!(
            report["readiness_layers"][layer]["status"], "not_assessed",
            "verify must not claim {layer} readiness"
        );
    }
    assert_eq!(report["network_accessed"], false);
    assert_eq!(report["orders_sent"], false);
    assert!(first.result_hashes.is_empty());
    assert!(first.artifacts.is_empty(), "verify wrote files: {first:?}");
}
