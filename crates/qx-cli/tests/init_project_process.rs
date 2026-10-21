//! Project initialization contracts exercised through the built CLI process.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_case_dir(label: &str) -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("target/qx-cli-test-tmp")
        .join(format!(
            "qianxing-cli-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
    std::fs::create_dir_all(&root).unwrap();
    root.canonicalize().unwrap()
}

fn init(runtime: &Path, profile: Option<&str>, strategy: Option<&str>) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_qx-cli"));
    command.arg("init").arg(runtime);
    if let Some(profile) = profile {
        command.args(["--profile", profile]);
    }
    if let Some(strategy) = strategy {
        command.args(["--strategy", strategy]);
    }
    let output = command.output().expect("启动 qx-cli init 失败");
    assert!(
        output.status.success(),
        "init 应成功生成项目资产:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn doctor_config_check_passes(runtime: &Path) {
    let output = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args(["doctor", runtime.to_string_lossy().as_ref(), "--json"])
        .output()
        .expect("启动 qx-cli doctor 失败");
    assert!(
        output.status.success(),
        "doctor 应能读取初始化配置:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .any(|check| { check["name"] == "config" && check["status"] == "pass" }));
}

#[test]
fn init_creates_self_contained_project_assets_and_builtin_strategy() {
    let root = temp_case_dir("init-process");
    let runtime = root.join("qianxing.runtime.json");
    init(&runtime, None, Some("macd"));

    let config: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&runtime).unwrap()).unwrap();
    assert_eq!(config["strategy"]["builtin_strategy"], "macd");
    assert_eq!(
        config["strategy"]["bars_snapshot_path"],
        "qianxing.bar-frame.example.json"
    );
    for asset in [
        "qianxing.scheduler.jobs.example.json",
        "qianxing.scheduler.paper-order-smoke.json",
        "qianxing.bar-frame.example.json",
        "qianxing.dataset-bundle.bar-frame.example.json",
        "qianxing.dataset-component.arrow.example.json",
        "qianxing.binance.spot.spec.json",
        "qianxing.strategy-target.paper.json",
        "README.qianxing.md",
    ] {
        assert!(root.join(asset).is_file(), "missing init asset {asset}");
    }
    doctor_config_check_passes(&runtime);

    let duplicate = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args([
            "init",
            runtime.to_string_lossy().as_ref(),
            "--strategy",
            "macd",
        ])
        .output()
        .expect("启动 qx-cli init 失败");
    assert!(
        !duplicate.status.success(),
        "init 不得覆盖已存在的运行时配置"
    );
}

#[test]
fn init_profiles_rewrite_deploy_paths_and_validate_references() {
    for profile in ["paper", "ccxt", "ashare", "multi-venue", "backtest"] {
        let root = temp_case_dir(&format!("profile-{profile}"));
        let runtime = root.join("qianxing.runtime.json");
        init(&runtime, Some(profile), None);
        let runtime_text = std::fs::read_to_string(&runtime).unwrap();
        assert!(
            !runtime_text.contains("deploy/"),
            "profile={profile} leaked repository-relative deploy paths"
        );
        doctor_config_check_passes(&runtime);
    }
}

#[test]
fn doctor_json_is_machine_readable_and_never_claims_network_or_orders() {
    let root = temp_case_dir("doctor-process");
    let runtime = root.join("qianxing.runtime.json");
    init(&runtime, None, Some("macd"));

    let output = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args(["doctor", runtime.to_string_lossy().as_ref(), "--json"])
        .output()
        .expect("启动 qx-cli doctor 失败");
    assert!(
        output.status.success(),
        "doctor 应成功返回结构化诊断:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["schema_version"], 2);
    assert_eq!(report["ok"], true);
    assert_eq!(report["network_accessed"], false);
    assert_eq!(report["orders_sent"], false);
    let checks = report["checks"].as_array().unwrap();
    assert!(checks
        .iter()
        .any(|check| { check["name"] == "config" && check["status"] == "pass" }));
    let build = checks
        .iter()
        .find(|check| check["name"] == "runtime_supervisor_build")
        .expect("doctor 必须报告监督器可构建性");
    assert_eq!(build["status"], "pass");
    assert!(build["message"]
        .as_str()
        .unwrap()
        .contains("不代表运行健康"));
}
