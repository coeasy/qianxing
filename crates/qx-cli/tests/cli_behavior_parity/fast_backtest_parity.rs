use super::{observe_in, temp_dir, Observation};
use serde_json::Value;
use std::path::Path;
use std::process::Command;

fn copy_deploy_examples(root: &Path) {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("qx-cli tests are two levels below the workspace root");
    for entry in std::fs::read_dir(repo.join("deploy")).expect("read deploy examples") {
        let source = entry.expect("read deploy entry").path();
        if source.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let target = root.join(source.file_name().expect("deploy file name"));
        let bytes = std::fs::read(&source).expect("read deploy JSON");
        let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            std::fs::write(target, bytes).expect("copy non-document JSON example");
            continue;
        };
        if let Some(data_dir) = value
            .get_mut("storage")
            .and_then(serde_json::Value::as_object_mut)
            .and_then(|storage| storage.get_mut("data_dir"))
            .and_then(|value| value.as_str())
            .map(str::to_owned)
        {
            let suffix = data_dir.strip_prefix("data/").unwrap_or(&data_dir);
            let isolated = root.join("data").join(suffix);
            value["storage"]["data_dir"] =
                serde_json::Value::String(isolated.to_string_lossy().replace('\\', "/"));
        }
        std::fs::write(
            target,
            serde_json::to_vec_pretty(&value).expect("serialize example"),
        )
        .expect("write isolated deploy example");
    }
}

