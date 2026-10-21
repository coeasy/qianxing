//! 读模型端到端的"缺席 ≠ 零"（V12 R1）：`report` 与 `status` 念一份摘要时，
//! 缺键必须印 `absent`，写过的 0 必须仍印 0。单元侧（`src/tests/report_readout.rs`）
//! 证明排版函数正确，本文件证明**真命令**走的确实是这条排版，而不是别处的旧文案。

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn deploy(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|crates| crates.parent())
        .expect("仓库根目录")
        .join("deploy")
        .join(name)
}

fn temp_root(label: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "qianxing-report-readout-{label}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(root.join("data/runs")).expect("创建用例目录失败");
    root
}

fn run(args: &[&str]) -> (i32, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args(args)
        .output()
        .expect("启动 qx-cli 失败");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

/// 把示例运行时配置的 `data_dir` 指到用例临时目录，其余保持原样（`status` 要读的就是真配置）。
fn runtime_pointing_at(root: &Path) -> PathBuf {
    let template = std::fs::read_to_string(deploy("qianxing.runtime.example.json"))
        .expect("读取示例运行时配置失败");
    let mut config: serde_json::Value = serde_json::from_str(&template).unwrap();
    config["storage"]["data_dir"] =
        serde_json::Value::String(root.join("data").to_string_lossy().into_owned());
    let path = root.join("runtime.json");
    std::fs::write(&path, serde_json::to_string_pretty(&config).unwrap())
        .expect("写入用例运行时配置失败");
    path
}

fn write_summary(root: &Path, name: &str, summary: &serde_json::Value) -> PathBuf {
    let path = root.join("data/runs").join(name);
    std::fs::write(&path, serde_json::to_string_pretty(summary).unwrap())
        .expect("写入用例摘要失败");
    path
}

/// v1 世代：没有 `input`/`account`/`replay` 块，`metrics` 里也没写收益。
/// 这些格子的正确答案是"这个世代没声明过"，而不是印一个 0。
fn v1_summary_without_those_blocks() -> serde_json::Value {
    serde_json::json!({
        "schema_version": 1,
        "strategy_id": "ema_cross",
        "bars": 120,
        "input_data_hash": "deadbeef",
        "metrics": { "fees_raw": "0" },
    })
}

#[test]
fn report_refuses_legacy_summary_before_printing_undeclared_fields() {
    let root = temp_root("v1");
    let summary = write_summary(&root, "v1.summary.json", &v1_summary_without_those_blocks());
    let (code, stdout, stderr) = run(&["report", &summary.to_string_lossy()]);
    assert_eq!(code, 2, "report 不得读取旧世代摘要: {stdout}");
    assert!(stderr.contains("schema_version=1 已拒绝"), "{stderr}");
    assert!(
        stderr.contains("qx-cli backtest <runtime.json>"),
        "{stderr}"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn report_refuses_legacy_summary_before_printing_recompute_verdict() {
    // 真命令那一格的原文：没有 input 块的产物必须被念成"未经核对"，而不是整栏消失。
    // 排版函数里删掉这一行不会让上面任何断言变红，所以这条单独问一次。
    let root = temp_root("verdict");
    let summary = write_summary(&root, "v1.summary.json", &v1_summary_without_those_blocks());
    let (code, stdout, stderr) = run(&["report", &summary.to_string_lossy()]);
    assert_eq!(code, 2, "report 不得读取旧世代摘要: {stdout}");
    assert!(stderr.contains("schema_version=1 已拒绝"), "{stderr}");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn report_refuses_v4_summary_even_when_it_declares_zero_values() {
    let root = temp_root("v4");
    let summary = write_summary(
        &root,
        "v4.summary.json",
        &serde_json::json!({
            "schema_version": 4,
            "strategy_id": "ema_cross",
            "instrument": "BTCUSDT",
            "bars": 120,
            "fills": 0,
            "result_hash": "cafe",
            "account": { "initial_cash_raw": "100000000000000", "source": "config" },
            "metrics": { "return_bps": 0, "max_drawdown_bps": 0, "fees_raw": "0" },
        }),
    );
    let (code, stdout, stderr) = run(&["report", &summary.to_string_lossy()]);
    assert_eq!(code, 2, "report 不得读取旧世代摘要: {stdout}");
    assert!(stderr.contains("schema_version=4 已拒绝"), "{stderr}");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn status_latest_backtest_line_shares_the_same_readout() {
    let root = temp_root("status");
    let runtime = runtime_pointing_at(&root);
    write_summary(&root, "v1.summary.json", &v1_summary_without_those_blocks());
    let (code, stdout, stderr) = run(&["status", &runtime.to_string_lossy()]);
    assert_eq!(code, 0, "status 失败: {stderr}");
    assert!(
        stdout.contains("[Latest Backtest] schema_version=1"),
        "status 要先报产物世代: {stdout}"
    );
    assert!(
        stdout.contains("fills=absent") && stdout.contains("return_bps=absent"),
        "status 把缺键印成了 0: {stdout}"
    );
    assert!(
        !stdout.contains("fills=0"),
        "同一份摘要里两个答案并存，说明排版有两处真值: {stdout}"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn status_reports_no_summary_instead_of_an_empty_backtest() {
    let root = temp_root("status-empty");
    let runtime = runtime_pointing_at(&root);
    let (code, stdout, stderr) = run(&["status", &runtime.to_string_lossy()]);
    assert_eq!(code, 0, "status 在无产物时也要成功: {stderr}");
    assert!(
        stdout.contains("[Latest Backtest] 暂无已保存回测摘要"),
        "没有产物要说没有产物: {stdout}"
    );
    assert!(
        !stdout.contains("schema_version="),
        "无产物时不该念出一份摘要: {stdout}"
    );
    let _ = std::fs::remove_dir_all(root);
}
