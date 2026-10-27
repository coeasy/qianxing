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

#[test]
fn doctor_exposes_the_offline_product_capability_catalog_and_detail_by_stable_id() {
    let summary = Command::new(qx_cli_binary())
        .args(["doctor", "--capabilities", "--json"])
        .output()
        .expect("启动 qx-cli 能力目录查询失败");
    let stderr = String::from_utf8_lossy(&summary.stderr);
    assert_eq!(summary.status.code(), Some(0), "能力目录查询失败: {stderr}");
    let summary_json: serde_json::Value =
        serde_json::from_slice(&summary.stdout).expect("--json stdout 必须只有一份能力目录 JSON");
    let entries = summary_json["capabilities"]
        .as_array()
        .expect("能力目录轻索引应为数组");
    assert_eq!(entries.len(), 16);
    assert!(entries
        .iter()
        .any(|entry| entry["id"] == "qx.venue.binance_spot"));

    let detail = Command::new(qx_cli_binary())
        .args([
            "doctor",
            "--capabilities",
            "--capability-id",
            "qx.venue.binance_spot",
            "--json",
        ])
        .output()
        .expect("启动 qx-cli 能力详情查询失败");
    let stderr = String::from_utf8_lossy(&detail.stderr);
    assert_eq!(detail.status.code(), Some(0), "能力详情查询失败: {stderr}");
    let detail_json: serde_json::Value =
        serde_json::from_slice(&detail.stdout).expect("能力详情 --json stdout 必须只有一份 JSON");
    let capability = &detail_json["capability"];
    assert_eq!(capability["id"], "qx.venue.binance_spot");
    assert!(capability["matrix"]["limitations"].as_array().is_some());
    assert!(capability["maturity_evidence"].as_array().is_some());
}

#[test]
fn doctor_capability_query_filters_venue_product_and_account_mode() {
    let output = Command::new(qx_cli_binary())
        .args([
            "doctor",
            "--capabilities",
            "--venue",
            "BINANCE",
            "--product",
            "perpetual",
            "--account-mode",
            "hedge",
            "--json",
        ])
        .output()
        .expect("启动 qx-cli 交易范围能力查询失败");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "范围查询失败: {stderr}");
    let result: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("范围查询 --json 必须只有一份 JSON");
    assert_eq!(result["schema_version"], 2);
    assert_eq!(result["match_count"], 1);
    let matched = &result["matches"][0];
    assert_eq!(matched["capability_id"], "qx.venue.ccxt_process");
    assert_eq!(matched["scope"]["venue"], "BINANCE");
    assert_eq!(matched["scope"]["product"], "perpetual");
    assert_eq!(matched["scope"]["account_mode"], "hedge");
    assert_eq!(matched["scope"]["status"], "implemented-unverified");
    assert_eq!(matched["capability"]["matrix"]["sandbox"], "unsupported");

    let account_model = Command::new(qx_cli_binary())
        .args([
            "doctor",
            "--capabilities",
            "--venue",
            "BITGET",
            "--product",
            "spot",
            "--account-model",
            "UTA",
            "--margin-mode",
            "cash",
            "--json",
        ])
        .output()
        .expect("启动 qx-cli 账户体系和保证金模式查询失败");
    let stderr = String::from_utf8_lossy(&account_model.stderr);
    assert_eq!(
        account_model.status.code(),
        Some(0),
        "账户体系/保证金查询失败: {stderr}"
    );
    let result: serde_json::Value = serde_json::from_slice(&account_model.stdout)
        .expect("账户体系/保证金查询 --json 必须只有一份 JSON");
    assert_eq!(result["match_count"], 1);
    assert_eq!(result["matches"][0]["scope"]["venue"], "BITGET");
    assert_eq!(result["matches"][0]["scope"]["margin_modes"][0], "cash");
}

#[test]
fn doctor_capability_query_rejects_unknown_ids_and_mixed_runtime_config_mode() {
    let unknown = Command::new(qx_cli_binary())
        .args([
            "doctor",
            "--capabilities",
            "--capability-id",
            "qx.venue.unknown",
        ])
        .output()
        .expect("启动 qx-cli 未知能力 ID 查询失败");
    let stderr = String::from_utf8_lossy(&unknown.stderr);
    assert_eq!(unknown.status.code(), Some(2));
    assert!(
        stderr.contains("能力 ID 不存在"),
        "错误应指出未知 ID: {stderr}"
    );

    for arguments in [
        vec!["doctor", "--capability-id", "qx.venue.binance_spot"],
        vec!["doctor", "runtime.json", "--capabilities"],
    ] {
        let output = Command::new(qx_cli_binary())
            .args(&arguments)
            .output()
            .expect("启动 qx-cli 参数冲突检查失败");
        assert_eq!(
            output.status.code(),
            Some(2),
            "参数组合必须在运行前被拒绝: {}\n{}",
            arguments.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
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
