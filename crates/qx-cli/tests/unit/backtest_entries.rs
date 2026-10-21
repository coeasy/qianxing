use super::*;

#[test]
fn cross_language_strategy_backtest_publishes_the_v7_artifact_contract() {
    let (deploy, frame, template) = builtin_backtest_example_paths();
    let mut base = read_runtime_config(&template).unwrap();
    let python_strategy = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("python/tests/fixtures/strategy_target.py");
    base.strategy.builtin_strategy = None;
    base.strategy.python_module = Some(python_strategy.to_string_lossy().into_owned());
    let (root, runtime) = isolated_backtest_runtime(&deploy, &base, "cross-language-backtest");

    run_strategy_backtest(&runtime, &frame, None).expect("跨语言策略经 qx-app 执行");

    let summaries = list_backtest_summary_paths(&root.join("runs")).unwrap();
    assert_eq!(summaries.len(), 1, "应只发布一份 v7 摘要");
    assert!(
        summaries[0]
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(".v7.summary.json")),
        "summary filenames must carry the schema generation"
    );
    let summary: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&summaries[0]).unwrap()).unwrap();
    assert_eq!(summary["schema_version"], 7);
    assert_eq!(summary["strategy_id"], "strategy-builtin");
    assert_eq!(summary["sample_unit"], "bar");
    assert_eq!(summary["bars"].as_u64(), Some(70));
    assert_eq!(summary["result_hash"], summary["replay"]["log_digest"]);
    assert!(Path::new(summary["run_record"].as_str().unwrap()).is_file());
    let manifests = std::fs::read_dir(root.join("runs"))
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
        .filter(|entry| entry.file_name().to_string_lossy().ends_with(".run.json"))
        .count();
    assert_eq!(manifests, 1, "RunManifest 仍与 v7 摘要一同发布");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn summary_listing_hides_older_generations_of_the_same_run() {
    let root = temp_cli_case_dir("summary-generation-listing");
    let runs = root.join("runs");
    std::fs::create_dir_all(&runs).unwrap();
    for name in [
        "same-run.summary.json",
        "same-run.v6.summary.json",
        "same-run.v7.summary.json",
        "distinct-run.v6.summary.json",
    ] {
        std::fs::write(runs.join(name), "{}").unwrap();
    }

    let listed = list_backtest_summary_paths(&runs).unwrap();
    let names = listed
        .iter()
        .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        names,
        std::collections::BTreeSet::from([
            "distinct-run.v6.summary.json".to_owned(),
            "same-run.v7.summary.json".to_owned(),
        ])
    );
    let _ = std::fs::remove_dir_all(root);
}

