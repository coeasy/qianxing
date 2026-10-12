use crate::*;
use qx_app::MultiLegBacktestOutcome;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn fixture_paths() -> (app_surface::AppPaths, String) {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("target")
        .join(format!("qx-api-multi-leg-{}-{unique}", std::process::id()));
    let data_root = root.join("data");
    std::fs::create_dir_all(&data_root).unwrap();
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy");
    for (source_name, target_name) in [
        (
            "qianxing.bar-frame.pairs-primary.example.json",
            "primary.json",
        ),
        (
            "qianxing.bar-frame.pairs-reference.example.json",
            "reference.json",
        ),
    ] {
        std::fs::copy(fixtures.join(source_name), data_root.join(target_name)).unwrap();
    }
    let paths = app_surface::AppPaths {
        data_root: data_root.clone(),
        artifact_root: root.join("artifacts"),
    };
    let spec = serde_json::json!({
        "schema_version": 1,
        "run_id": "api-multi-leg",
        "primary_bars_path": data_root.join("primary.json"),
        "reference_bars_path": data_root.join("reference.json"),
        "settlement_currency": "USDT",
        "primary_initial_cash_raw": 1_000_000_000_000_000_i128,
        "reference_initial_cash_raw": 1_000_000_000_000_000_i128,
        "seed": 9,
        "output_dir": "client-controlled-path-is-overridden",
        "strategy": {
            "kind": "pairs_arbitrage",
            "strategy_id": "api-pairs",
            "quantity_raw": qx_core::SCALE,
            "fast_window": 5,
            "slow_window": 20,
            "period": 14,
            "threshold_bps": 100,
        },
    });
    (paths, spec.to_string())
}

fn multi_account_fixture() -> (app_surface::AppPaths, String) {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("target")
        .join(format!(
            "qx-api-multi-account-{}-{unique}",
            std::process::id()
        ));
    let data_root = root.join("data");
    std::fs::create_dir_all(&data_root).unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy/qianxing.bar-frame.example.json");
    std::fs::copy(&fixture, data_root.join("first.json")).unwrap();
    std::fs::copy(&fixture, data_root.join("second.json")).unwrap();
    let paths = app_surface::AppPaths {
        data_root,
        artifact_root: root.join("artifacts"),
    };
    let make_run = |run_id: &str, account_id: &str, bars_path: &str| {
        serde_json::json!({
            "schema_version": 1,
            "run_id": run_id,
            "instrument": "BTCUSDT.BINANCE",
            "bars_path": bars_path,
            "settlement_currency": "USDT",
            "initial_cash_raw": 1_000_000_000_000_000_i128,
            "seed": 7,
            "output_dir": "client-controlled-path-is-overridden",
            "account_id": account_id,
            "strategy": {
                "kind": "sma_cross",
                "strategy_id": run_id,
                "quantity_raw": qx_core::SCALE,
                "fast_window": 2,
                "slow_window": 4,
                "period": 14,
                "threshold_bps": 0,
            },
        })
    };
    let body = serde_json::json!({
        "schema_version": 1,
        "batch_id": "api-multi-account",
        "runs": [
            make_run("api-account-one", "account-one", "first.json"),
            make_run("api-account-two", "account-two", "second.json"),
        ],
    })
    .to_string();
    (paths, body)
}

#[test]
fn multi_leg_http_facade_uses_service_paths_and_the_same_outcome_contract() {
    let (paths, body) = fixture_paths();
    let response = app_surface::multi_leg_backtest(&body, &paths);
    assert_eq!(response.status, 200, "{}", response.body);
    let outcome: MultiLegBacktestOutcome = serde_json::from_str(&response.body).unwrap();
    assert_eq!(outcome.run_id, "api-multi-leg");
    assert!(PathBuf::from(&outcome.attribution_artifact)
        .starts_with(paths.artifact_root.join(&outcome.run_id)));
    assert!(Path::new(&outcome.attribution_artifact).is_file());
}

#[test]
fn multi_leg_http_facades_reject_dot_segment_run_ids_before_registry_or_path_setup() {
    let (paths, body) = fixture_paths();
    let unsafe_body = body.replace("\"run_id\":\"api-multi-leg\"", "\"run_id\":\"..\"");
    let sync = app_surface::multi_leg_backtest(&unsafe_body, &paths);
    assert_eq!(sync.status, 400, "{}", sync.body);
    assert!(sync.body.contains("run_id"), "{}", sync.body);
    assert!(
        !paths.artifact_root.exists(),
        "invalid identity must be rejected before the output tree is created"
    );

    let registry = app_surface::AppRunRegistry::default();
    let asynchronous = app_surface::app_run_route(
        "POST",
        "/app/multi-leg-backtest/start",
        &unsafe_body,
        &paths,
        &registry,
    )
    .unwrap();
    assert_eq!(asynchronous.status, 400, "{}", asynchronous.body);
    let missing = registry.status("..");
    assert_eq!(missing.status, 404, "{}", missing.body);
    assert!(
        !paths.artifact_root.exists(),
        "invalid identity must not reserve a run or create output paths"
    );
}

