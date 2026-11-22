use super::*;
use qx_app::{BacktestDatasetInput, BuiltinStrategySpec};

#[test]
fn bar_verify_restricts_paths_and_run_record_indirections_to_artifact_root() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let root = repo
        .join("target")
        .join(format!("qx-api-verify-{}-{nonce}", std::process::id()));
    let artifact_root = root.join("artifacts");
    std::fs::create_dir_all(&artifact_root).unwrap();
    let bars = repo.join("deploy/qianxing.bar-frame.example.json");
    let strategy =
        BuiltinStrategySpec::new(qx_strategy::BuiltinStrategyKind::SmaCross, "verify-sma");
    let spec = BacktestSpec::new(
        BacktestDatasetInput {
            run_id: "rooted-verify".into(),
            instrument: "BTCUSDT.BINANCE".into(),
            bars_path: bars.to_string_lossy().into_owned(),
        },
        "USDT",
        100_000 * 1_000_000_000,
        7,
        artifact_root.to_string_lossy().into_owned(),
        strategy,
    );
    let outcome = run_backtest(&spec, &context("rooted-verify")).unwrap();
    let paths = AppPaths {
        data_root: root.clone(),
        artifact_root: artifact_root.clone(),
    };
    let valid = post_verify_run(&outcome.to_json().unwrap(), &paths);
    assert_eq!(valid.status, 200, "{}", valid.body);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&valid.body).unwrap()["verified"],
        true
    );

    let outside = root.join("outside.json");
    std::fs::write(&outside, "private-outside-sentinel").unwrap();
    let mut escaped = outcome.clone();
    escaped.artifacts.summary = outside.to_string_lossy().into_owned();
    let denied = post_verify_run(&escaped.to_json().unwrap(), &paths);
    assert_eq!(denied.status, 400, "{}", denied.body);
    assert!(!denied.body.contains("private-outside-sentinel"));

    #[cfg(unix)]
    {
        let link = artifact_root.join("outside-link.json");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        escaped.artifacts.summary = link.to_string_lossy().into_owned();
        let denied = post_verify_run(&escaped.to_json().unwrap(), &paths);
        assert_eq!(denied.status, 400, "{}", denied.body);
    }

    let mut missing = outcome.clone();
    missing.artifacts.summary = artifact_root
        .join("missing.json")
        .to_string_lossy()
        .into_owned();
    let missing = post_verify_run(&missing.to_json().unwrap(), &paths);
    assert_eq!(missing.status, 200, "{}", missing.body);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&missing.body).unwrap()["verified"],
        false
    );

    let summary_path = std::path::Path::new(&outcome.artifacts.summary);
    let mut summary: serde_json::Value =
        serde_json::from_slice(&std::fs::read(summary_path).unwrap()).unwrap();
    summary["run_record"] = serde_json::json!(outside);
    std::fs::write(summary_path, serde_json::to_vec(&summary).unwrap()).unwrap();
    let indirect = post_verify_run(&outcome.to_json().unwrap(), &paths);
    assert_eq!(indirect.status, 200, "{}", indirect.body);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&indirect.body).unwrap()["verified"],
        false
    );
    assert!(!indirect.body.contains("private-outside-sentinel"));
    assert!(
        indirect.body.contains("必须位于服务配置的产物根目录"),
        "{}",
        indirect.body
    );
}

#[test]
fn depth_verify_restricts_run_record_indirections_to_artifact_root() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let root = repo
        .join("target")
        .join(format!("qx-depth-verify-{}-{nonce}", std::process::id()));
    let data_root = root.join("data");
    let artifact_root = root.join("artifacts");
    std::fs::create_dir_all(&data_root).unwrap();
    std::fs::create_dir_all(&artifact_root).unwrap();
    let snapshots = (0..48).map(|index| {
        let phase = index % 20;
        let mid = if phase < 10 { 100 + phase * 2 } else { 100 + (20 - phase) * 2 };
        serde_json::json!({
            "instrument": {"symbol": "BTCUSDT", "venue": "BINANCE"},
            "ts": 1000 + index as u64 * 1000, "sequence": index as u64 + 1,
            "bids": [{"price": (mid - 1) as i128 * 1_000_000_000_i128, "qty": 100_000_000_000_i128}],
            "asks": [{"price": (mid + 1) as i128 * 1_000_000_000_i128, "qty": 100_000_000_000_i128}]
        })
    }).collect::<Vec<_>>();
    let depth = data_root.join("ticks.json");
    std::fs::write(
        &depth,
        serde_json::json!({"schema_version": 1, "source": "depth-verify-test",
        "instrument": {"symbol": "BTCUSDT", "venue": "BINANCE"}, "snapshots": snapshots})
        .to_string(),
    )
    .unwrap();
    let spec = serde_json::json!({"schema_version": 1, "run_id": "rooted-depth",
        "depth_path": depth, "tier": "l1", "settlement_currency": "USDT",
        "initial_cash_raw": 1_000_000_000_000_000_i128, "fee_bps": 0,
        "latency_snapshots": 0, "queue_position_bps": 0, "market_impact_bps": 0,
        "output_dir": "server-controlled", "strategy": {"kind": "sma_cross", "strategy_id": "depth-sma",
        "quantity_raw": 1_000_000_000_i128, "fast_window": 2, "slow_window": 3}});
    let paths = AppPaths {
        data_root,
        artifact_root: artifact_root.clone(),
    };
    let run = depth_backtest(&spec.to_string(), &paths);
    assert_eq!(run.status, 200, "{}", run.body);
    let outcome = DepthBacktestOutcome::from_json(&run.body).unwrap();
    let verified = post_verify_depth_run(&run.body, &paths);
    assert_eq!(verified.status, 200, "{}", verified.body);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&verified.body).unwrap()["verified"],
        true
    );

    let outside = root.join("outside.json");
    std::fs::write(&outside, "private-depth-sentinel").unwrap();
    let mut escaped = outcome.clone();
    escaped.artifacts.summary = outside.to_string_lossy().into_owned();
    let denied = post_verify_depth_run(&escaped.to_json().unwrap(), &paths);
    assert_eq!(denied.status, 400, "{}", denied.body);
    let summary_path = std::path::Path::new(&outcome.artifacts.summary);
    let mut summary: serde_json::Value =
        serde_json::from_slice(&std::fs::read(summary_path).unwrap()).unwrap();
    summary["run_record"] = serde_json::json!(outside);
    std::fs::write(summary_path, serde_json::to_vec(&summary).unwrap()).unwrap();
    let indirect = post_verify_depth_run(&run.body, &paths);
    assert_eq!(indirect.status, 200, "{}", indirect.body);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&indirect.body).unwrap()["verified"],
        false
    );
    assert!(!indirect.body.contains("private-depth-sentinel"));
    assert!(
        indirect.body.contains("必须位于服务配置的产物根目录"),
        "{}",
        indirect.body
    );
}
