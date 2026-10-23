//! Dataset-quality CLI contracts split from the main A-share report journey.

use super::{output_text, run, write_json};
use serde_json::{json, Value};
use std::io::Read;
use std::path::{Path, PathBuf};

pub(super) fn create_manifest(root: &Path, frame: &Path) -> PathBuf {
    let manifest = root.join("bars.dataset-manifest-v2.json");
    let args = vec![
        "data-validate".into(),
        frame.to_string_lossy().into_owned(),
        "strategy-bars:000001.SZSE".into(),
        qx_data::BARFRAME_DATASET_VERSION.into(),
        "60000".into(),
        manifest.to_string_lossy().into_owned(),
    ];
    let output = run(root, &args);
    let (stdout, stderr) = output_text(&output);
    assert_eq!(
        output.status.code(),
        Some(0),
        "A-share fixture quality manifest generation failed: {stderr}\n{stdout}"
    );
    manifest
}

pub(super) fn assert_summary(summary: &Value, interval_ms: u64, row_count: u64) {
    assert!(
        summary["execution_attempts"].as_array().is_some(),
        "summary must include per-order execution records"
    );
    assert_eq!(
        summary["execution_attempt_count"].as_u64(),
        summary["execution_attempts"]
            .as_array()
            .map(|attempts| attempts.len() as u64),
        "summary must expose a count matching the per-order execution records"
    );
    assert_eq!(
        summary.pointer("/dataset_quality/quality_report/interval_ms"),
        Some(&json!(interval_ms)),
        "the run summary must carry the verified data interval"
    );
    assert_eq!(
        summary.pointer("/dataset_quality/quality_report/row_count"),
        Some(&json!(row_count)),
        "the run summary must carry the verified source row count"
    );
}

pub(super) fn assert_html_and_evidence_quality(report: &Value) {
    let html_path = Path::new(report["generated_artifacts"]["html"].as_str().unwrap());
    let html = std::fs::read_to_string(html_path).expect("read A-share report HTML");
    assert!(
        html.contains("lang=\"zh-CN\"")
            && html.contains("name=\"viewport\"")
            && html.contains("<section class=\"charts\">")
            && html.matches("<svg").count() >= 3
            && html.contains("<style>")
            && html.contains("<script>")
            && !html.contains("<script src=")
            && html.contains("position_raw")
            && !html.contains("src=\"http"),
        "single-run HTML must keep inline interactive charts and raw position data without external resources"
    );
    assert!(
        html.contains("间隔 60000 ms · 缺口 0") && html.contains("24 行"),
        "HTML must render the same verified quality report as the summary"
    );
    let evidence_path = Path::new(report["run_evidence"]["path"].as_str().unwrap());
    let evidence: Value =
        serde_json::from_slice(&std::fs::read(evidence_path).expect("read generated run evidence"))
            .expect("run evidence artifact is JSON");
    assert_eq!(
        evidence.pointer("/generation/run/dataset/quality_report/interval_ms"),
        Some(&json!(60_000)),
        "EvidenceBundle must preserve the verified quality interval"
    );
}

pub(super) fn assert_xlsx_quality(path: &Path) {
    let workbook_file = std::fs::File::open(path).expect("A-share XLSX exists");
    let mut workbook = zip::ZipArchive::new(workbook_file).expect("A-share XLSX is valid ZIP");
    let mut overview = String::new();
    workbook
        .by_name("xl/worksheets/sheet1.xml")
        .expect("XLSX Overview sheet")
        .read_to_string(&mut overview)
        .expect("read XLSX Overview sheet");
    assert!(
        overview.contains("Dataset interval (ms)")
            && overview.contains("Dataset missing intervals")
            && overview.contains("60000"),
        "XLSX Overview must carry the same verified quality report"
    );
    let mut execution = String::new();
    workbook
        .by_name("xl/worksheets/sheet5.xml")
        .expect("XLSX Execution sheet")
        .read_to_string(&mut execution)
        .expect("read XLSX Execution sheet");
    assert!(
        execution.contains("requested_qty_raw") && execution.contains("reason"),
        "XLSX must expose per-order execution decisions"
    );
}

