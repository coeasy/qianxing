//! `reconcile` CLI 的真实进程行为合同。

use std::process::Command;

/// V10 §4.3 第 3 项：`reconcile` 无参曾经打印两份手写向量的“差异”，看起来像真对账能力。
#[test]
fn reconcile_without_sources_is_a_usage_error_not_a_demo() {
    let output = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .arg("reconcile")
        .output()
        .expect("启动 qx-cli 失败");
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert_eq!(
        output.status.code(),
        Some(2),
        "reconcile 缺少本地/远端来源必须是用法错误并退出 2:\n{stdout}{stderr}"
    );
    assert!(
        stderr.contains("本地") && stderr.contains("远端"),
        "用法错误必须点名两个必需输入: {stderr}"
    );
    assert!(
        !stdout.contains("差异数="),
        "不得再打印硬编码假订单的演示差异:\n{stdout}"
    );
}
