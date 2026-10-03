use super::*;

/// 成本规则文件在用例里的固定文件名；`isolated_backtest_runtime` 把运行时配置落在
/// 临时目录根部，相对路径就是按那个目录解析的。
const COST_FILE: &str = "costs.json";

/// 构造一份完整的成本规则 JSON。`ExecutionCostRules` 用的是 `#[serde(default)]`，
/// 少写一个字段就会拿内核默认值补上，那样"改了这一项"与"没改"就分不开了。
fn cost_rules_json(name: &str, maker_bp: i64, taker_bp: i64, latency_base_ns: u64) -> String {
    serde_json::to_string(&serde_json::json!({
        "name": name,
        "maker_bp": maker_bp,
        "taker_bp": taker_bp,
        "latency_base_ns": latency_base_ns,
        "latency_insert_ns": 0,
    }))
    .unwrap()
}

/// 带成本规则文件的运行时配置：写文件、把路径挂进 `strategy.cost_rules_path`。
fn runtime_with_cost_rules(
    deploy: &Path,
    template: &Path,
    label: &str,
    rules: &str,
) -> (PathBuf, PathBuf) {
    let mut config = read_runtime_config(template).unwrap();
    config.strategy.cost_rules_path = Some(COST_FILE.into());
    let (root, runtime) = isolated_backtest_runtime(deploy, &config, label);
    std::fs::write(root.join(COST_FILE), rules).unwrap();
    (root, runtime)
}

/// 归因产物里的金额字段是字符串（`i128` 的十进制口径），断言前先取回来。
fn attribution_total(attribution: &serde_json::Value, key: &str) -> i128 {
    attribution["totals"][key]
        .as_str()
        .unwrap_or_else(|| panic!("归因产物缺少 totals.{key}"))
        .parse()
        .unwrap()
}

/// 只换成本文件跑一遍多腿回测，返回归因产物与它的临时目录。
fn multi_attribution_with_costs(label: &str, rules: &str) -> (serde_json::Value, PathBuf) {
    let (deploy, _, template) = builtin_backtest_example_paths();
    let (root, runtime) = runtime_with_cost_rules(&deploy, &template, label, rules);
    run_multi_builtin_backtest(
        "pairs_arbitrage",
        &deploy.join("qianxing.bar-frame.pairs-primary.example.json"),
        &deploy.join("qianxing.bar-frame.pairs-reference.example.json"),
        None,
        None,
        1,
        0,
        Some(&root),
        Some(&runtime),
    )
    .unwrap();
    (read_first_artifact(&root, ".spread-attribution.json"), root)
}

/// V11 §3B 末条（E3 的死配置面）的正面用例：`strategy.cost_rules_path` 不是一行死配置。
///
/// 反向验证口径就在断言里——把 `BarBacktestAssembly::new` 的成本入参换回内置默认，
/// "费率 0 的那一轮费用仍为 0、费率 25bp 的那一轮费用变成名义额的 25bp" 立刻不成立。
#[test]
fn cost_rules_file_changes_backtest_fees() {
    // 没配成本文件时连键都不序列化：已验收的 config_fingerprint 与 RunManifest 必须逐字节不变。
    let template = builtin_backtest_example_paths().2;
    let config = read_runtime_config(&template).unwrap();
    assert!(config.strategy.cost_rules_path.is_none());
    assert!(
        !serde_json::to_string(&config)
            .unwrap()
            .contains("cost_rules_path"),
        "未配置的成本口径不该进运行时文件"
    );

    let (silent, silent_root) =
        multi_attribution_with_costs("q0c-fee-0", &cost_rules_json("f0", 0, 0, 0));
    let (priced, priced_root) =
        multi_attribution_with_costs("q0c-fee-25", &cost_rules_json("f25", 0, 25, 0));

    // 成本只改价格、不改行为：两条腿的成交名义额必须一模一样。
    let turnover = attribution_total(&silent, "turnover_raw");
    assert!(turnover > 0, "多腿示例本来就该有成交，否则费率断言是空转");
    assert_eq!(
        attribution_total(&priced, "turnover_raw"),
        turnover,
        "换费率不该改变成交"
    );
    assert_eq!(attribution_total(&silent, "fees_raw"), 0);
    assert_eq!(
        attribution_total(&priced, "fees_raw") * 10_000,
        turnover * 25,
        "归因费用必须按配置里的 taker_bp 计提"
    );

    // 产物还要说清这组数字从哪来：两份产物各自指向自己那份成本文件。
    for (attribution, root) in [(&silent, &silent_root), (&priced, &priced_root)] {
        assert_eq!(
            attribution["execution_costs"]["source"],
            format!("cost-rules-file:{}", root.join(COST_FILE).display())
        );
    }

    for root in [silent_root, priced_root] {
        let _ = std::fs::remove_dir_all(root);
    }
}

