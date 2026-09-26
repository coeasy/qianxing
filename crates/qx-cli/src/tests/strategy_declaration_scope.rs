use super::*;

/// 命令行回测的四处读点（`configured_fill_model`、`configured_initial_cash_raw`、
/// `configured_instrument_product`、`backtest_risk_binding`）取的都是顶层
/// `strategy.<字段>`，而 `config validate` 的逐块体检把 `strategies[]` 也算在内：一份"只在
/// 实例里写了撮合口径"的配置能校验通过、跑起来却按内核默认走。风控门那一处一次取走
/// `risk_rules`、`margin_mode`、`allow_short` 三格，合计六格。这正是 V11 N5 给
/// `cost_rules_path` 记下的那道断层，这里把同一条判据铺到其余六格（V11 O2）。
///
/// 判据只放在命令行回测侧：`strategy backtest` 那条链逐块装配（`strategy_config.strategy =
/// strategy`），实例声明在那儿是真生效，搬到 `config validate` 反而会把那份合法配置拒掉。
///
/// 声明按 JSON 写：判据要认的是用户实际落盘的那份形状，而不是 Rust 字段的默认值。
fn runtime_declaring(
    label: &str,
    fields: &serde_json::Value,
    where_to: &[&str],
) -> (PathBuf, PathBuf) {
    let (deploy, _, template) = builtin_backtest_example_paths();
    let mut doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&template).unwrap()).unwrap();
    // 拿模板里那份完整合法的 strategy 块当底子，只覆盖要判的几格。
    let base = doc["strategy"].clone();
    for target in where_to {
        let mut block = base.clone();
        for (key, value) in fields.as_object().expect("声明得是一个对象") {
            block[key] = value.clone();
        }
        match *target {
            // 顶层那一格沿用模板自己的 id：换了它，接住它的 worker 就找不到了。
            "strategy" => {
                block["id"] = base["id"].clone();
                doc["strategy"] = block;
            }
            "strategies" => {
                // 拓扑校验要求每条实例都有一个启用的 Strategy worker 接住（V11 E7 那一批的
                // 口径），所以这份声明是合法拓扑，不是坏配置——正因如此它才必须被拒在
                // "没人应用"这一条上，而不是被拓扑先挡掉。
                let id = block["id"].as_str().expect("实例声明要带 id").to_string();
                let workers = doc["workers"]
                    .as_array_mut()
                    .expect("示例配置本该有 workers");
                let seeded = workers
                    .iter()
                    .find(|worker| worker["role"] == "strategy" && worker["enabled"] == true)
                    .cloned()
                    .expect("示例运行时配置里本就该有启用的 Strategy worker");
                let mut extra = seeded;
                extra["id"] = serde_json::json!(id);
                workers.push(extra);
                doc["strategies"] = serde_json::json!([block]);
            }
            other => panic!("未知的声明位置: {other}"),
        }
    }
    let config = RuntimeConfig::from_json(&serde_json::to_string(&doc).unwrap()).unwrap();
    isolated_backtest_runtime(&deploy, &config, label)
}

/// 四格读点各自报自己那一格：`outcome` 是 `Err` 时要求文案点名 `field` 与那块声明。
fn assert_refused(field: &str, outcome: Result<(), String>, block: &str) {
    let error = outcome.expect_err("只在实例里声明的口径不该被当成\"没声明\"静默走默认");
    assert!(
        error.contains("不会被应用"),
        "{field} 的拒绝要交代后果: {error}"
    );
    assert!(error.contains(block), "{field} 要指认是哪一块: {error}");
    assert!(error.contains(field), "{field} 要说出是哪一格: {error}");
}

