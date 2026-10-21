//! Backtest argument behavior verified through the built CLI process.

use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_case_dir() -> PathBuf {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("target/qx-cli-test-tmp")
        .join(format!(
            "qianxing-cli-backtest-args-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
    std::fs::create_dir_all(&root).unwrap();
    root.canonicalize().unwrap()
}

/// V11 §4 P0 第 2 项（决策 E2）：`backtest --config` 曾被 clap 收下、在分派里丢弃，
/// 使用者以为换了风控与费用口径而实际什么都没生效。旗标整体必须按未知参数拒绝，
/// 而真正读取配置的子入口仍要在帮助中暴露它。
#[test]
fn backtest_rejects_the_config_flag_it_used_to_swallow() {
    let output = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args([
            "backtest",
            "--config",
            "deploy/qianxing.runtime.example.json",
        ])
        .output()
        .expect("启动 qx-cli 失败");
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert_eq!(
        output.status.code(),
        Some(2),
        "`backtest --config` 必须按用法错误退出 2，而不是静默接受:\n{stderr}"
    );
    assert!(
        stderr.contains("--config"),
        "错误必须点名被拒绝的旗标，让人知道它已不存在: {stderr}"
    );
    let help = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args(["backtest", "builtin", "--help"])
        .output()
        .expect("启动 qx-cli 失败");
    assert_eq!(
        help.status.code(),
        Some(0),
        "backtest builtin 的帮助必须可用"
    );
    assert!(
        String::from_utf8_lossy(&help.stdout).contains("--config"),
        "backtest builtin 仍须暴露 --config（它真的吃这份配置）"
    );
}

/// V12 R4-e：外层位置参数曾可与 `backtest builtin` 混写，但派发只读子命令输入，
/// 造成用户以为切换了配置和行情帧，实际执行另一份输入。混写必须拒绝，合法写法仍要进入执行链。
#[test]
fn backtest_subcommand_rejects_outer_positionals_it_never_reads() {
    let dir = temp_case_dir();
    let outer_runtime = dir.join("outer-runtime.json").display().to_string();
    let outer_frame = dir.join("outer-frame.csv").display().to_string();
    let inner_frame = dir.join("inner-frame.csv").display().to_string();
    let mixed = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args([
            "backtest",
            &outer_runtime,
            &outer_frame,
            "builtin",
            "sma_cross",
            &inner_frame,
        ])
        .output()
        .expect("启动 qx-cli 失败");
    let mixed_stderr = String::from_utf8_lossy(&mixed.stderr).to_string();
    assert_eq!(
        mixed.status.code(),
        Some(2),
        "外层位置参数与 backtest 子命令混写必须按用法错误退出 2，而不是静默丢弃:\n{mixed_stderr}"
    );
    assert!(
        mixed_stderr.contains("backtest builtin"),
        "错误必须点名子命令: {mixed_stderr}"
    );
    let normalize_path = |path: &str| path.replace('\\', "/");
    let normalized_stderr = normalize_path(&mixed_stderr);
    assert!(
        normalized_stderr.contains(&normalize_path(&outer_runtime)),
        "错误必须点名被丢弃的 runtime 路径 {outer_runtime}: {mixed_stderr}"
    );
    assert!(
        normalized_stderr.contains(&normalize_path(&outer_frame)),
        "错误必须点名被丢弃的 frame 路径 {outer_frame}: {mixed_stderr}"
    );
    assert!(
        mixed_stderr.contains("不会参与计算"),
        "错误必须说明外层参数不会参与计算: {mixed_stderr}"
    );

    for (label, args, consumed) in [
        (
            "仅子命令",
            &["builtin", "sma_cross", &inner_frame][..],
            &inner_frame,
        ),
        (
            "统一入口",
            &[outer_runtime.as_str(), outer_frame.as_str()][..],
            &outer_runtime,
        ),
    ] {
        let legal = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
            .arg("backtest")
            .args(args)
            .output()
            .expect("启动 qx-cli 失败");
        let stderr = String::from_utf8_lossy(&legal.stderr).to_string();
        assert!(
            normalize_path(&stderr).contains(&normalize_path(consumed)),
            "{label} 写法必须真的进入读自己那份输入的执行链，而不是被用法错误挡住:\n{stderr}"
        );
        assert!(
            !stderr.contains("不会参与计算"),
            "{label} 写法是合法形状，不得被新闸门拦下:\n{stderr}"
        );
    }
}
