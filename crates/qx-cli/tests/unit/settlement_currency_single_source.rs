//! 记账币种的单源与指纹用例（V13 R1-A4）。
//!
//! 两条理由：
//! 1. **缺省值只能有一处**。回测装配、worker 声明、账户日志三条回落链各自抄 `"USDT"` 时，
//!    改一处就把另外两处留在原地，产物里的账簿币种与代码里的判词分叉而无人报错。
//! 2. **记账币种必须留在指纹里**。它决定现金腿落在哪本账：两条运行如果换了币种还能给出
//!    同一个 `result_hash`，产物就无法证明"这份收益是哪种币的收益"。这里钉的是这条口径
//!    真的进了发布产物，而不是只进了某个 helper 的返回值。

use super::*;

/// 三条回落链的缺省值必须是同一个常量，且 worker 侧仍按大写归一。
///
/// 账户日志那条走的是 `settlement_currency_among_workers`：它读的是各写入方声明的集合，
/// 集合为空才落到缺省，所以它的缺省与 worker 自己缺席时的缺省必须是同一句话。
#[test]
fn every_settlement_currency_fallback_reads_the_same_default() {
    assert_eq!(
        backtest_settlement_currency(None),
        DEFAULT_SETTLEMENT_CURRENCY,
        "回测装配的兜底必须取自单源常量"
    );
    let mut worker = mk_worker("currency-default", WorkerRole::Execution, "binance", None);
    assert_eq!(
        worker_settlement_currency(&worker),
        DEFAULT_SETTLEMENT_CURRENCY,
        "worker 未声明结算币种时的兜底必须取自单源常量"
    );
    worker.settlement_currency = Some("usdt".into());
    assert_eq!(
        worker_settlement_currency(&worker),
        DEFAULT_SETTLEMENT_CURRENCY.to_ascii_uppercase(),
        "声明值仍按大写归一：大小写错位会读到一个空账簿"
    );

    let deploy = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy");
    let mut config =
        read_runtime_config(&deploy.join("qianxing.runtime.paper-strategy.example.json")).unwrap();
    for worker in config.workers.iter_mut() {
        worker.settlement_currency = None;
    }
    assert!(
        config.workers.iter().any(owns_account_event_log),
        "paper 模板必须有账户日志的写入方，否则下面的断言是空的"
    );
    assert_eq!(
        settlement_currency_for_log(&config, &paper_account_log()).unwrap(),
        DEFAULT_SETTLEMENT_CURRENCY,
        "账户日志的写入方全部缺席时，兜底也必须取自同一个常量"
    );
}

/// spec 声明的币种与缺省常量同值时，回测装配不该给出第二个答案。
///
/// 这条等式就是"兜底与显式声明等价"：它让 `DEFAULT_SETTLEMENT_CURRENCY` 成为唯一一处
/// 需要核对的地方，不必再问"这次运行是没写还是写了 USDT"。
#[test]
fn declaring_the_default_currency_matches_declaring_nothing() {
    let spec = TradingInstrumentSpec {
        settlement_currency: DEFAULT_SETTLEMENT_CURRENCY.into(),
        ..workspace_spot_spec()
    };
    assert_eq!(
        backtest_settlement_currency(Some(&spec)),
        backtest_settlement_currency(None),
        "显式声明缺省币种与不声明必须给出同一个账簿键"
    );
}

/// 换掉结算币种——其余输入逐字节相同——两份发布产物里的 `result_hash` 必须跟着动。
///
/// 两份规格只差 `settlement_currency` 一格，所以哈希的任何差异都只可能来自币种。
/// 这里刻意比的是 `result_hash` 而不是 `RunManifest::digest()`：后者还覆盖 `clock_start`/
/// `clock_end`（墙钟）与 `config_hash`（含隔离根目录的绝对路径），同一币种重跑就会变，
/// 拿它当判据等于把"币种动了"和"这次运行本身不同"混成一格（实测：USDT 重跑得
/// `d49deb7142214d09` 与 `864beecf27bd706a`）。`result_hash` 是事件日志摘要，币种经由
/// 初始 `Ledger::deposit` 落进 `LedgerApplied` 事实，所以它是这条链上唯一既跟着币种动、
/// 又只由输入决定的那一格。
///
/// 反向验证：把 `Ledger::deposit` 的币种参数写死成常量，本用例必须变红。
#[test]
fn changing_only_the_settlement_currency_moves_the_published_fingerprint() {
    let (deploy, frame, template) = builtin_backtest_example_paths();
    let mut config = read_runtime_config(&template).unwrap();
    config.strategy.fill_model = Some("one_tick_slippage".into());
    let run = |currency: &str| {
        let (root, runtime) =
            isolated_backtest_runtime(&deploy, &config, &format!("a4-{currency}"));
        let spec = spot_spec_settled_in(&root, currency);
        run_strategy_backtest(&runtime, &frame, Some(&spec))
            .unwrap_or_else(|error| panic!("{currency} 链必须跑通: {error}"));
        let summary = read_first_backtest_summary(&root);
        assert!(
            summary["fills"].as_i64().unwrap_or_default() > 0,
            "{currency} 侧零成交，下面的指纹对比是空的: {summary}"
        );
        let manifest = read_run_manifest(&summary);
        let facts = (
            summary["result_hash"].as_str().unwrap().to_string(),
            manifest.result_hash.clone(),
        );
        let _ = std::fs::remove_dir_all(&root);
        facts
    };
    let usdt = run("USDT");
    let cny = run("CNY");
    assert_eq!(
        usdt.0, usdt.1,
        "摘要与 RunManifest 必须报同一个 result_hash，否则下面比的是两把不同的尺"
    );
    assert_eq!(
        cny.0, cny.1,
        "摘要与 RunManifest 必须报同一个 result_hash，否则下面比的是两把不同的尺"
    );
    assert_ne!(
        usdt.0, cny.0,
        "只差一格结算币种却给出同一个 result_hash，产物就无法声明这份收益是哪种币的"
    );
    // 上面那对差异得是"币种"造成的，而不是运行本身不稳定：同币种重跑必须逐格相同。
    assert_eq!(
        usdt,
        run("USDT"),
        "同一币种重跑给出不同 result_hash，那么换币种的红就没有取证价值"
    );
}

/// 摘要里 `run_manifest` 指针指向的那份清单，按发布口径解析（空字段当场拒）。
fn read_run_manifest(summary: &serde_json::Value) -> RunManifest {
    let payload = std::fs::read_to_string(summary["run_manifest"].as_str().unwrap())
        .expect("摘要必须指向一份可读的 RunManifest");
    RunManifest::from_json(&payload).expect("产物里的 RunManifest 必须过自己的校验")
}

/// 仓库现货规格的解析结果，供"声明值 == 缺省值"这类等式使用。
fn workspace_spot_spec() -> TradingInstrumentSpec {
    let payload = std::fs::read_to_string(workspace_binance_spot_spec()).unwrap();
    serde_json::from_str(&payload).unwrap()
}
