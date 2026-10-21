//! Depth-market application contract across the CLI, HTTP and Python facades.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

static ENV_LOCK: Mutex<()> = Mutex::new(());

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("qx-cli has a repository root")
        .to_path_buf()
}

fn fresh_run_dir(name: &str) -> PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock is after the Unix epoch")
        .as_nanos();
    let path = repo_root()
        .join("target")
        .join(format!("{name}-{}-{nonce}", std::process::id()));
    std::fs::create_dir_all(&path).expect("create unique workspace-local test output");
    path
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

fn python_run(root: &Path, payload: &str, function: &str) -> Option<Value> {
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
        "import json, sys\nsys.path.insert(0, {python_dir:?})\nfrom qianxing_bridge import app, native\nif not native.app_available(): sys.exit(3)\ntry:\n result=app.{function}(json.loads({payload:?}))\nexcept Exception as error:\n print(json.dumps({{'error': app.app_error_payload(error) or str(error)}})); sys.exit(4)\nprint(json.dumps(result))\n",
        python_dir = root.join("python").to_string_lossy(),
        function = function,
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
    let _env_guard = ENV_LOCK.lock().expect("lock process environment for test");
    let root = repo_root();
    let run_dir = fresh_run_dir("qx-app-depth-sdk");
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
    if let Some(python) = python_run(&root, &python_spec.to_string(), "run_depth_backtest") {
        assert_eq!(identities(&python), identities(&cli));
    } else {
        eprintln!("[skip] Python app extension is not built in this test environment");
    }
}

#[test]
fn depth_strategy_portfolio_uses_the_same_shared_result_through_cli_http_and_python() {
    let _env_guard = ENV_LOCK.lock().expect("lock process environment for test");
    let root = repo_root();
    let run_dir = fresh_run_dir("qx-app-depth-portfolio");
    let depth_path = run_dir.join("portfolio-ticks.json");
    std::fs::write(&depth_path, sample_depth()).expect("write portfolio Tick frame");
    let mut spec: Value = serde_json::from_str(&depth_spec(
        "depth-portfolio-parity",
        &depth_path,
        &run_dir.join("cli-artifacts"),
    ))
    .expect("DepthBacktestSpec JSON");
    spec["portfolio"] = json!({
        "primary_priority": 0,
        "primary_reserved_capital_raw": 400_000_000_000_000_i128,
        "additional_strategies": [{
            "priority": 1,
            "reserved_capital_raw": 400_000_000_000_000_i128,
            "strategy": {
                "kind": "momentum",
                "strategy_id": "depth-sdk-momentum",
                "quantity_raw": 1_000_000_000_i128,
                "fast_window": 2,
                "slow_window": 3,
                "period": 14,
                "threshold_bps": 100,
            }
        }]
    });
    let payload = spec.to_string();
    let spec_path = run_dir.join("portfolio-spec.json");
    std::fs::write(&spec_path, &payload).expect("write portfolio spec");
    let cli_output = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args(["app", "depth-backtest"])
        .arg(&spec_path)
        .output()
        .expect("start CLI depth portfolio");
    assert_eq!(
        cli_output.status.code(),
        Some(0),
        "CLI depth portfolio failed: {}",
        String::from_utf8_lossy(&cli_output.stderr)
    );
    let cli: Value = serde_json::from_slice(&cli_output.stdout).expect("CLI portfolio JSON");

    std::env::set_var("QX_API_DATA_ROOT", &root);
    std::env::set_var("QX_API_ARTIFACT_ROOT", run_dir.join("http-artifacts"));
    let service = qx_api::ApiService::new(qx_api::ApiState::default());
    let response = service.handle("POST", "/app/depth-backtest", &payload, 0);
    let http: Value = serde_json::from_str(&response.body).expect("HTTP portfolio JSON");
    assert_eq!(response.status, 200, "HTTP portfolio failed: {http}");

    let mut python_spec = spec.clone();
    python_spec["output_dir"] = json!(run_dir.join("python-artifacts"));
    let python = python_run(&root, &python_spec.to_string(), "run_depth_backtest");
    let identities = |value: &Value| {
        json!({
            "run_id": value["run_id"],
            "instrument": value["instrument"],
            "tier": value["tier"],
            "result_hash": value["result_hash"],
            "fills": value["fills"],
            "equity_points": value["equity_points"],
        })
    };
    assert_eq!(identities(&cli), identities(&http));
    let mut outcomes = vec![&cli, &http];
    if let Some(python) = python.as_ref() {
        assert_eq!(identities(&cli), identities(python));
        outcomes.push(python);
    } else {
        eprintln!("[skip] Python app extension is not built in this test environment");
    }
    for outcome in outcomes {
        let summary_path = outcome["artifacts"]["summary"]
            .as_str()
            .expect("summary artifact path");
        let summary: Value =
            serde_json::from_slice(&std::fs::read(summary_path).expect("read portfolio summary"))
                .expect("portfolio summary JSON");
        let counts = summary["strategy_fills"]
            .as_object()
            .expect("strategy fill attribution");
        assert_eq!(counts.len(), 2);
        assert!(counts
            .values()
            .all(|count| count.as_u64().unwrap_or_default() > 0));
    }
}

