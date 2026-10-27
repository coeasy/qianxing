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
    let coverage = &catalog["coverage_catalog"];
    assert_eq!(coverage["schema_version"], 1);
    let product_capabilities = coverage["capabilities"].as_array().unwrap();
    assert_eq!(product_capabilities.len(), 16);
    let binance = product_capabilities
        .iter()
        .find(|capability| capability["id"] == "qx.venue.binance_spot")
        .expect("stable product capability is discoverable");
    assert_eq!(binance["matrix_status"]["sandbox"], "unsupported");
    assert!(binance["maturity_refs"]
        .as_array()
        .unwrap()
        .contains(&json!("binance_direct")));
}

#[test]
fn product_capability_query_returns_support_limits_and_maturity_evidence_by_stable_id() {
    let service = ApiService::new(ApiState::default());
    let response = service.handle(
        "POST",
        "/app/capabilities/query",
        r#"{"capability_id":"qx.venue.binance_spot"}"#,
        0,
    );
    assert_eq!(response.status, 200, "{}", response.body);
    let result: Value = serde_json::from_str(&response.body).unwrap();
    let capability = &result["capability"];
    assert_eq!(capability["id"], "qx.venue.binance_spot");
    assert_eq!(capability["matrix"]["sandbox"], "unsupported");
    let direct = capability["maturity_evidence"]
        .as_array()
        .unwrap()
        .iter()
        .find(|evidence| evidence["id"] == "binance_direct")
        .expect("linked internal maturity entry is present");
    assert_eq!(direct["stages"]["implementation"], true);
    assert!(direct["unverified_stages"]
        .as_array()
        .unwrap()
        .contains(&json!("sandbox_tested")));

    let missing = service.handle(
        "POST",
        "/app/capabilities/query",
        r#"{"capability_id":"qx.venue.not_registered"}"#,
        1,
    );
    assert_eq!(missing.status, 404, "{}", missing.body);
    let malformed = service.handle(
        "POST",
        "/app/capabilities/query",
        r#"{"capability_id":"qx.venue.binance_spot","status":"supported"}"#,
        2,
    );
    assert_eq!(malformed.status, 400, "{}", malformed.body);
}

#[test]
fn product_capability_query_filters_declared_venue_product_and_account_scopes() {
    let service = ApiService::new(ApiState::default());
    let response = service.handle(
        "POST",
        "/app/capabilities/query",
        r#"{"venue":"BINANCE","product":"perpetual","account_mode":"hedge","margin_mode":"isolated"}"#,
        0,
    );
    assert_eq!(response.status, 200, "{}", response.body);
    let result: Value = serde_json::from_str(&response.body).unwrap();
    assert_eq!(result["schema_version"], 2);
    assert_eq!(result["match_count"], 1);
    let matched = &result["matches"][0];
    assert_eq!(matched["capability_id"], "qx.venue.ccxt_process");
    assert_eq!(matched["scope"]["venue"], "BINANCE");
    assert_eq!(matched["scope"]["product"], "perpetual");
    assert_eq!(matched["scope"]["account_mode"], "hedge");
    assert_eq!(matched["scope"]["margin_modes"][0], "cross");
    assert_eq!(matched["scope"]["margin_modes"][1], "isolated");
    assert_eq!(matched["scope"]["status"], "implemented-unverified");
    assert_eq!(matched["capability"]["matrix"]["sandbox"], "unsupported");

    let bitget_uta = service.handle(
        "POST",
        "/app/capabilities/query",
        r#"{"venue":"bitget","product":"spot","account_mode":"cash","account_model":"UTA"}"#,
        1,
    );
    assert_eq!(bitget_uta.status, 200, "{}", bitget_uta.body);
    let result: Value = serde_json::from_str(&bitget_uta.body).unwrap();
    assert_eq!(result["match_count"], 1);
    assert_eq!(
        result["matches"][0]["scope"]["account_models"][0],
        "classic"
    );
    assert_eq!(result["matches"][0]["scope"]["account_models"][1], "uta");

    let private_balance = service.handle(
        "POST",
        "/app/capabilities/query",
        r#"{"venue":"BITGET","product":"spot","account_model":"uta","operation":"balance.read"}"#,
        2,
    );
    assert_eq!(private_balance.status, 200, "{}", private_balance.body);
    let result: Value = serde_json::from_str(&private_balance.body).unwrap();
    assert_eq!(result["match_count"], 1);
    assert_eq!(
        result["matches"][0]["capability_id"],
        "qx.venue.ccxt_process"
    );
    assert_eq!(result["matches"][0]["scope"]["product"], "spot");
    assert!(result["matches"][0]["scope"]["operations"]
        .as_array()
        .unwrap()
        .contains(&json!("balance.read")));

    let account_settings = service.handle(
        "POST",
        "/app/capabilities/query",
        r#"{"venue":"BITGET","product":"perpetual","account_model":"uta","operation":"account_settings.read"}"#,
        3,
    );
    assert_eq!(account_settings.status, 200, "{}", account_settings.body);
    let result: Value = serde_json::from_str(&account_settings.body).unwrap();
    assert_eq!(result["match_count"], 1);
    assert!(result["matches"][0]["scope"]["operations"]
        .as_array()
        .unwrap()
        .contains(&json!("account_settings.read")));

    let cancel_scope = service.handle(
        "POST",
        "/app/capabilities/query",
        r#"{"venue":"OKX","product":"perpetual","account_mode":"hedge","operation":"cancel"}"#,
        4,
    );
    assert_eq!(cancel_scope.status, 200, "{}", cancel_scope.body);
    let result: Value = serde_json::from_str(&cancel_scope.body).unwrap();
    assert_eq!(result["match_count"], 1);
    assert_eq!(
        result["matches"][0]["capability_id"],
        "qx.venue.ccxt_process"
    );
    assert!(result["matches"][0]["scope"]["operations"]
        .as_array()
        .unwrap()
        .contains(&json!("cancel")));

    let unsupported = service.handle(
        "POST",
        "/app/capabilities/query",
        r#"{"venue":"BITGET","operation":"account.withdraw"}"#,
        5,
    );
    assert_eq!(unsupported.status, 400, "{}", unsupported.body);
    assert!(unsupported
        .body
        .contains("unsupported_capability_operation"));
}

#[test]
fn product_capability_scope_query_requires_a_filter_and_does_not_promote_acceptance() {
    let service = ApiService::new(ApiState::default());
    let empty = service.handle("POST", "/app/capabilities/query", "{}", 0);
    assert_eq!(empty.status, 400, "{}", empty.body);
    assert!(empty.body.contains("capability_query_filter_required"));

    let unmatched = service.handle(
        "POST",
        "/app/capabilities/query",
        r#"{"venue":"OKX","product":"spot","account_mode":"hedge"}"#,
        1,
    );
    assert_eq!(unmatched.status, 200, "{}", unmatched.body);
    let result: Value = serde_json::from_str(&unmatched.body).unwrap();
    assert_eq!(result["match_count"], 0);
    assert!(result["matches"].as_array().unwrap().is_empty());

    let malformed = service.handle("POST", "/app/capabilities/query", r#"{"venue":"  "}"#, 2);
    assert_eq!(malformed.status, 400, "{}", malformed.body);
    assert!(malformed.body.contains("venue_must_be_non_empty"));
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
