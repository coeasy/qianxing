//! Depth-market application contract across the CLI, HTTP and Python facades.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("qx-cli has a repository root")
        .to_path_buf()
}

fn depth_spec(run_id: &str, depth_path: &Path, output_dir: &Path) -> String {
    json!({
        "schema_version": 1,
        "run_id": run_id,
        "depth_path": depth_path,
        "tier": "l1",
        "settlement_currency": "USDT",
        "initial_cash_raw": 1_000_000_000_000_000_i128,
        "fee_bps": 0,
        "latency_snapshots": 0,
        "queue_position_bps": 0,
        "market_impact_bps": 0,
        "output_dir": output_dir,
        "market_spec": {
            "instrument": {"symbol": "BTCUSDT", "venue": "BINANCE"},
            "product": "perpetual",
            "base_currency": "BTC",
            "quote_currency": "USDT",
            "settlement_currency": "USDT",
            "contract_size": 1_000_000_000_i128,
            "linear": true,
            "inverse": false,
            "price_tick": 1_i128,
            "qty_step": 1_i128,
            "min_qty": 1_i128,
            "max_leverage": 100,
            "maintenance_margin_bps": 500,
            "valid_from": 1_000_u64,
            "valid_to": 49_000_u64,
        },
        "order_policy": {
            "reduce_only": false,
            "position_side": "net",
            "margin_mode": "cross",
            "position_mode": "one_way",
            "leverage": 2,
            "post_only": false,
        },
        "strategy": {
            "kind": "sma_cross",
            "strategy_id": "depth-sdk-parity",
            "quantity_raw": 1_000_000_000_i128,
            "fast_window": 2,
            "slow_window": 3,
            "period": 14,
            "threshold_bps": 100,
        },
    })
    .to_string()
}

fn sample_depth() -> String {
    let snapshots = (0..48)
        .map(|index| {
            let phase = index % 20;
            let mid = 100
                + if phase < 10 {
                    phase * 2
                } else {
                    (20 - phase) * 2
                };
            json!({
                "instrument": {"symbol": "BTCUSDT", "venue": "BINANCE"},
                "ts": 1_000 + index as u64 * 1_000,
                "sequence": index as u64 + 1,
                "bids": [{"price": (mid - 1) * 1_000_000_000_i128, "qty": 100_000_000_000_i128}],
                "asks": [{"price": (mid + 1) * 1_000_000_000_i128, "qty": 100_000_000_000_i128}],
            })
        })
        .collect::<Vec<_>>();
    json!({
        "schema_version": 1,
        "source": "app-depth-sdk-parity",
        "instrument": {"symbol": "BTCUSDT", "venue": "BINANCE"},
        "snapshots": snapshots,
    })
    .to_string()
}

fn python_run(root: &Path, payload: &str) -> Option<Value> {
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
        "import json, sys\nsys.path.insert(0, {python_dir:?})\nfrom qianxing_bridge import app, native\nif not native.app_available(): sys.exit(3)\ntry:\n result=app.run_depth_backtest(json.loads({payload:?}))\nexcept Exception as error:\n print(json.dumps({{'error': app.app_error_payload(error) or str(error)}})); sys.exit(4)\nprint(json.dumps(result))\n",
        python_dir = root.join("python").to_string_lossy(),
    );
    let output = Command::new(python)
        .arg("-c")
        .arg(script)
        .current_dir(root.join("python"))
        .output()
        .expect("start Python SDK contract process");
    match output.status.code() {
        Some(0) => Some(serde_json::from_slice(&output.stdout).expect("Python result JSON")),
        Some(3) => None,
        _ => panic!(
            "Python depth SDK failed: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    }
}

#[test]
fn market_aware_tick_spec_has_the_same_rust_result_through_cli_http_and_python() {
    let root = repo_root();
    let run_dir = root
        .join("target")
        .join(format!("qx-app-depth-sdk-{}", std::process::id()));
    std::fs::create_dir_all(&run_dir).expect("create workspace-local test output");
    let depth_path = run_dir.join("ticks.json");
    std::fs::write(&depth_path, sample_depth()).expect("write Tick frame");
    let spec = depth_spec(
        "depth-sdk-perpetual",
        &depth_path,
        &run_dir.join("artifacts"),
    );
    let spec_path = run_dir.join("depth-spec.json");
    std::fs::write(&spec_path, &spec).expect("write DepthBacktestSpec");

    let cli_output = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args(["app", "depth-backtest"])
        .arg(&spec_path)
        .output()
        .expect("start qx-cli depth-backtest");
    assert_eq!(
        cli_output.status.code(),
        Some(0),
        "CLI depth run failed: {}",
        String::from_utf8_lossy(&cli_output.stderr)
    );
    let cli: Value = serde_json::from_slice(&cli_output.stdout).expect("CLI result JSON");

    std::env::set_var("QX_API_DATA_ROOT", &root);
    std::env::set_var("QX_API_ARTIFACT_ROOT", run_dir.join("http-artifacts"));
    let service = qx_api::ApiService::new(qx_api::ApiState::default());
    let response = service.handle("POST", "/app/depth-backtest", &spec, 0);
    let http: Value = serde_json::from_str(&response.body).expect("HTTP result JSON");
    assert_eq!(response.status, 200, "HTTP depth run failed: {http}");

    let identities = |value: &Value| {
        json!({
            "run_id": value["run_id"],
            "instrument": value["instrument"],
            "tier": value["tier"],
            "result_hash": value["result_hash"],
            "data_fingerprint": value["data_fingerprint"],
            "fills": value["fills"],
            "equity_points": value["equity_points"],
            "return_bps": value["return_bps"],
            "max_drawdown_bps": value["max_drawdown_bps"],
        })
    };
    assert!(cli["fills"].as_u64().unwrap_or_default() > 0);
    assert_eq!(identities(&cli), identities(&http));

    // Every facade writes to its own artifact root; run identity includes the persisted spec,
    // so sharing the CLI directory would correctly reject the same run id as a conflict.
    let mut python_spec: Value = serde_json::from_str(&spec).expect("DepthBacktestSpec JSON");
    python_spec["output_dir"] = json!(run_dir.join("python-artifacts"));
    if let Some(python) = python_run(&root, &python_spec.to_string()) {
        assert_eq!(identities(&python), identities(&cli));
    } else {
        eprintln!("[skip] Python app extension is not built in this test environment");
    }
}