#[test]
fn tick_parameter_experiment_is_shared_by_cli_http_python_and_run_lifecycle() {
    let _env_guard = ENV_LOCK.lock().expect("lock process environment for test");
    let root = repo_root();
    let run_dir = fresh_run_dir("qx-app-depth-grid");
    let depth_path = run_dir.join("grid-ticks.json");
    std::fs::write(&depth_path, sample_depth()).expect("write Tick frame");
    let mut base: Value = serde_json::from_str(&depth_spec(
        "unused-depth-grid-base",
        &depth_path,
        &run_dir.join("base-artifacts"),
    ))
    .expect("DepthBacktestSpec JSON");
    base["market_spec"] = Value::Null;
    base.as_object_mut().unwrap().remove("market_spec");
    base["order_policy"] = Value::Null;
    base.as_object_mut().unwrap().remove("order_policy");
    let make_spec = |experiment_id: &str, output_dir: &Path| {
        let mut base = base.clone();
        base["output_dir"] = json!(output_dir);
        json!({
            "schema_version": 1,
            "experiment_id": experiment_id,
            "base": base,
            "parameter_space": [{"name": "quantity_raw", "values": [0, 1_000_000_000]}],
        })
        .to_string()
    };
    let spec = make_spec("tick-grid-parity", &run_dir.join("cli-artifacts"));
    let spec_path = run_dir.join("grid-spec.json");
    std::fs::write(&spec_path, &spec).expect("write experiment spec");
    let cli_output = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args(["app", "run-depth-experiment"])
        .arg(&spec_path)
        .output()
        .expect("start qx-cli depth experiment");
    assert_eq!(
        cli_output.status.code(),
        Some(0),
        "CLI depth experiment failed: {}",
        String::from_utf8_lossy(&cli_output.stderr)
    );
    let cli: Value = serde_json::from_slice(&cli_output.stdout).expect("CLI grid JSON");

    std::env::set_var("QX_API_DATA_ROOT", &root);
    std::env::set_var("QX_API_ARTIFACT_ROOT", run_dir.join("http-artifacts"));
    let service = qx_api::ApiService::new(qx_api::ApiState::default());
    let response = service.handle("POST", "/app/run-depth-experiment", &spec, 0);
    let http: Value = serde_json::from_str(&response.body).expect("HTTP grid JSON");
    assert_eq!(response.status, 200, "HTTP experiment failed: {http}");
    let identities = |value: &Value| {
        json!({
            "experiment_id": value["experiment_id"],
            "total_candidates": value["total_candidates"],
            "completed_candidates": value["completed_candidates"],
            "succeeded_candidates": value["succeeded_candidates"],
            "failed_candidates": value["failed_candidates"],
            "ranking": value["ranking"],
            "candidates": value["candidates"].as_array().unwrap().iter().map(|candidate| json!({
                "ordinal": candidate["ordinal"],
                "parameters": candidate["parameters"],
                "run_id": candidate["outcome"]["run_id"],
                "result_hash": candidate["outcome"]["result_hash"],
                "return_bps": candidate["outcome"]["return_bps"],
                "error": candidate["error"],
            })).collect::<Vec<_>>(),
        })
    };
    assert_eq!(identities(&cli), identities(&http));
    assert_eq!(cli["failed_candidates"], 1);
    assert_eq!(cli["succeeded_candidates"], 1);
    if let Some(python) = python_run(
        &root,
        &make_spec("tick-grid-parity", &run_dir.join("python-artifacts")),
        "run_depth_experiment",
    ) {
        assert_eq!(identities(&python), identities(&cli));
    } else {
        eprintln!("[skip] Python app extension is not built in this test environment");
    }

    let handle_run_id = format!(
        "tick-grid-handle-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock is after the Unix epoch")
            .as_nanos()
    );
    let handle_spec = make_spec(&handle_run_id, &run_dir.join("handle-artifacts"));
    let started = service.handle("POST", "/app/run-depth-experiment/start", &handle_spec, 0);
    assert_eq!(
        started.status, 200,
        "depth grid start failed: {}",
        started.body
    );
    let started: Value = serde_json::from_str(&started.body).expect("start status JSON");
    assert_eq!(started["run_id"], handle_run_id);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let status = service.handle("GET", &format!("/app/runs/{handle_run_id}"), "", 0);
        let status: Value = serde_json::from_str(&status.body).expect("terminal status JSON");
        if matches!(
            status["status"].as_str(),
            Some("succeeded" | "failed" | "cancelled")
        ) {
            assert_eq!(status["status"], "succeeded");
            assert_eq!(status["result"]["completed_candidates"], 2);
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "depth experiment lifecycle stalled"
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}

