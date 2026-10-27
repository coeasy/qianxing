//! 命令表逐颗用例：clap 表里每条顶层命令都至少有一个真跑的用例（fam12 / WP-12）。
//!
//! `tools/check_architecture.py` 的 `cli_help_surface_check` 只保证「help 印出的入口 ≡ clap 命令表
//! ≡ `cli.rs` 显式派发分支」三侧**集合相等**，`cli_dispatch_check` 只保证分派点唯一——但
//! 「某条入口其实一敲就崩 / 它自己的用法渲染不出来」这两件事，两者都看不见。这里对每条命令实跑一次
//! `--help`：它真的走一遍 clap 解析 + 该命令自己的用法渲染，任一条命令没接上、或渲染失败，都会让
//! 本用例当场变红。
//!
//! 命令名清单由 clap 参数源生成到 maturity/cli_command_manifest.json；架构门禁要求生成物逐字节
//! 新鲜，且这里从该文件逐条启动 binary。因此新增或改名路由后必须重生成，才能同时刷新逐颗用例。

use std::process::Command;

fn assert_same_process_replay(first: &std::process::Output, second: &std::process::Output) {
    assert_eq!(first.status.code(), second.status.code());
    assert_eq!(
        first.stdout, second.stdout,
        "stdout changed across CLI replays"
    );
    assert_eq!(
        first.stderr, second.stderr,
        "stderr changed across CLI replays"
    );
}

#[test]
fn provider_backtest_without_configured_sources_fails_closed() {
    use qx_app::{BacktestDatasetInput, BacktestSpec, BuiltinStrategySpec, ProviderBacktestSpec};
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
            BacktestDatasetInput {
                run_id: "cli-provider-no-source".into(),
                instrument: "BTCUSDT.BINANCE".into(),
                bars_path: "target/provider-backtest/bars.json".into(),
            },
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

    let run = || {
        Command::new(env!("CARGO_BIN_EXE_qx-cli"))
            .current_dir(root)
            .args([
                "app",
                "provider-backtest",
                "deploy/qianxing.runtime.example.json",
            ])
            .arg(&spec_path)
            .output()
            .expect("run provider-backtest without configured sources")
    };
    let first = run();
    let second = run();
    assert_same_process_replay(&first, &second);
    let output = first;
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
    use qx_provider::{DataKind, DataQuery, DataRequest, DataRequirements, MarketDataTier};
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
            DataRequirements {
                required_data_tier: Some(MarketDataTier::L1),
                ..DataRequirements::default()
            },
        ),
        backtest: DepthBacktestSpec {
            schema_version: DEPTH_BACKTEST_SPEC_SCHEMA_VERSION,
            run_id: "cli-provider-depth-no-source".into(),
            depth_path: "target/provider-depth/depth.json".into(),
            tier: "l1".into(),
            settlement_currency: "USDT".into(),
            initial_cash_raw: 10_000 * SCALE,
            fee_bps: Some(0),
            latency_snapshots: 0,
            queue_position_bps: 0,
            queue_position_model: Default::default(),
            market_impact_bps: 0,
            funding_bps: 0,
            funding_events: Vec::new(),
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
            manifest_run_id: None,
            execution_cost_rules: None,
            execution_cost_source: None,
            risk_rules: None,
            allow_short: None,
            declared_ashare_rules: None,
            declared_fill_model: None,
            declared_bar_volume_participation_bps: None,
            quality_manifest_path: None,
            runtime_config_path: None,
            market_spec_path: None,
            market_spec_source: None,
            account_base_source: None,
            signal_source: None,
            signal_declared_unused: None,
        },
    };
    std::fs::write(&spec_path, serde_json::to_vec(&spec).unwrap())
        .expect("write valid ProviderDepthBacktestSpec");

    let run = || {
        Command::new(env!("CARGO_BIN_EXE_qx-cli"))
            .current_dir(root)
            .args([
                "app",
                "provider-depth-backtest",
                "deploy/qianxing.runtime.example.json",
            ])
            .arg(&spec_path)
            .output()
            .expect("run provider-depth-backtest without configured sources")
    };
    let first = run();
    let second = run();
    assert_same_process_replay(&first, &second);
    let output = first;
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("DATA_UNAVAILABLE"), "{stderr}");
    assert!(stderr.contains("没有 api.data_providers"), "{stderr}");
}

/// 顶层命令表（与 `cli_args.rs` 的 clap 派生逐一相等，由门禁核对）。
const COMMAND_MANIFEST: &str = include_str!("../src/cli_command_manifest.json");

fn generated_routes() -> Vec<serde_json::Value> {
    let manifest: serde_json::Value = serde_json::from_str(COMMAND_MANIFEST)
        .expect("generated CLI command manifest is valid JSON");
    manifest["commands"]
        .as_array()
        .expect("manifest commands is an array")
        .clone()
}