fn read_all_four(runtime: &Path) -> [(&'static str, Result<(), String>); 4] {
    [
        (
            "fill_model",
            configured_fill_model(Some(runtime)).map(|_| ()),
        ),
        (
            "initial_cash_raw",
            configured_initial_cash_raw(Some(runtime)).map(|_| ()),
        ),
        (
            "product",
            configured_instrument_product(Some(runtime)).map(|_| ()),
        ),
        (
            "risk_rules",
            backtest_risk_binding(Some(runtime), true).map(|_| ()),
        ),
    ]
}

#[test]
fn an_instance_only_declaration_is_refused_rather_than_silently_ignored() {
    let (_root, runtime) = runtime_declaring(
        "decl-scope-solo",
        &serde_json::json!({
            "id": "solo",
            "fill_model": "immediate",
            "initial_cash_raw": 250_000,
            "product": "perpetual",
            "margin_mode": "cross",
            "allow_short": true,
            "risk_rules": {"version": "solo-rules"},
        }),
        &["strategies"],
    );
    for (field, outcome) in read_all_four(&runtime) {
        assert_refused(field, outcome, "strategies[solo]");
    }
    // 风控门那三条声明同在一处读点：逐格都要能单独咬住，不能只靠 risk_rules 代言。
    // 每条都自带 `product: perpetual`：现货块只许 Cash/1x/OneWay（拓扑校验），那会把"实例
    // 声明合法、只是没人应用"这份形状先拒掉，判据就再也咬不到了。
    for (field, value) in [
        ("margin_mode", serde_json::json!("isolated")),
        ("allow_short", serde_json::json!(true)),
        ("risk_rules", serde_json::json!({"version": "lone-rules"})),
    ] {
        let (_solo_root, solo_runtime) = runtime_declaring(
            &format!("decl-scope-{field}"),
            &serde_json::json!({"id": "solo", "product": "perpetual", field: value}),
            &["strategies"],
        );
        let error = backtest_risk_binding(Some(&solo_runtime), true).unwrap_err();
        assert_refused(field, Err(error), "strategies[solo]");
    }
}

#[test]
fn the_same_declaration_on_both_blocks_still_assembles() {
    let declared = serde_json::json!({
        "id": "agree",
        "fill_model": "immediate",
        "initial_cash_raw": 250_000,
        "product": "perpetual",
        "margin_mode": "cross",
        "allow_short": true,
        "risk_rules": {"version": "same-rules"},
    });
    let (_root, runtime) = runtime_declaring("decl-scope-agree", &declared, &["strategy"]);
    // 正向对照：顶层与实例写同一份时，四格读点照常取值，预算没有把正常配置一并挡掉。
    let (_agree_root, agree_runtime) = runtime_declaring(
        "decl-scope-agree-both",
        &declared,
        &["strategy", "strategies"],
    );
    for path in [&runtime, &agree_runtime] {
        for (field, outcome) in read_all_four(path) {
            outcome.unwrap_or_else(|error| panic!("{field} 在两侧同一份声明下不该被拒: {error}"));
        }
    }
    assert_eq!(
        configured_fill_model(Some(&agree_runtime))
            .unwrap()
            .as_deref(),
        Some("immediate")
    );
    assert_eq!(
        configured_initial_cash_raw(Some(&agree_runtime)).unwrap(),
        Some(250_000)
    );
    assert_eq!(
        configured_instrument_product(Some(&agree_runtime)).unwrap(),
        Some(TradingProduct::Perpetual)
    );
}

#[test]
fn a_conflicting_declaration_between_the_two_blocks_is_refused() {
    let (_root, runtime) = runtime_declaring(
        "decl-scope-top",
        &serde_json::json!({"fill_model": "one_tick_slippage"}),
        &["strategy"],
    );
    // 顶层单独声明是正常用法：判据只咬"实例写了、顶层没写或写得不一样"。
    assert_eq!(
        configured_fill_model(Some(&runtime)).unwrap().as_deref(),
        Some("one_tick_slippage")
    );
    let (_root, runtime) = runtime_declaring(
        "decl-scope-conflict",
        &serde_json::json!({"id": "conflict", "fill_model": "immediate"}),
        &["strategy", "strategies"],
    );
    // 两侧写的是同一份（都来自这一格），所以先把顶层改回另一档，制造真冲突。
    let mut doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&runtime).unwrap()).unwrap();
    doc["strategy"]["fill_model"] = serde_json::json!("one_tick_slippage");
    std::fs::write(&runtime, serde_json::to_string(&doc).unwrap()).unwrap();
    let error = configured_fill_model(Some(&runtime)).unwrap_err();
    assert!(error.contains("顶层当前生效的是"), "{error}");
    assert!(error.contains("one_tick_slippage"), "{error}");
    assert!(error.contains("immediate"), "{error}");
}
