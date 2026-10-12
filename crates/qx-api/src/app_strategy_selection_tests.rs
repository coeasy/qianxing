use super::*;
use serde_json::{json, Value};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

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
        ]
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
}
