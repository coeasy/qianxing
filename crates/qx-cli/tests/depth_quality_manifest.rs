use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|crates| crates.parent())
        .expect("repository root")
        .join("deploy/qianxing.depth-frame.example.json")
}

fn temp_root() -> PathBuf {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|crates| crates.parent())
        .expect("repository root");
    let root = workspace.join("target").join(format!(
        "qianxing-depth-quality-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).expect("create isolated test root");
    root
}

fn run(args: &[&str]) -> (i32, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args(args)
        .output()
        .expect("start qx-cli");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

fn run_with_temp(args: &[&str], temp: &Path) -> (i32, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args(args)
        .env("TEMP", temp)
        .env("TMP", temp)
        .output()
        .expect("start qx-cli with isolated temp directory");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

fn summary_path(root: &Path) -> PathBuf {
    let runs = root.join("runs");
    std::fs::read_dir(runs)
        .expect("read runs")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(".v7.summary.json"))
        })
        .expect("depth backtest summary exists")
}

#[test]
fn depth_manifest_is_generated_bound_reported_and_rejects_quality_drift() {
    let root = temp_root();
    let frame = root.join("source-depth-frame.json");
    std::fs::copy(fixture(), &frame).expect("copy DepthFrame into source workspace");
    let frame_text = frame.to_string_lossy().into_owned();
    let manifest = root.join("depth.dataset-manifest-v2.json");
    let manifest_text = manifest.to_string_lossy().into_owned();

    let (code, stdout, stderr) = run(&[
        "data-validate-depth",
        &frame_text,
        "--tier",
        "l2",
        "1000",
        &manifest_text,
    ]);
    assert_eq!(code, 0, "depth manifest generation failed: {stderr}");
    assert!(stdout.contains("rows=80"), "missing row count: {stdout}");
    let manifest_json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&manifest).expect("read generated depth manifest"))
            .expect("manifest is JSON");
    assert_eq!(manifest_json["tier"], "l2");
    assert_eq!(manifest_json["quality_report"]["row_count"], 80);
    assert_eq!(manifest_json["quality_report"]["interval_ms"], 1000);
    assert_eq!(manifest_json["quality_report"]["missing_intervals"], 0);

    let run_root = root.join("valid-run");
    let run_root_text = run_root.to_string_lossy().into_owned();
    let (code, stdout, stderr) = run(&[
        "backtest",
        "book",
        "--fill-tier",
        "l2",
        "--root",
        &run_root_text,
        "sma-cross",
        &frame_text,
        "--dataset-manifest",
        &manifest_text,
    ]);
    assert_eq!(code, 0, "quality-bound depth run failed: {stderr}");
    assert!(
        stdout.contains("[Data · Quality]"),
        "missing quality line: {stdout}"
    );
    let summary_path = summary_path(&run_root);
    let summary: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&summary_path).expect("read quality-bound summary"))
            .expect("summary is JSON");
    assert_eq!(summary["run_id"], "book-l2-sma_cross-BTCUSDT.BINANCE");
    assert_eq!(
        summary["manifest_run_id"],
        "depth-backtest:l2:sma_cross:BTCUSDT.BINANCE"
    );
    assert_eq!(
        summary["dataset_quality"]["quality_report"]["row_count"],
        80
    );
    assert_eq!(
        summary["dataset_quality"]["quality_report"]["missing_intervals"],
        0
    );

    let summary_text = summary_path.to_string_lossy().into_owned();
    let (code, _, stderr) = run(&["report", &summary_text, "--html", "--evidence"]);
    assert_eq!(code, 0, "depth quality report export failed: {stderr}");
    let html_path = summary_path.with_file_name(
        summary_path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .replace(".summary.json", ".report.html"),
    );
    let html = std::fs::read_to_string(&html_path).expect("read quality report HTML");
    assert!(html.contains("数据质量"), "HTML lacks data quality section");
    let manifest_fingerprint = summary["dataset_quality"]["manifest_fingerprint"]
        .as_str()
        .expect("summary contains manifest fingerprint");
    assert!(
        html.contains(manifest_fingerprint),
        "HTML lacks manifest identity"
    );
    let evidence_path = summary_path.with_file_name(
        summary_path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .replace(".summary.json", ".evidence.json"),
    );
    let evidence: serde_json::Value = serde_json::from_slice(
        &std::fs::read(&evidence_path).expect("read depth quality EvidenceBundle"),
    )
    .expect("EvidenceBundle is JSON");
    assert_eq!(evidence["generation"]["run"]["dataset"]["row_count"], 80);
    assert_eq!(
        evidence["generation"]["run"]["dataset"]["quality_report"]["interval_ms"],
        1000
    );
    assert_eq!(
        evidence["generation"]["run"]["dataset"]["manifest_fingerprint"],
        manifest_fingerprint
    );

    let original_summary = std::fs::read(&summary_path).expect("read original summary bytes");
    let mut tampered_summary = summary.clone();
    tampered_summary["manifest_run_id"] = serde_json::json!("depth-backtest:tampered");
    std::fs::write(
        &summary_path,
        serde_json::to_vec_pretty(&tampered_summary).expect("serialize tampered summary"),
    )
    .expect("write tampered summary");
    let (code, _, stderr) = run(&["report", &summary_text, "--evidence"]);
    assert_ne!(code, 0, "tampered manifest identity was accepted");
    assert!(
        stderr.contains("manifest.run_id 不一致"),
        "tampered manifest identity was not identified precisely: {stderr}"
    );
    std::fs::write(&summary_path, original_summary).expect("restore original summary");

    let mut drifted = manifest_json;
    drifted["quality_report"]["missing_intervals"] = serde_json::json!(1);
    let drifted_manifest = root.join("drifted.dataset-manifest-v2.json");
    std::fs::write(
        &drifted_manifest,
        serde_json::to_vec_pretty(&drifted).expect("serialize drifted manifest"),
    )
    .expect("write drifted manifest");
    let drifted_manifest_text = drifted_manifest.to_string_lossy().into_owned();
    let rejected_root = root.join("rejected-run");
    let rejected_root_text = rejected_root.to_string_lossy().into_owned();
    let (code, _, stderr) = run(&[
        "backtest",
        "book",
        "--fill-tier",
        "l2",
        "--root",
        &rejected_root_text,
        "sma-cross",
        &frame_text,
        "--dataset-manifest",
        &drifted_manifest_text,
    ]);
    assert_eq!(code, 2, "drifted depth quality was accepted");
    assert!(
        stderr.contains("missing 清单=1 重算=0"),
        "unclear drift: {stderr}"
    );
    assert!(
        !rejected_root.exists(),
        "rejected run left artifacts behind"
    );

    let evidence_text = evidence_path.to_string_lossy().into_owned();
    let recompute_temp = root.join("recompute-temp");
    std::fs::create_dir_all(&recompute_temp).expect("create isolated recompute temp directory");
    let local_verify = run_with_temp(
        &["evidence", "verify-run", &evidence_text, "--recompute"],
        &recompute_temp,
    );
    assert_eq!(
        local_verify.0, 0,
        "single-market depth evidence must independently recompute: {}",
        local_verify.2
    );

    let portable_root = root.join("portable-depth-run");
    let portable_root_text = portable_root.to_string_lossy().into_owned();
    let exported = run(&[
        "evidence",
        "export-run",
        &evidence_text,
        "--output-dir",
        &portable_root_text,
    ]);
    assert_eq!(
        exported.0, 0,
        "single-market depth run export failed: {}",
        exported.2
    );

    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let relocated_root = root.with_file_name(format!(
        "qianxing-depth-quality-relocated-{}-{stamp}",
        std::process::id()
    ));
    let relocated_temp = root.with_file_name(format!(
        "qianxing-depth-quality-recompute-temp-{}-{stamp}",
        std::process::id()
    ));
    assert_eq!(relocated_root.parent(), root.parent());
    assert!(!relocated_root.starts_with(&root));
    assert_eq!(relocated_temp.parent(), root.parent());
    assert!(!relocated_temp.starts_with(&root));
    std::fs::create_dir_all(&relocated_temp).expect("create relocated recompute temp directory");
    std::fs::rename(&portable_root, &relocated_root)
        .expect("move portable depth evidence outside source workspace");
    std::fs::remove_dir_all(&root).expect("remove DepthFrame, quality manifest and run artifacts");

    let relocated_bundle = relocated_root.join("evidence.json");
    let relocated_bundle_text = relocated_bundle.to_string_lossy().into_owned();
    let relocated_verify = run_with_temp(
        &[
            "evidence",
            "verify-run",
            &relocated_bundle_text,
            "--recompute",
        ],
        &relocated_temp,
    );
    assert_eq!(
        relocated_verify.0, 0,
        "portable single-market depth recompute must not depend on the source workspace: {}",
        relocated_verify.2
    );
    let relocated_result: serde_json::Value =
        serde_json::from_str(&relocated_verify.1).expect("portable recompute emits JSON");
    assert_eq!(relocated_result["result_recomputed"], true);
    let _ = std::fs::remove_dir_all(relocated_root);
    let _ = std::fs::remove_dir_all(relocated_temp);
}