fn generated_top_level_commands() -> Vec<String> {
    generated_routes()
        .into_iter()
        .filter(|route| route["parent"].is_null() && route["name"] != "help")
        .map(|route| {
            route["name"]
                .as_str()
                .expect("route name is a string")
                .to_owned()
        })
        .collect()
}

fn generated_nested_commands() -> Vec<String> {
    generated_routes()
        .into_iter()
        .filter(|route| route["parent"].is_string())
        .map(|route| {
            format!(
                "{} {}",
                route["parent"].as_str().expect("nested route has a parent"),
                route["name"].as_str().expect("route name is a string")
            )
        })
        .collect()
}

/// 逐颗命令实跑 `--help`：clap 必须认这个入口（退 0），且印出的用法里带该命令名。
#[test]
fn every_command_renders_its_own_help() {
    for name in generated_top_level_commands() {
        let output = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
            .args([name.as_str(), "--help"])
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
            stdout.contains(name.as_str()),
            "`qx-cli {name} --help` 的用法必须点名该命令:\n{stdout}"
        );
    }
}

/// 二级入口表：`config <sub>` / `run <sub>` / `strategy <sub>` / `backtest <sub>` / `app <sub>`。
///
/// 顶层表只保证每条入口能渲染自己的用法；`backtest ccxt-builtin`、`strategy list` 这类
/// **二级入口**此前一颗用例都没点过名（旧判据 `cli_command_test_evidence_check` 报出的正是
/// 这两条）——它一敲就崩、或自己的用法渲染不出来时没人红。这里将生成清单里的二级入口逐条实跑
/// `--help`，与顶层那条同一口径（退 0 + 用法里带叶子名）。
/// 逐条二级入口实跑 `--help`：clap 必须认它（退 0），且用法里带子命令名。
#[test]
fn every_nested_command_renders_its_own_help() {
    for spec in generated_nested_commands() {
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

/// Pure local validation failures for command families that otherwise need a venue or a
/// feature-gated backend. These cases pin the process boundary without contacting a venue or DB.
#[test]
fn venue_and_optional_backend_commands_fail_closed_before_external_io() {
    let cases: [(&[&str], &str); 7] = [
        (
            &["binance-public-probe", "unsupported-network"],
            "network 必须是 testnet 或 mainnet",
        ),
        (
            &["storage-adopt-postgres", "--schema", "qx_test"],
            "需要使用 --features postgres 构建 qx-cli",
        ),
        (
            &[
                "outbox-relay",
                "target/outbox",
                "nats://127.0.0.1:4222",
                "qx",
            ],
            "需要使用 --features nats 构建 qx-cli",
        ),
        (
            &[
                "outbox-relay-postgres",
                "deploy/runtime.json",
                "nats://127.0.0.1:4222",
                "qx",
            ],
            "需要使用 --features 'nats postgres' 构建 qx-cli",
        ),
        (
            &["outbox-relay-worker", "deploy/runtime.json", "relay-1"],
            "需要使用 --features nats 构建 qx-cli",
        ),
        (
            &["event-consumer-worker", "deploy/runtime.json", "consumer-1"],
            "需要使用 --features nats 构建 qx-cli",
        ),
        (
            &[
                "consumer-dlq-replay",
                "deploy/runtime.json",
                "group-1",
                "event-1",
            ],
            "需要使用 --features nats 构建 qx-cli",
        ),
    ];

    for (args, expected_error) in cases {
        // These expectations describe a backend that is not compiled into this binary. Feature
        // matrix builds exercise the enabled path through runtime/config contracts instead.
        if (args[0] == "storage-adopt-postgres" && cfg!(feature = "postgres"))
            || (matches!(
                args[0],
                "outbox-relay"
                    | "outbox-relay-worker"
                    | "event-consumer-worker"
                    | "consumer-dlq-replay"
            ) && cfg!(feature = "nats"))
            || (args[0] == "outbox-relay-postgres"
                && cfg!(all(feature = "nats", feature = "postgres")))
        {
            continue;
        }
        let run = || {
            Command::new(env!("CARGO_BIN_EXE_qx-cli"))
                .args(args)
                .output()
                .expect("启动 qx-cli 失败")
        };
        let first = run();
        let second = run();
        assert_same_process_replay(&first, &second);
        let output = first;
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(
            output.status.code(),
            Some(2),
            "`qx-cli {}` 必须以确定性错误拒绝输入：\n{stderr}",
            args.join(" ")
        );
        assert!(
            stderr.contains(expected_error),
            "`qx-cli {}` 的错误应包含 `{expected_error}`：\n{stderr}",
            args.join(" ")
        );
    }
}
