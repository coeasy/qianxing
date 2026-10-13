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

#[test]
fn provider_backtest_without_configured_sources_fails_closed() {
    use qx_app::{BacktestSpec, BuiltinStrategySpec, ProviderBacktestSpec};
    use qx_provider::{DataKind, DataQuery, DataRequest, DataRequirements};
    use std::collections::BTreeSet;

    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("qx-cli package lives under the workspace root");
    let spec_path = root.join("target").join(format!(
        "qx-cli-provider-backtest-no-source-{}.json",
        std::process::id()
    ));
    let spec = ProviderBacktestSpec {
        data_request: DataRequest::new(
            DataQuery {
                kind: DataKind::Bar,
                asset_class: "crypto".into(),
                instrument_set: BTreeSet::from(["BTCUSDT.BINANCE".into()]),
                field_set: BTreeSet::from(["close".into()]),
                frequency: "1m".into(),
                adjustment: "none".into(),
                quality_policy: "strict".into(),
                start: 60_000,
                end: 3_840_000,
                as_of: None,
            },
            DataRequirements::default(),
        ),
        backtest: BacktestSpec::new(
            "cli-provider-no-source",
            "BTCUSDT.BINANCE",
            "target/provider-backtest/bars.json",
            "USDT",
            100_000_000_000_000,
            20261013,
            "target/provider-backtest/run",
            BuiltinStrategySpec::new(qx_strategy::BuiltinStrategyKind::SmaCross, "cli-sma"),
        ),
        dataset_id: "cli-provider-bars".into(),
        dataset_version: "v1".into(),
        interval_ms: 60_000,
        timezone: "UTC".into(),
        manifest_path: None,
    };
    std::fs::write(&spec_path, serde_json::to_vec(&spec).unwrap())
        .expect("write valid provider-backtest spec");

    let output = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .current_dir(root)
        .args([
            "app",
            "provider-backtest",
            "deploy/qianxing.runtime.example.json",
        ])
        .arg(&spec_path)
        .output()
        .expect("run provider-backtest without configured sources");
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("DATA_UNAVAILABLE"), "{stderr}");
    assert!(stderr.contains("没有 api.data_providers"), "{stderr}");
}

#[test]
fn provider_depth_backtest_without_configured_sources_fails_closed() {
    use qx_app::{
        BuiltinStrategySpec, DepthBacktestSpec, ProviderDepthBacktestSpec,
        DEPTH_BACKTEST_SPEC_SCHEMA_VERSION,
    };
    use qx_core::SCALE;
    use qx_provider::{DataKind, DataQuery, DataRequest, DataRequirements};
    use std::collections::BTreeSet;

    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("qx-cli package lives under the workspace root");
    let spec_path = root.join("target").join(format!(
        "qx-cli-provider-depth-backtest-no-source-{}.json",
        std::process::id()
    ));
    let spec = ProviderDepthBacktestSpec {
        schema_version: qx_app::PROVIDER_DEPTH_BACKTEST_SPEC_SCHEMA_VERSION,
        data_request: DataRequest::new(
            DataQuery {
                kind: DataKind::Quote,
                asset_class: "crypto".into(),
                instrument_set: BTreeSet::from(["BTCUSDT.BINANCE".into()]),
                field_set: BTreeSet::new(),
                frequency: "tick".into(),
                adjustment: "none".into(),
                quality_policy: "strict".into(),
                start: 1,
                end: 3,
                as_of: None,
            },
            DataRequirements::default(),
        ),
        backtest: DepthBacktestSpec {
            schema_version: DEPTH_BACKTEST_SPEC_SCHEMA_VERSION,
            run_id: "cli-provider-depth-no-source".into(),
            depth_path: "target/provider-depth/depth.json".into(),
            tier: "l1".into(),
            settlement_currency: "USDT".into(),
            initial_cash_raw: 10_000 * SCALE,
            fee_bps: 0,
            latency_snapshots: 0,
            queue_position_bps: 0,
            market_impact_bps: 0,
            output_dir: "target/provider-depth/run".into(),
            strategy: BuiltinStrategySpec::new(
                qx_strategy::BuiltinStrategyKind::SmaCross,
                "cli-provider-depth-sma",
            ),
            market_spec: None,
            order_policy: None,
            market_making: None,
            account_id: None,
            portfolio: None,
        },
    };
    std::fs::write(&spec_path, serde_json::to_vec(&spec).unwrap())
        .expect("write valid ProviderDepthBacktestSpec");

    let output = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .current_dir(root)
        .args([
            "app",
            "provider-depth-backtest",
            "deploy/qianxing.runtime.example.json",
        ])
        .arg(&spec_path)
        .output()
        .expect("run provider-depth-backtest without configured sources");
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("DATA_UNAVAILABLE"), "{stderr}");
    assert!(stderr.contains("没有 api.data_providers"), "{stderr}");
}

