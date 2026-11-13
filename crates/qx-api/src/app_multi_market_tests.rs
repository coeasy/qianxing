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

fn depth_fixture() -> (AppPaths, Value) {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("target")
        .join(format!(
            "qx-api-multi-market-depth-{}-{unique}",
            std::process::id()
        ));
    let data_root = root.join("data");
    std::fs::create_dir_all(&data_root).unwrap();
    let markets = [("BTCUSDT.BINANCE", "BTC", 80), ("ETHUSDT.BYBIT", "ETH", 60)]
        .into_iter()
        .enumerate()
        .map(|(index, (instrument, base, price))| {
            let (symbol, venue) = instrument.split_once('.').unwrap();
            let snapshots = [1_u64, 2]
                .into_iter()
                .map(|ts| {
                    json!({
                        "instrument": {"symbol": symbol, "venue": venue},
                        "ts": ts,
                        "sequence": ts,
                        "bids": [{"price": price * 1_000_000_000_i64, "qty": 2_000_000_000_i64}],
                        "asks": [{"price": price * 1_000_000_000_i64, "qty": 2_000_000_000_i64}]
                    })
                })
                .collect::<Vec<_>>();
            let frame = json!({
                "schema_version": 1,
                "source": format!("api-depth-fixture-{index}"),
                "instrument": {"symbol": symbol, "venue": venue},
                "snapshots": snapshots
            });
            let path = data_root.join(format!("depth-{index}.json"));
            std::fs::write(&path, serde_json::to_vec(&frame).unwrap()).unwrap();
            json!({
                "instrument": instrument,
                "depth_path": path,
                "priority": index,
                "market_spec": {
                    "instrument": {"symbol": symbol, "venue": venue},
                    "product": "spot",
                    "base_currency": base,
                    "quote_currency": "USDT",
                    "settlement_currency": "USDT",
                    "contract_size": 1_000_000_000_i64,
                    "linear": false,
                    "inverse": false,
                    "price_tick": 1,
                    "qty_step": 1,
                    "min_qty": 1,
                    "max_leverage": 1,
                    "maintenance_margin_bps": 0,
                    "valid_from": 1,
                    "valid_to": null
                },
                "fee_bps": 0,
                "latency_snapshots": 0,
                "queue_position_bps": 0,
                "market_impact_bps": 0,
                "strategy": {"kind":"sma_cross", "strategy_id":format!("api-depth-{index}"), "quantity_raw":1_000_000_000_i64, "fast_window":2, "slow_window":3}
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
            "run_id": "api-multi-market-depth-sync",
            "account_id": "api-depth-shared",
            "settlement_currency": "USDT",
            "initial_cash_raw": 100_000_000_000_i64,
            "output_dir": "client-path-is-overridden",
            "markets": markets
        }),
    )
}

fn joint_fixture() -> (AppPaths, Value) {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("target")
        .join(format!("qx-api-joint-{}-{unique}", std::process::id()));
    let data_root = root.join("data");
    std::fs::create_dir_all(&data_root).unwrap();
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy/qianxing.bar-frame.example.json");
    let template: Value = serde_json::from_slice(&std::fs::read(source).unwrap()).unwrap();
    let accounts = [
        ("okx", "BTC-USDT.OKX"),
        ("binance", "ETHUSDT.BINANCE"),
        ("bitget", "PEPEUSDT.BITGET"),
    ]
    .into_iter()
    .enumerate()
    .map(|(index, (exchange_id, instrument))| {
        let mut frame = template.clone();
        frame["instrument"] = json!(instrument);
        let path = data_root.join(format!("joint-market-{index}.json"));
        std::fs::write(&path, serde_json::to_vec(&frame).unwrap()).unwrap();
        json!({
            "exchange_id": exchange_id,
            "account_id": "primary",
            "initial_cash_raw": 100_000_000_000_000_i128,
            "markets": [{
                "instrument": instrument,
                "bars_path": path,
                "priority": 0,
                "market_spec": null,
                "fee_schedule": null,
                "strategies": [{
                    "priority": 0,
                    "reserved_capital_raw": 20_000_000_000_000_i128,
                    "strategy": {
                        "kind": "sma_cross",
                        "strategy_id": format!("api-joint-{index}"),
                        "quantity_raw": 1_000_000_000_i128,
                        "fast_window": 2,
                        "slow_window": 3
                    }
                }]
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
            "run_id": "api-joint-sync",
            "settlement_currency": "USDT",
            "seed": 7,
            "output_dir": "client-controlled-path-is-overridden",
            "accounts": accounts
        }),
    )
}

#[test]
fn multi_account_joint_sync_and_run_handle_routes_cover_three_venues() {
    let (paths, body) = joint_fixture();
    let response = app_surface::multi_account_joint(&body.to_string(), &paths);
    assert_eq!(response.status, 200, "{}", response.body);
    let outcome: Value = serde_json::from_str(&response.body).unwrap();
    assert_eq!(outcome["accounts_total"], 3);
    assert_eq!(outcome["markets_total"], 3);
    assert_eq!(
        outcome["initial_cash_raw"].as_i64(),
        Some(300_000_000_000_000_i64)
    );
    assert!(outcome["successful_fills"].as_u64().unwrap() > 0);

    let mut asynchronous = body;
    asynchronous["run_id"] = json!("api-joint-async");
    let registry = AppRunRegistry::default();
    let started = app_surface::route_app_run(
        "POST",
        "/app/multi-account-joint-backtest/start",
        "",
        &asynchronous.to_string(),
        &paths,
        &registry,
        None,
    )
    .unwrap();
    assert_eq!(started.status, 200, "{}", started.body);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let status = registry.status("api-joint-async");
        let document: Value = serde_json::from_str(&status.body).unwrap();
        if document["status"] == "succeeded" {
            assert_eq!(document["result"]["accounts_total"], 3);
            assert_eq!(document["result"]["markets_total"], 3);
            break;
        }
        assert!(
            Instant::now() < deadline,
            "joint RunHandle timed out: {document}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
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
        None,
    )
    .unwrap();
    assert_eq!(started.status, 200, "{}", started.body);
    let deadline = Instant::now() + Duration::from_secs(5);
    let async_observed = loop {
        let status = registry.status("api-multi-market-async");
        let document: Value = serde_json::from_str(&status.body).unwrap();
        if document["status"] == "succeeded" {
            assert_eq!(document["result"]["markets_total"], 3);
            break document["result"]["successful_fills"]
                .as_u64()
                .expect("successful fills are reported");
        }
        assert!(
            Instant::now() < deadline,
            "multi-market RunHandle timed out: {document}"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    println!(
        "MULTI_MARKET_HTTP_OBSERVED sync_markets={} sync_fills={} async_fills={} run_handle=succeeded",
        outcome["markets_total"].as_u64().unwrap(),
        outcome["successful_fills"].as_u64().unwrap(),
        async_observed
    );
}

#[test]
fn multi_market_depth_sync_and_run_handle_routes_use_shared_account_l2_engine() {
    let (paths, body) = depth_fixture();
    let response = app_surface::multi_market_depth(&body.to_string(), &paths);
    assert_eq!(response.status, 200, "{}", response.body);
    let outcome: Value = serde_json::from_str(&response.body).unwrap();
    assert_eq!(outcome["markets_total"], 2);
    assert_eq!(outcome["equity_points"], 2);
    println!(
        "MULTI_MARKET_DEPTH_HTTP_OBSERVED sync_markets={} equity_points=2 async_run_handle=succeeded",
        outcome["markets_total"]
    );

    let mut asynchronous = body;
    asynchronous["run_id"] = json!("api-multi-market-depth-async");
    let registry = AppRunRegistry::default();
    let started = app_surface::route_app_run(
        "POST",
        "/app/multi-market-depth-backtest/start",
        "",
        &asynchronous.to_string(),
        &paths,
        &registry,
        None,
    )
    .unwrap();
    assert_eq!(started.status, 200, "{}", started.body);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let status = registry.status("api-multi-market-depth-async");
        let document: Value = serde_json::from_str(&status.body).unwrap();
        if document["status"] == "succeeded" {
            assert_eq!(document["result"]["markets_total"], 2);
            break;
        }
        assert!(
            Instant::now() < deadline,
            "multi-market L2 run timed out: {document}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn multi_market_depth_experiment_sync_and_run_handle_share_candidate_engine() {
    let (paths, base) = depth_fixture();
    let target_strategy_id = base["markets"][0]["strategy"]["strategy_id"]
        .as_str()
        .unwrap()
        .to_string();
    let mut spec = json!({
        "schema_version": 1,
        "experiment_id": "api-multi-market-depth-grid",
        "target_strategy_id": target_strategy_id,
        "base": base,
        "parameter_space": [{"name":"quantity_raw","values":[500_000_000_i64,1_000_000_000_i64]}],
        "strategy_kinds": ["sma_cross", "momentum"]
    });
    let response = app_surface::multi_market_depth_experiment(&spec.to_string(), &paths);
    assert_eq!(response.status, 200, "{}", response.body);
    let result: Value = serde_json::from_str(&response.body).unwrap();
    assert_eq!(result["total_candidates"], 4);
    assert_eq!(result["completed_candidates"], 4);
    assert_eq!(result["ranking"].as_array().unwrap().len(), 4);
    let sync_artifact = PathBuf::from(result["artifact_path"].as_str().unwrap());
    let sync_experiment_dir = paths.artifact_root.join("api-multi-market-depth-grid");
    assert!(
        sync_artifact.is_file(),
        "missing sync artifact: {sync_artifact:?}"
    );
    assert_eq!(
        sync_artifact.parent().unwrap().canonicalize().unwrap(),
        sync_experiment_dir.canonicalize().unwrap(),
        "sync artifact must be written under one experiment directory"
    );

    spec["experiment_id"] = json!("api-multi-market-depth-grid-async");
    let registry = AppRunRegistry::default();
    let started = app_surface::route_app_run(
        "POST",
        "/app/run-multi-market-depth-experiment/start",
        "",
        &spec.to_string(),
        &paths,
        &registry,
        None,
    )
    .unwrap();
    assert_eq!(started.status, 200, "{}", started.body);
    let deadline = Instant::now() + Duration::from_secs(5);
    let async_candidates;
    loop {
        let status = registry.status("api-multi-market-depth-grid-async");
        let document: Value = serde_json::from_str(&status.body).unwrap();
        if document["status"] == "succeeded" {
            assert_eq!(document["result"]["total_candidates"], 4);
            assert_eq!(document["result"]["ranking"].as_array().unwrap().len(), 4);
            let async_artifact =
                PathBuf::from(document["result"]["artifact_path"].as_str().unwrap());
            let async_experiment_dir = paths
                .artifact_root
                .join("api-multi-market-depth-grid-async");
            assert!(
                async_artifact.is_file(),
                "missing async artifact: {async_artifact:?}"
            );
            assert_eq!(
                async_artifact.parent().unwrap().canonicalize().unwrap(),
                async_experiment_dir.canonicalize().unwrap(),
                "async artifact must be written under one experiment directory"
            );
            async_candidates = document["result"]["completed_candidates"].as_u64().unwrap();
            break;
        }
        assert!(
            Instant::now() < deadline,
            "multi-market L2 experiment timed out: {document}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    println!(
        "MULTI_MARKET_DEPTH_EXPERIMENT_OBSERVED sync_candidates={} async_candidates={} rankings=4 shared_engine=true",
        result["completed_candidates"].as_u64().unwrap(),
        async_candidates
    );
}
