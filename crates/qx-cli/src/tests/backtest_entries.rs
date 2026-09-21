use super::*;

#[test]
fn strategy_backtest_accepts_builtin_runtime_config() {
    let (deploy, frame, template) = builtin_backtest_example_paths();
    let base = read_runtime_config(&template).unwrap();
    let (root, runtime) = isolated_backtest_runtime(&deploy, &base, "builtin-backtest");
    run_strategy_backtest(&runtime, &frame, None).unwrap();
    let summaries = list_backtest_summary_paths(&root.join("runs")).unwrap();
    assert_eq!(summaries.len(), 1, "回测应只产出一份摘要");
    let _ = std::fs::remove_dir_all(&root);
}

/// Phase 4 内核合并的钉桩：三条 Bar 回测链共用 `BarBacktestAssembly`，
/// 撮合/延迟/数据档位/乘数/初始资金与"缺省即有门禁"的口径只有一份实现。
#[test]
fn bar_backtest_assembly_pins_the_shared_engine_defaults() {
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let config = BarBacktestAssembly::new(&instrument, "main", 20260914).into_config();
    assert_eq!(config.instrument, instrument);
    assert_eq!(config.account_id, "main");
    assert_eq!(config.currency, "USDT");
    assert_eq!(config.initial_cash.raw(), Money::from_i64(100_000).raw());
    assert_eq!(config.multiplier, 1);
    assert_eq!(config.seed, 20260914);
    assert!(
        matches!(config.data_tier, DataTier::Bar),
        "Bar 内核必须是 Bar 数据档: {:?}",
        config.data_tier
    );
    assert!(config.instrument_spec.is_none());
    assert!(config.virtual_trading.ashare_rules.is_none());
    assert_eq!(
        config.risk.rule_set().version(),
        strategy_risk_gate(None, false).rule_set().version(),
        "缺省风控门必须与策略回测同源，不允许退回空 RiskGate"
    );
}

/// `backtest builtin` 入口：输入校验 fail-closed，且真实跑通共用内核。
#[test]
fn builtin_backtest_entry_validates_inputs_and_runs_on_the_shared_kernel() {
    let (_deploy, frame, _template) = builtin_backtest_example_paths();
    assert!(run_builtin_backtest("not_a_strategy", &frame, None, 1, None).is_err());
    assert!(run_builtin_backtest("sma_cross", &frame, None, 0, None).is_err());
    assert!(
        run_builtin_backtest("sma_cross", Path::new("missing-frame.json"), None, 1, None).is_err()
    );
    run_builtin_backtest("sma_cross", &frame, None, 1, None).unwrap();
    let spec_root = temp_cli_case_dir("builtin-broken-market-spec");
    let broken_spec = spec_root.join("spec.json");
    std::fs::write(&broken_spec, "{ not json").unwrap();
    let error = run_builtin_backtest("sma_cross", &frame, Some(&broken_spec), 1, None).unwrap_err();
    assert!(
        error.contains("内置策略 market spec JSON 无效"),
        "market spec 必须走共用读取口径并 fail-closed: {error}"
    );
    let _ = std::fs::remove_dir_all(&spec_root);
}

