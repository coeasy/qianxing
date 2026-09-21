//! Bar 链撮合口径的命令面用例（V11 Q1a 第二批）。
//!
//! §6 对口径变化的要求是"结果确实变了"，不是"参数被读到了"：所以每个可达组合都要
//! 拿终值哈希/成交额说话，而每个被拒绝的组合都要说明**为什么**被拒绝。

use super::*;

/// 用给定的 `strategy.fill_model` 跑一遍策略回测并读回摘要。`spec` 是 market spec 路径：
/// `one_tick_slippage` 的一档只能从它取，所以带该模型的用例必须固定同一份 spec。
fn strategy_summary_with_fill_model(
    label: &str,
    fill_model: Option<&str>,
    spec: Option<&Path>,
) -> serde_json::Value {
    let (deploy, frame, template) = builtin_backtest_example_paths();
    let mut config = read_runtime_config(&template).unwrap();
    config.strategy.fill_model = fill_model.map(str::to_string);
    let (root, runtime) = isolated_backtest_runtime(&deploy, &config, label);
    run_strategy_backtest(&runtime, &frame, spec).unwrap();
    let summary = read_first_backtest_summary(&root);
    let _ = std::fs::remove_dir_all(root);
    summary
}

/// 摘要里实际生效的撮合模型描述子。`model_descriptors` 的顺序由内核写死为
/// `[fill, fee, latency, margin]`（`backtest.rs`），第一项就是撮合口径。
fn fill_descriptor(summary: &serde_json::Value) -> String {
    let descriptors = summary["model_descriptors"]
        .as_array()
        .unwrap_or_else(|| panic!("摘要缺少 model_descriptors"));
    descriptors[0]
        .as_str()
        .unwrap_or_else(|| panic!("描述子必须是字符串: {descriptors:?}"))
        .to_string()
}

fn metric_i128(summary: &serde_json::Value, key: &str) -> i128 {
    summary["metrics"][key]
        .as_i64()
        .unwrap_or_else(|| panic!("摘要缺少 metrics.{key}")) as i128
}

/// 三种可达口径各自都要"确实换个结果"，并且把口径写进摘要。
///
/// 反向验证口径：把 `BarBacktestAssembly::into_config` 的 `fill` 换回硬编码的
/// `NextBarOpenFillModel`，`best_price` 与 `one_tick_slippage` 两轮的描述子立刻全部退回
/// `NextBarOpen@v1`，下面的 `assert!` 当场不成立。
#[test]
fn each_reachable_fill_model_changes_the_result_and_is_declared() {
    let next_bar_open = strategy_summary_with_fill_model("q1a2-default", None, None);
    assert_eq!(next_bar_open["fill_model"]["name"], "next_bar_open");
    assert_eq!(next_bar_open["fill_model"]["source"], "builtin-default");
    assert!(
        fill_descriptor(&next_bar_open).starts_with("NextBarOpen@v1[tier=Bar"),
        "缺省描述子必须是下一根开盘: {}",
        fill_descriptor(&next_bar_open)
    );

    let best_price = strategy_summary_with_fill_model("q1a2-best", Some("best_price"), None);
    assert_eq!(best_price["fill_model"]["name"], "best_price");
    // 来源必须区分"没人提这一项"与"配置里声明了同一个模型"，否则默认值冒充选择。
    assert_eq!(best_price["fill_model"]["source"], "runtime-config");
    assert!(
        fill_descriptor(&best_price).starts_with("BestPrice@v1[tier=Bar"),
        "换模型必须写进描述子: {}",
        fill_descriptor(&best_price)
    );
    assert!(
        metric_i128(&best_price, "turnover_raw") > 0,
        "示例本来就该有成交"
    );
    assert_ne!(
        best_price["result_hash"], next_bar_open["result_hash"],
        "当根收盘成交与下一根开盘成交不可能同一条权益曲线"
    );
    assert_ne!(
        metric_i128(&best_price, "turnover_raw"),
        metric_i128(&next_bar_open, "turnover_raw"),
        "成交价从 open 换成 close，成交额必须跟着变"
    );

    // 一档滑点：同一份 spec 下与缺省口径比较，唯一变量才是成交价加一档。
    let spec_dir = temp_cli_case_dir("q1a2-spec");
    let spec = spot_market_spec(&spec_dir);
    let with_spec = strategy_summary_with_fill_model("q1a2-tick-base", None, Some(&spec));
    let one_tick =
        strategy_summary_with_fill_model("q1a2-tick", Some("one_tick_slippage"), Some(&spec));
    assert_eq!(with_spec["fill_model"]["name"], "next_bar_open");
    assert_eq!(one_tick["fill_model"]["name"], "one_tick_slippage");
    assert_eq!(one_tick["fill_model"]["source"], "runtime-config");
    let descriptor = fill_descriptor(&one_tick);
    assert!(
        descriptor.starts_with("OneTickSlippage@v1[tier=Bar;params=tick="),
        "滑点模型必须把档位写进参数: {descriptor}"
    );
    assert!(
        descriptor.contains(&format!("tick={}", declared_tick(&spec))),
        "档位必须等于 market spec 声明的 price_tick: {descriptor}"
    );
    assert_ne!(
        one_tick["result_hash"], with_spec["result_hash"],
        "加了一档滑点却结果不变，说明模型没装进撮合"
    );
    assert_ne!(
        metric_i128(&one_tick, "turnover_raw"),
        metric_i128(&with_spec, "turnover_raw"),
        "成交价整体移动一档，成交额必须跟着变"
    );
    let _ = std::fs::remove_dir_all(spec_dir);
}

