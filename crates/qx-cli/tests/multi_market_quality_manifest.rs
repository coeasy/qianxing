use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn root() -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .unwrap()
        .join("target")
        .join(format!("multi-market-quality-cli-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn cli(arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args(arguments)
        .output()
        .expect("start qx-cli")
}

#[test]
fn multi_market_quality_reports_survive_evidence_pack_and_offline_verification() {
    let root = root();
    let deploy = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy");
    let source: Value = serde_json::from_slice(
        &std::fs::read(deploy.join("qianxing.bar-frame.example.json")).unwrap(),
    )
    .unwrap();
    let instruments = ["BTCUSDT.BINANCE", "ETHUSDT.BINANCE", "SOLUSDT.BINANCE"];
    let mut markets = Vec::new();
    let mut quality_path = None;
    for (index, instrument) in instruments.iter().enumerate() {
        let frame_path = root.join(format!("market-{index}.json"));
        let mut frame = source.clone();
        frame["instrument"] = json!(instrument);
        std::fs::write(&frame_path, serde_json::to_vec(&frame).unwrap()).unwrap();
        if index == 0 {
            let manifest = root.join("market-0.quality.json");
            let output = cli(&[
                "data-validate",
                frame_path.to_str().unwrap(),
                "research:market-0",
                "daily-v1",
                "1000",
                manifest.to_str().unwrap(),
                "--timezone",
                "UTC",
            ]);
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            quality_path = Some(manifest);
        }
        let mut market = json!({
            "instrument": instrument,
            "bars_path": frame_path,
            "priority": index,
            "market_spec": null,
            "fee_schedule": null,
            "strategies": [{
                "priority": 0,
                "reserved_capital_raw": 20_000_000_000_000_i128,
                "strategy": {
                    "kind": "sma_cross",
                    "strategy_id": format!("multi-quality-{index}"),
                    "quantity_raw": 1_000_000_000_i128,
                    "fast_window": 1,
                    "slow_window": 3
                }
            }]
        });
        if index == 0 {
            market["dataset_manifest_path"] = json!(quality_path.as_ref().unwrap());
        }
        markets.push(market);
    }
    let artifact_root = root.join("runs");
    let experiment_markets = markets.clone();
    let spec = root.join("multi-market.json");
    std::fs::write(
        &spec,
        json!({
            "schema_version": 1,
            "run_id": "multi-market-quality-cli",
            "account_id": "multi-market-quality-account",
            "settlement_currency": "USDT",
            "initial_cash_raw": 100_000_000_000_000_i128,
            "seed": 7,
            "output_dir": artifact_root,
            "markets": markets
        })
        .to_string(),
    )
    .unwrap();
    let run = cli(&["app", "multi-market-backtest", spec.to_str().unwrap()]);
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let outcome: Value = serde_json::from_slice(&run.stdout).unwrap();
    let summary = PathBuf::from(outcome["artifact_path"].as_str().unwrap());
    let package = cli(&[
        "evidence",
        "pack-multi-market-run",
        summary.to_str().unwrap(),
    ]);
    assert!(
        package.status.success(),
        "{}",
        String::from_utf8_lossy(&package.stderr)
    );
    let evidence_path = summary.with_file_name("multi-market-quality-cli.evidence.json");
    let evidence: Value = serde_json::from_slice(&std::fs::read(&evidence_path).unwrap()).unwrap();
    let component = evidence
        .pointer("/generation/run/dataset/components/0")
        .unwrap();
    assert!(component["manifest_fingerprint"].as_str().is_some());
    assert_eq!(component["quality_report"]["missing_intervals"], 0);
    assert!(component["quality_manifest_path"].as_str().is_some());
    assert!(
        evidence["generation"]["run"]["artifacts"]
            .as_array()
            .unwrap()
            .len()
            > 4
    );

    let verified = cli(&["evidence", "verify-run", evidence_path.to_str().unwrap()]);
    assert!(
        verified.status.success(),
        "{}",
        String::from_utf8_lossy(&verified.stderr)
    );

    let experiment_spec = root.join("multi-market-experiment.json");
    std::fs::write(
        &experiment_spec,
        json!({
            "schema_version": 1,
            "experiment_id": "multi-market-quality-grid",
            "target_strategy_id": "multi-quality-0",
            "base": {
                "schema_version": 1,
                "run_id": "unused-quality-grid-base",
                "account_id": "quality-grid-account",
                "settlement_currency": "USDT",
                "initial_cash_raw": 100_000_000_000_000_i128,
                "seed": 7,
                "output_dir": root.join("experiment-runs"),
                "markets": experiment_markets
            },
            "parameter_space": [{ "name": "fast_window", "values": [1] }]
        })
        .to_string(),
    )
    .unwrap();
    let experiment_run = cli(&[
        "app",
        "run-multi-market-experiment",
        experiment_spec.to_str().unwrap(),
    ]);
    assert!(
        experiment_run.status.success(),
        "{}",
        String::from_utf8_lossy(&experiment_run.stderr)
    );
    let experiment: Value = serde_json::from_slice(&experiment_run.stdout).unwrap();
    let experiment_artifact = experiment["artifact_path"].as_str().unwrap();
    let portable = root.join("portable-experiment");
    let packed = cli(&[
        "evidence",
        "pack-multi-market-experiment",
        experiment_artifact,
        "--output-dir",
        portable.to_str().unwrap(),
    ]);
    assert!(
        packed.status.success(),
        "{}",
        String::from_utf8_lossy(&packed.stderr)
    );
    let portable_spec: Value =
        serde_json::from_slice(&std::fs::read(portable.join("experiment.spec.json")).unwrap())
            .unwrap();
    assert!(portable_spec
        .pointer("/base/markets/0/dataset_manifest_path")
        .and_then(Value::as_str)
        .is_some_and(|path| path.starts_with("inputs/market-01-quality.")));
    let bundle_verified = cli(&[
        "evidence",
        "verify-multi-market-experiment",
        portable.to_str().unwrap(),
        "--recompute",
    ]);
    assert!(
        bundle_verified.status.success(),
        "{}",
        String::from_utf8_lossy(&bundle_verified.stderr)
    );
}
