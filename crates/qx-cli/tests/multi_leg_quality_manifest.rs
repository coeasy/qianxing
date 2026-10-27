use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|crates| crates.parent())
        .expect("repository root")
        .join("deploy")
        .join(name)
}

fn temp_root() -> PathBuf {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|crates| crates.parent())
        .expect("repository root");
    let root = workspace.join("target").join(format!(
        "qianxing-multi-leg-quality-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).expect("create isolated multi-leg quality root");
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

fn find_composition_summary(root: &Path) -> PathBuf {
    std::fs::read_dir(root.join("runs"))
        .expect("read runs")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(".summary.json"))
                && std::fs::read_to_string(path)
                    .ok()
                    .and_then(|payload| serde_json::from_str::<serde_json::Value>(&payload).ok())
                    .is_some_and(|summary| summary["input"]["kind"] == "multi-barframe")
        })
        .expect("multi-leg composition summary exists")
}

#[test]
fn multi_leg_manifests_are_bound_reported_and_rechecked() {
    let root = temp_root();
    let source_root = root.join("source-workspace");
    let input_root = source_root.join("inputs");
    std::fs::create_dir_all(&input_root).expect("create isolated multi-leg source inputs");
    let primary = input_root.join("primary.bar-frame.json");
    let reference = input_root.join("reference.bar-frame.json");
    std::fs::copy(
        fixture("qianxing.bar-frame.pairs-primary.example.json"),
        &primary,
    )
    .expect("copy primary BarFrame into isolated source workspace");
    std::fs::copy(
        fixture("qianxing.bar-frame.pairs-reference.example.json"),
        &reference,
    )
    .expect("copy reference BarFrame into isolated source workspace");
    let primary_manifest = source_root.join("primary.dataset-manifest-v2.json");
    let reference_manifest = source_root.join("reference.dataset-manifest-v2.json");
    let primary_text = primary.to_string_lossy().into_owned();
    let reference_text = reference.to_string_lossy().into_owned();
    let primary_manifest_text = primary_manifest.to_string_lossy().into_owned();
    let reference_manifest_text = reference_manifest.to_string_lossy().into_owned();

    for (frame, dataset_id, version, output) in [
        (
            &primary_text,
            "strategy-bars:BTCUSDT.BINANCE",
            qx_data::BARFRAME_DATASET_VERSION,
            &primary_manifest_text,
        ),
        (
            &reference_text,
            "strategy-bars:ETHUSDT.BINANCE",
            qx_data::BARFRAME_DATASET_VERSION,
            &reference_manifest_text,
        ),
    ] {
        let (code, _, stderr) = run(&[
            "data-validate",
            frame,
            dataset_id,
            version,
            "3600000",
            output,
        ]);
        assert_eq!(code, 0, "manifest generation failed: {stderr}");
    }

    let run_root = source_root.join("valid-run");
    let run_root_text = run_root.to_string_lossy().into_owned();
    let (code, stdout, stderr) = run(&[
        "backtest",
        "multi-builtin",
        "pairs_arbitrage",
        &primary_text,
        &reference_text,
        "--root",
        &run_root_text,
        "--primary-dataset-manifest",
        &primary_manifest_text,
        "--reference-dataset-manifest",
        &reference_manifest_text,
    ]);
    assert_eq!(code, 0, "quality-bound multi-leg run failed: {stderr}");
    assert!(stdout.contains("[Multi-leg · Backtest]"), "{stdout}");

    let summary_path = find_composition_summary(&run_root);
    let summary: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&summary_path).expect("read composition summary"))
            .expect("composition summary is JSON");
    assert_eq!(
        summary.pointer("/dataset_quality/quality_report/row_count"),
        Some(&serde_json::json!(120))
    );
    let manifest_path = PathBuf::from(
        summary["run_manifest"]
            .as_str()
            .expect("composition summary references RunManifest"),
    );
    let manifest: serde_json::Value = serde_json::from_slice(
        &std::fs::read(&manifest_path).expect("composition RunManifest exists"),
    )
    .expect("composition RunManifest is JSON");
    assert_eq!(manifest["result_hash"], summary["result_hash"]);
    assert_eq!(manifest["data_fingerprint"], summary["input_data_hash"]);
    let record_path = PathBuf::from(
        summary["run_record"]
            .as_str()
            .expect("composition summary references RunRecord"),
    );
    let record_payload =
        std::fs::read_to_string(&record_path).expect("composition RunRecord exists");
    let record = qx_spec::RunRecordV2::from_json(&record_payload)
        .expect("composition RunRecord must use the pathless V2 contract");
    assert_eq!(record.run_id, manifest["run_id"].as_str().unwrap());
    assert_eq!(
        record.input_digest,
        manifest["data_fingerprint"].as_str().unwrap()
    );
    assert_eq!(record.status, qx_spec::RunStatus::Completed);
    assert_eq!(record.replay_verdict, qx_spec::ReplayVerdict::Verified);
    assert_eq!(record.artifact_refs.len(), 4);
    for artifact_name in ["summary", "equity", "fills", "run_manifest"] {
        let artifact = record
            .artifact_refs
            .iter()
            .find(|artifact| artifact.name == artifact_name)
            .unwrap_or_else(|| panic!("RunRecord omits {artifact_name}"));
        let location = summary["artifact_locations"][artifact_name]
            .as_str()
            .expect("RunRecord V2 storage location");
        assert!(Path::new(location).is_file());
        let bytes = std::fs::read(location).expect("RunRecord V2 artifact bytes");
        assert_eq!(artifact.byte_len, bytes.len() as u64);
        assert_eq!(artifact.artifact_id, qx_strategy::sha256_hex(&bytes));
    }
    let primary_fingerprint = summary["dataset_quality"]["legs"]["primary"]["manifest_fingerprint"]
        .as_str()
        .expect("primary manifest fingerprint");
    let reference_fingerprint = summary["dataset_quality"]["legs"]["reference"]
        ["manifest_fingerprint"]
        .as_str()
        .expect("reference manifest fingerprint");

    let summary_text = summary_path.to_string_lossy().into_owned();
    let report_xlsx = root.join("multi-leg-report.xlsx");
    let report_xlsx_text = report_xlsx.to_string_lossy().into_owned();
    let (code, _, stderr) = run(&[
        "report",
        &summary_text,
        "--html",
        "--evidence",
        "--xlsx",
        &report_xlsx_text,
    ]);
    assert_eq!(code, 0, "multi-leg report export failed: {stderr}");
    let html_path = summary_path.with_file_name(
        summary_path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .replace(".summary.json", ".report.html"),
    );
    let html = std::fs::read_to_string(html_path).expect("read multi-leg HTML");
    assert!(html.contains("组合腿") && html.contains("primary") && html.contains("reference"));
    assert!(
        html.contains(primary_fingerprint),
        "HTML omits primary manifest identity"
    );
    assert!(
        html.contains(reference_fingerprint),
        "HTML omits reference manifest identity"
    );
    let mut workbook = zip::ZipArchive::new(std::fs::File::open(&report_xlsx).unwrap())
        .expect("multi-leg XLSX is a ZIP workbook");
    let mut workbook_xml = String::new();
    for index in 0..workbook.len() {
        let mut entry = workbook.by_index(index).unwrap();
        if entry.name().ends_with(".xml") {
            entry.read_to_string(&mut workbook_xml).unwrap();
        }
    }
    assert!(
        workbook_xml.contains(primary_fingerprint),
        "XLSX omits primary manifest identity"
    );
    assert!(
        workbook_xml.contains(reference_fingerprint),
        "XLSX omits reference manifest identity"
    );
    assert!(workbook_xml.contains("primary") && workbook_xml.contains("reference"));

    let evidence_path = summary_path.with_file_name(
        summary_path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .replace(".summary.json", ".evidence.json"),
    );
    let evidence: serde_json::Value = serde_json::from_slice(
        &std::fs::read(&evidence_path).expect("read multi-leg EvidenceBundle"),
    )
    .expect("EvidenceBundle is JSON");
    assert_eq!(
        evidence["generation"]["run"]["dataset"]["components"][0]["manifest_fingerprint"],
        primary_fingerprint
    );
    assert_eq!(
        evidence["generation"]["run"]["dataset"]["components"][1]["manifest_fingerprint"],
        reference_fingerprint
    );
    for (index, manifest) in [&primary_manifest, &reference_manifest]
        .into_iter()
        .enumerate()
    {
        let component = &evidence["generation"]["run"]["dataset"]["components"][index];
        let manifest = manifest.canonicalize().unwrap();
        let manifest = manifest.to_string_lossy();
        assert_eq!(
            component["quality_manifest_path"].as_str(),
            Some(manifest.as_ref())
        );
        assert!(component["quality_manifest_sha256"].as_str().is_some());
    }

    let verify = run(&[
        "evidence",
        "verify-run",
        evidence_path.to_str().unwrap(),
        "--recompute",
    ]);
    assert_eq!(
        verify.0, 0,
        "multi-leg offline recompute failed: {}",
        verify.2
    );
    let portable_root = root.join("portable-multi-leg");
    let portable_root_text = portable_root.to_string_lossy().into_owned();
    let export = run(&[
        "evidence",
        "export-run",
        evidence_path.to_str().unwrap(),
        "--output-dir",
        &portable_root_text,
    ]);
    assert_eq!(
        export.0, 0,
        "multi-leg EvidenceBundle export failed: {}",
        export.2
    );
    let portable_evidence = portable_root.join("evidence.json");
    let portable_verify = run(&[
        "evidence",
        "verify-run",
        portable_evidence.to_str().unwrap(),
        "--recompute",
    ]);
    assert_eq!(
        portable_verify.0, 0,
        "portable multi-leg offline recompute failed: {}",
        portable_verify.2
    );

    let alternate_root = root.join("alternate-run");
    let alternate_root_text = alternate_root.to_string_lossy().into_owned();
    let (code, _, stderr) = run(&[
        "backtest",
        "multi-builtin",
        "pairs_arbitrage",
        &primary_text,
        &reference_text,
        "--quantity",
        "2",
        "--root",
        &alternate_root_text,
        "--primary-dataset-manifest",
        &primary_manifest_text,
        "--reference-dataset-manifest",
        &reference_manifest_text,
    ]);
    assert_eq!(code, 0, "alternate multi-leg run failed: {stderr}");
    let alternate_summary = find_composition_summary(&alternate_root);
    let alternate_summary_text = alternate_summary.to_string_lossy().into_owned();
    let comparison_xlsx = root.join("multi-leg-comparison.xlsx");
    let comparison_html = root.join("multi-leg-comparison.html");
    let comparison_xlsx_text = comparison_xlsx.to_string_lossy().into_owned();
    let comparison_html_text = comparison_html.to_string_lossy().into_owned();
    let (code, stdout, stderr) = run(&[
        "report",
        &summary_text,
        "--against",
        &alternate_summary_text,
        "--json",
        "--html",
        "--output",
        &comparison_html_text,
        "--xlsx",
        &comparison_xlsx_text,
    ]);
    assert_eq!(code, 0, "multi-leg same-input comparison failed: {stderr}");
    let comparison: serde_json::Value =
        serde_json::from_str(&stdout).expect("comparison output is JSON");
    assert_eq!(comparison["comparison_kind"], "same_input_backtest_runs");
    assert!(comparison_html.is_file(), "comparison HTML missing");
    assert!(comparison_xlsx.is_file(), "comparison XLSX missing");

    let mut drifted: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&primary_manifest).expect("read primary manifest"))
            .expect("primary manifest is JSON");
    drifted["quality_report"]["missing_intervals"] = serde_json::json!(1);
    let drifted_path = root.join("drifted-primary.dataset-manifest-v2.json");
    std::fs::write(
        &drifted_path,
        serde_json::to_vec_pretty(&drifted).expect("serialize drifted manifest"),
    )
    .expect("write drifted manifest");
    let drifted_text = drifted_path.to_string_lossy().into_owned();
    let rejected_root = root.join("rejected-run");
    let rejected_root_text = rejected_root.to_string_lossy().into_owned();
    let (code, _, stderr) = run(&[
        "backtest",
        "multi-builtin",
        "pairs_arbitrage",
        &primary_text,
        &reference_text,
        "--root",
        &rejected_root_text,
        "--primary-dataset-manifest",
        &drifted_text,
        "--reference-dataset-manifest",
        &reference_manifest_text,
    ]);
    assert_eq!(code, 2, "quality drift must fail closed: {stderr}");
    assert!(!rejected_root.exists(), "rejected run wrote artifacts");

    let relocated_portable_root = root.join("relocated-portable-multi-leg");
    std::fs::rename(&portable_root, &relocated_portable_root)
        .expect("move multi-leg portable evidence package");
    std::fs::remove_dir_all(&source_root)
        .expect("remove original BarFrames, quality manifests and run artifacts");
    let relocated_evidence = relocated_portable_root.join("evidence.json");
    let relocated_verify = run(&[
        "evidence",
        "verify-run",
        relocated_evidence.to_str().unwrap(),
        "--recompute",
    ]);
    assert_eq!(
        relocated_verify.0, 0,
        "relocated multi-leg recompute must not depend on the source workspace: {}",
        relocated_verify.2
    );
}