/// 三种来源必须在产物里可区分：`builtin-default`（压根没给配置）不等于
/// `runtime-config-default`（给了配置但没挂成本文件），后者又不等于真的读了文件。
#[test]
fn cost_source_distinguishes_unset_from_default_rates() {
    let (deploy, _, template) = builtin_backtest_example_paths();
    let no_root = temp_cli_case_dir("q0c-source-none");
    run_multi_builtin_backtest(
        "pairs_arbitrage",
        &deploy.join("qianxing.bar-frame.pairs-primary.example.json"),
        &deploy.join("qianxing.bar-frame.pairs-reference.example.json"),
        None,
        None,
        1,
        0,
        Some(&no_root),
        None,
    )
    .unwrap();
    assert_eq!(
        read_first_artifact(&no_root, ".spread-attribution.json")["execution_costs"]["source"],
        "builtin-default"
    );

    let (plain_root, plain_runtime) = {
        let config = read_runtime_config(&template).unwrap();
        isolated_backtest_runtime(&deploy, &config, "q0c-source-config")
    };
    run_multi_builtin_backtest(
        "pairs_arbitrage",
        &deploy.join("qianxing.bar-frame.pairs-primary.example.json"),
        &deploy.join("qianxing.bar-frame.pairs-reference.example.json"),
        None,
        None,
        1,
        0,
        Some(&plain_root),
        Some(&plain_runtime),
    )
    .unwrap();
    assert_eq!(
        read_first_artifact(&plain_root, ".spread-attribution.json")["execution_costs"]["source"],
        "runtime-config-default"
    );

    for root in [no_root, plain_root] {
        let _ = std::fs::remove_dir_all(root);
    }
}

/// 成本规则里的延迟只有 Bar 内核吃得下：那条链必须把它装进撮合，深度链必须拒绝，
/// 两边都不许把"配了延迟"演成"延迟为 0"。
#[test]
fn cost_rules_latency_reaches_the_bar_kernel_and_is_refused_by_depth() {
    let (deploy, frame, template) = builtin_backtest_example_paths();
    let rules = cost_rules_json("latency", 2, 5, 2_000_000);
    let (root, runtime) = runtime_with_cost_rules(&deploy, &template, "q0c-latency", &rules);

    run_strategy_backtest(&runtime, &frame, None).unwrap();
    let summary = read_first_backtest_summary(&root);
    assert_eq!(
        summary["execution_costs"]["source"],
        format!("cost-rules-file:{}", root.join(COST_FILE).display())
    );
    let descriptors = summary["model_descriptors"].as_array().unwrap();
    assert!(
        descriptors.iter().any(|descriptor| {
            descriptor.as_str().unwrap() == "StaticLatency@v1[base_ns=2000000;insert_ns=0]"
        }),
        "Bar 摘要必须写明实际生效的延迟模型: {descriptors:?}"
    );

    let depth_root = temp_cli_case_dir("q0c-latency-depth");
    let error = run_depth_backtest(
        "l1",
        "sma_cross",
        &deploy.join("qianxing.depth-frame.l1.example.json"),
        None,
        1,
        None,
        DepthExecutionModel::default(),
        &depth_root,
        Some(&runtime),
    )
    .unwrap_err();
    assert!(
        error.contains("深度档回测不接受成本规则里的延迟设置"),
        "深度链要显式拒绝非零延迟: {error}"
    );
    assert!(error.contains(COST_FILE), "报错要带上那份成本文件: {error}");

    for path in [root, depth_root] {
        let _ = std::fs::remove_dir_all(path);
    }
}

/// 深度档跑一份成本文件（可选显式 `--fee-bps`），返回摘要与两个临时目录。
fn depth_summary_with_costs(
    deploy: &Path,
    template: &Path,
    label: &str,
    rules: &str,
    flag: Option<i64>,
) -> (serde_json::Value, PathBuf, PathBuf) {
    let (root, runtime) = runtime_with_cost_rules(deploy, template, label, rules);
    let out = temp_cli_case_dir(label);
    run_depth_backtest(
        "l1",
        "sma_cross",
        &deploy.join("qianxing.depth-frame.l1.example.json"),
        None,
        1,
        flag,
        DepthExecutionModel::default(),
        &out,
        Some(&runtime),
    )
    .unwrap();
    (read_first_backtest_summary(&out), root, out)
}

