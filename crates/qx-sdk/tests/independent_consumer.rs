//! 独立消费者（方案 §6.1「examples 和独立消费者构建」/ §7 阶段 4 退出门）。
//!
//! 这个文件是门面的**实物证据**：它的 import 只有 `qx_sdk::` 一个前缀——不 import `qx-app`、
//! 不 import 任何领域 crate、不起进程。如果哪天门面漏掉一块（比如忘了重导出 `BuiltinStrategyKind`），
//! 这里会**编译不过**，而不是等到集成方才发现"这个方法不存在"。
//!
//! 它做的事与任何第三方嵌入方完全相同：建会话 → 校验数据 → 同步回测 → 复核产物 →
//! 再用异步句柄跑一次、等它、取结果。
//!
//! 数据集用仓库里那份公开样例（`deploy/qianxing.bar-frame.example.json`，70 根 Bar），
//! 而不是在测试里现造：独立消费者的第一件事本来就是"拿到一份别人的数据"。

use qx_sdk::{
    BacktestSpec, BuiltinStrategyKind, BuiltinStrategySpec, CallerCapability, Client, DatasetSpec,
    RunStatus, BACKTEST_SPEC_SCHEMA_VERSION,
};
use std::path::{Path, PathBuf};

/// 仓库根：`crates/qx-sdk/tests/<file>` 往上三层。
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/qx-sdk 一定有仓库根")
        .to_path_buf()
}

/// 仓库自带的样例 BarFrame（70 根 BTCUSDT.BINANCE Bar）。
fn sample_bars() -> PathBuf {
    repo_root()
        .join("deploy")
        .join("qianxing.bar-frame.example.json")
}

/// 本用例独享的产物目录（并发跑不互相踩）。
fn scratch(label: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("qx-sdk-consumer/{label}-{}", std::process::id()));
    std::fs::create_dir_all(&root).expect("建临时目录");
    root
}

fn spec(run_id: &str, output_dir: &Path) -> BacktestSpec {
    let mut strategy = BuiltinStrategySpec::new(BuiltinStrategyKind::SmaCross, "sdk-sma");
    strategy.fast_window = 2;
    strategy.slow_window = 3;
    BacktestSpec::new(
        run_id,
        "BTCUSDT.BINANCE",
        sample_bars().to_string_lossy().into_owned(),
        "USDT",
        // 10 万 USDT 本金（定点 1e9）。写死标度而不是引内部常量：这一份是**独立消费者的输入**，
        // 它只承诺与三个门面同形的 JSON，不承诺与某个内部常量同源。
        100_000 * 1_000_000_000,
        20261010,
        output_dir.to_string_lossy().into_owned(),
        strategy,
    )
}

#[test]
fn an_independent_consumer_runs_and_verifies_a_backtest_through_the_facade_only() {
    let output_dir = scratch("sync");
    let backtest = spec("sdk-run", &output_dir);
    assert_eq!(backtest.schema_version, BACKTEST_SPEC_SCHEMA_VERSION);

    let client = Client::new(CallerCapability::Research, "sdk-run");

    // ① 数据可用性是**裁定**：usable=false 是结论，不是错误。
    let dataset = DatasetSpec::new("sdk-dataset", sample_bars().to_string_lossy().into_owned());
    let verdict = client.validate_dataset(&dataset).expect("数据集可读");
    assert!(verdict.usable, "样例数据集必须可用: {:?}", verdict.gaps);
    assert_eq!(verdict.rows, 70);

    // ② 同步回测，四份产物落盘。
    let outcome = client.run_backtest(&backtest).expect("回测跑通");
    assert_eq!(outcome.equity_points, 70);
    assert!(outcome.fills > 0, "样例数据必须真跑出成交");
    for path in [
        &outcome.artifacts.run_manifest,
        &outcome.artifacts.summary,
        &outcome.artifacts.equity,
        &outcome.artifacts.fills,
    ] {
        assert!(Path::new(path).is_file(), "产物缺失: {path}");
    }

    // ③ 独立复核：篡改过的读数必须判失败而不是报错。
    let verification = client.verify_run(&outcome).expect("复核跑得起来");
    assert!(
        verification.verified,
        "独立消费者必须能自己复核产物: {:?}",
        verification.mismatches
    );
    assert_eq!(verification.result_hash, outcome.result_hash);
}

#[test]
fn an_independent_consumer_drives_the_async_handle_and_sees_the_same_hash() {
    let output_dir = scratch("async");
    let backtest = spec("sdk-async", &output_dir);
    let client = Client::new(CallerCapability::Research, "sdk-async");

    // 句柄式异步：立刻拿到 handle，由调用方决定等多久（缺省 30 s，有限）。
    let mut handle = client.start_backtest(backtest.clone());
    assert_eq!(handle.run_id(), "sdk-async");
    assert_eq!(handle.wait(), RunStatus::Succeeded);
    let outcome = handle
        .try_take_result()
        .expect("终态必须能取到结果")
        .expect("成功的运行必须是 Ok");
    // 一次性读取：再取一次就是 None（结果不会被执行两次）。
    assert!(handle.try_take_result().is_none());

    // 同一身份重跑逐位同哈希——"可复现"这条承诺在独立消费者手上同样成立。
    let again = client.run_backtest(&backtest).expect("重跑");
    assert_eq!(again.result_hash, outcome.result_hash);
}

#[test]
fn an_independent_consumer_sees_the_same_error_document_the_facades_do() {
    let output_dir = scratch("error");
    let mut broken = spec("sdk-broken", &output_dir);
    broken.bars_path = output_dir
        .join("missing-bars.json")
        .to_string_lossy()
        .into_owned();
    let client = Client::new(CallerCapability::Research, "sdk-broken");
    let error = client.run_backtest(&broken).expect_err("缺数据必须失败");

    assert_eq!(error.category().as_str(), "DATA_UNAVAILABLE");
    assert_eq!(error.source_code(), Some("io:NotFound"));
    assert_eq!(error.correlation_id(), Some("sdk-broken"));
    assert!(!error.safe_to_retry());
    // 错误文档与三个门面报的是同一份形状——独立消费者不需要另一套解析。
    let payload: serde_json::Value = serde_json::from_str(&error.to_json()).expect("错误可解析");
    assert_eq!(payload["action"], "PROVIDE_DATA");
    assert_eq!(payload["retry"], "NEVER");
}
