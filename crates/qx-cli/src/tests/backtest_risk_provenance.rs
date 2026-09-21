use super::*;

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
        1,
        25,
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
    run_depth_backtest(
        "l1",
        "sma_cross",
        &deploy.join("qianxing.depth-frame.l1.example.json"),
        None,
        1,
        Some(5),
        &depth_root,
        Some(&runtime),
    )
    .unwrap();
    let depth_summary = read_first_backtest_summary(&depth_root);
    assert_eq!(depth_summary["risk_rules"]["rule_set_version"], expected);
    assert_eq!(depth_summary["risk_rules"]["source"], "runtime-config");
    assert_eq!(depth_summary["matching_kernel"], TICK_MATCHING_KERNEL);
    assert_ne!(
        depth_summary["matching_kernel"], summary["matching_kernel"],
        "深度链与 Bar 链的内核声明必须可区分"
    );

    // 4) `backtest builtin` 没有产物文件，规则绑定必须与上面三条同版本。
    let binding = backtest_risk_binding(Some(&runtime), true).unwrap();
    assert_eq!(binding.gate().rule_set().version(), expected);
    assert_eq!(binding.source(), "runtime-config");
    // 未给 `--config` 时才允许保守默认，且来源标注必须不同。
    let default_binding = backtest_risk_binding(None, true).unwrap();
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
    // `--config` 悬空引用必须报错，而不是静默退回保守默认。
    let error = backtest_risk_binding(Some(&runtime.join("missing.json")), false).unwrap_err();
    assert!(error.contains("读取"), "缺文件要显式失败: {error}");

    for path in [strategy_root, multi_root, depth_root] {
        let _ = std::fs::remove_dir_all(path);
    }
}

/// 读取回测产物目录里的第一份摘要。
fn read_first_backtest_summary(root: &Path) -> serde_json::Value {
    read_first_artifact(root, ".summary.json")
}

fn read_first_artifact(root: &Path, suffix: &str) -> serde_json::Value {
    let runs = root.join("runs");
    let mut paths = std::fs::read_dir(&runs)
        .unwrap_or_else(|error| panic!("读取 {} 失败: {error}", runs.display()))
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.file_name()
                .map(|name| name.to_string_lossy().ends_with(suffix))
                .unwrap_or(false)
        })
        .collect::<Vec<_>>();
    assert!(!paths.is_empty(), "{} 下缺少 {suffix} 产物", runs.display());
    paths.sort();
    let payload = std::fs::read_to_string(&paths[0]).unwrap();
    serde_json::from_str(&payload).unwrap()
}
