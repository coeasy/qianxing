//! 命令面的"机读输出"诚实性（V12 R4-f）：`--json` 只能出现在真的产出 JSON 的入口上。
//!
//! `config validate|fingerprint|lock` 曾各自声明一个 `--json`，实际只拿它压掉横幅，
//! stdout 照旧是 `[PASS] config validate 通过: …` 这类人读文本；帮助里也从没承诺这三个
//! 入口有机读输出，CI 与文档用的都是真的产 JSON 的 `config explain --json`。
//! 三个旗标因此整体摘掉：传它必须按未知参数拒绝，而不是继续假装支持。

use super::*;

/// 逐条确认旗标已不存在，且拒绝发生在任何副作用之前。
#[test]
fn config_subcommands_reject_the_json_flag_they_never_delivered() {
    for case in [
        vec!["validate", "--json"],
        vec!["fingerprint", "--json"],
        vec!["lock", "--json"],
        vec!["lock", "in.json", "out.json", "--json"],
    ] {
        let output = Command::new(qx_cli_binary())
            .arg("config")
            .args(&case)
            .output()
            .expect("启动 qx-cli 失败");
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        assert_eq!(
            output.status.code(),
            Some(2),
            "`config {}` 接受 --json 必须是用法错误并退出 2:\n{stderr}",
            case.join(" ")
        );
        assert!(
            stderr.contains("--json"),
            "错误必须点名被拒绝的旗标，让人知道它已不存在: {stderr}"
        );
    }
    // 上面的 `lock … out.json --json` 只有被 clap 挡下才不会写盘。
    assert!(
        !Path::new("out.json").exists(),
        "config lock 被拒时不得留下发布配置产物"
    );
}

/// 负向对照：真的产 JSON 的那两个入口一个都不能被顺手改坏。
#[test]
fn config_explain_and_doctor_still_emit_machine_readable_json() {
    for case in [
        vec!["config", "explain", "--json"],
        vec!["doctor", "--json"],
    ] {
        let output = Command::new(qx_cli_binary())
            .args(&case)
            .output()
            .expect("启动 qx-cli 失败");
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        assert_eq!(
            output.status.code(),
            Some(0),
            "`{} --json` 必须仍然可用:\n{stderr}",
            case.join(" ")
        );
        // --json 的契约是“stdout 只有机器可读的那一份”，横幅由 machine_output() 抑制；
        // 这里不容忍任何前导文本，否则旗标退化回“只压横幅”时用例照样绿。
        let value: serde_json::Value = stdout.trim().parse().unwrap_or_else(|error| {
            panic!(
                "`{} --json` 的 stdout 必须是单个 JSON 值: {error}\n{stdout}",
                case.join(" ")
            )
        });
        assert!(value.is_object(), "机读输出必须是 JSON 对象: {stdout}");
    }
}

/// 人读形状是门禁脚本的取数点，删旗标不能顺手改掉它。
#[test]
fn config_validate_keeps_its_human_pass_line() {
    let output = Command::new(qx_cli_binary())
        .args(["config", "validate"])
        .output()
        .expect("启动 qx-cli 失败");
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert_eq!(
        output.status.code(),
        Some(0),
        "config validate 本身必须照常通过:\n{stderr}"
    );
    assert!(
        stdout.contains("[PASS] config validate 通过:"),
        "人读结论行不得因删旗标而消失:\n{stdout}"
    );
}
