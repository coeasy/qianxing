use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn unique_root() -> PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .unwrap()
        .join("target")
        .join(format!(
            "multi-account-joint-evidence-{}-{stamp}",
            std::process::id()
        ));
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn cli(arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args(arguments)
        .output()
        .expect("start qx-cli")
}

fn market(inputs: &Path, base: &Value, index: usize, instrument: &str, strategy_id: &str) -> Value {
    let frame_path = inputs.join(format!("market-{index}.bar-frame.json"));
    let mut frame = base.clone();
    frame["instrument"] = json!(instrument);
    std::fs::write(&frame_path, serde_json::to_vec(&frame).unwrap()).unwrap();
    json!({
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
                "strategy_id": strategy_id,
                "quantity_raw": 1_000_000_000_i128,
                "fast_window": 1,
                "slow_window": 3
            }
        }]
    })
}

#[test]
fn joint_accounts_package_relocates_and_recomputes_after_source_removal() {
    let case_root = unique_root();
    let source_root = case_root.join("source-workspace");
    let inputs = source_root.join("inputs");
    std::fs::create_dir_all(&inputs).unwrap();
    let deploy = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy");
    let base: Value = serde_json::from_slice(
        &std::fs::read(deploy.join("qianxing.bar-frame.example.json")).unwrap(),
    )
    .unwrap();
    let okx_btc = market(&inputs, &base, 0, "BTC-USDT.OKX", "joint-okx-btc");
    let binance_eth = market(&inputs, &base, 1, "ETHUSDT.BINANCE", "joint-binance-eth");
    let binance_sol = market(&inputs, &base, 2, "SOLUSDT.BINANCE", "joint-binance-sol");
    let spec_path = source_root.join("joint-spec.json");
    std::fs::write(
        &spec_path,
        json!({
            "schema_version": 1,
            "run_id": "joint-portable-recompute",
            "settlement_currency": "USDT",
            "seed": 29,
            "output_dir": source_root.join("runs"),
            "accounts": [
                {
                    "exchange_id": "okx",
                    "account_id": "portfolio-a",
                    "initial_cash_raw": 100_000_000_000_000_i128,
                    "markets": [okx_btc]
                },
                {
                    "exchange_id": "binance",
                    "account_id": "portfolio-b",
                    "initial_cash_raw": 150_000_000_000_000_i128,
                    "markets": [binance_eth, binance_sol]
                }
            ]
        })
        .to_string(),
    )
    .unwrap();

    let run = cli(&[
        "app",
        "multi-account-joint-backtest",
        spec_path.to_str().unwrap(),
    ]);
    assert!(
        run.status.success(),
        "joint backtest: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    let outcome: Value = serde_json::from_slice(&run.stdout).unwrap();
    assert_eq!(outcome["accounts_total"], 2);
    assert_eq!(outcome["markets_total"], 3);
    let result_path = source_root.join("joint-result.json");
    std::fs::write(&result_path, serde_json::to_vec_pretty(&outcome).unwrap()).unwrap();

    let package = case_root.join("portable-package");
    let packed = cli(&[
        "evidence",
        "pack-multi-account-joint-run",
        result_path.to_str().unwrap(),
        "--spec",
        spec_path.to_str().unwrap(),
        "--output-dir",
        package.to_str().unwrap(),
    ]);
    assert!(
        packed.status.success(),
        "pack joint evidence: {}",
        String::from_utf8_lossy(&packed.stderr)
    );
    let checked = cli(&[
        "evidence",
        "verify-multi-account-joint-run",
        package.to_str().unwrap(),
    ]);
    assert!(
        checked.status.success(),
        "verify joint package: {}",
        String::from_utf8_lossy(&checked.stderr)
    );

    let relocated = case_root.join("relocated-package");
    std::fs::rename(&package, &relocated).unwrap();
    std::fs::remove_dir_all(&source_root).unwrap();
    let verified = cli(&[
        "evidence",
        "verify-multi-account-joint-run",
        relocated.to_str().unwrap(),
        "--recompute",
    ]);
    assert!(
        verified.status.success(),
        "relocated joint package must report and recompute after source removal: {}",
        String::from_utf8_lossy(&verified.stderr)
    );
    let verified: Value = serde_json::from_slice(&verified.stdout).unwrap();
    assert_eq!(verified["result_recomputed"], true);
    assert_eq!(verified["verified_account_count"], 2);
    assert_eq!(verified["verified_market_count"], 3);

    let joint_manifest_path = relocated.join("multi-account-joint-evidence.json");
    let joint_manifest_bytes = std::fs::read(&joint_manifest_path).unwrap();
    let joint_manifest: Value = serde_json::from_slice(&joint_manifest_bytes).unwrap();
    let mut traversal = joint_manifest.clone();
    traversal["parent_artifacts"]["joint_fills"]["path"] = "../outside.json".into();
    std::fs::write(
        &joint_manifest_path,
        serde_json::to_vec_pretty(&traversal).unwrap(),
    )
    .unwrap();
    let traversal_rejected = cli(&[
        "evidence",
        "verify-multi-account-joint-run",
        relocated.to_str().unwrap(),
    ]);
    assert!(!traversal_rejected.status.success());
    assert!(
        String::from_utf8_lossy(&traversal_rejected.stderr).contains("安全的包内相对路径"),
        "path traversal should be rejected before artifact access"
    );
    std::fs::write(&joint_manifest_path, &joint_manifest_bytes).unwrap();

    let child_evidence = relocated
        .join(
            joint_manifest["accounts"][0]["evidence_dir"]
                .as_str()
                .unwrap(),
        )
        .join("evidence.json");
    let mut tampered = std::fs::OpenOptions::new()
        .append(true)
        .open(child_evidence)
        .unwrap();
    tampered.write_all(b"tampered").unwrap();
    let rejected = cli(&[
        "evidence",
        "verify-multi-account-joint-run",
        relocated.to_str().unwrap(),
    ]);
    assert!(
        !rejected.status.success(),
        "a modified child EvidenceBundle must be rejected"
    );
    assert!(
        String::from_utf8_lossy(&rejected.stderr).contains("SHA-256"),
        "tamper rejection should identify the child evidence digest"
    );
    std::fs::remove_dir_all(case_root).unwrap();
}