/// 装配拒绝时的原话。`BarFillModelBinding` 装着 `Box<dyn FillModel>` 而没有 `Debug`，
/// 所以取错误要过一层 `map`——这也顺带钉住"错误里带名字"这件事。
fn fill_model_error(configured: &str, spec: Option<&TradingInstrumentSpec>) -> String {
    bar_fill_model(Some(configured), spec)
        .map(|binding| binding.name)
        .unwrap_err()
}

/// 回测入口的 market spec 故意取的档位：它既不等于 `ccxt_market_to_spec` 缺字段时的兜底
/// `1`，也不等于仓库里那份已验收现货规格的 `1_000_000`。摘要里出现这个数，才证明成交
/// 一档真的取自使用者给的那份文件，而不是某处兜底。
const SPEC_PRICE_TICK_RAW: i128 = 3_000_000;

/// 落一份 CCXT 形状的现货市场快照到 `dir` 并返回路径。回测的 spec 走
/// `ccxt_market_to_spec`，吃的是交易所原始字段（`base`/`quote`/`price_tick_raw`），
/// 与 worker 侧 `instrument_spec_path` 吃的那份 `TradingInstrumentSpec` 不是同一形状。
fn spot_market_spec(dir: &Path) -> PathBuf {
    let path = dir.join("binance-btcusdt.market.json");
    std::fs::write(
        &path,
        serde_json::to_string(&serde_json::json!({
            "market_type": "spot",
            "base": "BTC",
            "quote": "USDT",
            "settle": "USDT",
            "contract_size_raw": 1_000_000_000_i128,
            "linear": true,
            "inverse": false,
            "price_tick_raw": SPEC_PRICE_TICK_RAW,
            "qty_step_raw": 1_000_000_i128,
            "min_qty_raw": 1_000_000_i128,
            "max_leverage": 1,
            "maintenance_margin_bps": 0,
            "valid_from": 0,
            "valid_to": serde_json::Value::Null,
        }))
        .unwrap(),
    )
    .unwrap();
    path
}

/// market spec 里那一档的真实大小，用例从文件读回来而不是抄字面量。
fn declared_tick(spec_path: &Path) -> i128 {
    let payload = std::fs::read_to_string(spec_path).unwrap();
    let market: serde_json::Value = serde_json::from_str(&payload).unwrap();
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    ccxt_market_to_spec(&instrument, &market)
        .unwrap()
        .price_tick
}

