//! 命令表逐颗用例：clap 表里每条顶层命令都至少有一个真跑的用例（fam12 / WP-12）。
//!
//! `tools/check_architecture.py` 的 `cli_help_surface_check` 只保证「help 印出的入口 ≡ clap 命令表
//! ≡ `cli.rs` 显式派发分支」三侧**集合相等**，`cli_dispatch_check` 只保证分派点唯一——但
//! 「某条入口其实一敲就崩 / 它自己的用法渲染不出来」这两件事，两者都看不见。这里对每条命令实跑一次
//! `--help`：它真的走一遍 clap 解析 + 该命令自己的用法渲染，任一条命令没接上、或渲染失败，都会让
//! 本用例当场变红。
//!
//! 命令名清单必须与 clap 表**逐一相等**——由门禁 `cli_surface_coverage_check` 看守：清单里少了
//! 命令、或新增命令没进清单，都会红。所以这张表是「逐颗用例」的事实来源，不是装饰。

use std::process::Command;

/// 顶层命令表（与 `cli_args.rs` 的 clap 派生逐一相等，由门禁核对）。
const CLI_COMMANDS: [&str; 44] = [
    "all",
    "backtest",
    "binance-private-probe",
    "binance-public-probe",
    "binance-submit-order",
    "binance-worker",
    "builtin-strategies",
    "ccxt-fetch-ohlcv",
    "ccxt-market-spec",
    "ccxt-worker",
    "config",
    "consumer-dlq-replay",
    "data-validate",
    "dataset-bundle",
    "dataset-ingest",
    "doctor",
    "ecosystem",
    "event-consumer-worker",
    "fast-backtest",
    "init",
    "live-check",
    "outbox-relay",
    "outbox-relay-postgres",
    "outbox-relay-worker",
    "paper",
    "paper-check",
    "paper-e2e",
    "paper-submit-order",
    "paper-worker",
    "plan",
    "quickstart",
    "reconcile",
    "recovery-child",
    "report",
    "run",
    "runtime-check",
    "scheduler-worker",
    "serve",
    "status",
    "strategy",
    "strategy-worker",
    "supervise",
    "verify",
    "version",
];

/// 逐颗命令实跑 `--help`：clap 必须认这个入口（退 0），且印出的用法里带该命令名。
#[test]
fn every_command_renders_its_own_help() {
    for name in CLI_COMMANDS {
        let output = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
            .args([name, "--help"])
            .output()
            .expect("启动 qx-cli 失败");
        let code = output.status.code().unwrap_or(-1);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(
            code, 0,
            "`qx-cli {name} --help` 必须退 0（clap 认不出这条入口？），stderr: {stderr}"
        );
        assert!(
            stdout.contains(name),
            "`qx-cli {name} --help` 的用法必须点名该命令:\n{stdout}"
        );
    }
}
