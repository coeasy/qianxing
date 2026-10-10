//! 退出门 G2 的第二条：**一个独立 consumer 不启动 `qx-cli` 也能跑完一次回测并复核产物**。
//!
//! 这个测试是"独立 consumer"的实物：它的依赖只有 `qx-app`（以及 `qx-app` 自己依赖的领域件），
//! 不 import `qx-cli`、不 spawn 任何进程、不读 CLI 的产物格式约定之外的任何东西。它做的事与
//! 任何第三方嵌入方要做的事完全相同：
//!
//! 1. 写一份 spec JSON（三个入口共用的那种文档）；
//! 2. 调 `qx_app::run_backtest` 拿 `BacktestOutcome`；
//! 3. 把 `outcome.to_json()` 交给 `qx_app::verify_run` 复核四份产物；
//! 4. 换一个 `run_id` 再跑一次，断言同一身份重跑逐位同哈希。
//!
//! 为什么它必须住在 `crates/qx-app/tests/`（而不是 `qx-cli` 的用例里）：G2 要证明的是
//! **应用层可以被独立复用**。把这条链写在 CLI 的用例里，即使它一次都不 spawn 二进制，
//! 读者也无法从"它住在哪个 crate"看出这一点——而"看不出"正是这条退出门要消灭的状态。
//!
//! 数据集用仓库里那份公开样例（`deploy/qianxing.bar-frame.example.json`，70 根 Bar），
//! 而不是在测试里现造：独立 consumer 的第一件事本来就是"拿到一份别人的数据"。

use qx_app::{
    run_backtest, validate_dataset, verify_run, BacktestOutcome, BacktestSpec, BuiltinStrategySpec,
    CallerCapability, DatasetSpec, RunContext, BACKTEST_SPEC_SCHEMA_VERSION,
};
use qx_strategy::BuiltinStrategyKind;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

/// 仓库根：`crates/qx-app/tests/<file>` 往上三层。
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/qx-app 一定有仓库根")
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
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let serial = COUNTER.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "qx-app-consumer/{label}-{}-{serial}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).expect("建临时目录");
    root
}

fn research(correlation_id: &str) -> RunContext {
    RunContext::new(CallerCapability::Research, correlation_id)
}

fn spec(run_id: &str, output_dir: &Path) -> BacktestSpec {
    let mut strategy = BuiltinStrategySpec::new(BuiltinStrategyKind::SmaCross, "consumer-sma");
    strategy.fast_window = 2;
    strategy.slow_window = 3;
    BacktestSpec::new(
        run_id,
        "BTCUSDT.BINANCE",
        sample_bars().to_string_lossy().into_owned(),
        "USDT",
        // 10 万 USDT 本金（定点 1e9）。写死标度而不是引 `qx_core::SCALE`：这一份是**独立
        // consumer 的输入**，它只承诺与三个门面同形的 JSON，不承诺与某个内部常量同源。
        100_000 * 1_000_000_000,
        20261010,
        output_dir.to_string_lossy().into_owned(),
        strategy,
    )
}

#[test]
fn an_independent_consumer_completes_a_backtest_and_verifies_it_without_the_cli() {
    let output_dir = scratch("run");
    let backtest = spec("consumer-run", &output_dir);
    assert_eq!(backtest.schema_version, BACKTEST_SPEC_SCHEMA_VERSION);

    // ① 先校验数据集——独立 consumer 的第一件事。
    let dataset = DatasetSpec::new(
        "consumer-dataset",
        sample_bars().to_string_lossy().into_owned(),
    );
    let verdict = validate_dataset(&dataset, &research("consumer-dataset")).expect("数据集可读");
    assert!(verdict.usable, "样例数据集必须可用: {:?}", verdict.gaps);
    assert_eq!(verdict.rows, 70);

    // ② 跑一次回测，四份产物落盘。
    let outcome = run_backtest(&backtest, &research("consumer-run")).expect("回测跑通");
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

    // ③ 复核：把 `to_json()` 那一份文档重新读回来，再交给用例。
    let document = outcome.to_json().expect("结果可序列化");
    let reloaded = BacktestOutcome::from_json(&document).expect("结果可读回");
    assert_eq!(reloaded, outcome, "结果的 JSON 往返必须逐字段相等");
    let verification = verify_run(&reloaded, &research("consumer-run")).expect("复核跑得起来");
    assert!(
        verification.verified,
        "独立 consumer 必须能自己复核产物: {:?}",
        verification.mismatches
    );
    assert_eq!(verification.result_hash, outcome.result_hash);

    // ④ 同一身份重跑逐位同哈希——"可复现"这条承诺在独立 consumer 手上同样成立。
    let again = run_backtest(&backtest, &research("consumer-run")).expect("重跑");
    assert_eq!(again.result_hash, outcome.result_hash);
}

#[test]
fn an_independent_consumer_sees_the_same_error_document_the_facades_do() {
    // 失败面同样是独立 consumer 要能自己解释的：类别 + 底层诊断码 + correlation id。
    let output_dir = scratch("error");
    let mut broken = spec("consumer-broken", &output_dir);
    broken.bars_path = output_dir
        .join("missing-bars.json")
        .to_string_lossy()
        .into_owned();
    let error = run_backtest(&broken, &research("consumer-broken")).expect_err("缺数据必须失败");
    assert_eq!(error.category().as_str(), "DATA_UNAVAILABLE");
    assert_eq!(error.source_code(), Some("io:NotFound"));
    assert_eq!(error.correlation_id(), Some("consumer-broken"));
    assert!(!error.safe_to_retry());
    // 错误文档与三个门面报的是同一份形状——独立 consumer 不需要另一套解析。
    let payload: serde_json::Value = serde_json::from_str(&error.to_json()).expect("错误可解析");
    assert_eq!(payload["action"], "PROVIDE_DATA");
    assert_eq!(payload["retry"], "NEVER");
}