fn prepare_ashare_quality_fast_backtest(root: &Path) -> String {
    copy_deploy_examples(root);
    let frame = root.join("qianxing.ashare.bar-frame.example.json");
    let quality_manifest = root.join("ashare-quality.dataset-manifest-v2.json");
    let output = Command::new(super::binary())
        .current_dir(root)
        .env_remove("QX_DEPLOY_DIR")
        .env_remove("QX_PYTHON")
        .args([
            "data-validate",
            frame.to_str().expect("A-share frame path is UTF-8"),
            "strategy-bars:000001.SZSE",
            qx_data::BARFRAME_DATASET_VERSION,
            "1800000",
            quality_manifest
                .to_str()
                .expect("quality manifest path is UTF-8"),
        ])
        .output()
        .expect("generate fast-backtest quality manifest");
    assert_eq!(
        output.status.code(),
        Some(0),
        "data-validate failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let manifest = root.join("qianxing.fast-backtest.ashare.example.json");
    let mut payload: Value = serde_json::from_slice(
        &std::fs::read(&manifest).expect("read A-share fast-backtest manifest"),
    )
    .expect("fast-backtest manifest is JSON");
    payload["jobs"][0]["dataset_manifest"] = Value::String(
        quality_manifest
            .file_name()
            .expect("manifest file name")
            .to_string_lossy()
            .into_owned(),
    );
    std::fs::write(
        &manifest,
        serde_json::to_vec_pretty(&payload).expect("serialize fast-backtest manifest"),
    )
    .expect("write A-share fast-backtest quality manifest binding");
    manifest.to_string_lossy().into_owned()
}

fn clean_replay_root(root: &Path) {
    std::fs::remove_dir_all(root).expect("remove previous fast-backtest replay");
    std::fs::create_dir(root).expect("recreate fast-backtest replay root");
    copy_deploy_examples(root);
}

fn assert_same_process_contract(first: &Observation, second: &Observation) {
    assert_eq!(first.exit_code, second.exit_code);
    assert_eq!(first.stdout, second.stdout);
    assert_eq!(first.result_hashes, second.result_hashes);
    assert_eq!(first.artifacts, second.artifacts);
}

#[test]
fn ashare_fast_backtest_replays_outputs_and_artifact_set() {
    let root = temp_dir("fast-backtest", 1);
    let manifest = prepare_ashare_quality_fast_backtest(&root);
    let args = ["fast-backtest", manifest.as_str()];
    let first = observe_in(&root, &args);
    clean_replay_root(&root);
    let manifest = prepare_ashare_quality_fast_backtest(&root);
    let args = ["fast-backtest", manifest.as_str()];
    let second = observe_in(&root, &args);

    assert_eq!(first.exit_code, 0, "fast-backtest failed: {first:?}");
    assert_same_process_contract(&first, &second);
    assert!(
        first.stdout.contains("jobs=1 completed=1")
            && first.stdout.contains("source=ashare-example-qfq-v1"),
        "A-share dataset provenance and completed job must be reported: {first:?}"
    );
    assert!(!first.result_hashes.is_empty());
    let summary_path = first
        .artifacts
        .iter()
        .find(|path| path.ends_with(".summary.json"))
        .expect("fast-backtest summary artifact");
    let summary: Value = serde_json::from_slice(
        &std::fs::read(root.join(summary_path)).expect("read fast-backtest summary"),
    )
    .expect("fast-backtest summary is JSON");
    assert_eq!(
        summary.pointer("/dataset_quality/quality_report/interval_ms"),
        Some(&serde_json::json!(1_800_000))
    );
    assert_eq!(
        summary.pointer("/dataset_quality/quality_report/row_count"),
        Some(&serde_json::json!(6))
    );
    for suffix in [".summary.json", ".equity.csv", ".fills.csv", ".run.json"] {
        assert!(
            first.artifacts.iter().any(|path| path.ends_with(suffix)),
            "missing fast-backtest artifact {suffix}: {first:?}"
        );
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn multi_venue_fast_backtest_replays_parallel_jobs_and_artifact_set() {
    let root = temp_dir("fast-backtest-multi-venue", 1);
    copy_deploy_examples(&root);
    let manifest = root
        .join("qianxing.fast-backtest.example.json")
        .to_string_lossy()
        .into_owned();
    let args = ["fast-backtest", manifest.as_str()];
    let first = observe_in(&root, &args);
    clean_replay_root(&root);
    let second = observe_in(&root, &args);
    let _ = std::fs::remove_dir_all(&root);

    assert_eq!(
        first.exit_code, 0,
        "multi-venue fast-backtest failed: {first:?}"
    );
    assert_same_process_contract(&first, &second);
    assert!(
        first.stdout.contains("jobs=2 completed=2")
            && first.stdout.contains("instrument=BTCUSDT.BINANCE")
            && first.stdout.contains("instrument=BTC/USDT:USDT.OKX"),
        "both venue jobs must complete and retain their instrument identity: {first:?}"
    );
    assert!(first.result_hashes.len() >= 2);
    assert!(
        first
            .artifacts
            .iter()
            .filter(|path| path.ends_with(".summary.json"))
            .count()
            >= 2,
        "each parallel job must persist its own summary: {first:?}"
    );
    assert!(
        first
            .artifacts
            .iter()
            .filter(|path| path.ends_with(".run.json"))
            .count()
            >= 2,
        "each parallel job must persist its own RunManifest: {first:?}"
    );
}

#[test]
fn invalid_fast_backtest_manifest_replays_the_same_fail_closed_contract() {
    let root = temp_dir("fast-backtest-invalid", 1);
    let manifest = root.join("invalid.json");
    std::fs::write(&manifest, r#"{"schema_version":1,"jobs":[]}"#)
        .expect("write invalid fast-backtest manifest");
    let manifest = manifest.to_string_lossy().into_owned();
    let args = ["fast-backtest", manifest.as_str()];
    let run = || {
        let output = Command::new(super::binary())
            .current_dir(&root)
            .env_remove("QX_DEPLOY_DIR")
            .env_remove("QX_PYTHON")
            .args(args)
            .output()
            .expect("run invalid fast-backtest manifest");
        (
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    };
    let first = run();
    let second = run();
    assert_eq!(
        first, second,
        "invalid manifest rejection must replay exactly"
    );
    assert_eq!(first.0, 2, "invalid manifest must fail with usage status");
    assert!(
        first.2.contains("1..=256"),
        "diagnostic must explain the jobs count contract: {first:?}"
    );
    assert_eq!(
        super::artifact_files(&root),
        ["invalid.json"],
        "rejected manifest must not create run artifacts"
    );
    let _ = std::fs::remove_dir_all(root);
}
