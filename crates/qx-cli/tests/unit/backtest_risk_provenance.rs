use super::*;
use qx_service::backtest_bindings::{app_backtest_risk_rules, backtest_risk_binding_from_config};

/// V10 §4.2 / D2 的收口用例：同一份 `strategy.risk_rules` 必须被四条回测链读成同一个
/// 规则集版本；产物还要如实写明规则来源与实际撮合内核（深度链不得冒充 Bar 内核）。
#[test]
fn every_backtest_entry_reads_the_same_risk_rules_from_one_config() {
    let (deploy, frame, template) = builtin_backtest_example_paths();
    let rules = qx_runtime::RiskRulesConfig {
        version: "p0b-shared-v1".into(),
        max_qty_raw: Some(3 * SCALE),
        max_notional_raw: Some(1_000 * SCALE),
        no_short: true,
    };
    let mut config = read_runtime_config(&template).unwrap();
    config.strategy.risk_rules = Some(rules.clone());
    let (strategy_root, runtime) = isolated_backtest_runtime(&deploy, &config, "p0b-provenance");
    let expected = strategy_risk_gate(
        Some(&rules),
        strategy_allows_short(&config, config_margin_mode(&config)),
    )
    .rule_set()
    .version()
    .to_string();
    assert!(
        !expected.contains("+conservative-max-qty"),
        "配置里已有数量上限时不该再叠加保守默认: {expected}"
    );
    let app_rules = app_backtest_risk_rules(Some(&rules)).expect("configured rules are present");
    assert_eq!(app_rules.version, rules.version);
    assert_eq!(app_rules.max_qty_raw, rules.max_qty_raw);
    assert_eq!(app_rules.max_notional_raw, rules.max_notional_raw);
    assert_eq!(app_rules.no_short, rules.no_short);

    // 1) `strategy backtest`：摘要里的规则版本与来源。
    run_strategy_backtest(&runtime, &frame, None).unwrap();
    let summary = read_first_backtest_summary(&strategy_root);
    assert_eq!(summary["risk_rules"]["rule_set_version"], expected);
    assert_eq!(summary["risk_rules"]["source"], "runtime-config");
    assert_eq!(summary["matching_kernel"], BAR_MATCHING_KERNEL);

    // 2) `backtest multi-builtin`：归因产物带同一份规则来源。
    let multi_root = temp_cli_case_dir("p0b-multi");
    run_multi_builtin_backtest(
        "pairs_arbitrage",
        &deploy.join("qianxing.bar-frame.pairs-primary.example.json"),
        &deploy.join("qianxing.bar-frame.pairs-reference.example.json"),
        None,
        None,
        None,
        None,
        1,
        0,
        Some(&multi_root),
        Some(&runtime),
    )
    .unwrap();
    let attribution = read_first_artifact(&multi_root, ".spread-attribution.json");
    assert_eq!(attribution["risk_rules"]["rule_set_version"], expected);
    assert_eq!(attribution["risk_rules"]["source"], "runtime-config");
    assert_eq!(
        attribution["risk_rules"]["matching_kernel"],
        BAR_MATCHING_KERNEL
    );

    // 3) `backtest book`：深度档必须声明自己的撮合内核。
    let depth_root = temp_cli_case_dir("p0b-depth");
    run_depth_backtest_with_quality_manifest(DepthBacktestCommand {
        tier: "l1",
        strategy_name: "sma_cross",
        frame_path: &deploy.join("qianxing.depth-frame.l1.example.json"),
        spec_path: None,
        quantity: 1,
        fee_bps: Some(5),
        execution: DepthExecutionModel::default(),
        root: &depth_root,
        runtime_config_path: Some(&runtime),
        quality_manifest_path: None,
    })
    .unwrap();
    let depth_summary = read_first_backtest_summary(&depth_root);
    assert_eq!(depth_summary["risk_rules"]["rule_set_version"], expected);
    assert_eq!(depth_summary["risk_rules"]["source"], "runtime-config");
    assert_eq!(
        depth_summary["matching_kernel"],
        qx_app::TICK_MATCHING_KERNEL
    );
    assert_ne!(
        depth_summary["matching_kernel"], summary["matching_kernel"],
        "深度链与 Bar 链的内核声明必须可区分"
    );

    // 4) `backtest builtin` 没有产物文件，规则绑定必须与上面三条同版本。
    let binding = backtest_risk_binding_from_config(Some(&config), true);
    assert_eq!(binding.gate().rule_set().version(), expected);
    assert_eq!(binding.source(), "runtime-config");
    // 未给 `--config` 时才允许保守默认，且来源标注必须不同。
    let default_binding = backtest_risk_binding_from_config(None, true);
    assert_eq!(default_binding.source(), "conservative-default");
    assert!(default_binding
        .gate()
        .rule_set()
        .version()
        .contains(CONSERVATIVE_DEFAULT_RULE_SET_VERSION));
    assert!(!default_binding
        .gate()
        .rule_set()
        .version()
        .contains("p0b-shared"));
    // runtime 配置必须在入口读入；路径错误在解析阶段失败，不能静默退回保守默认。
    let error = read_runtime_config(&runtime.join("missing.json")).unwrap_err();
    assert!(error.contains("读取"), "缺文件要显式失败: {error}");

    for path in [strategy_root, multi_root, depth_root] {
        let _ = std::fs::remove_dir_all(path);
    }
}
