use super::observe;
use std::path::Path;

#[test]
fn dataset_bundle_cli_replays_the_persisted_artifact_contract() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("qx-cli tests are two levels below the workspace root");
    let bundle = root
        .join("deploy/qianxing.dataset-bundle.bar-frame.example.json")
        .to_string_lossy()
        .into_owned();
    let bars = root
        .join("deploy/qianxing.bar-frame.example.json")
        .to_string_lossy()
        .into_owned();
    let args = ["dataset-bundle", bundle.as_str(), "data", bars.as_str()];
    let first = observe("dataset-bundle", &args, 1);
    let second = observe("dataset-bundle", &args, 2);
    assert_eq!(first.exit_code, 0, "dataset-bundle failed: {first:?}");
    assert_eq!(first, second, "dataset-bundle changed across clean replays");
    assert!(
        first
            .stdout
            .contains("[Data · Bundle] bundle=strategy-bars.BTCUSDT.BINANCE"),
        "dataset-bundle must report the saved manifest identity: {first:?}"
    );
    assert_eq!(
        first.artifacts,
        ["data/bundles/strategy-bars.BTCUSDT.BINANCE--barframe-example-v1.bundle.json"],
        "dataset-bundle must write exactly the versioned manifest"
    );
    assert_eq!(first.artifact_digests.len(), 1);
    assert!(first.result_hashes.is_empty());
}