pub(super) fn assert_drift_rejected(
    root: &Path,
    quality_manifest: &Path,
    runtime: &Path,
    frame: &Path,
    spec: &Path,
) {
    let mut invalid_quality: Value = serde_json::from_slice(
        &std::fs::read(quality_manifest).expect("read validated quality manifest"),
    )
    .expect("quality manifest is JSON");
    invalid_quality["quality_report"]["missing_intervals"] = json!(1);
    let invalid_quality_path = root.join("invalid-quality.dataset-manifest-v2.json");
    write_json(&invalid_quality_path, &invalid_quality);

    let mut config: Value = serde_json::from_slice(&std::fs::read(runtime).expect("read runtime"))
        .expect("runtime config is JSON");
    let invalid_data_root = root.join("data/invalid-quality");
    config["storage"]["data_dir"] = json!(invalid_data_root.to_string_lossy());
    let invalid_runtime = root.join("invalid-quality.runtime.json");
    write_json(&invalid_runtime, &config);
    let args = vec![
        "strategy".into(),
        "backtest".into(),
        invalid_runtime.to_string_lossy().into_owned(),
        frame.to_string_lossy().into_owned(),
        spec.to_string_lossy().into_owned(),
        "--dataset-manifest".into(),
        invalid_quality_path.to_string_lossy().into_owned(),
    ];
    let rejected = run(root, &args);
    let (_, stderr) = output_text(&rejected);
    assert_eq!(
        rejected.status.code(),
        Some(2),
        "quality drift must fail closed"
    );
    assert!(
        stderr.contains("missing_intervals 清单=1 重算=0"),
        "diagnostic must expose declared and recomputed quality: {stderr}"
    );
    assert!(
        !invalid_data_root.exists(),
        "quality mismatch must be rejected before creating backtest artifacts"
    );
}

pub(super) fn assert_builtin_manifest_binding(
    root: &Path,
    quality_manifest: &Path,
    runtime: &Path,
    frame: &Path,
    spec: &Path,
) {
    let mut builtin_config: Value = serde_json::from_slice(
        &std::fs::read(runtime).expect("read builtin runtime configuration"),
    )
    .expect("builtin runtime configuration is JSON");
    builtin_config["storage"]["data_dir"] = json!(root.join("data/builtin").to_string_lossy());
    let builtin_runtime = root.join("builtin.runtime.json");
    write_json(&builtin_runtime, &builtin_config);
    let args = vec![
        "backtest".into(),
        "builtin".into(),
        "ema_cross".into(),
        frame.to_string_lossy().into_owned(),
        spec.to_string_lossy().into_owned(),
        "100".into(),
        "--config".into(),
        builtin_runtime.to_string_lossy().into_owned(),
        "--dataset-manifest".into(),
        quality_manifest.to_string_lossy().into_owned(),
    ];
    let accepted = run(root, &args);
    let (stdout, stderr) = output_text(&accepted);
    assert_eq!(
        accepted.status.code(),
        Some(0),
        "A-share builtin must accept its verified quality manifest: {stderr}\n{stdout}"
    );
    assert!(
        stdout.contains("[Data · Quality]")
            && stdout.contains("rows=24 interval_ms=60000 missing_intervals=0"),
        "builtin output must expose the quality report it verified: {stdout}"
    );
    let summary_path = stdout
        .lines()
        .find_map(|line| {
            line.strip_prefix("[Artifacts] summary=")
                .and_then(|value| value.split_once(" equity=").map(|(path, _)| path))
        })
        .map(PathBuf::from)
        .expect("builtin prints its durable summary path");
    let summary: Value = serde_json::from_slice(
        &std::fs::read(&summary_path).expect("builtin summary is persisted"),
    )
    .expect("builtin summary is JSON");
    assert_summary(&summary, 60_000, 24);
    let report = run(
        root,
        &[
            "report".into(),
            summary_path.to_string_lossy().into_owned(),
            "--json".into(),
            "--html".into(),
            "--evidence".into(),
        ],
    );
    let (report_stdout, report_stderr) = output_text(&report);
    assert_eq!(
        report.status.code(),
        Some(0),
        "builtin summary must enter the report pipeline: {report_stderr}\n{report_stdout}"
    );
    let report: Value =
        serde_json::from_str(&report_stdout).expect("builtin report --json emits one JSON value");
    assert_html_and_evidence_quality(&report);
    let xlsx = root.join("builtin-report.xlsx");
    let xlsx_report = run(
        root,
        &[
            "report".into(),
            summary_path.to_string_lossy().into_owned(),
            "--xlsx".into(),
            xlsx.to_string_lossy().into_owned(),
        ],
    );
    assert_eq!(
        xlsx_report.status.code(),
        Some(0),
        "builtin summary must export XLSX: {}",
        output_text(&xlsx_report).1
    );
    assert_xlsx_quality(&xlsx);

    let mut invalid: Value = serde_json::from_slice(
        &std::fs::read(quality_manifest).expect("read valid quality manifest"),
    )
    .expect("quality manifest is JSON");
    invalid["quality_report"]["missing_intervals"] = json!(1);
    let invalid_path = root.join("invalid-builtin-quality.dataset-manifest-v2.json");
    write_json(&invalid_path, &invalid);
    let mut invalid_args = args;
    let path_arg = invalid_args.len() - 1;
    invalid_args[path_arg] = invalid_path.to_string_lossy().into_owned();
    let rejected = run(root, &invalid_args);
    let (_, stderr) = output_text(&rejected);
    assert_eq!(
        rejected.status.code(),
        Some(2),
        "builtin must reject quality drift before executing: {stderr}"
    );
    assert!(
        stderr.contains("missing_intervals 清单=1 重算=0"),
        "builtin must report declared and recomputed quality: {stderr}"
    );
}