/// 顶层命令表（与 `cli_args.rs` 的 clap 派生逐一相等，由门禁核对）。
const CLI_COMMANDS: [&str; 47] = [
    "all",
    "app",
    "backtest",
    "binance-private-probe",
    "binance-public-probe",
    "binance-submit-order",
    "binance-worker",
    "builtin-strategies",
    "ccxt-fetch-ohlcv",
    "ccxt-market-spec",
    "ccxt-submit-order",
    "ccxt-worker",
    "config",
    "console",
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

/// 二级入口表：`config <sub>` / `run <sub>` / `strategy <sub>` / `backtest <sub>` / `app <sub>`。
///
/// 顶层表只保证每条入口能渲染自己的用法；`backtest ccxt-builtin`、`strategy list` 这类
/// **二级入口**此前一颗用例都没点过名（旧判据 `cli_command_test_evidence_check` 报出的正是
/// 这两条）——它一敲就崩、或自己的用法渲染不出来时没人红。这里把 21 条二级入口逐条实跑
/// `--help`，与顶层那条同一口径（退 0 + 用法里带叶子名）。
const NESTED_COMMANDS: [&str; 34] = [
    "config explain",
    "config validate",
    "config fingerprint",
    "config lock",
    "run backtest",
    "run paper",
    "run paper-check",
    "run doctor",
    "run live-check",
    "run runtime-check",
    "run report",
    "strategy list",
    "strategy init",
    "strategy backtest",
    "backtest builtin",
    "backtest multi-builtin",
    "backtest ccxt-builtin",
    "backtest book",
    "app validate-dataset",
    "app backtest",
    "app provider-backtest",
    "app provider-depth-backtest",
    "app verify",
    "app compare-runs",
    "app plan-risk-parity",
    "app plan-algo-execution",
    "app run-experiment",
    "app run-depth-experiment",
    "app run-multi-leg-experiment",
    "app run-multi-market-experiment",
    "app depth-backtest",
    "app verify-depth",
    "app multi-leg-backtest",
    "app multi-account-backtest",
];

/// 逐条二级入口实跑 `--help`：clap 必须认它（退 0），且用法里带子命令名。
#[test]
fn every_nested_command_renders_its_own_help() {
    for spec in NESTED_COMMANDS {
        let args = spec.split(' ').chain(["--help"]).collect::<Vec<_>>();
        let output = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
            .args(&args)
            .output()
            .expect("启动 qx-cli 失败");
        let code = output.status.code().unwrap_or(-1);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(
            code, 0,
            "`qx-cli {spec} --help` 必须退 0（clap 认不出这条二级入口？），stderr: {stderr}"
        );
        let leaf = spec.split(' ').next_back().unwrap();
        assert!(
            stdout.contains(leaf),
            "`qx-cli {spec} --help` 的用法必须点名 `{leaf}`:\n{stdout}"
        );
    }
}