/// `backtest multi-builtin` 入口：多腿归因产物必须由腿级共用装配的真实成交计提。
#[test]
fn multi_builtin_backtest_entry_writes_spread_attribution_from_leg_fills() {
    let deploy = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy");
    let root = temp_cli_case_dir("multi-builtin-attribution");
    run_multi_builtin_backtest(
        "pairs_arbitrage",
        &deploy.join("qianxing.bar-frame.pairs-primary.example.json"),
        &deploy.join("qianxing.bar-frame.pairs-reference.example.json"),
        None,
        None,
        1,
        25,
        Some(&root),
        None,
    )
    .unwrap();
    let artifacts = std::fs::read_dir(root.join("runs"))
        .unwrap()
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            path.file_name()?
                .to_string_lossy()
                .ends_with(".spread-attribution.json")
                .then_some(path)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        artifacts.len(),
        1,
        "多腿回测应产出一份归因产物: {}",
        root.display()
    );
    let payload: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&artifacts[0]).unwrap()).unwrap();
    assert_eq!(payload["strategy_id"], "builtin-pairs_arbitrage-multi");
    assert_eq!(payload["primary_instrument"], "BTCUSDT.BINANCE");
    assert_eq!(payload["reference_instrument"], "ETHUSDT.BINANCE");
    assert_eq!(payload["funding_bps"], 25);
    let groups = payload["groups"].as_array().unwrap();
    assert_eq!(groups.len(), 3, "三组配对信号应全部归因");
    let turnover: i128 = payload["totals"]["turnover_raw"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let fees: i128 = payload["totals"]["fees_raw"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let filled: i128 = payload["totals"]["filled_qty_raw"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!(turnover > 0 && filled > 0, "腿级成交必须进入归因");
    assert_eq!(
        fees * 10_000,
        turnover * 5,
        "无 market spec 的现货腿应全部按 taker 5 bp 计提，且与单标的回测同一费用口径"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// 回测摘要必须带上风控来源版本号，且版本号与真正参与判定的门禁同源。
#[test]
fn backtest_summary_records_the_effective_risk_rule_set_version() {
    let (deploy, frame, template) = builtin_backtest_example_paths();
    let base = read_runtime_config(&template).unwrap();
    let cases = [
        None,
        Some(qx_runtime::RiskRulesConfig {
            version: "scenario-v2".into(),
            max_qty_raw: Some(2 * SCALE),
            max_notional_raw: None,
            no_short: false,
        }),
        Some(qx_runtime::RiskRulesConfig {
            version: "scenario-v3".into(),
            max_qty_raw: None,
            max_notional_raw: None,
            no_short: true,
        }),
    ];
    for rules in cases {
        let mut config = base.clone();
        config.strategy.risk_rules = rules.clone();
        let (root, runtime) = isolated_backtest_runtime(&deploy, &config, "risk-provenance");
        run_strategy_backtest(&runtime, &frame, None).unwrap();
        let summaries = list_backtest_summary_paths(&root.join("runs")).unwrap();
        let summary_path = summaries
            .first()
            .unwrap_or_else(|| panic!("回测未生成摘要: {}", root.display()));
        let summary: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(summary_path).unwrap()).unwrap();
        let recorded = summary["risk_rules"]["rule_set_version"]
            .as_str()
            .unwrap_or_else(|| panic!("回测摘要缺少风控来源: {summary}"));
        let expected = strategy_risk_gate(
            config.strategy.risk_rules.as_ref(),
            strategy_allows_short(&config, config_margin_mode(&config)),
        )
        .rule_set()
        .version()
        .to_string();
        assert_eq!(
            recorded, expected,
            "摘要记录的版本必须等于实际生效的门禁版本: {rules:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}

/// 回测与 Paper worker 必须由同一个门禁构造入口得到同一套规则。
#[test]
fn backtest_and_paper_worker_share_one_risk_gate_builder() {
    let (_deploy, _frame, template) = builtin_backtest_example_paths();
    let base = read_runtime_config(&template).unwrap();

    // 现货（未声明 product）：禁空是推导出来的，版本号要显式标注。
    let spot_gate = strategy_risk_gate(
        base.strategy.risk_rules.as_ref(),
        strategy_allows_short(&base, config_margin_mode(&base)),
    );
    assert!(!strategy_allows_short(&base, config_margin_mode(&base)));
    let version = spot_gate.rule_set().version().to_string();
    assert!(
        version.ends_with("+implicit-no-short"),
        "推导禁空必须体现在版本号里: {version}"
    );
    // 保守默认 + 禁空，两条规则。
    assert_eq!(spot_gate.rule_set().rule_count(), 2);
    // Paper 路径对同一份配置必须给出同样的拒绝。
    let rejected = build_strategy_order(&base, "strategy-spot", 9401, 10, 0, -1)
        .err()
        .unwrap_or_default();
    assert!(
        rejected.contains("RiskGate 拒绝"),
        "现货禁空必须同时作用于回测与 Paper: {rejected}"
    );

    // 衍生品 + allow_short=true：不追加隐式禁空。
    let mut derivative = base.clone();
    derivative.strategy.product = Some(TradingProduct::Perpetual);
    derivative.strategy.margin_mode = Some(MarginMode::Isolated);
    derivative.strategy.position_mode = Some(PositionMode::OneWay);
    derivative.strategy.leverage = Some(5);
    derivative.strategy.allow_short = Some(true);
    let derivative_gate = strategy_risk_gate(
        derivative.strategy.risk_rules.as_ref(),
        strategy_allows_short(&derivative, config_margin_mode(&derivative)),
    );
    assert!(!derivative_gate.rule_set().version().contains("+implicit"));
    assert_eq!(derivative_gate.rule_set().rule_count(), 1);
    assert!(build_strategy_order(&derivative, "strategy-perp", 9402, 10, 0, -1).is_ok());

    // 配置里已写 no_short：不得重复追加，也不该出现隐式后缀。
    let mut explicit = base.clone();
    explicit.strategy.risk_rules = Some(qx_runtime::RiskRulesConfig {
        version: "explicit-no-short".into(),
        max_qty_raw: None,
        max_notional_raw: None,
        no_short: true,
    });
    let explicit_gate = strategy_risk_gate(
        explicit.strategy.risk_rules.as_ref(),
        strategy_allows_short(&explicit, config_margin_mode(&explicit)),
    );
    assert_eq!(explicit_gate.rule_set().version(), "explicit-no-short");
    assert_eq!(explicit_gate.rule_set().rule_count(), 1);
}

#[test]
fn builtin_strategy_worker_path_reads_bar_snapshot() {
    let root = std::env::temp_dir().join(format!(
        "qianxing-cli-builtin-worker-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let runtime = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy")
        .join("qianxing.runtime.builtin-strategy.example.json");
    let mut config = read_runtime_config(&runtime).unwrap();
    config.storage.data_dir = root.to_string_lossy().into_owned();
    resolve_strategy_runtime_paths(&mut config.strategy, &runtime);
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let output =
        invoke_builtin_strategy(&root, &config, &instrument, "builtin-worker-test", u64::MAX)
            .unwrap();
    assert_eq!(output.request_id, "builtin-worker-test");
    assert_eq!(output.strategy_id, "strategy-builtin");
    assert_eq!(output.instrument, "BTCUSDT.BINANCE");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn dedicated_paper_spread_recovery_worker_runs_one_scan_without_orders() {
    let root = std::env::temp_dir().join(format!(
        "qianxing-cli-paper-recovery-worker-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let template = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy")
        .join("qianxing.runtime.paper-strategy.example.json");
    let mut config = read_runtime_config(&template).unwrap();
    config.storage.data_dir = root.join("data").to_string_lossy().into_owned();
    config
        .workers
        .iter_mut()
        .find(|worker| worker.id == "paper-spread-recovery")
        .expect("paper sample must include disabled spread recovery")
        .enabled = true;
    let runtime = root.join("runtime.json");
    std::fs::write(&runtime, config.to_json().unwrap()).unwrap();
    run_paper_spread_recovery_worker(&runtime, "paper-spread-recovery", true).unwrap();
    assert!(root.join("data").exists());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn dedicated_spread_recovery_disables_legacy_execution_scan_only_for_same_account_and_venue() {
    let template = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy")
        .join("qianxing.runtime.paper-strategy.example.json");
    let mut config = read_runtime_config(&template).unwrap();
    let execution = config
        .workers
        .iter()
        .find(|worker| worker.role == WorkerRole::Execution)
        .cloned()
        .unwrap();
    assert!(!dedicated_spread_recovery_configured(&config, &execution));
    config
        .workers
        .iter_mut()
        .find(|worker| worker.id == "paper-spread-recovery")
        .expect("paper sample must include disabled spread recovery")
        .enabled = true;
    assert!(dedicated_spread_recovery_configured(&config, &execution));
    config.workers.last_mut().unwrap().venue_id = Some("other".into());
    assert!(!dedicated_spread_recovery_configured(&config, &execution));
}

/// `RunManifest.code_commit` 参与清单摘要：它恒为常量时，两份不同代码跑出的回测清单
/// 长得一模一样，事后无法判定收益出自哪个提交，重放校验也就失去了比较对象。构建身份
/// 由 `build.rs` 烧进二进制，这里同时锁定形状与"确实落进了产物文件"。
#[test]
fn run_manifests_bear_the_built_code_identity() {
    let identity = env!("QX_GIT_COMMIT");
    let commit = identity.strip_suffix("-dirty").unwrap_or(identity);
    assert!(
        commit == "unknown"
            || (commit.len() == 40
                && commit
                    .chars()
                    .all(|character| character.is_ascii_hexdigit())),
        "代码身份必须是 git 提交哈希或 unknown 回落值，实际为 {identity:?}"
    );
    assert_eq!(
        scheduler_manifest("job-worker", "2026-01-05", 1_700_000_000_000).code_commit,
        identity
    );

    let (deploy, frame, template) = builtin_backtest_example_paths();
    let base = read_runtime_config(&template).unwrap();
    let (root, runtime) = isolated_backtest_runtime(&deploy, &base, "code-identity");
    run_strategy_backtest(&runtime, &frame, None).unwrap();
    let persisted = std::fs::read_dir(root.join("runs"))
        .unwrap()
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .find(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().ends_with(".run.json"))
        })
        .expect("回测 RunManifest 未落盘");
    let payload: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&persisted).unwrap()).unwrap();
    assert_eq!(payload["code_commit"].as_str(), Some(identity));
    let _ = std::fs::remove_dir_all(&root);
}
