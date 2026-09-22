//! market spec 单一读法的用例（V11 Q54）。
//!
//! 这一族用例钉的是一件事：**回测链和 live/paper worker 必须把同一份文件读成同一份口径**。
//! 之前回测只认 CCXT 归一化形状，而 worker 两种都认，于是 `init` 打印的首条回测命令
//! （带着它自己生成的那份 `qianxing.binance.spot.spec.json`）直接失败在
//! `CCXT market 缺少 base`。第二件事是缺字段一律拒绝：兜底的 `1` 与真实声明的 `1`
//! 在产物里长得一模一样，闸门却已经废了。

use super::*;

/// 仓库现货规格的 instrument，用例一律从这里取，不抄第二份字面量。
fn btcusdt() -> InstrumentId {
    InstrumentId::parse("BTCUSDT.BINANCE").expect("仓库夹具的 instrument 合法")
}

fn market_value(path: &Path) -> serde_json::Value {
    let payload = std::fs::read_to_string(path).unwrap();
    serde_json::from_str(&payload).unwrap()
}

/// 把已冻结的产品规格改写成 CCXT 归一化形状（同一份事实，交易所侧的字段名）。
fn ccxt_shaped(market: &serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "market_type": market["product"],
        "base": market["base_currency"],
        "quote": market["quote_currency"],
        "settle": market["settlement_currency"],
        "contract_size_raw": market["contract_size"],
        "linear": market["linear"],
        "inverse": market["inverse"],
        "price_tick_raw": market["price_tick"],
        "qty_step_raw": market["qty_step"],
        "min_qty_raw": market["min_qty"],
        "max_leverage": market["max_leverage"],
        "maintenance_margin_bps": market["maintenance_margin_bps"],
        "valid_to": market["valid_to"],
    })
}

/// 两种形状读到同一份事实：这是"回测与实盘同源"最直接的一条等式。
#[test]
fn both_market_spec_shapes_resolve_to_the_same_spec() {
    let path = workspace_binance_spot_spec();
    let frozen = market_value(&path);
    assert!(
        frozen.get("base_currency").is_some(),
        "夹具必须仍是产品规格形状，否则本用例什么都没比: {frozen}"
    );
    let from_frozen = market_spec_from_value(&btcusdt(), &frozen).unwrap();
    let from_ccxt = market_spec_from_value(&btcusdt(), &ccxt_shaped(&frozen)).unwrap();
    assert_eq!(
        from_frozen, from_ccxt,
        "同一份市场的两种形状必须读出同一份规格"
    );
    assert_eq!(from_frozen.price_tick, 1_000_000);
}

/// 规格属于别的标的时必须拒绝：位置参数没有名字可核对，instrument 是唯一防线。
#[test]
fn a_spec_for_another_instrument_fails_closed() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy")
        .join("qianxing.ccxt.okx.perpetual.spec.json");
    let eth = InstrumentId::parse("ETHUSDT.BINANCE").unwrap();
    let problem = market_spec_from_value(&eth, &market_value(&path)).unwrap_err();
    assert!(
        problem.contains("与标的") && problem.contains("ETHUSDT.BINANCE"),
        "错标的要报出双方身份: {problem}"
    );
}

/// CCXT 快照缺精度口径时不再凭空兜底：报错点名缺哪一项、怎么补。
#[test]
fn an_under_specified_ccxt_market_is_refused_not_invented() {
    let frozen = market_value(&workspace_binance_spot_spec());
    for key in ["price_tick_raw", "qty_step_raw", "min_qty_raw"] {
        let mut market = ccxt_shaped(&frozen);
        market[key] = serde_json::Value::Null;
        let problem = market_spec_from_value(&btcusdt(), &market).unwrap_err();
        assert!(
            problem.contains(key) && problem.contains("缺少"),
            "缺 {key} 必须点名它，而不是退回一个最小单位: {problem}"
        );
        assert!(
            problem.contains("base_currency"),
            "要告诉使用者补不上时改传哪种形状: {problem}"
        );
    }
    // 衍生品没有杠杆上限与维持保证金率时，兜底等于放行 100x / 猜一个 500bp。
    for key in ["max_leverage", "maintenance_margin_bps"] {
        let mut market = ccxt_shaped(&frozen);
        market["market_type"] = serde_json::Value::String("swap".into());
        market["linear"] = serde_json::Value::Bool(true);
        market["max_leverage"] = serde_json::json!(10);
        market[key] = serde_json::Value::Null;
        let problem = market_spec_from_value(&btcusdt(), &market).unwrap_err();
        assert!(
            problem.contains(key),
            "衍生品缺 {key} 必须拒绝而不是猜: {problem}"
        );
    }
    // 反例：现货没有这两项可猜，缺省时 1x / 0bp 是唯一安全值，所以照旧放行。
    let mut spot = ccxt_shaped(&frozen);
    spot["max_leverage"] = serde_json::Value::Null;
    spot["maintenance_margin_bps"] = serde_json::Value::Null;
    assert_eq!(
        market_spec_from_value(&btcusdt(), &spot)
            .unwrap()
            .max_leverage,
        1
    );
}

