use super::{observe_in, temp_dir};
use std::path::Path;

#[test]
fn dataset_ingest_replays_the_same_persisted_data_contract() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("qx-cli tests are two levels below the workspace root");
    let fixture = root.join("deploy/qianxing.ashare.bar-frame.example.json");
    let mut observations = Vec::new();

    for replay in 1..=2 {
        let cwd = temp_dir("dataset-ingest", replay);
        std::fs::copy(&fixture, cwd.join("bars.json")).expect("copy fixed BarFrame fixture");
        let observation = observe_in(
            &cwd,
            &[
                "dataset-ingest",
                "bars.json",
                "parity-dataset",
                "v1",
                "data",
            ],
        );
        let _ = std::fs::remove_dir_all(&cwd);
        observations.push(observation);
    }

    let first = &observations[0];
    let second = &observations[1];
    assert_eq!(first.exit_code, 0, "dataset-ingest failed: {first:?}");
    assert_eq!(first, second, "dataset-ingest changed across clean replays");
    assert!(
        first
            .stdout
            .contains("[Data · Ingest] dataset=parity-dataset version=v1"),
        "dataset-ingest must report the persisted dataset identity: {first:?}"
    );
    assert!(
        first
            .artifacts
            .iter()
            .any(|path| path == "data/datasets.manifest.json"),
        "dataset-ingest must persist its registry manifest: {first:?}"
    );
    assert!(
        first
            .artifacts
            .iter()
            .any(|path| path == "data/parity-dataset.bars.json"),
        "dataset-ingest must persist normalized bar data: {first:?}"
    );
    assert!(
        first.artifact_digests.len() >= 3,
        "expected manifest and bar artifacts: {first:?}"
    );
    assert!(first.result_hashes.is_empty());
}
