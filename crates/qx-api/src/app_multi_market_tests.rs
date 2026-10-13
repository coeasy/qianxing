use crate::app_surface::{self, AppPaths, AppRunRegistry};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn fixture() -> (AppPaths, Value) {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("target")
        .join(format!(
            "qx-api-multi-market-{}-{unique}",
            std::process::id()
        ));
    let data_root = root.join("data");
    std::fs::create_dir_all(&data_root).unwrap();
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy/qianxing.bar-frame.example.json");
    let mut frame: Value = serde_json::from_slice(&std::fs::read(source).unwrap()).unwrap();
    let instruments = ["BTCUSDT.BINANCE", "ETHUSDT.KRAKEN", "SOLUSDT.BYBIT"];
    let markets = instruments
        .iter()
        .enumerate()
        .map(|(index, instrument)| {
            frame["instrument"] = json!(instrument);
            let path = data_root.join(format!("market-{index}.json"));
            std::fs::write(&path, serde_json::to_vec(&frame).unwrap()).unwrap();
            json!({
                "instrument": instrument,
                "bars_path": path,
                "priority": index,
                "market_spec": null,
                "fee_schedule": null,
                "strategies": [{
                    "priority": 0,
                    "reserved_capital_raw": 20_000_000_000_000_i128,
                    "strategy": {
                        "kind": "sma_cross",
                        "strategy_id": format!("api-market-{index}"),
                        "quantity_raw": 1_000_000_000_i128,
                        "fast_window": 2,
                        "slow_window": 3,
                    }
                }]
            })
        })
        .collect::<Vec<_>>();
    (
        AppPaths {
            data_root,
            artifact_root: root.join("artifacts"),
        },
        json!({
            "schema_version": 1,
            "run_id": "api-multi-market-sync",
            "account_id": "api-shared-account",
            "settlement_currency": "USDT",
            "initial_cash_raw": 100_000_000_000_000_i128,
            "seed": 7,
            "output_dir": "client-controlled-path-is-overridden",
            "markets": markets,
        }),
    )
}

#[test]
fn multi_market_sync_and_run_handle_routes_use_the_shared_rust_workflow() {
    let (paths, body) = fixture();
    let response = app_surface::multi_market(&body.to_string(), &paths);
    assert_eq!(response.status, 200, "{}", response.body);
    let outcome: Value = serde_json::from_str(&response.body).unwrap();
    assert_eq!(outcome["markets_total"], 3);
    assert!(outcome["successful_fills"].as_u64().unwrap() > 0);

    let mut asynchronous = body;
    asynchronous["run_id"] = json!("api-multi-market-async");
    let registry = AppRunRegistry::default();
    let started = app_surface::route_app_run(
        "POST",
        "/app/multi-market-backtest/start",
        "",
        &asynchronous.to_string(),
        &paths,
        &registry,
    )
    .unwrap();
    assert_eq!(started.status, 200, "{}", started.body);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let status = registry.status("api-multi-market-async");
        let document: Value = serde_json::from_str(&status.body).unwrap();
        if document["status"] == "succeeded" {
            assert_eq!(document["result"]["markets_total"], 3);
            break;
        }
        assert!(
            Instant::now() < deadline,
            "multi-market RunHandle timed out: {document}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}
