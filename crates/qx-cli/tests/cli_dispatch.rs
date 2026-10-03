//! CLI 命令分派契约：`cli.rs` 是唯一分派点，`verify`/`all` 共用同一条自校验链路，
//! 未知命令必须 fail-closed 退出 2 而不是静默落到默认演示。

use std::process::Command;

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

/// `verify` 只校验确定性内核；插件装配与 Paper 冒烟属于 `all` 的深度。
#[test]
fn verify_runs_the_deterministic_kernel_only() {
    let (code, stdout, stderr) = run(&["verify"]);
    assert_eq!(code, 0, "verify 失败: {stderr}");
    assert!(
        stdout.contains("同输入两次运行哈希一致 : true")
            && stdout.contains("改参数后哈希发生变化   : true"),
        "verify 必须完成重放双重校验:\n{stdout}"
    );
    assert!(
        !stdout.contains("全部自校验通过"),
        "verify 不应越过内核进入完整自校验:\n{stdout}"
    );
}

/// `all` 在 verify 的基础上继续走完插件装配与 Paper 主链路冒烟。
#[test]
fn all_extends_verify_with_plugin_and_paper_stages() {
    let (code, stdout, stderr) = run(&["all"]);
    assert_eq!(code, 0, "all 失败: {stderr}");
    assert!(
        stdout.contains("卯眼 · 插件装配") && stdout.contains("全部自校验通过 ✓"),
        "all 必须执行插件装配并给出总校验结论:\n{stdout}"
    );
    assert!(
        stdout.contains("针路 · PaperVenue"),
        "all 必须包含 Paper 主链路冒烟:\n{stdout}"
    );
}

/// 未知命令不得静默成功。
#[test]
fn unknown_command_fails_closed() {
    let (code, stdout, stderr) = run(&["definitely-not-a-command"]);
    assert_eq!(code, 2, "未知命令必须退出 2，实际 stdout:\n{stdout}");
    assert!(
        stderr.contains("未知命令"),
        "stderr 需点名未知命令: {stderr}"
    );
}