/// 首条 `init` 命令形状：回测链必须吃得下 `init` 自己生成的那份产品规格。
///
/// 反向验证：把 [`super::market_spec_with_margin`] 换回只走 CCXT 形状，本用例立刻死在
/// `CCXT market 缺少 base`（V11 §16.2 记录过这条命令行证据）。
#[test]
fn the_backtest_chain_reads_the_generated_frozen_spec() {
    let (deploy, frame, template) = builtin_backtest_example_paths();
    let mut config = read_runtime_config(&template).unwrap();
    config.strategy.fill_model = Some("one_tick_slippage".into());
    let (root, runtime) = isolated_backtest_runtime(&deploy, &config, "q54-frozen-spec");
    let spec = deploy.join("qianxing.binance.spot.spec.json");
    assert!(spec.exists(), "init 生成的就是这份夹具");
    run_strategy_backtest(&runtime, &frame, Some(&spec)).unwrap_or_else(|error| {
        panic!("回测链读得懂 worker 那一形状的 spec，实际: {error}");
    });
    let summary = read_first_backtest_summary(&root);
    assert!(
        summary["fills"].as_i64().unwrap_or_default() > 0,
        "换了 spec 也要真的成交: {summary}"
    );
    // 一档取自那份文件声明的 price_tick，不是任何兜底值。
    let descriptors = summary["model_descriptors"].as_array().unwrap();
    assert!(
        descriptors[0].as_str().unwrap().contains("tick=1000000"),
        "滑点档位必须来自夹具: {descriptors:?}"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// 回测链必须**当场**吃得下 CCXT 形状，而不是只在 loader 的单元测试里吃得下。
///
/// 上一条用例喂给回测的是仓库里那份冻结产品规格，所以把回测侧的 loader 调用换成
/// `serde_json::from_value::<TradingInstrumentSpec>` 它照样能过——真正咬住"回测链只认
/// 一种形状"这个原始缺陷的是这里。反向验证见 V11 §17：M1 变异下本用例必须死在
/// `missing field \`instrument\``。
#[test]
fn the_backtest_chain_reads_a_ccxt_shaped_spec_to_the_same_result() {
    let (deploy, frame, template) = builtin_backtest_example_paths();
    let frozen = market_value(&deploy.join("qianxing.binance.spot.spec.json"));
    let mut config = read_runtime_config(&template).unwrap();
    config.strategy.fill_model = Some("one_tick_slippage".into());
    let mut hashes = Vec::new();
    for (label, market) in [
        ("frozen-shape", frozen.clone()),
        ("ccxt-shape", ccxt_shaped(&frozen)),
    ] {
        let (root, runtime) = isolated_backtest_runtime(&deploy, &config, &format!("q55-{label}"));
        let spec = root.join(format!("{label}.spec.json"));
        std::fs::write(&spec, serde_json::to_string_pretty(&market).unwrap()).unwrap();
        run_strategy_backtest(&runtime, &frame, Some(&spec))
            .unwrap_or_else(|error| panic!("回测链读不懂 {label}，实际: {error}"));
        let summary = read_first_backtest_summary(&root);
        assert!(
            summary["fills"].as_i64().unwrap_or_default() > 0,
            "{label} 也要真的成交: {summary}"
        );
        assert!(
            summary["model_descriptors"].as_array().unwrap()[0]
                .as_str()
                .unwrap()
                .contains("tick=1000000"),
            "{label} 的一档必须来自文件声明而非兜底: {summary}"
        );
        hashes.push(summary["result_hash"].as_str().unwrap().to_string());
        let _ = std::fs::remove_dir_all(root);
    }
    assert_eq!(
        hashes[0], hashes[1],
        "同一份市场的两种形状必须给同一个结果，否则单一读法只是口号"
    );
}

/// 摘要背后的 RunManifest 里那一行规格来源：读的是产物，不是 stdout。
fn published_spec_source(summary: &serde_json::Value) -> String {
    let manifest_path = summary["run_manifest"].as_str().unwrap_or_default();
    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(manifest_path).unwrap()).unwrap();
    manifest["instrument_spec_version"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// 规格来源必须按**实际判定的形状**写进产物，而不是按"有没有传路径"。
///
/// 三种可达输入各得一个标签：产品规格形状、CCXT 归一化形状、根本没给 spec。旧口径只有后
/// 两种的区分度，于是前两种在同一帧同一配置下顶着一个 `ccxt-market-spec-v1`，而规格内容
/// 不进 `model_fingerprint`，这一行是唯一交代"按哪种形状读的"地方（V11 Q63）。
///
/// 反向验证：把 `single_strategy.rs` 写进产物的那一行换回 `instrument_spec_version:
/// CCXT_MARKET_SPEC_VERSION`，本用例死在第一档的期望上（门禁日志的 MQ63a）。
#[test]
fn each_market_spec_shape_publishes_its_own_source_label() {
    let (deploy, frame, template) = builtin_backtest_example_paths();
    let frozen = market_value(&deploy.join("qianxing.binance.spot.spec.json"));
    let config = read_runtime_config(&template).unwrap();
    let mut published = Vec::new();
    for (case, market) in [
        ("product-shape", Some(frozen.clone())),
        ("ccxt-shape", Some(ccxt_shaped(&frozen))),
        ("no-spec", None),
    ] {
        let (root, runtime) = isolated_backtest_runtime(&deploy, &config, &format!("q63-{case}"));
        let spec = root.join(format!("{case}.spec.json"));
        if let Some(market) = market {
            std::fs::write(&spec, serde_json::to_string_pretty(&market).unwrap()).unwrap();
        }
        run_strategy_backtest(&runtime, &frame, spec.exists().then_some(&spec))
            .unwrap_or_else(|error| panic!("回测链读不懂 {case}，实际: {error}"));
        let summary = read_first_backtest_summary(&root);
        let label = published_spec_source(&summary);
        let expected = match case {
            "product-shape" => PRODUCT_MARKET_SPEC_VERSION,
            "ccxt-shape" => CCXT_MARKET_SPEC_VERSION,
            _ => DEFAULT_INSTRUMENT_SPEC_VERSION,
        };
        assert_eq!(
            label, expected,
            "{case} 的来源标签必须按实际判定的形状写，摘要: {summary}"
        );
        published.push(label);
        let _ = std::fs::remove_dir_all(root);
    }
    published.sort();
    published.dedup();
    assert_eq!(
        published.len(),
        3,
        "三种可达输入必须给出三种来源；两档标签会把两种形状混成一种: {published:?}"
    );
}

/// 深度链是第二条把规格来源写进产物的链，它必须问同一个形状问题。
///
/// 单独一条用例而不是复用上一条：两条链各自有一处 `RunManifestIdentity`，任何一处回到
/// "按有没有路径猜"都只在**自己**那档输入上说谎（V11 Q63 的 MQ63b 变异只让本用例变红）。
#[test]
fn the_depth_chain_publishes_the_shape_it_actually_read() {
    let (deploy, _, template) = builtin_backtest_example_paths();
    let frozen = market_value(&deploy.join("qianxing.binance.spot.spec.json"));
    let config = read_runtime_config(&template).unwrap();
    let mut published = Vec::new();
    for (case, market) in [
        ("product-shape", Some(frozen.clone())),
        ("ccxt-shape", Some(ccxt_shaped(&frozen))),
        ("no-spec", None),
    ] {
        let (root, runtime) =
            isolated_backtest_runtime(&deploy, &config, &format!("q63-depth-{case}"));
        let out = temp_cli_case_dir(&format!("q63-depth-{case}"));
        let spec = root.join(format!("{case}.spec.json"));
        if let Some(market) = market {
            std::fs::write(&spec, serde_json::to_string_pretty(&market).unwrap()).unwrap();
        }
        run_depth_backtest(
            "l1",
            "sma_cross",
            &deploy.join("qianxing.depth-frame.l1.example.json"),
            spec.exists().then_some(&spec),
            1,
            None,
            DepthExecutionModel::default(),
            &out,
            Some(&runtime),
        )
        .unwrap_or_else(|error| panic!("深度链读不懂 {case}，实际: {error}"));
        let summary = read_first_backtest_summary(&out);
        let label = published_spec_source(&summary);
        let expected = match case {
            "product-shape" => PRODUCT_MARKET_SPEC_VERSION,
            "ccxt-shape" => CCXT_MARKET_SPEC_VERSION,
            _ => DEFAULT_INSTRUMENT_SPEC_VERSION,
        };
        assert_eq!(
            label, expected,
            "{case} 的深度链来源标签必须按实际判定的形状写，摘要: {summary}"
        );
        published.push(label);
        for path in [root, out] {
            let _ = std::fs::remove_dir_all(path);
        }
    }
    published.sort();
    published.dedup();
    assert_eq!(
        published.len(),
        3,
        "深度链也要给出三种可达来源，否则它只是在复用别人的证据: {published:?}"
    );
}