/// `backtest builtin` 入口：输入校验 fail-closed，且真实跑通共用内核。
#[test]
fn builtin_backtest_entry_validates_inputs_and_runs_on_the_shared_kernel() {
    let (_deploy, frame, _template) = builtin_backtest_example_paths();
    assert!(run_builtin_backtest("not_a_strategy", &frame, None, 1, None, None).is_err());
    assert!(run_builtin_backtest("sma_cross", &frame, None, 0, None, None).is_err());
    assert!(run_builtin_backtest(
        "sma_cross",
        Path::new("missing-frame.json"),
        None,
        1,
        None,
        None
    )
    .is_err());
    run_builtin_backtest("sma_cross", &frame, None, 1, None, None).unwrap();
    let spec_root = temp_cli_case_dir("builtin-broken-market-spec");
    let broken_spec = spec_root.join("spec.json");
    std::fs::write(&broken_spec, "{ not json").unwrap();
    let error =
        run_builtin_backtest("sma_cross", &frame, Some(&broken_spec), 1, None, None).unwrap_err();
    assert!(
        error.contains("策略回测 market spec JSON 无效"),
        "market spec 必须走共用读取口径并 fail-closed: {error}"
    );
    let _ = std::fs::remove_dir_all(&spec_root);
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

/// V11 Q0b：内置策略 kind 的"回测准入分区"必须三处同口径 —— 内核侧的配置合法性、
/// 两个入口函数的真实判定、`builtin-strategies` 与帮助正文印出来的话。
/// 此前帮助只笼统宣称"17 个内置策略可直接用于回测/Paper/策略接入"，而 4 个套利 kind
/// 实际只被 `multi-builtin` 接受（`book` 明确拒绝它们）。
#[test]
fn builtin_strategy_entries_match_the_printed_partition() {
    let case_root = temp_cli_case_dir("partition");
    let missing = case_root.join("missing-frame.json");
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let mut single_leg = 0;
    let mut multi_leg = 0;
    for kind in BuiltinStrategyKind::ALL {
        let wants_two_legs = MULTI_LEG_KINDS.contains(&kind);
        if wants_two_legs {
            multi_leg += 1;
        } else {
            single_leg += 1;
        }
        // 内核侧是同一条事实的另一面：多腿 kind 缺 reference_instrument 即非法。
        let config = BuiltinStrategyConfig::new(
            kind,
            format!("builtin-{}", kind.name()),
            instrument.clone(),
            Quantity::from_i64(1),
        );
        assert_eq!(
            config.is_err(),
            wants_two_legs,
            "{:?} 的配置合法性必须与准入分区一致",
            kind
        );
        // 深度档入口在读取数据帧之前就按 kind 拒绝。
        let depth = run_depth_backtest_with_quality_manifest(DepthBacktestCommand {
            tier: "l1",
            strategy_name: kind.name(),
            frame_path: &missing,
            spec_path: None,
            quantity: 1,
            fee_bps: Some(5),
            execution: DepthExecutionModel::default(),
            root: &case_root,
            runtime_config_path: None,
            quality_manifest_path: None,
        })
        .unwrap_err();
        assert_eq!(
            depth.contains("只支持单标的策略"),
            wants_two_legs,
            "深度档入口对 {} 的判定偏离分区: {depth}",
            kind.name()
        );
        // 多腿入口只接受这四类。
        let legs = run_multi_builtin_backtest(
            kind.name(),
            &missing,
            &missing,
            None,
            None,
            None,
            None,
            1,
            0,
            None,
            None,
        )
        .unwrap_err();
        assert_eq!(
            legs.contains("只接受套利策略"),
            !wants_two_legs,
            "多腿入口对 {} 的判定偏离分区: {legs}",
            kind.name()
        );
    }
    assert_eq!(
        (single_leg, multi_leg),
        (13, 4),
        "17 个 kind 的分区必须由用例逐条数出来"
    );
    // 帮助正文的分区口径必须由同一轮数出的数字复现，不得停留在"17 个都能用于回测/Paper"。
    let help_source = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src")
            .join("cli_help.rs"),
    )
    .expect("读取 cli_help.rs 失败")
    // 断言的是文本内容；Windows 工作树在 core.autocrlf=true 下会把整棵树换成 CRLF，
    // 那属于检出形态，不该让一条跨行的文本核对变红。
    .replace('\r', "");
    assert!(
        help_source.contains(&format!(
            "{single_leg} 个单标的策略可走 builtin / ccxt-builtin /\n      book，{multi_leg} 个套利 kind"
        )),
        "帮助里 builtin-strategies 的说明必须写明 {single_leg}/{multi_leg} 的真实准入分区"
    );
    let output = Command::new(qx_cli_binary())
        .arg("builtin-strategies")
        .output()
        .expect("启动 qx-cli 失败");
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    for kind in BuiltinStrategyKind::ALL {
        let line = stdout
            .lines()
            .find(|line| line.starts_with(&format!("{}\t", kind.name())))
            .unwrap_or_else(|| panic!("builtin-strategies 未印出 {}", kind.name()));
        assert!(
            line.contains(backtest_entry_of(kind)),
            "{} 的准入列必须是该 kind 真正可用的入口，实际行: {line}",
            kind.name()
        );
        // 生效列与内核清单同源（V12 #102）：这一格没了或写了别的，读者就会在只读 period 的
        // kind 上调快慢窗口。准入列有断言、生效列没有，等于清单只剩一半牙齿。
        let last_column = line.trim_end().rsplit('\t').next().unwrap_or_default();
        assert_eq!(
            format!("knobs={}", kind.signal_knob_list()),
            last_column,
            "{} 的生效列必须逐项等于内核清单，实际行: {line}",
            kind.name()
        );
    }
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

#[test]
fn ccxt_builtin_rejects_missing_runtime_before_starting_external_worker() {
    let root = temp_cli_case_dir("ccxt-runtime-preflight");
    let runtime = root.join("missing-runtime.json");
    let ccxt = root.join("missing-ccxt.json");
    let (_, _, valid_runtime) = builtin_backtest_example_paths();
    let month_error = run_ccxt_builtin_backtest(CcxtBuiltinBacktestInput {
        ccxt_config_path: &ccxt,
        strategy_name: "sma_cross",
        instrument: "BTCUSDT.BINANCE",
        timeframe: "1M",
        start_ms: 0,
        end_ms: 1,
        spec_path: None,
        quantity: 1,
        runtime_config_path: Some(&valid_runtime),
    })
    .expect_err("不支持的月线周期必须在启动外部 worker 前拒绝");
    assert!(
        month_error.contains("月线"),
        "应返回周期拒绝: {month_error}"
    );
    assert!(
        !month_error.contains("启动公共 CCXT Worker"),
        "不支持的周期不得先触发 worker 启动: {month_error}"
    );

    let error = run_ccxt_builtin_backtest(CcxtBuiltinBacktestInput {
        ccxt_config_path: &ccxt,
        strategy_name: "sma_cross",
        instrument: "BTCUSDT.BINANCE",
        timeframe: "1d",
        start_ms: 0,
        end_ms: 1,
        spec_path: None,
        quantity: 1,
        runtime_config_path: Some(&runtime),
    })
    .expect_err("Runtime 必须在外部 worker 启动前被完整校验");
    assert!(error.contains("读取"), "应返回 runtime 读取错误: {error}");
    assert!(
        !error.contains("启动公共 CCXT Worker"),
        "无效 runtime 不得先触发 worker 启动: {error}"
    );
    let _ = std::fs::remove_dir_all(root);
}