/// 没配这一项时连键都不序列化：已验收的 `config_fingerprint` 与 `RunManifest` 必须
/// 逐字节不变，否则"新增一个配置面"会静默改掉所有历史结果的工件路径。
#[test]
fn unconfigured_fill_model_is_absent_from_the_runtime_bytes() {
    let template = builtin_backtest_example_paths().2;
    let config = read_runtime_config(&template).unwrap();
    assert!(config.strategy.fill_model.is_none());
    assert!(
        !serde_json::to_string(&config)
            .unwrap()
            .contains("fill_model"),
        "未配置的撮合口径不该进运行时文件"
    );
    // 反过来：声明了就必须留痕，否则来源标注无从判断。
    let mut declared = config.clone();
    declared.strategy.fill_model = Some("best_price".into());
    assert!(serde_json::to_string(&declared)
        .unwrap()
        .contains("best_price"));
}

/// 不可达与缺前置的组合一律 fail-closed，并且报错说清"为什么"。
///
/// 这里同时钉住校验与装配同源：`fill_model_problem` 与 `bar_fill_model` 用同一张表，
/// 所以 `config validate` 放行过的名字，装配要么跑得动要么只缺 market spec。
#[test]
fn unreachable_or_under_specified_fill_models_fail_closed() {
    let spec = workspace_binance_spot_spec();
    let parsed: TradingInstrumentSpec =
        serde_json::from_str(&std::fs::read_to_string(&spec).unwrap()).unwrap();
    assert!(parsed.price_tick > 0, "示例 spec 必须声明真实一档");

    // 一档滑点没有 spec：拒绝为标的猜一档，并给出可用的替代口径。
    let missing_tick = fill_model_error("one_tick_slippage", None);
    assert!(
        missing_tick.contains("只认 market spec 的 price_tick"),
        "缺 spec 要说清缺的是什么: {missing_tick}"
    );
    assert!(
        missing_tick.contains("next_bar_open") && missing_tick.contains("best_price"),
        "拒绝时要把可用口径摆出来: {missing_tick}"
    );
    // spec 声明了一档为 0 的产品：不能拿它当滑点，也不能静默退成"无滑点"。
    let mut zero_tick = parsed.clone();
    zero_tick.price_tick = 0;
    assert!(fill_model_error("one_tick_slippage", Some(&zero_tick)).contains("不是正数"));

    // 内核另两个成员要 L1/L2L3 盘口，Bar 输入撑不起：报档位，不报"未知模型"。
    for (configured, tier) in [
        ("probabilistic", "L1 一档盘口"),
        ("volume_sensitive", "L2/L3 深度盘口"),
    ] {
        let error = fill_model_error(configured, Some(&parsed));
        assert!(
            error.contains(tier)
                && error.contains("输入只有 OHLCV")
                && error.contains("暂时没有任何回测入口"),
            "{configured} 的拒绝理由要说明档位缺失: {error}"
        );
        assert!(
            !error.contains("未知"),
            "内核有这个名字的模型，不能报成拼写错误: {error}"
        );
        // 校验与装配给同一份结论。
        assert_eq!(
            fill_model_problem(configured).as_deref(),
            Some(error.as_str())
        );
    }
    // 拼错的名字才是"未知"，并且报出可达清单。
    let typo = fill_model_error("next-bar-open", None);
    assert!(typo.contains("未知"), "拼错要说是未知: {typo}");
    assert!(
        typo.contains("next_bar_open / best_price / one_tick_slippage"),
        "未知名字要报全清单: {typo}"
    );
    assert_eq!(
        fill_model_problem("next-bar-open").as_deref(),
        Some(typo.as_str())
    );
    assert!(fill_model_problem(" best_price ").is_none());
    // `config validate` 走同一份判据，只多套一层字段名——报红文案的形状在这里钉住。
    assert_eq!(fill_model_failure(None, "strategy[main]"), None);
    assert_eq!(
        fill_model_failure(Some("next-bar-open"), "strategy[main]"),
        Some(format!("strategy[main].fill_model {typo}"))
    );
}

