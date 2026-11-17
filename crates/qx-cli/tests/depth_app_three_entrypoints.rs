//! Cross-entry contract for the shared Rust Tick/OrderBook application workflow.

use serde_json::{json, Value};
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::Command;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/qx-cli has a repository root")
        .to_path_buf()
}

fn scratch(label: &str) -> PathBuf {
    let root = repo_root().join("target").join(format!(
        "qx-app-depth-contract/{label}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).expect("create test directory");
    root
}

fn python_depth(spec: &str) -> Option<Value> {
    let root = repo_root();
    let python = std::env::var("QX_PYTHON").unwrap_or_else(|_| {
        if cfg!(windows) {
            root.join("python/.venv/Scripts/python.exe")
                .to_string_lossy()
                .into_owned()
        } else {
            root.join("python/.venv/bin/python")
                .to_string_lossy()
                .into_owned()
        }
    });
    if !Path::new(&python).is_file() {
        return None;
    }
    let script = format!(
        "import json, sys\nsys.path.insert(0, {python_dir:?})\nfrom qianxing_bridge import app, native\nif not native.app_available(): sys.exit(3)\nprint(json.dumps(app.run_depth_backtest({spec:?})))\n",
        python_dir = root.join("python").to_string_lossy(),
    );
    let output = Command::new(python)
        .arg("-c")
        .arg(script)
        .current_dir(root.join("python"))
        .output()
        .expect("start Python SDK");
    if output.status.code() == Some(3) {
        return None;
    }
    assert_eq!(
        output.status.code(),
        Some(0),
        "Python depth call failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Some(serde_json::from_slice(&output.stdout).expect("Python SDK returns JSON"))
}

#[test]
fn l1_tick_run_has_the_same_hash_through_cli_http_and_python() {
    let root = scratch("l1");
    let repository = repo_root();
    let data_dir = repository
        .join("target")
        .join(format!("qx-app-depth-contract-{}", std::process::id()));
    std::fs::create_dir_all(&data_dir).expect("create API-contained data directory");
    let snapshots = (0..48)
        .map(|index| {
            let phase = index % 20;
            let mid = if phase < 10 {
                100 + phase * 2
            } else {
                100 + (20 - phase) * 2
            };
            json!({
                "instrument": {"symbol": "BTCUSDT", "venue": "BINANCE"},
                "ts": 1000 + index as u64 * 1000,
                "sequence": index as u64 + 1,
                "bids": [{"price": (mid - 1) as i128 * 1_000_000_000_i128, "qty": 100_000_000_000_i128}],
                "asks": [{"price": (mid + 1) as i128 * 1_000_000_000_i128, "qty": 100_000_000_000_i128}]
            })
        })
        .collect::<Vec<_>>();
    let data_path = data_dir.join("ticks.json");
    std::fs::write(
        &data_path,
        json!({
            "schema_version": 1,
            "source": "depth-three-entrypoint-contract",
            "instrument": {"symbol": "BTCUSDT", "venue": "BINANCE"},
            "snapshots": snapshots
        })
        .to_string(),
    )
    .expect("write L1 frame");
    std::env::set_var("QX_API_DATA_ROOT", &repository);
    std::env::set_var("QX_API_ARTIFACT_ROOT", root.join("http-artifacts"));

    let spec = json!({
        "schema_version": 1,
        "run_id": "depth-cross-entrypoint",
        "depth_path": data_path.to_string_lossy(),
        "tier": "l1",
        "settlement_currency": "USDT",
        "initial_cash_raw": 1_000_000_000_000_000_i128,
        "fee_bps": 0,
        "latency_snapshots": 0,
        "queue_position_bps": 0,
        "market_impact_bps": 0,
        "output_dir": root.join("cli-artifacts").to_string_lossy(),
        "strategy": {
            "kind": "sma_cross",
            "strategy_id": "depth-contract-sma",
            "quantity_raw": 1_000_000_000_i128,
            "fast_window": 2,
            "slow_window": 3
        }
    })
    .to_string();
    let spec_path = root.join("depth-spec.json");
    std::fs::write(&spec_path, &spec).expect("write spec");
    let cli_output = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args(["app", "depth-backtest"])
        .arg(&spec_path)
        .output()
        .expect("start CLI depth workflow");
    assert_eq!(
        cli_output.status.code(),
        Some(0),
        "CLI failed: {}",
        String::from_utf8_lossy(&cli_output.stderr)
    );
    let cli: Value = serde_json::from_slice(&cli_output.stdout).expect("CLI JSON result");

    let api = qx_api::ApiService::new(qx_api::ApiState::default());
    let response = api.handle("POST", "/app/depth-backtest", &spec, 0);
    let http: Value = serde_json::from_str(&response.body).expect("HTTP JSON result");
    assert_eq!(response.status, 200, "HTTP failed: {http}");
    assert_eq!(cli["result_hash"], http["result_hash"]);
    assert_eq!(cli["data_fingerprint"], http["data_fingerprint"]);
    assert_eq!(cli["fills"], http["fills"]);

    let outcome_path = root.join("depth-outcome.json");
    std::fs::write(&outcome_path, &cli_output.stdout).expect("write depth outcome");
    let verify_output = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args(["app", "verify-depth"])
        .arg(&outcome_path)
        .output()
        .expect("start CLI depth artifact verification");
    assert_eq!(verify_output.status.code(), Some(0));
    let cli_verification: Value =
        serde_json::from_slice(&verify_output.stdout).expect("verification JSON");
    assert_eq!(cli_verification["verified"], true);

    let cli_report_output = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args(["app", "read-depth-report"])
        .arg(&outcome_path)
        .output()
        .expect("start CLI shared depth report reader");
    assert_eq!(
        cli_report_output.status.code(),
        Some(0),
        "CLI shared depth report failed: {}",
        String::from_utf8_lossy(&cli_report_output.stderr)
    );
    let cli_report: Value =
        serde_json::from_slice(&cli_report_output.stdout).expect("CLI shared depth report JSON");
    assert_eq!(cli_report["schema_version"], 1);
    assert_eq!(cli_report["run_id"], cli["run_id"]);
    assert_eq!(cli_report["verification"]["verified"], true);

    let api_outcome = serde_json::to_string(&http).expect("serialize API outcome");
    let api_verification = api.handle("POST", "/app/verify-depth", &api_outcome, 0);
    let http_verification: Value =
        serde_json::from_str(&api_verification.body).expect("HTTP verification JSON");
    assert_eq!(api_verification.status, 200);
    assert_eq!(http_verification["verified"], true);
    let mut escaped = http.clone();
    escaped["artifacts"]["fills"] = Value::String("../outside.csv".into());
    let escaped_body = serde_json::to_string(&escaped).expect("serialize escaped outcome");
    let refused = api.handle("POST", "/app/verify-depth", &escaped_body, 0);
    assert_eq!(
        refused.status, 400,
        "artifact paths must stay in the server artifact root"
    );

    let mut python_spec: Value = serde_json::from_str(&spec).expect("decode depth spec");
    python_spec["output_dir"] = json!(root.join("python-artifacts").to_string_lossy());
    if let Some(python) = python_depth(&python_spec.to_string()) {
        assert_eq!(cli["result_hash"], python["result_hash"]);
        assert_eq!(cli["data_fingerprint"], python["data_fingerprint"]);
        assert_eq!(cli["fills"], python["fills"]);
        let bridge = python_verify_depth(&cli_output.stdout);
        assert_eq!(bridge["verification"]["verified"], true);
        assert_eq!(
            bridge["report"], cli_report,
            "Python and CLI share report DTO for the same outcome"
        );
    } else {
        eprintln!("skip Python leg: qx-python extension is not built or is stale");
    }

    let summary_path = PathBuf::from(cli["artifacts"]["summary"].as_str().unwrap());
    let xlsx_path = root.join("interactive-depth-report.xlsx");
    let interactive_report = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args(["report"])
        .arg(&summary_path)
        .args(["--json", "--html", "--evidence", "--xlsx"])
        .arg(&xlsx_path)
        .output()
        .expect("start interactive depth report");
    assert_eq!(
        interactive_report.status.code(),
        Some(0),
        "interactive Depth report failed: {}",
        String::from_utf8_lossy(&interactive_report.stderr)
    );
    let interactive: Value =
        serde_json::from_slice(&interactive_report.stdout).expect("interactive Depth report JSON");
    assert_eq!(interactive["run_kind"], "depth");
    assert_eq!(interactive["verification"]["verified"], true);
    assert_eq!(interactive["summary"]["run_id"], cli["run_id"]);
    assert!(Path::new(interactive["generated_artifacts"]["html"].as_str().unwrap()).is_file());
    assert!(Path::new(interactive["run_evidence"]["path"].as_str().unwrap()).is_file());
    assert!(xlsx_path.is_file());

    let mut tampered_summary: Value =
        serde_json::from_slice(&std::fs::read(&summary_path).unwrap()).unwrap();
    tampered_summary["return_bps"] = json!(cli["return_bps"].as_i64().unwrap() + 1);
    std::fs::write(
        &summary_path,
        serde_json::to_vec(&tampered_summary).unwrap(),
    )
    .unwrap();
    let rejected = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args(["report"])
        .arg(&summary_path)
        .arg("--json")
        .output()
        .expect("verify tampered interactive Depth report");
    assert_ne!(rejected.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("深度研究报告复核失败"));
}

#[test]
fn l2_market_making_spec_has_one_hash_through_cli_http_and_python() {
    let root = scratch("l2-market-making");
    let repository = repo_root();
    let data_dir = repository
        .join("target")
        .join(format!("qx-app-depth-mm-contract-{}", std::process::id()));
    std::fs::create_dir_all(&data_dir).expect("create API-contained data directory");
    let snapshots = (0..48)
        .map(|index| {
            let phase = index % 20;
            let mid = if phase < 10 {
                100 + phase * 2
            } else {
                100 + (20 - phase) * 2
            };
            json!({
                "instrument": {"symbol": "BTCUSDT", "venue": "BINANCE"},
                "ts": 1000 + index as u64 * 1000,
                "sequence": index as u64 + 1,
                "bids": [
                    {"price": (mid - 1) as i128 * 1_000_000_000_i128, "qty": 100_000_000_000_i128},
                    {"price": (mid - 2) as i128 * 1_000_000_000_i128, "qty": 100_000_000_000_i128}
                ],
                "asks": [
                    {"price": (mid + 1) as i128 * 1_000_000_000_i128, "qty": 100_000_000_000_i128},
                    {"price": (mid + 2) as i128 * 1_000_000_000_i128, "qty": 100_000_000_000_i128}
                ]
            })
        })
        .collect::<Vec<_>>();
    let data_path = data_dir.join("market-making-depth.json");
    std::fs::write(
        &data_path,
        json!({
            "schema_version": 1,
            "source": "depth-market-making-entrypoint-contract",
            "instrument": {"symbol": "BTCUSDT", "venue": "BINANCE"},
            "snapshots": snapshots
        })
        .to_string(),
    )
    .expect("write L2 frame");
    std::env::set_var("QX_API_DATA_ROOT", &repository);
    std::env::set_var("QX_API_ARTIFACT_ROOT", root.join("http-artifacts"));
    let spec = json!({
        "schema_version": 1,
        "run_id": "depth-market-making-cross-entrypoint",
        "depth_path": data_path.to_string_lossy(),
        "tier": "l2",
        "settlement_currency": "USDT",
        "initial_cash_raw": 1_000_000_000_000_000_i128,
        "fee_bps": 10,
        "latency_snapshots": 0,
        "queue_position_bps": 0,
        "market_impact_bps": 0,
        "output_dir": root.join("cli-artifacts").to_string_lossy(),
        "strategy": {
            "kind": "sma_cross", "strategy_id": "depth-market-making-cross",
            "quantity_raw": 1_000_000_000_i128, "fast_window": 2, "slow_window": 3
        },
        "market_making": {
            "spread_bps": 100, "quote_qty_raw": 1_000_000_000_i128,
            "max_inventory_raw": 5_000_000_000_i128,
            "tick_size_raw": 10_000_000_i128, "inventory_skew_bps": 50
        }
    })
    .to_string();
    let spec_path = root.join("market-making-spec.json");
    std::fs::write(&spec_path, &spec).expect("write market-making spec");
    let cli_output = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args(["app", "depth-backtest"])
        .arg(&spec_path)
        .output()
        .expect("start CLI market-making workflow");
    assert_eq!(
        cli_output.status.code(),
        Some(0),
        "CLI failed: {}",
        String::from_utf8_lossy(&cli_output.stderr)
    );
    let cli: Value = serde_json::from_slice(&cli_output.stdout).expect("CLI JSON");
    assert!(cli["fills"].as_u64().unwrap_or_default() > 0);

    let api = qx_api::ApiService::new(qx_api::ApiState::default());
    let response = api.handle("POST", "/app/depth-backtest", &spec, 0);
    let http: Value = serde_json::from_str(&response.body).expect("HTTP JSON");
    assert_eq!(response.status, 200, "HTTP failed: {http}");
    assert_eq!(cli["result_hash"], http["result_hash"]);
    assert_eq!(cli["fills"], http["fills"]);

    let mut python_spec: Value = serde_json::from_str(&spec).unwrap();
    python_spec["output_dir"] = json!(root.join("python-artifacts").to_string_lossy());
    if let Some(python) = python_depth(&python_spec.to_string()) {
        assert_eq!(cli["result_hash"], python["result_hash"]);
        assert_eq!(cli["fills"], python["fills"]);
    } else {
        eprintln!("skip Python leg: qx-python extension is not built or is stale");
    }
}

fn python_verify_depth(outcome: &[u8]) -> Value {
    let root = repo_root();
    let python = std::env::var("QX_PYTHON").unwrap_or_else(|_| {
        if cfg!(windows) {
            root.join("python/.venv/Scripts/python.exe")
                .to_string_lossy()
                .into_owned()
        } else {
            root.join("python/.venv/bin/python")
                .to_string_lossy()
                .into_owned()
        }
    });
    let script = format!(
        r#"
import json, sys
sys.path.insert(0, {python_dir:?})
from qianxing import DepthBacktestOutcome, read_depth_run_report_v1, verify_depth_run
outcome = DepthBacktestOutcome.from_dict(json.loads(sys.stdin.read()))
print(json.dumps({{"verification": verify_depth_run(outcome).__dict__, "report": read_depth_run_report_v1(outcome)}}))
"#,
        python_dir = root.join("python").to_string_lossy(),
    );
    let mut child = Command::new(python)
        .arg("-c")
        .arg(script)
        .current_dir(root.join("python"))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("start Python depth verification");
    use std::io::Write;
    child
        .stdin
        .take()
        .expect("python stdin")
        .write_all(outcome)
        .expect("write outcome");
    let result = child
        .wait_with_output()
        .expect("wait for Python depth verification");
    assert!(
        result.status.success(),
        "Python verification failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    serde_json::from_slice(&result.stdout).expect("Python verification JSON")
}

#[test]
fn multi_account_depth_backtest_and_report_reader_work_through_cli() {
    let root = scratch("multi-account-depth-report");
    let repository = repo_root();
    let fixture = repository.join("deploy/qianxing.depth-frame.l1.example.json");
    let source_depth = root.join("source-depth.json");
    std::fs::copy(&fixture, &source_depth).expect("copy source depth frame");
    let source_quality = root.join("source-depth-quality.json");
    let quality_output = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .arg("data-validate-depth")
        .arg(&source_depth)
        .args(["--tier", "l1", "1000"])
        .arg(&source_quality)
        .output()
        .expect("create depth quality manifest");
    assert_eq!(
        quality_output.status.code(),
        Some(0),
        "depth quality manifest generation failed: {}",
        String::from_utf8_lossy(&quality_output.stderr)
    );
    let member = |account: &str, run_id: &str| {
        json!({
            "schema_version": 1,
            "run_id": run_id,
            "account_id": account,
            "depth_path": source_depth,
            "quality_manifest_path": source_quality,
            "tier": "l1",
            "settlement_currency": "USDT",
            "initial_cash_raw": 1_000_000_000_000_000_i128,
            "fee_bps": 0,
            "latency_snapshots": 0,
            "queue_position_bps": 0,
            "market_impact_bps": 0,
            "output_dir": root.join(account),
            "strategy": {
                "kind": "sma_cross",
                "strategy_id": format!("{account}-depth-sma"),
                "quantity_raw": 1_000_000_000_i128,
                "fast_window": 2,
                "slow_window": 3
            }
        })
    };
    let spec_path = root.join("multi-account-depth-spec.json");
    std::fs::write(
        &spec_path,
        serde_json::to_vec(&json!({
            "schema_version": 1,
            "batch_id": "cli-depth-multi-account-report",
            "runs": [member("depth-a", "depth-report-a"), member("depth-b", "depth-report-b")]
        }))
        .expect("serialize multi-account Depth spec"),
    )
    .expect("write multi-account Depth spec");
    let run_output = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args(["app", "multi-account-depth-backtest"])
        .arg(&spec_path)
        .output()
        .expect("start multi-account Depth CLI backtest");
    assert_eq!(
        run_output.status.code(),
        Some(0),
        "multi-account Depth CLI failed: {}",
        String::from_utf8_lossy(&run_output.stderr)
    );
    let outcome: Value =
        serde_json::from_slice(&run_output.stdout).expect("multi-account Depth CLI outcome JSON");
    assert_eq!(outcome["result_kind"], "multi_account_depth");
    assert_eq!(outcome["summary"]["accounts_succeeded"], 2);

    let outcome_path = root.join("multi-account-depth-outcome.json");
    std::fs::write(&outcome_path, &run_output.stdout).expect("save multi-account Depth result");
    let report_output = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args(["app", "read-multi-account-report"])
        .arg(&outcome_path)
        .output()
        .expect("start shared multi-account Depth CLI report reader");
    assert_eq!(
        report_output.status.code(),
        Some(0),
        "multi-account Depth report reader failed: {}",
        String::from_utf8_lossy(&report_output.stderr)
    );
    let report: Value =
        serde_json::from_slice(&report_output.stdout).expect("multi-account Depth report DTO JSON");
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["run_id"], "cli-depth-multi-account-report");
    assert_eq!(report["verification"]["verified"], true);
    assert_eq!(report["summary"]["accounts_succeeded"], 2);
    assert_eq!(report["accounts"].as_array().unwrap().len(), 2);

    let rich_html = root.join("multi-account-depth-report.html");
    let rich_xlsx = root.join("multi-account-depth-report.xlsx");
    let rich_output = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args(["report"])
        .arg(&outcome_path)
        .args(["--json", "--html", "--output"])
        .arg(&rich_html)
        .args(["--xlsx"])
        .arg(&rich_xlsx)
        .arg("--evidence")
        .output()
        .expect("start rich multi-account Depth report");
    assert_eq!(
        rich_output.status.code(),
        Some(0),
        "multi-account Depth rich report failed: {}",
        String::from_utf8_lossy(&rich_output.stderr)
    );
    let rich_report: Value =
        serde_json::from_slice(&rich_output.stdout).expect("multi-account Depth rich report JSON");
    assert_eq!(
        rich_report["report"]["report_kind"],
        "multi_account_depth_backtest"
    );
    assert_eq!(rich_report["report"]["accounts"][0]["tier"], "l1");
    assert!(
        rich_report["report"]["accounts"][0]["snapshots"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert_eq!(
        rich_report["report"]["accounts"][0]["event_rows_total"].as_u64(),
        Some(
            rich_report["report"]["accounts"][0]["event_rows"]
                .as_array()
                .unwrap()
                .len() as u64
        )
    );
    assert!(rich_html.is_file() && rich_xlsx.is_file());
    assert!(std::fs::read_to_string(&rich_html)
        .unwrap()
        .contains("深度输入"));
    let mut workbook = zip::ZipArchive::new(
        std::fs::File::open(&rich_xlsx).expect("open multi-account Depth workbook"),
    )
    .expect("read multi-account Depth workbook");
    let mut account_sheet = String::new();
    workbook
        .by_name("xl/worksheets/sheet2.xml")
        .expect("find account worksheet")
        .read_to_string(&mut account_sheet)
        .expect("read account worksheet");
    assert!(account_sheet.contains("<t xml:space=\"preserve\">tier</t>"));
    assert!(account_sheet.contains("<t xml:space=\"preserve\">snapshots</t>"));

    let mut legacy_spec = qx_app::MultiAccountDepthBacktestSpec::from_json(
        &std::fs::read_to_string(&spec_path).expect("read original depth spec"),
    )
    .expect("parse original depth spec");
    legacy_spec
        .runs
        .sort_by(|left, right| left.account_id.cmp(&right.account_id));
    for member in &mut legacy_spec.runs {
        member.depth_path.clear();
        member.output_dir.clear();
    }
    let mut legacy_hasher = qx_core::Fnv1a::new();
    legacy_hasher.write_text(&legacy_spec.to_json().expect("encode legacy depth spec"));
    let legacy_fingerprint = format!("{:016x}", legacy_hasher.finish());
    let mut legacy_result: Value =
        serde_json::from_slice(&std::fs::read(&outcome_path).expect("read original result"))
            .expect("parse original depth result");
    legacy_result["spec_fingerprint"] = json!(legacy_fingerprint);
    legacy_result["run_manifest"]["config_hash"] = json!(legacy_fingerprint);
    let legacy_result_path = root.join("legacy-path-sensitive-result.json");
    std::fs::write(
        &legacy_result_path,
        serde_json::to_vec_pretty(&legacy_result).expect("encode legacy result"),
    )
    .expect("write legacy path-sensitive result");

    let portable = root.join("portable-depth-batch");
    let packed = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args(["evidence", "pack-multi-account-depth-run"])
        .arg(&legacy_result_path)
        .arg("--spec")
        .arg(&spec_path)
        .arg("--output-dir")
        .arg(&portable)
        .output()
        .expect("pack multi-account depth evidence");
    assert_eq!(
        packed.status.code(),
        Some(0),
        "multi-account depth evidence packaging failed: {}",
        String::from_utf8_lossy(&packed.stderr)
    );
    let package_result: Value = serde_json::from_slice(&packed.stdout).expect("package result");
    assert_eq!(package_result["accounts_verified"], 2);
    let packaged_spec: Value = serde_json::from_slice(
        &std::fs::read(portable.join("multi-account-depth-spec.json"))
            .expect("read portable depth spec"),
    )
    .expect("parse portable depth spec");
    assert!(packaged_spec["runs"].as_array().unwrap().iter().all(|run| {
        run["depth_path"]
            .as_str()
            .is_some_and(|path| path.starts_with("accounts/"))
            && run["quality_manifest_path"]
                .as_str()
                .is_some_and(|path| path.starts_with("accounts/"))
    }));
    let packaged_result: Value = serde_json::from_slice(
        &std::fs::read(portable.join("multi-account-depth-result.json"))
            .expect("read normalized portable result"),
    )
    .expect("parse normalized portable result");
    assert_ne!(packaged_result["spec_fingerprint"], legacy_fingerprint);
    let evidence_manifest: Value = serde_json::from_slice(
        &std::fs::read(portable.join("multi-account-depth-evidence.json"))
            .expect("read depth evidence manifest"),
    )
    .expect("parse depth evidence manifest");
    assert_eq!(
        evidence_manifest["spec_fingerprint"],
        packaged_result["spec_fingerprint"]
    );
    std::fs::remove_file(&source_depth).expect("remove source depth frame after packaging");
    std::fs::remove_file(&source_quality).expect("remove source quality manifest after packaging");
    let moved = root.join("relocated/depth-batch");
    std::fs::create_dir_all(moved.parent().unwrap()).expect("create depth relocation parent");
    std::fs::rename(&portable, &moved).expect("relocate multi-account depth package");
    let verified = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args(["evidence", "verify-multi-account-depth-run"])
        .arg(&moved)
        .arg("--recompute")
        .output()
        .expect("verify and recompute relocated depth package");
    assert_eq!(
        verified.status.code(),
        Some(0),
        "relocated multi-account depth evidence failed verification/recompute: {}",
        String::from_utf8_lossy(&verified.stderr)
    );
    let verified_json: Value = serde_json::from_slice(&verified.stdout).expect("verification JSON");
    assert_eq!(verified_json["verified_account_count"], 2);
    assert_eq!(verified_json["result_recomputed"], true);
}

#[test]
fn partial_multi_account_depth_package_recomputes_missing_inputs_after_relocation() {
    let root = scratch("multi-account-depth-partial-evidence");
    let fixture = repo_root().join("deploy/qianxing.depth-frame.l1.example.json");
    let source_depth = root.join("source-depth.json");
    std::fs::copy(&fixture, &source_depth).expect("copy successful source depth frame");
    let source_quality = root.join("source-quality.json");
    let quality_output = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .arg("data-validate-depth")
        .arg(&source_depth)
        .args(["--tier", "l1", "1000"])
        .arg(&source_quality)
        .output()
        .expect("create depth quality manifest");
    assert_eq!(quality_output.status.code(), Some(0));

    let missing_depth = root.join("missing-depth.json");
    let missing_quality = root.join("missing-quality.json");
    let member = |account: &str, run_id: &str, data: &Path, quality: &Path, output: &Path| {
        json!({
            "schema_version": 1,
            "run_id": run_id,
            "account_id": account,
            "depth_path": data,
            "quality_manifest_path": quality,
            "tier": "l1",
            "settlement_currency": "USDT",
            "initial_cash_raw": 1_000_000_000_000_000_i128,
            "fee_bps": 0,
            "latency_snapshots": 0,
            "queue_position_bps": 0,
            "market_impact_bps": 0,
            "output_dir": output,
            "strategy": {
                "kind": "sma_cross",
                "strategy_id": format!("{account}-depth-sma"),
                "quantity_raw": 1_000_000_000_i128,
                "fast_window": 2,
                "slow_window": 3
            }
        })
    };
    let spec_path = root.join("partial-depth-spec.json");
    std::fs::write(
        &spec_path,
        serde_json::to_vec_pretty(&json!({
            "schema_version": 1,
            "batch_id": "partial-depth-portable-batch",
            "runs": [
                member("depth-ok", "depth-ok-run", &source_depth, &source_quality, &root.join("ok-output")),
                member("depth-missing", "depth-missing-run", &missing_depth, &missing_quality, &root.join("missing-output"))
            ]
        }))
        .expect("serialize partial depth spec"),
    )
    .expect("write partial depth spec");
    let run = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args(["app", "multi-account-depth-backtest"])
        .arg(&spec_path)
        .output()
        .expect("run partial depth batch");
    assert_eq!(
        run.status.code(),
        Some(0),
        "partial depth batch failed: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    let result: Value = serde_json::from_slice(&run.stdout).expect("partial depth result");
    assert_eq!(result["summary"]["accounts_succeeded"], 1);
    assert_eq!(result["summary"]["accounts_failed"], 1);
    let result_path = root.join("partial-depth-result.json");
    std::fs::write(&result_path, &run.stdout).expect("save partial depth result");
    let report = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .arg("report")
        .arg(&result_path)
        .args(["--json", "--evidence"])
        .output()
        .expect("generate successful child evidence");
    assert_eq!(
        report.status.code(),
        Some(0),
        "partial depth evidence report failed: {}",
        String::from_utf8_lossy(&report.stderr)
    );
    let portable = root.join("portable-partial-depth");
    let packed = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args(["evidence", "pack-multi-account-depth-run"])
        .arg(&result_path)
        .arg("--spec")
        .arg(&spec_path)
        .arg("--output-dir")
        .arg(&portable)
        .output()
        .expect("pack partial depth evidence");
    assert_eq!(
        packed.status.code(),
        Some(0),
        "partial depth packaging failed: {}",
        String::from_utf8_lossy(&packed.stderr)
    );
    let manifest: Value = serde_json::from_slice(
        &std::fs::read(portable.join("multi-account-depth-evidence.json"))
            .expect("read partial depth manifest"),
    )
    .expect("parse partial depth manifest");
    assert_eq!(manifest["accounts"].as_array().unwrap().len(), 1);
    let inputs = manifest["inputs"].as_array().unwrap();
    assert_eq!(inputs.len(), 2);
    assert!(inputs.iter().any(|item| item["missing"] == true));
    assert!(inputs
        .iter()
        .any(|item| item["quality_manifest_missing"] == true));

    std::fs::remove_file(&source_depth).expect("remove original depth frame");
    std::fs::remove_file(&source_quality).expect("remove original quality manifest");
    let moved = root.join("relocated/partial-depth");
    std::fs::create_dir_all(moved.parent().unwrap()).expect("create partial relocation parent");
    std::fs::rename(&portable, &moved).expect("relocate partial depth package");
    let verified = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args(["evidence", "verify-multi-account-depth-run"])
        .arg(&moved)
        .arg("--recompute")
        .output()
        .expect("verify and recompute partial portable depth batch");
    assert_eq!(
        verified.status.code(),
        Some(0),
        "partial depth package recompute failed: {}",
        String::from_utf8_lossy(&verified.stderr)
    );
    let verified: Value = serde_json::from_slice(&verified.stdout).expect("partial verification");
    assert_eq!(verified["verified_account_count"], 1);
    assert_eq!(verified["failed_account_count"], 1);
    assert_eq!(verified["result_recomputed"], true);
}
