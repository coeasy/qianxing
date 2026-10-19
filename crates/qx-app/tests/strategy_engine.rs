use qx_app::{
    build_strategy_context, run_bar_backtest_with_outcome, run_bar_backtest_with_strategy,
    run_native_builtin_strategy_backtest, BacktestDatasetInput, BacktestRiskRules, BacktestSpec,
    BuiltinStrategySpec, CallerCapability, NativeStrategyBacktestContextBindings, RunContext,
    StrategyContextBindings,
};
use qx_core::{InstrumentId, Money, Quantity};
use qx_guanxing::Bar;
use qx_strategy::{BuiltinStrategyConfig, BuiltinStrategyKind};
use qx_xingban::BarStrategy;

struct NoOrders;

impl BarStrategy for NoOrders {}

#[test]
fn strategy_context_projection_is_validated_in_the_application_layer() {
    let bindings = |as_of| StrategyContextBindings {
        strategy_id: "strategy-context-test".into(),
        strategy_version: "v1".into(),
        account_id: "account-test".into(),
        venue_id: "paper".into(),
        data_fingerprint: "fixture:verified".into(),
        as_of,
        positions: std::collections::BTreeMap::from([("T.SIM".into(), 5)]),
        cash: std::collections::BTreeMap::from([("USD".into(), 100)]),
        available_margin_raw: Some(80),
        risk_state: "ledger-replayed-account-state+paper-cash-only-buying-power".into(),
    };

    let context = build_strategy_context(bindings(10)).expect("有效上下文应通过应用层校验");
    assert_eq!(context.positions.get("T.SIM"), Some(&5));
    assert_eq!(context.cash.get("USD"), Some(&100));
    assert_eq!(context.available_margin_raw, Some(80));

    let error = build_strategy_context(bindings(0)).expect_err("as_of 为零必须拒绝");
    assert!(error.to_string().contains("策略上下文无效"));
}

#[test]
fn caller_supplied_bar_strategy_preserves_the_report_returning_api() {
    let spec = BacktestSpec::new(
        BacktestDatasetInput {
            run_id: "adapter-test".into(),
            instrument: "T.SIM".into(),
            bars_path: "memory://adapter-test".into(),
        },
        "USD",
        Money::from_i64(1_000).raw(),
        7,
        ".",
        BuiltinStrategySpec::new(BuiltinStrategyKind::SmaCross, "app-test"),
    );
    let bars = [
        Bar::new(1, 100, 101, 99, 100, 10),
        Bar::new(2, 100, 102, 99, 101, 10),
        Bar::new(3, 101, 103, 100, 102, 10),
    ];
    let mut strategy = NoOrders;
    let context = RunContext::new(CallerCapability::Research, "bar-strategy-test");

    let report = run_bar_backtest_with_strategy(&spec, &bars, &mut strategy, &context)
        .expect("应用边界执行并复核回测");

    assert_eq!(report.equity.len(), bars.len());
    assert_eq!(report.clock_start, bars[0].ts);
    assert_eq!(report.clock_end, bars[2].ts);
}

#[test]
fn native_builtin_strategy_config_is_resolved_and_run_inside_the_application_use_case() {
    let instrument = InstrumentId::parse("T.SIM").expect("valid test instrument");
    let mut strategy_config = BuiltinStrategyConfig::new(
        BuiltinStrategyKind::SmaCross,
        "app-builtin",
        instrument.clone(),
        Quantity::from_i64(1),
    )
    .expect("valid built-in strategy config");
    strategy_config.fast_window = 2;
    strategy_config.slow_window = 3;
    let initial_cash_raw = Money::from_i64(1_000).raw();
    let mut spec = BacktestSpec::new(
        BacktestDatasetInput {
            run_id: "native-builtin-test".into(),
            instrument: instrument.to_string(),
            bars_path: "memory://native-builtin-test".into(),
        },
        "USD",
        initial_cash_raw,
        7,
        ".",
        BuiltinStrategySpec::new(BuiltinStrategyKind::SmaCross, "placeholder"),
    );
    spec.account_id = Some("account-test".into());
    let bars = (0..24)
        .map(|index| {
            let close = 100 + index as i128;
            Bar::new(
                1_704_159_000_000 + index as u64 * 60_000,
                close,
                close + 1,
                close - 1,
                close,
                10,
            )
        })
        .collect::<Vec<_>>();
    let context = RunContext::new(CallerCapability::Research, "native-builtin-test")
        .with_code_commit("test-revision");

    let report = run_native_builtin_strategy_backtest(
        &spec,
        &bars,
        strategy_config,
        NativeStrategyBacktestContextBindings {
            account_id: "account-test".into(),
            venue_id: "SIM".into(),
            data_fingerprint: "fixture:verified".into(),
            as_of: bars[0].ts,
            settlement_currency: "USD".into(),
            initial_cash_raw,
        },
        &context,
    )
    .expect("application owns built-in config projection and verified run");

    assert_eq!(report.equity.len(), bars.len());
    assert_eq!(report.clock_start, bars[0].ts);
    assert_eq!(report.clock_end, bars[bars.len() - 1].ts);
    assert_ne!(report.result_hash(), 0);
}

#[test]
fn backtest_outcome_reports_the_exact_executed_risk_policy_version() {
    let mut spec = BacktestSpec::new(
        BacktestDatasetInput {
            run_id: "risk-version-test".into(),
            instrument: "T.SIM".into(),
            bars_path: "memory://risk-version-test".into(),
        },
        "USD",
        Money::from_i64(1_000).raw(),
        7,
        ".",
        BuiltinStrategySpec::new(BuiltinStrategyKind::SmaCross, "risk-version-test"),
    );

    let bars = [
        Bar::new(1, 100, 101, 99, 100, 10),
        Bar::new(2, 100, 102, 99, 101, 10),
        Bar::new(3, 101, 103, 100, 102, 10),
    ];
    let run = |spec: &BacktestSpec, suffix: &str| {
        let mut strategy = NoOrders;
        let context = RunContext::new(CallerCapability::Research, format!("risk-policy-{suffix}"));
        run_bar_backtest_with_outcome(spec, &bars, &mut strategy, &context)
            .expect("回测应返回实际执行策略及其审计标签")
            .risk_rule_set_version
    };

    assert_eq!(
        run(&spec, "default"),
        "conservative-default-v1+implicit-no-short"
    );

    spec.allow_short = Some(true);
    assert_eq!(run(&spec, "short"), "conservative-default-v1");

    spec.allow_short = Some(false);
    spec.risk_rules = Some(BacktestRiskRules {
        version: "runtime-risk-v7".into(),
        max_qty_raw: Some(10 * qx_core::SCALE),
        max_notional_raw: None,
        no_short: false,
    });
    assert_eq!(run(&spec, "qty"), "runtime-risk-v7+implicit-no-short");

    spec.allow_short = Some(true);
    spec.risk_rules = Some(BacktestRiskRules {
        version: "explicit-no-short-v2".into(),
        max_qty_raw: None,
        max_notional_raw: None,
        no_short: true,
    });
    assert_eq!(run(&spec, "no-short"), "explicit-no-short-v2");
}
