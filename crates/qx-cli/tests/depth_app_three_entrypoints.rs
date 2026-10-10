//! Cross-entry contract for the shared Rust Tick/OrderBook application workflow.

use serde_json::{json, Value};
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
    let root = std::env::temp_dir().join(format!(
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

    if let Some(python) = python_depth(&spec) {
        assert_eq!(cli["result_hash"], python["result_hash"]);
        assert_eq!(cli["data_fingerprint"], python["data_fingerprint"]);
        assert_eq!(cli["fills"], python["fills"]);
        let bridge = python_verify_depth(&cli_output.stdout);
        assert_eq!(bridge["verified"], true);
    } else {
        eprintln!("skip Python leg: qx-python extension is not built or is stale");
    }
}

fn python_verify_depth(outcome: &[u8]) -> Value {
    let script = r#"
import json, sys
sys.path.insert(0, 'python')
from qianxing import DepthBacktestOutcome, verify_depth_run
outcome = DepthBacktestOutcome.from_dict(json.loads(sys.stdin.read()))
print(json.dumps(verify_depth_run(outcome).__dict__))
"#;
    let mut child = Command::new("python")
        .args(["-c", script])
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