#[test]
fn multi_leg_async_route_runs_to_a_terminal_result_through_the_shared_registry() {
    let (paths, body) = fixture_paths();
    let registry = app_surface::AppRunRegistry::open(paths.artifact_root.join(".app-runs"));
    let started = app_surface::app_run_route(
        "POST",
        "/app/multi-leg-backtest/start",
        &body,
        &paths,
        &registry,
    )
    .unwrap();
    assert_eq!(started.status, 200, "{}", started.body);
    let started_json: serde_json::Value = serde_json::from_str(&started.body).unwrap();
    assert_eq!(started_json["run_id"], "api-multi-leg");
    let deadline = Instant::now() + Duration::from_secs(5);
    let record = paths.artifact_root.join(".app-runs/api-multi-leg.json");
    let completed = loop {
        if let Ok(body) = std::fs::read_to_string(&record) {
            let document: serde_json::Value = serde_json::from_str(&body).unwrap();
            if document["status"] == "succeeded" {
                break document;
            }
        }
        assert!(
            Instant::now() < deadline,
            "multi-leg handle did not persist a terminal state"
        );
        std::thread::sleep(Duration::from_millis(2));
    };
    drop(registry);

    let reopened = app_surface::AppRunRegistry::open(paths.artifact_root.join(".app-runs"));
    let restored = reopened.status("api-multi-leg");
    let restored_json: serde_json::Value = serde_json::from_str(&restored.body).unwrap();
    assert_eq!(restored.status, 200);
    assert_eq!(restored_json["status"], completed["status"]);
    assert_eq!(restored_json["result"]["run_id"], "api-multi-leg");
    let events = app_surface::app_run_route(
        "GET",
        "/app/runs/api-multi-leg/events",
        "",
        &paths,
        &reopened,
    )
    .unwrap();
    let events_json: serde_json::Value = serde_json::from_str(&events.body).unwrap();
    assert_eq!(events.status, 200, "{}", events.body);
    assert_eq!(events_json[0]["kind"], "started");
    assert_eq!(events_json[1]["kind"], "succeeded");
    let tail = app_surface::app_run_route(
        "GET",
        "/app/runs/api-multi-leg/events?after_seq=1",
        "",
        &paths,
        &reopened,
    )
    .unwrap();
    let tail_json: serde_json::Value = serde_json::from_str(&tail.body).unwrap();
    assert_eq!(tail_json.as_array().unwrap().len(), 1);
    assert_eq!(tail_json[0]["seq"], 2);
    let mut service = ApiService::new(ApiState::default());
    service.app_paths = paths;
    service.app_runs = reopened;
    let through_http = service.handle("GET", "/app/runs/api-multi-leg/events?after_seq=1", "", 1);
    assert_eq!(through_http.status, 200, "{}", through_http.body);
    let through_http: serde_json::Value = serde_json::from_str(&through_http.body).unwrap();
    assert_eq!(through_http[0]["kind"], "succeeded");
}

#[test]
fn multi_account_async_route_runs_to_a_terminal_result_through_the_shared_registry() {
    let (paths, body) = multi_account_fixture();
    let registry = app_surface::AppRunRegistry::open(paths.artifact_root.join(".app-runs"));
    let started = app_surface::app_run_route(
        "POST",
        "/app/multi-account-backtest/start",
        &body,
        &paths,
        &registry,
    )
    .unwrap();
    assert_eq!(started.status, 200, "{}", started.body);
    let started_json: serde_json::Value = serde_json::from_str(&started.body).unwrap();
    assert_eq!(started_json["run_id"], "api-multi-account");

    let deadline = Instant::now() + Duration::from_secs(5);
    let completed = loop {
        let response = registry.status("api-multi-account");
        let document: serde_json::Value = serde_json::from_str(&response.body).unwrap();
        if document["status"] == "succeeded" {
            break document;
        }
        assert!(
            Instant::now() < deadline,
            "multi-account run did not finish: {}",
            response.body
        );
        std::thread::sleep(Duration::from_millis(2));
    };
    assert_eq!(completed["result"]["batch_id"], "api-multi-account");
    assert_eq!(completed["result"]["summary"]["accounts_succeeded"], 2);
    assert_eq!(completed["result"]["runs"].as_array().unwrap().len(), 2);

    let events = app_surface::app_run_route(
        "GET",
        "/app/runs/api-multi-account/events",
        "",
        &paths,
        &registry,
    )
    .unwrap();
    let events_json: serde_json::Value = serde_json::from_str(&events.body).unwrap();
    assert_eq!(events.status, 200, "{}", events.body);
    assert_eq!(events_json[0]["kind"], "started");
    assert_eq!(events_json[1]["kind"], "succeeded");
}
