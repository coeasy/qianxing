//! Fixed-input process replay for the Bar dataset quality command.

use super::*;
use std::path::Path;

#[test]
fn bar_dataset_validation_replays_the_same_manifest_contract() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("qx-cli tests are two levels below the workspace root");
    let frame = workspace
        .join("deploy/qianxing.bar-frame.example.json")
        .to_string_lossy()
        .into_owned();
    let args = [
        "data-validate",
        frame.as_str(),
        "parity-bars",
        "v1",
        "1000",
        "bars.manifest.json",
        "--json",
    ];

    let first = observe("bar-data-validate", &args, 1);
    let second = observe("bar-data-validate", &args, 2);
    assert_eq!(first.exit_code, 0, "first validation failed: {first:?}");
    assert_eq!(first, second, "data-validate changed across clean replays");
    assert_eq!(first.artifacts, ["bars.manifest.json"]);
    assert!(first.result_hashes.is_empty());

    let manifest: serde_json::Value =
        serde_json::from_str(&first.stdout).expect("--json emits a DatasetManifestV2 object");
    assert_eq!(manifest["manifest_version"], 1);
    assert_eq!(manifest["dataset"]["dataset_id"], "parity-bars");
    assert_eq!(manifest["dataset"]["version"], "v1");
    assert_eq!(manifest["instrument"], "BTCUSDT.BINANCE");
    assert_eq!(manifest["quality_report"]["row_count"], 70);
    assert_eq!(manifest["quality_report"]["interval_ms"], 1000);
    assert_eq!(manifest["quality_report"]["missing_intervals"], 0);

    let cwd = temp_dir("bar-data-validate-idempotent", 1);
    let first_write = observe_in(&cwd, &args);
    let second_read = observe_in(&cwd, &args);
    assert_eq!(
        first_write.exit_code, 0,
        "initial write failed: {first_write:?}"
    );
    assert_eq!(
        second_read.exit_code, 0,
        "idempotent read failed: {second_read:?}"
    );
    assert_eq!(first_write.stdout, second_read.stdout);
    assert_eq!(first_write.artifact_digests, second_read.artifact_digests);
    assert_eq!(first_write.artifacts, ["bars.manifest.json"]);
    let _ = std::fs::remove_dir_all(cwd);
}
