//! 两格产品政策（`strategy.leverage` / `strategy.position_mode`）在四条回测链上的落点判据。
//!
//! 它们的落点只有一个：进内核的那张订单上的 `OrderPolicy`。内置策略链派生不出它，于是订单
//! 一律带 `OrderPolicy::default()`（Cash / OneWay / 1x）进内核，而内核**确实**按那份政策记
//! 初始保证金与双向持仓——收下声明再静默丢掉，产物里不会留下任何"这轮其实是 1x"的痕迹
//! （V11 R4-5，与 R11 拒绝 `fill_model` 同一条判据）。

use super::*;

/// 闸门文案里那句后果说明，用例靠它区分"被产品政策拒"与"因别的口径失败"。
const REFUSAL_MARK: &str = "从未生效过的杠杆";

/// 衍生品底子 + 只改要判的那一格：`config validate` 那条拓扑校验只管自相矛盾的写法
/// （现货块只许 Cash/1x/OneWay），要判的这一族得先是一份合法声明才轮得到落点判据。
fn declare(strategy: &mut qx_runtime::StrategyRuntimeConfig, field: &str) {
    strategy.product = Some(TradingProduct::Perpetual);
    strategy.margin_mode = Some(MarginMode::Cross);
    match field {
        "leverage" => strategy.leverage = Some(5),
        "position_mode" => strategy.position_mode = Some(PositionMode::Hedge),
        "none" => {}
        other => panic!("未知的待判字段: {other}"),
    }
}

/// 落一份 CCXT 形状的永续合约快照：`strategy backtest` 在闸门之前还有一条"衍生品必须给
/// market spec"的检查，缺它就只能看到那条错误，看不到这一族的判据。
fn perpetual_market_spec(dir: &Path) -> PathBuf {
    let path = dir.join("binance-btcusdt.swap.market.json");
    std::fs::write(
        &path,
        serde_json::to_string(&serde_json::json!({
            "market_type": "swap",
            "base": "BTC",
            "quote": "USDT",
            "settle": "USDT",
            "contract_size_raw": 1_000_000_000_i128,
            "linear": true,
            "inverse": false,
            "price_tick_raw": 1_000_000_i128,
            "qty_step_raw": 1_000_000_i128,
            "min_qty_raw": 1_000_000_i128,
            "max_leverage": 10,
            "maintenance_margin_bps": 50,
            "valid_from": 0,
            "valid_to": serde_json::Value::Null,
        }))
        .unwrap(),
    )
    .unwrap();
    path
}

/// 只改要判的那一格，底子仍是模板那份完整合法的 `strategy` 块。
fn runtime_declaring(
    deploy: &Path,
    template: &Path,
    label: &str,
    field: &str,
) -> (PathBuf, PathBuf) {
    let mut config = read_runtime_config(template).unwrap();
    declare(&mut config.strategy, field);
    isolated_backtest_runtime(deploy, &config, label)
}

/// 拒绝文案必须同时点名三件事：哪条链、哪一格、后果是什么。缺任何一件，用户都只能整份配置
/// 挨个猜——而这三件恰好是判据自己知道的信息。
fn assert_refused(field: &str, entry: &str, outcome: Option<String>) {
    let error = outcome
        .unwrap_or_else(|| panic!("{field} 声明在 {entry} 上没有落点，却按默认政策静默跑完了"));
    assert!(
        error.contains(entry),
        "{field} 的拒绝要指认是哪条链: {error}"
    );
    assert!(error.contains(field), "{field} 要说出是哪一格: {error}");
    assert!(
        error.contains(REFUSAL_MARK),
        "{field} 的拒绝要交代后果: {error}"
    );
}

#[test]
fn every_builtin_strategy_chain_refuses_a_product_policy_it_cannot_apply() {
    let (deploy, frame, template) = builtin_backtest_example_paths();
    let primary = deploy.join("qianxing.bar-frame.pairs-primary.example.json");
    let reference = deploy.join("qianxing.bar-frame.pairs-reference.example.json");
    let depth_frame = deploy.join("qianxing.depth-frame.l1.example.json");
    let out = temp_cli_case_dir("r4-5-out");

    for field in ["leverage", "position_mode", "none"] {
        let (root, runtime) =
            runtime_declaring(&deploy, &template, &format!("r4-5-{field}"), field);
        let spec_dir = temp_cli_case_dir(&format!("r4-5-spec-{field}"));
        let spec = perpetual_market_spec(&spec_dir);
        let builtin = run_builtin_backtest("sma_cross", &frame, None, 1, Some(&runtime)).err();
        let multi = run_multi_builtin_backtest(
            "pairs_arbitrage",
            &primary,
            &reference,
            None,
            None,
            1,
            0,
            Some(&root),
            Some(&runtime),
        )
        .err();
        let depth = run_depth_backtest(
            "l1",
            "sma_cross",
            &depth_frame,
            None,
            1,
            None,
            DepthExecutionModel::default(),
            &out,
            Some(&runtime),
        )
        .err();
        let strategy = run_strategy_backtest(
            &runtime,
            &frame,
            if field == "none" { None } else { Some(&spec) },
        )
        .err();
        if field == "none" {
            // 正向对照：两格都不声明时，四条链都不该被这道闸门拦下。
            for (entry, error) in [
                ("backtest builtin", &builtin),
                ("backtest multi-builtin", &multi),
                ("backtest book", &depth),
                ("strategy backtest", &strategy),
            ] {
                assert!(
                    !error
                        .as_deref()
                        .is_some_and(|text| text.contains(REFUSAL_MARK)),
                    "{entry} 不该拒一份没有声明产品政策的配置: {error:?}"
                );
            }
        } else {
            assert_refused(field, "backtest builtin / backtest ccxt-builtin", builtin);
            assert_refused(field, "backtest multi-builtin", multi);
            assert_refused(field, "backtest book", depth);
            assert_refused(field, "strategy backtest 的内置策略分支", strategy);
        }
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(spec_dir);
    }
    let _ = std::fs::remove_dir_all(out);
}