#[test]
fn depth_strategy_selection_is_shared_by_cli_http_and_python() {
    let _env_guard = ENV_LOCK.lock().expect("lock process environment for test");
    let root = repo_root();
    let run_dir = fresh_run_dir("qx-app-depth-selection");
    let depth_path = run_dir.join("selection-ticks.json");
    std::fs::write(&depth_path, sample_depth()).expect("write Tick frame");
    let make_spec = |experiment_id: &str, output_dir: &Path| {
        let mut base: Value = serde_json::from_str(&depth_spec(
            "unused-depth-selection-base",
            &depth_path,
            output_dir,
        ))
        .expect("DepthBacktestSpec JSON");
        base["market_spec"] = Value::Null;
        base.as_object_mut().unwrap().remove("market_spec");
        base["order_policy"] = Value::Null;
        base.as_object_mut().unwrap().remove("order_policy");
        json!({
            "schema_version": 1,
            "experiment_id": experiment_id,
            "base": base,
            "parameter_space": [],
            "strategy_kinds": ["sma_cross", "ema_cross", "rsi"],
        })
        .to_string()
    };
    let spec_path = run_dir.join("selection-spec.json");
    let cli_spec = make_spec("depth-selection-parity", &run_dir.join("cli-artifacts"));
    std::fs::write(&spec_path, &cli_spec).expect("write strategy selection spec");
    let cli_output = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args(["app", "run-depth-experiment"])
        .arg(&spec_path)
        .output()
        .expect("run CLI strategy selection");
    assert_eq!(
        cli_output.status.code(),
        Some(0),
        "CLI depth selection failed: {}",
        String::from_utf8_lossy(&cli_output.stderr)
    );
    let cli: Value = serde_json::from_slice(&cli_output.stdout).expect("CLI selection JSON");

    std::env::set_var("QX_API_DATA_ROOT", &root);
    std::env::set_var("QX_API_ARTIFACT_ROOT", run_dir.join("http-artifacts"));
    let service = qx_api::ApiService::new(qx_api::ApiState::default());
    let http_spec = make_spec("depth-selection-parity", &run_dir.join("http-artifacts"));
    let response = service.handle("POST", "/app/run-depth-experiment", &http_spec, 0);
    let http: Value = serde_json::from_str(&response.body).expect("HTTP selection JSON");
    assert_eq!(response.status, 200, "HTTP selection failed: {http}");
    let identities = |value: &Value| {
        json!({
            "experiment_id": value["experiment_id"],
            "total_candidates": value["total_candidates"],
            "succeeded_candidates": value["succeeded_candidates"],
            "failed_candidates": value["failed_candidates"],
            "ranking": value["ranking"],
            "candidates": value["candidates"].as_array().unwrap().iter().map(|candidate| json!({
                "ordinal": candidate["ordinal"],
                "strategy_kind": candidate["strategy_kind"],
                "run_id": candidate["outcome"]["run_id"],
                "result_hash": candidate["outcome"]["result_hash"],
                "error": candidate["error"],
            })).collect::<Vec<_>>(),
        })
    };
    assert_eq!(identities(&cli), identities(&http));
    assert_eq!(cli["succeeded_candidates"], 3);
    let python_spec = make_spec("depth-selection-parity", &run_dir.join("python-artifacts"));
    if let Some(python) = python_run(&root, &python_spec, "run_depth_experiment") {
        assert_eq!(identities(&python), identities(&cli));
    } else {
        eprintln!("[skip] Python app extension is not built in this test environment");
    }
}
