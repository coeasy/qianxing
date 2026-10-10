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
    let registry = app_surface::AppRunRegistry::default();
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
    loop {
        let status =
            app_surface::app_run_route("GET", "/app/runs/api-multi-leg", "", &paths, &registry)
                .unwrap();
        let document: serde_json::Value = serde_json::from_str(&status.body).unwrap();
        if document["status"] == "succeeded" {
            assert_eq!(document["result"]["run_id"], "api-multi-leg");
            break;
        }
        assert!(
            Instant::now() < deadline,
            "multi-leg handle did not finish: {}",
            status.body
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}
