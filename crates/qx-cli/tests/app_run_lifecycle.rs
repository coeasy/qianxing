//! End-to-end HTTP lifecycle contract for shared Rust Bar and depth workers.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("qx-cli has a repository root")
        .to_path_buf()
}

fn terminal_result(api: &qx_api::ApiService, run_id: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let response = api.handle("GET", &format!("/app/runs/{run_id}"), "", 0);
        assert_eq!(
            response.status, 200,
            "status route failed: {}",
            response.body
        );
        let document: Value = serde_json::from_str(&response.body).expect("run status JSON");
        if matches!(
            document["status"].as_str(),
            Some("succeeded" | "failed" | "cancelled")
        ) {
            return document;
        }
        assert!(Instant::now() < deadline, "run did not finish: {document}");
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn http_lifecycle_starts_polls_cancels_and_retains_bar_and_depth_results() {
    let root = std::env::temp_dir().join(format!("qx-app-http-lifecycle-{}", std::process::id()));
    let data_root = root.join("data");
    let artifact_root = root.join("artifacts");
    std::fs::create_dir_all(&data_root).expect("create input root");
    let repository = repository_root();
    std::fs::copy(
        repository.join("deploy/qianxing.bar-frame.example.json"),
        data_root.join("bars.json"),
    )
    .expect("copy bar fixture");
    std::fs::copy(
        repository.join("deploy/qianxing.depth-frame.l1.example.json"),
        data_root.join("ticks.json"),
    )
    .expect("copy tick fixture");
    std::env::set_var("QX_API_DATA_ROOT", &data_root);
    std::env::set_var("QX_API_ARTIFACT_ROOT", &artifact_root);
    let api = qx_api::ApiService::new(qx_api::ApiState::default());

    let bar_spec = json!({
        "schema_version": 1,
        "run_id": "http-bar-lifecycle",
        "instrument": "BTCUSDT.BINANCE",
        "bars_path": "bars.json",
        "settlement_currency": "USDT",
        "initial_cash_raw": 100_000_000_000_000_i128,
        "seed": 20261011,
        "output_dir": "caller-path-is-replaced",
        "strategy": {
            "kind": "sma_cross",
            "strategy_id": "http-bar-lifecycle-sma",
            "quantity_raw": 1_000_000_000_i128,
            "fast_window": 2,
            "slow_window": 3
        }
    })
    .to_string();
    let started = api.handle("POST", "/app/backtest/start", &bar_spec, 0);
    assert_eq!(started.status, 200, "start failed: {}", started.body);
    let document = terminal_result(&api, "http-bar-lifecycle");
    assert_eq!(document["status"], "succeeded");
    assert_eq!(document["result"]["equity_points"], 70);
    assert!(document["result"]["artifacts"]["run_manifest"]
        .as_str()
        .is_some());
    let repeated = api.handle("GET", "/app/runs/http-bar-lifecycle", "", 0);
    assert_eq!(
        serde_json::from_str::<Value>(&repeated.body).unwrap(),
        document
    );

    let cancel = api.handle("POST", "/app/runs/http-bar-lifecycle/cancel", "{}", 0);
    assert_eq!(cancel.status, 202);
    assert_eq!(api.handle("GET", "/app/runs/../outside", "", 0).status, 404);
    assert_eq!(
        api.handle("GET", "/app/runs/http-bar-lifecycle?ignored=true", "", 0)
            .status,
        400,
        "app routes reject query parameters rather than silently ignoring them"
    );
    assert_eq!(
        api.handle("GET", "/app/runs/missing-run", "", 0).status,
        404
    );
    assert_eq!(
        api.handle("POST", "/app/backtest/start", &bar_spec, 0)
            .status,
        409,
        "run ids remain reserved while their result is retained"
    );

    let depth_spec = json!({
        "schema_version": 1,
        "run_id": "http-depth-lifecycle",
        "depth_path": "ticks.json",
        "tier": "l1",
        "settlement_currency": "USDT",
        "initial_cash_raw": 1_000_000_000_000_000_i128,
        "fee_bps": 0,
        "latency_snapshots": 0,
        "queue_position_bps": 0,
        "market_impact_bps": 0,
        "output_dir": "caller-path-is-replaced",
        "strategy": {
            "kind": "sma_cross",
            "strategy_id": "http-depth-lifecycle-sma",
            "quantity_raw": 1_000_000_000_i128,
            "fast_window": 2,
            "slow_window": 3
        }
    })
    .to_string();
    let started = api.handle("POST", "/app/depth-backtest/start", &depth_spec, 0);
    assert_eq!(started.status, 200, "depth start failed: {}", started.body);
    let document = terminal_result(&api, "http-depth-lifecycle");
    assert_eq!(document["status"], "succeeded");
    assert_eq!(document["result"]["tier"], "l1");
    assert_eq!(document["result"]["equity_points"], 80);

    let experiment_spec = json!({
        "schema_version": 1,
        "experiment_id": "http-experiment-lifecycle",
        "base": {
            "schema_version": 1,
            "run_id": "unused-run-id",
            "instrument": "BTCUSDT.BINANCE",
            "bars_path": "bars.json",
            "settlement_currency": "USDT",
            "initial_cash_raw": 100_000_000_000_000_i128,
            "seed": 20261011,
            "output_dir": "caller-path-is-replaced",
            "strategy": {
                "kind": "sma_cross",
                "strategy_id": "http-experiment-sma",
                "quantity_raw": 1_000_000_000_i128,
                "fast_window": 2,
                "slow_window": 4
            }
        },
        "parameter_space": [{"name": "fast_window", "values": [2, 3]}]
    })
    .to_string();
    let started = api.handle("POST", "/app/run-experiment/start", &experiment_spec, 0);
    assert_eq!(
        started.status, 200,
        "experiment start failed: {}",
        started.body
    );
    let document = terminal_result(&api, "http-experiment-lifecycle");
    assert_eq!(document["status"], "succeeded");
    assert_eq!(document["result"]["completed_candidates"], 2);
    assert_eq!(document["result"]["succeeded_candidates"], 2);
}
