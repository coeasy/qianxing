//! Process-level contract for `backtest ccxt-builtin` without an external venue.

use serde_json::{json, Value};
use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("qx-cli integration tests live two levels below the workspace root")
        .to_path_buf()
}

fn case_root() -> PathBuf {
    let path = repo_root()
        .join("target")
        .join(format!("qx-cli-ccxt-builtin-parity-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).expect("create CCXT builtin parity case directory");
    path
}

#[test]
fn ccxt_builtin_process_preserves_fetch_and_backtest_contract() {
    let root = repo_root();
    let case = case_root();
    let python_root = case.join("python");
    let package = python_root.join("qianxing_ccxt");
    std::fs::create_dir_all(&package).expect("create fake CCXT worker package");
    std::fs::write(package.join("__init__.py"), "").expect("write fake package marker");

    let frame_path = root.join("deploy/qianxing.bar-frame.example.json");
    let frame_payload = std::fs::read_to_string(&frame_path).expect("read deterministic BarFrame");
    let request_log = case.join("worker-request.json");
    let worker = format!(
        "import json, sys\nFRAME = json.loads({frame_payload})\nLOG = {request_log}\nfor line in sys.stdin:\n request = json.loads(line)\n with open(LOG, 'w', encoding='utf-8') as stream: json.dump(request, stream)\n print(json.dumps({{'ok': True, 'result': {{'frame': FRAME}}}}, separators=(',', ':')), flush=True)\n",
        frame_payload = serde_json::to_string(&frame_payload).expect("encode fixture for worker"),
        request_log =
            serde_json::to_string(&request_log.to_string_lossy()).expect("encode request log path"),
    );
    std::fs::write(package.join("worker.py"), worker).expect("write deterministic worker");

    let config_path = case.join("ccxt.json");
    std::fs::write(&config_path, json!({"timeout_ms": 2_000}).to_string())
        .expect("write worker config");
    let mut runtime: Value = serde_json::from_slice(
        &std::fs::read(root.join("deploy/qianxing.runtime.strategy-backtest.example.json"))
            .expect("read strategy backtest runtime template"),
    )
    .expect("runtime template is JSON");
    runtime["storage"]["data_dir"] = json!(case
        .strip_prefix(&root)
        .expect("case directory is inside the workspace")
        .join("data")
        .to_string_lossy());
    let runtime_path = case.join("runtime.json");
    std::fs::write(
        &runtime_path,
        serde_json::to_vec_pretty(&runtime).expect("encode isolated runtime"),
    )
    .expect("write isolated runtime");
    let mut python_paths = vec![python_root];
    if let Some(existing) = env::var_os("PYTHONPATH") {
        python_paths.extend(env::split_paths(&existing));
    }
    let python_path = env::join_paths(python_paths).expect("compose fake worker PYTHONPATH");
    let python = env::var("QX_PYTHON").unwrap_or_else(|_| "python".to_string());
    let output = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .current_dir(&root)
        .env("QX_PYTHON", python)
        .env("PYTHONPATH", python_path)
        .args(["backtest", "ccxt-builtin"])
        .arg(&config_path)
        .args(["sma_cross", "BTCUSDT.BINANCE", "1000", "70000", "1s"])
        .arg("--config")
        .arg(&runtime_path)
        .output()
        .expect("run backtest ccxt-builtin process");

    assert_eq!(
        output.status.code(),
        Some(0),
        "CCXT builtin must complete using the fixture worker: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let metrics = stdout
        .lines()
        .find(|line| line.starts_with("[Strategy · Backtest]"))
        .unwrap_or_else(|| {
            panic!("CCXT builtin must print the persistent backtest metrics: {stdout}")
        });
    for field in [
        "strategy=strategy-backtest",
        "instrument=BTCUSDT.BINANCE",
        "bars=70",
        "fills=",
        "result_hash=55a91efd8bfb7254",
    ] {
        assert!(metrics.contains(field), "missing {field} in {metrics}");
    }
    assert!(
        stdout.contains("[Data · Quality]"),
        "quality binding missing: {stdout}"
    );
    let artifact_line = stdout
        .lines()
        .find(|line| line.starts_with("[Artifacts] summary="))
        .expect("CCXT builtin must print persistent artifact paths");
    let summary_path = root.join(
        artifact_line
            .strip_prefix("[Artifacts] summary=")
            .and_then(|value| value.split_whitespace().next())
            .map(PathBuf::from)
            .expect("summary path is present"),
    );
    let summary_payload = std::fs::read(&summary_path).unwrap_or_else(|error| {
        panic!(
            "CCXT builtin summary is persisted at {}: {error}; {artifact_line}",
            summary_path.display()
        )
    });
    let summary: Value = serde_json::from_slice(&summary_payload).expect("summary is JSON");
    assert!(
        summary["input"]["path"]
            .as_str()
            .is_some_and(|path| path.contains("datasets") && path.contains("ccxt")),
        "input must point to the immutable normalized CCXT dataset cache: {}",
        summary["input"]["path"]
    );
    let quality = &summary["dataset_quality"];
    assert_eq!(quality["quality_report"]["interval_ms"], 1_000);
    assert_eq!(quality["quality_report"]["row_count"], 70);
    assert!(quality["manifest_fingerprint"].as_str().is_some());
    assert!(quality["manifest_path"]
        .as_str()
        .is_some_and(|path| { root.join(path).is_file() }));
    assert!(quality["manifest"]["source_lineage"]["primary_source"]
        .as_str()
        .is_some_and(|source| source.contains("requested_range=1000..70000")));

    let xlsx_path = case.join("ccxt.report.xlsx");
    let report = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .current_dir(&root)
        .args(["report"])
        .arg(&summary_path)
        .args(["--json", "--html", "--evidence", "--xlsx"])
        .arg(&xlsx_path)
        .output()
        .expect("render reports from persistent CCXT summary");
    assert_eq!(
        report.status.code(),
        Some(0),
        "CCXT summary must use the shared JSON/HTML/XLSX/EvidenceBundle report path: {}",
        String::from_utf8_lossy(&report.stderr)
    );
    let report_json: Value = serde_json::from_slice(&report.stdout)
        .expect("CCXT report --json emits one structured report");
    assert_eq!(
        report_json["summary"]["dataset_quality"]["manifest_fingerprint"],
        quality["manifest_fingerprint"],
        "report JSON must preserve the verified CCXT input lineage"
    );
    let html_path = report_json["generated_artifacts"]["html"]
        .as_str()
        .map(PathBuf::from)
        .expect("report JSON must name the generated HTML report");
    assert!(
        root.join(html_path).is_file(),
        "HTML report must be persisted"
    );
    assert!(
        std::fs::metadata(&xlsx_path).is_ok_and(|metadata| metadata.len() > 0),
        "XLSX report must be persisted"
    );
    let evidence_path = report_json["run_evidence"]["path"]
        .as_str()
        .map(PathBuf::from)
        .expect("report must emit the run EvidenceBundle path");
    let evidence_bundle: Value = serde_json::from_slice(
        &std::fs::read(&evidence_path).expect("report must persist a run EvidenceBundle"),
    )
    .expect("run EvidenceBundle is JSON");
    assert!(
        evidence_bundle["generation"]["run"]["dataset"]["quality_manifest_path"]
            .as_str()
            .is_some()
    );
    assert!(
        evidence_bundle["generation"]["run"]["dataset"]["quality_manifest_sha256"]
            .as_str()
            .is_some()
    );
    let recompute_temp = case.join("recompute-temp");
    std::fs::create_dir_all(&recompute_temp).expect("create workspace-local recompute temp root");
    let verify = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .current_dir(&root)
        .env("TEMP", &recompute_temp)
        .env("TMP", &recompute_temp)
        .args(["evidence", "verify-run"])
        .arg(&evidence_path)
        .arg("--recompute")
        .output()
        .expect("recompute CCXT run evidence");
    assert_eq!(
        verify.status.code(),
        Some(0),
        "CCXT EvidenceBundle must verify and recompute offline: {}",
        String::from_utf8_lossy(&verify.stderr)
    );
    let export_dir = case.join("portable-run");
    let runtime_config_path = root.join(
        summary["runtime_config"]
            .as_str()
            .expect("CCXT summary names its effective runtime snapshot"),
    );
    let export = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .current_dir(&root)
        .args(["evidence", "export-run"])
        .arg(&evidence_path)
        .arg("--output-dir")
        .arg(&export_dir)
        .arg("--runtime-config")
        .arg(&runtime_config_path)
        .output()
        .expect("export a portable CCXT EvidenceBundle");
    assert_eq!(
        export.status.code(),
        Some(0),
        "CCXT EvidenceBundle export must carry the quality manifest: {}",
        String::from_utf8_lossy(&export.stderr)
    );
    let portable_verify = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .current_dir(&root)
        .env("TEMP", &recompute_temp)
        .env("TMP", &recompute_temp)
        .args(["evidence", "verify-run"])
        .arg(export_dir.join("evidence.json"))
        .arg("--recompute")
        .output()
        .expect("recompute exported CCXT EvidenceBundle");
    assert_eq!(
        portable_verify.status.code(),
        Some(0),
        "portable CCXT EvidenceBundle must remain independently recomputable: {}",
        String::from_utf8_lossy(&portable_verify.stderr)
    );

    let request: Value = serde_json::from_slice(
        &std::fs::read(&request_log).expect("fake worker must record its request"),
    )
    .expect("worker request is JSON");
    assert_eq!(
        request,
        json!({
            "op": "fetch_ohlcv",
            "instrument": "BTCUSDT.BINANCE",
            "timeframe": "1s",
            "start_ms": 1000,
            "end_ms": 70000,
        }),
        "CCXT adapter must preserve the requested fetch window and instrument"
    );
    let _ = std::fs::remove_dir_all(case);
}
