use super::*;

#[test]
fn full_health_readout_replays_structured_json_and_paper_artifacts() {
    let first = observe("all-json", &["all", "--json"], 1);
    let second = observe("all-json", &["all", "--json"], 2);
    assert_eq!(first.exit_code, second.exit_code);
    assert_eq!(first.artifacts, second.artifacts);
    assert!(
        matches!(first.exit_code, 0 | 1),
        "all --json should report readiness through its normal pass/blocked exit: {first:?}"
    );
    let report: serde_json::Value = serde_json::from_str(&first.stdout).unwrap_or_else(|error| {
        panic!(
            "all --json must emit one JSON object: {error}\n{}",
            first.stdout
        )
    });
    let second_report: serde_json::Value = serde_json::from_str(&second.stdout)
        .unwrap_or_else(|error| panic!("replayed all --json must be valid JSON: {error}"));
    for key in [
        "schema_version",
        "command",
        "ok",
        "checks",
        "readiness_layers",
    ] {
        assert_eq!(
            report[key], second_report[key],
            "all --json field {key} changed across replays"
        );
    }
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["command"], "all");
    assert_eq!(report["ok"].as_bool(), Some(first.exit_code == 0));
    assert_eq!(report["network_accessed"], false);
    assert_eq!(report["orders_sent"], false);
    assert_eq!(report["checks"].as_array().map(Vec::len), Some(5));
    for layer in ["R", "P", "O", "L"] {
        assert!(
            matches!(
                report["readiness_layers"][layer]["status"].as_str(),
                Some("pass" | "blocked" | "not_assessed")
            ),
            "all --json omitted or invalidated readiness layer {layer}: {}",
            report["readiness_layers"][layer]
        );
    }
    for name in [
        "selfcheck",
        "doctor",
        "runtime_check",
        "paper_check",
        "live_check",
    ] {
        assert!(
            report["reports"][name].is_object(),
            "all --json omitted the {name} component report"
        );
        assert_eq!(
            report["reports"][name]["ok"], second_report["reports"][name]["ok"],
            "all --json {name} component changed readiness across replays"
        );
    }
    assert!(
        !first.artifacts.is_empty(),
        "all --json should retain the local Paper smoke artifacts it reports: {first:?}"
    );
}