/// `strategy backtest` 的内置分支只在派生不出政策时才拒：声明了对冲腿时 `primary_policy`
/// 真的跟着订单进内核，那时这一格有落点，闸门必须让路。
#[test]
fn the_strategy_backtest_builtin_branch_yields_once_the_policy_has_a_landing_site() {
    let (deploy, frame, template) = builtin_backtest_example_paths();
    let (blocked_root, blocked) =
        runtime_declaring(&deploy, &template, "r4-5-builtin-only", "leverage");
    let blocked_out = temp_cli_case_dir("r4-5-blocked");
    std::fs::create_dir_all(&blocked_out).unwrap();
    let refused =
        run_strategy_backtest(&blocked, &frame, Some(&perpetual_market_spec(&blocked_out)))
            .err()
            .unwrap_or_default();
    assert!(
        refused.contains(REFUSAL_MARK),
        "单腿内置策略没有落点，必须拒: {refused}"
    );

    let mut config = read_runtime_config(&template).unwrap();
    declare(&mut config.strategy, "leverage");
    config.strategy.builtin_reference_instrument = Some("ETHUSDT.BINANCE".into());
    let (wired_root, wired) = isolated_backtest_runtime(&deploy, &config, "r4-5-policy-wired");
    if let Err(error) =
        run_strategy_backtest(&wired, &frame, Some(&perpetual_market_spec(&wired_root)))
    {
        assert!(
            !error.contains(REFUSAL_MARK),
            "对冲腿已声明，政策有落点，闸门不该再拒: {error}"
        );
    }
    let _ = std::fs::remove_dir_all(blocked_root);
    let _ = std::fs::remove_dir_all(blocked_out);
    let _ = std::fs::remove_dir_all(wired_root);
}

/// 只在 `strategies[]` 里声明同样要拒：判据扫的是每一条策略块，写进列表绕不过闸门（V11 R1）。
/// 与 O2 那六格不同，这里不比"两侧是不是同一份"——两格在本链压根没有落点，写在哪儿都不会
/// 生效，所以顶层单独声明同样拒（正向对照交给 `field = "none"` 那一轮）。
#[test]
fn an_instance_only_product_policy_declaration_is_refused_too() {
    let (deploy, _, template) = builtin_backtest_example_paths();
    let mut config = read_runtime_config(&template).unwrap();
    let mut block = config.strategy.clone();
    declare(&mut block, "leverage");
    block.id = Some("solo".into());
    config.strategies = vec![block];
    // 拓扑校验要求每条实例都有启用的 Strategy worker 接住：这份声明是合法拓扑，
    // 正因如此它才必须被拒在"这条链没有落点"上，而不是被拓扑先挡掉。
    let mut solo_worker = config
        .workers
        .iter()
        .find(|worker| worker.enabled && worker.role == WorkerRole::Strategy)
        .cloned()
        .expect("示例运行时配置里本就该有启用的 Strategy worker");
    solo_worker.id = "solo".into();
    config.workers.push(solo_worker);
    let (_root, runtime) = isolated_backtest_runtime(&deploy, &config, "r4-5-instance-only");
    let error = reject_configured_product_policy(Some(&runtime), "backtest book").unwrap_err();
    assert!(error.contains("strategy[solo]"), "要指认是哪一块: {error}");
    assert!(error.contains("leverage"), "要说出是哪一格: {error}");
    assert!(error.contains(REFUSAL_MARK), "要交代后果: {error}");
    // 顶层单独声明：这一格在本链同样没有落点，不能因为"写在了被读的那一块"就放行。
    let (_top_root, top) = runtime_declaring(&deploy, &template, "r4-5-top-only", "position_mode");
    let error = reject_configured_product_policy(Some(&top), "backtest book").unwrap_err();
    assert!(error.contains("strategy.position_mode"), "{error}");
}
