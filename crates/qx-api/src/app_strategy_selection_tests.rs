use super::*;
use serde_json::{json, Value};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
fn http_strategy_discovery_matches_the_shared_rust_catalog() {
    let response = ApiService::new(ApiState::default()).handle("GET", "/app/strategies", "", 0);
    assert_eq!(response.status, 200, "{}", response.body);
    let catalog: Value = serde_json::from_str(&response.body).unwrap();
    let names = catalog
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["name"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        qx_strategy::BuiltinStrategyKind::ALL
            .iter()
            .map(|kind| kind.name())
            .collect::<Vec<_>>()
    );
    assert_eq!(catalog.as_array().unwrap().len(), 17);
    assert!(catalog.as_array().unwrap().iter().all(|entry| {
        entry["description"]
            .as_str()
            .is_some_and(|value| !value.is_empty())
            && entry["signal_knobs"].is_array()
            && entry["requires_reference_leg"].is_boolean()
    }));
}

#[test]
fn http_capability_discovery_is_versioned_and_does_not_claim_runtime_readiness() {
    let response = ApiService::new(ApiState::default()).handle("GET", "/app/capabilities", "", 0);
    assert_eq!(response.status, 200, "{}", response.body);
    let catalog: Value = serde_json::from_str(&response.body).unwrap();
    assert_eq!(catalog["schema_version"], 1);
    assert_eq!(catalog["execution_backend"], "shared-rust");
    assert_eq!(catalog["runtime_preflight_required"], true);
    let capabilities = catalog["capabilities"].as_array().unwrap();
    for id in [
        "backtest.bar",
        "backtest.tick_orderbook",
        "backtest.multi_leg",
        "backtest.multi_market",
        "backtest.multi_account",
        "experiments.parameter_grid",
        "paper.commands",
        "live.commands",
        "run.lifecycle_events",
    ] {
        assert!(
            capabilities.iter().any(|capability| capability["id"] == id),
            "missing {id}"
        );
    }
    let parameter_grid = capabilities
        .iter()
        .find(|capability| capability["id"] == "experiments.parameter_grid")
        .expect("parameter-grid capability is discoverable");
    let routes = parameter_grid["routes"].as_array().unwrap();
    for route in [
        "/app/run-experiment",
        "/app/run-depth-experiment",
        "/app/run-multi-leg-experiment",
        "/app/run-multi-market-experiment",
        "/app/run-multi-market-depth-experiment",
        "/app/run-multi-market-depth-experiment/start",
    ] {
        assert!(
            routes.iter().any(|candidate| candidate == route),
            "parameter-grid capability omits implemented route {route}"
        );
    }
    assert!(catalog["limitations"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| { item.as_str().is_some_and(|text| text.contains("runtime")) }));
}

#[test]
fn experiment_http_route_runs_the_multi_strategy_selection_on_the_shared_app_use_case() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("target")
        .join(format!(
            "qx-api-strategy-selection-{}-{unique}",
            std::process::id()
        ));
    let data_root = root.join("data");
    std::fs::create_dir_all(&data_root).unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy")
        .join("qianxing.bar-frame.example.json");
    let bars_path = data_root.join("bars.json");
    std::fs::copy(fixture, &bars_path).unwrap();

    let body = json!({
        "schema_version": 1,
        "experiment_id": "api-classic-strategy-selection",
        "base": {
            "schema_version": 1,
            "run_id": "unused-api-selection",
            "instrument": "BTCUSDT.BINANCE",
            "bars_path": bars_path,
            "settlement_currency": "USDT",
            "initial_cash_raw": 100_000_000_000_000_i128,
            "seed": 7,
            "output_dir": "caller-output-is-overridden",
            "strategy": {
                "kind": "sma_cross",
                "strategy_id": "api-classic-selection",
                "quantity_raw": 1_000_000_000_i128,
                "fast_window": 2,
                "slow_window": 4,
                "period": 14,
                "threshold_bps": 100
            }
        },
        "parameter_space": [],
        "strategy_kinds": [
            "sma_cross", "ema_cross", "macd", "rsi", "bollinger",
            "donchian_breakout", "momentum", "mean_reversion", "grid", "atr_trend"
        ],
        "split_plan": {"train_bars": 20, "test_bars": 10, "walk_forward": false, "step_bars": 0}
    })
    .to_string();
    let paths = app_surface::AppPaths {
        data_root,
        artifact_root: root.join("artifacts"),
    };
    let mut service = ApiService::new(ApiState::default());
    service.app_paths = paths;
    let response = service.handle("POST", "/app/run-experiment", &body, 1);
    assert_eq!(response.status, 200, "{}", response.body);
    let result: Value = serde_json::from_str(&response.body).unwrap();
    assert_eq!(result["total_candidates"], 10);
    assert_eq!(result["succeeded_candidates"], 10);
    assert_eq!(result["candidates"].as_array().unwrap().len(), 10);
    assert_eq!(result["candidates"][0]["strategy_kind"], "atr_trend");
    assert_eq!(result["comparison"]["runs"].as_array().unwrap().len(), 10);
    let fold = &result["out_of_sample"]["folds"][0];
    assert!(fold["selected_strategy_kind"].is_string());
    assert_eq!(
        fold["selected_strategy_kind"],
        result["candidates"][fold["selected_ordinal"].as_u64().unwrap() as usize - 1]
            ["strategy_kind"]
    );
}