/// 命令行入口认得同一个字段：给了 `--config` 就必须生效，缺 spec 时同样拒绝。
#[test]
fn command_line_entries_read_the_declared_fill_model() {
    let (deploy, frame, template) = builtin_backtest_example_paths();

    // `backtest builtin`：配置声明一档滑点却没给 market spec → 整轮失败。
    let mut config = read_runtime_config(&template).unwrap();
    config.strategy.fill_model = Some("one_tick_slippage".into());
    let (root, runtime) = isolated_backtest_runtime(&deploy, &config, "q1a2-builtin-tick");
    let error = run_builtin_backtest("sma_cross", &frame, None, 1, Some(&runtime)).unwrap_err();
    assert!(
        error.contains("只认 market spec 的 price_tick"),
        "内置链必须认配置里的口径: {error}"
    );
    // 同一份配置补上 spec 就能跑，且档位取自 spec。
    let spec_dir = temp_cli_case_dir("q1a2-builtin-spec");
    run_builtin_backtest(
        "sma_cross",
        &frame,
        Some(&spot_market_spec(&spec_dir)),
        1,
        Some(&runtime),
    )
    .unwrap();

    // 多腿链：两条腿共用一份声明，产物必须写出来。
    let primary = deploy.join("qianxing.bar-frame.pairs-primary.example.json");
    let reference = deploy.join("qianxing.bar-frame.pairs-reference.example.json");
    let mut declared = read_runtime_config(&template).unwrap();
    declared.strategy.fill_model = Some("best_price".into());
    let (declared_root, declared_runtime) =
        isolated_backtest_runtime(&deploy, &declared, "q1a2-multi-declared");
    run_multi_builtin_backtest(
        "pairs_arbitrage",
        &primary,
        &reference,
        None,
        None,
        1,
        25,
        Some(&declared_root),
        Some(&declared_runtime),
    )
    .unwrap();
    let attribution = read_first_artifact(&declared_root, ".spread-attribution.json");
    assert_eq!(attribution["fill_model"]["name"], "best_price");
    assert_eq!(attribution["fill_model"]["source"], "runtime-config");

    let (plain_root, plain_runtime) = {
        let config = read_runtime_config(&template).unwrap();
        isolated_backtest_runtime(&deploy, &config, "q1a2-multi-plain")
    };
    run_multi_builtin_backtest(
        "pairs_arbitrage",
        &primary,
        &reference,
        None,
        None,
        1,
        25,
        Some(&plain_root),
        Some(&plain_runtime),
    )
    .unwrap();
    let plain = read_first_artifact(&plain_root, ".spread-attribution.json");
    assert_eq!(plain["fill_model"]["name"], "next_bar_open");
    assert_eq!(plain["fill_model"]["source"], "builtin-default");

    for path in [root, spec_dir, declared_root, plain_root] {
        let _ = std::fs::remove_dir_all(path);
    }
}

/// 深度链不经过 `FillModel`：摘要里不该出现这个键，硬塞一个空值等于声称它有口径。
#[test]
fn depth_summary_carries_no_fill_model_key() {
    let (deploy, _, template) = builtin_backtest_example_paths();
    let (root, runtime) = {
        let config = read_runtime_config(&template).unwrap();
        isolated_backtest_runtime(&deploy, &config, "q1a2-depth")
    };
    let out = temp_cli_case_dir("q1a2-depth-out");
    run_depth_backtest(
        "l1",
        "sma_cross",
        &deploy.join("qianxing.depth-frame.l1.example.json"),
        None,
        1,
        None,
        DepthExecutionModel::default(),
        &out,
        Some(&runtime),
    )
    .unwrap();
    let summary = read_first_backtest_summary(&out);
    assert!(
        summary.get("fill_model").is_none(),
        "深度链的撮合口径在四参数描述子里，不该冒充 FillModel"
    );
    let descriptors = summary["model_descriptors"].as_array().unwrap();
    assert!(
        descriptors
            .iter()
            .any(|descriptor| descriptor.as_str().unwrap().starts_with("data_tier=")),
        "深度链自己那份档位描述子必须还在: {descriptors:?}"
    );
    assert!(
        !descriptors
            .iter()
            .any(|descriptor| descriptor.as_str().unwrap().contains("@v1[tier=")),
        "深度链不得出现 Bar FillModel 描述子: {descriptors:?}"
    );
    for path in [root, out] {
        let _ = std::fs::remove_dir_all(path);
    }
}
