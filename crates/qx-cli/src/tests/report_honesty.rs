//! 回测报告的"缺失诚实"用例（V11 D 轮 S6）：`report` 与 `status` 都不得把"这份产物没写过
//! 这一格"念成"这一轮算出来是 0"。
//!
//! 用例分两层：一层直接断言排版函数的输出（措辞与 0/not_declared 的分岔只在这里定义），
//! 一层跑真 binary 断命令面确实把那句话印了出来（否则改了就没人读的函数等于没改）。

use super::*;

/// v1 形状的最小摘要：有 bars/fills/metrics，没有 `replay`、没有 `account`、没有 `sample_unit`。
/// 这就是仓库里 16 份已提交产物的形状，不是假想的形状。
fn v1_shaped_summary() -> serde_json::Value {
    serde_json::json!({
        "schema_version": 1,
        "strategy_id": "strategy-backtest",
        "instrument": "BTCUSDT.BINANCE",
        "bars": 5,
        "fills": 1,
        "input_data_hash": "4a4d2b1771475d78",
        "result_hash": "8b50399899f492a2",
        "metrics": {
            "return_bps": 0,
            "max_drawdown_bps": 0,
            "fees_raw": 0,
            "turnover_raw": 102,
            "final_equity_raw": 100000000000001u64
        }
    })
}

#[test]
fn absent_summary_fields_are_not_read_as_zero() {
    let lines =
        backtest_report_lines(&v1_shaped_summary(), "input_verified=not_declared").join("\n");
    assert!(
        lines.contains("replay_events=not_declared"),
        "没有 replay 块时必须念 not_declared:\n{lines}"
    );
    assert!(
        lines.contains("replay_ledger_entries=not_declared/not_declared"),
        "重放账簿两格同样不得补 0:\n{lines}"
    );
    assert!(
        lines.contains("（该摘要没有 replay 块，重放结论未经核对）"),
        "整块缺失要单独说一句，三个 not_declared 容易被读成排版坏了:\n{lines}"
    );
    assert!(
        lines.contains("sample_unit=not_declared"),
        "v1 没有 sample_unit，不能沿用上一格的值:\n{lines}"
    );
    assert!(
        !lines.contains("replay_events=0"),
        "这份产物一个重放结论都没作过，输出里不该出现 0:\n{lines}"
    );
}

#[test]
fn genuine_zero_replay_events_still_print_zero() {
    // 与上一个用例成对：如果实现把"缺失"和"0"合成同一个分支，这两条断言必红一条。
    let mut summary = v1_shaped_summary();
    summary["schema_version"] = serde_json::json!(4);
    summary["sample_unit"] = serde_json::json!("bar");
    summary["replay"] = serde_json::json!({
        "log_digest": "8b50399899f492a2",
        "events": 0,
        "ledger_entries": 0,
        "run_ledger_entries": 0
    });
    let lines = backtest_report_lines(&summary, "input_verified=true").join("\n");
    assert!(
        lines.contains("replay_events=0 replay_ledger_entries=0/0"),
        "真算出来的 0 必须照旧念 0:\n{lines}"
    );
    assert!(
        !lines.contains("not_declared"),
        "v4 有 replay 块，不该再出现未经核对的措辞:\n{lines}"
    );
    assert!(
        lines.contains("sample_unit=bar"),
        "v4 有 sample_unit:\n{lines}"
    );
}

#[test]
fn null_summary_cells_read_as_not_declared() {
    let mut summary = v1_shaped_summary();
    summary["metrics"]["return_bps"] = serde_json::Value::Null;
    let lines = backtest_report_lines(&summary, "input_verified=not_declared").join("\n");
    assert!(
        lines.contains("return_bps=not_declared"),
        "显式 null 与缺失同义，都不能念成 0:\n{lines}"
    );
}

/// 命令面：`qx-cli report <v1 摘要>` 必须真的把那句话印到 stdout。
#[test]
fn report_command_prints_not_declared_for_a_v1_summary() {
    let dir = temp_cli_case_dir("report-honesty");
    let summary_path = dir.join("case.summary.json");
    std::fs::write(
        &summary_path,
        serde_json::to_string_pretty(&v1_shaped_summary()).unwrap(),
    )
    .unwrap();
    let output = Command::new(qx_cli_binary())
        .args(["report", &summary_path.to_string_lossy()])
        .output()
        .expect("启动 qx-cli 失败");
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert_eq!(
        output.status.code(),
        Some(0),
        "对没有 input 块的旧摘要出报告应当成功并说明未经核对，而不是拒绝:\n{stdout}{stderr}"
    );
    assert!(
        stdout.contains("input_verified=not_declared"),
        "输入身份未经核对要说出来:\n{stdout}"
    );
    assert!(
        stdout.contains("replay_events=not_declared"),
        "重放结论未经核对要说出来:\n{stdout}"
    );
    assert!(
        stdout.contains("bars=5") && stdout.contains("fills=1"),
        "真写过的格子照旧念原值:\n{stdout}"
    );
}

/// 命令面：`status` 的 `[Latest Backtest]` 行与 `report` 共用同一套口径。
/// 这里用一份连 `metrics` 都没有的更瘦的产物：`status` 那一行念的就是这三格，
/// 缺了它们曾经一律补 0（`report` 行的断言在上面的用例里）。
#[test]
fn status_command_prints_not_declared_for_a_v1_summary() {
    let (deploy, _, runtime_template) = builtin_backtest_example_paths();
    let config = read_runtime_config(&runtime_template).expect("读取示例运行时配置失败");
    let (root, runtime_path) = isolated_backtest_runtime(&deploy, &config, "status-honesty");
    let runs = root.join("runs");
    std::fs::create_dir_all(&runs).unwrap();
    let summary = serde_json::json!({
        "schema_version": 1,
        "strategy_id": "strategy-backtest",
        "instrument": "BTCUSDT.BINANCE",
        "result_hash": "8b50399899f492a2"
    });
    std::fs::write(
        runs.join("case.summary.json"),
        serde_json::to_string_pretty(&summary).unwrap(),
    )
    .unwrap();
    let output = Command::new(qx_cli_binary())
        .args(["status", &runtime_path.to_string_lossy()])
        .output()
        .expect("启动 qx-cli 失败");
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert_eq!(
        output.status.code(),
        Some(0),
        "status 必须能读旧摘要:\n{stdout}{stderr}"
    );
    let line = stdout
        .lines()
        .find(|line| line.starts_with("[Latest Backtest]"))
        .unwrap_or_else(|| panic!("status 没有印最近回测行:\n{stdout}"));
    assert!(
        line.contains("fills=not_declared")
            && line.contains("return_bps=not_declared")
            && line.contains("max_drawdown_bps=not_declared"),
        "没写过的三格必须念 not_declared，而不是替产物补一个 0:\n{line}"
    );
    assert!(
        line.contains("strategy=strategy-backtest")
            && line.contains("result_hash=8b50399899f492a2"),
        "写过的格子不受影响:\n{line}"
    );
}