/// 深度档的三层优先级：显式 `--fee-bps` > 成本文件 taker_bp > 内核默认。
#[test]
fn explicit_fee_flag_beats_the_cost_rules_file_on_depth() {
    let (deploy, _, template) = builtin_backtest_example_paths();
    let rules_25 = cost_rules_json("depth-25", 0, 25, 0);
    // 与内核默认费率取同一个数字的成本文件：只有"真的读了文件"才让两轮费用差 5 倍，
    // 否则缺省值退回内核常数时产物照样自称 cost-rules-file，光看来源字符串发现不了。
    let rules_5 = cost_rules_json("depth-5", 0, 5, 0);
    let (from_file, file_root, file_out) =
        depth_summary_with_costs(&deploy, &template, "q0c-depth-25", &rules_25, None);
    let (baseline, base_root, base_out) =
        depth_summary_with_costs(&deploy, &template, "q0c-depth-5", &rules_5, None);
    let (flagged, flag_root, flag_out) =
        depth_summary_with_costs(&deploy, &template, "q0c-depth-flag", &rules_25, Some(0));

    let fees = |summary: &serde_json::Value| summary["metrics"]["fees_raw"].as_i64().unwrap();
    let turnover =
        |summary: &serde_json::Value| summary["metrics"]["turnover_raw"].as_i64().unwrap();

    for (summary, root) in [(&from_file, &file_root), (&baseline, &base_root)] {
        assert_eq!(
            summary["execution_costs"]["source"],
            format!("cost-rules-file:{}", root.join(COST_FILE).display())
        );
    }
    assert!(turnover(&from_file) > 0, "深度示例本来就该有成交");
    assert_eq!(
        turnover(&baseline),
        turnover(&from_file),
        "换费率不该改变成交"
    );
    assert!(
        fees(&from_file) > fees(&baseline),
        "taker_bp 25 的费用必须真的高于 5：{:#?}",
        [fees(&from_file), fees(&baseline)]
    );

    assert_eq!(flagged["execution_costs"]["source"], "cli-flag");
    assert_eq!(fees(&flagged), 0);
    assert_eq!(
        turnover(&flagged),
        turnover(&from_file),
        "旗标只该换费率，不该换成交"
    );

    for path in [
        file_root, file_out, base_root, base_out, flag_root, flag_out,
    ] {
        let _ = std::fs::remove_dir_all(path);
    }
}

/// 成本文件缺失 / 坏 JSON / 费率越界一律失败，并且带上路径；`config validate` 与装配
/// 必须给同一份结论，否则"校验说没问题、装配跑不动"又会分裂成两件事。
#[test]
fn dangling_or_invalid_cost_rules_file_fails_closed() {
    let (deploy, _, template) = builtin_backtest_example_paths();
    let (root, runtime) = runtime_with_cost_rules(
        &deploy,
        &template,
        "q0c-fail-closed",
        &cost_rules_json("placeholder", 2, 5, 0),
    );
    // 文件根本不存在：装配与校验都要报，且报的是同一个路径。
    std::fs::remove_file(root.join(COST_FILE)).unwrap();
    let error = execution_cost_binding(Some(&runtime)).unwrap_err();
    assert!(
        error.contains("读取执行成本规则失败") && error.contains(COST_FILE),
        "缺文件要带着路径失败: {error}"
    );
    let problem = cost_rules_problem(&runtime, COST_FILE).unwrap();
    assert!(problem.contains("文件不存在"), "{problem}");

    // 内容非法：坏 JSON 与越界费率都要在这里被拦住。
    std::fs::write(root.join("bad.json"), "{ not json").unwrap();
    assert!(cost_rules_problem(&runtime, "bad.json")
        .unwrap()
        .contains("内容非法"));
    std::fs::write(
        root.join("huge.json"),
        cost_rules_json("huge", 0, 20_000, 0),
    )
    .unwrap();
    assert!(cost_rules_problem(&runtime, "huge.json")
        .unwrap()
        .contains("0..=10000"));

    // 仓库里的示例模板必须过生产加载器，而且口径与内核默认一致。
    let example = deploy.join("qianxing.costs.example.json");
    assert!(cost_rules_problem(&template, "qianxing.costs.example.json").is_none());
    let example_rules = ExecutionCostRules::load(&example).unwrap();
    let builtin = default_execution_cost_binding();
    assert_eq!(
        (
            example_rules.maker_bp,
            example_rules.taker_bp,
            example_rules.latency_base_ns,
            example_rules.latency_insert_ns
        ),
        (builtin.rules.maker_bp, builtin.rules.taker_bp, 0, 0),
        "示例模板写错费率会被当成默认口径照抄，所以两者必须核对"
    );

    let _ = std::fs::remove_dir_all(root);
}
