mod common;

use common::{experiment_spec, scratch, write_frame};
use qx_app::{
    prepare_quick_research_spec_v1, AppErrorCategory, BuiltinStrategySpec,
    ExperimentParameterSpace, ExperimentSplitPlan, MultiMarketBacktestMarket,
    MultiMarketBacktestSpec, PortfolioStrategySpec, RunExperimentSpec,
    RunMultiMarketExperimentSpec,
};
use qx_strategy::BuiltinStrategyKind;

fn split_plan() -> ExperimentSplitPlan {
    ExperimentSplitPlan {
        train_bars: 30,
        test_bars: 10,
        walk_forward: false,
        step_bars: 0,
        purge_bars: 0,
        embargo_bars: 0,
    }
}

fn experiment(root: &std::path::Path) -> RunExperimentSpec {
    let bars = write_frame(root, "bars.json", "BTCUSDT.BINANCE", 60);
    let mut spec = experiment_spec("quick-research", &bars, &root.join("out"), vec![2]);
    spec.split_plan = Some(split_plan());
    spec
}

#[test]
fn guided_single_market_research_defaults_to_buy_and_hold() {
    let root = scratch("quick-research-default-baseline");
    let spec = experiment(&root);
    assert!(spec.baselines.is_empty());

    let prepared = prepare_quick_research_spec_v1(&serde_json::to_string(&spec).unwrap())
        .expect("valid guided research spec");
    let prepared = RunExperimentSpec::from_json(&prepared).expect("prepared spec schema");

    assert_eq!(prepared.baselines, vec!["buy-and-hold"]);
    assert_eq!(prepared.split_plan, Some(split_plan()));
}

#[test]
fn guided_research_requires_an_out_of_sample_split_and_effectiveness_baseline() {
    let root = scratch("quick-research-required-gates");
    let mut spec = experiment(&root);
    spec.split_plan = None;
    let error = prepare_quick_research_spec_v1(&serde_json::to_string(&spec).unwrap())
        .expect_err("OOS split is required");
    assert_eq!(error.category(), AppErrorCategory::InvalidInput);
    assert!(error.to_string().contains("split_plan"));

    spec = experiment(&root);
    spec.baselines = vec!["none".into()];
    let error = prepare_quick_research_spec_v1(&serde_json::to_string(&spec).unwrap())
        .expect_err("none is not an effectiveness baseline");
    assert_eq!(error.category(), AppErrorCategory::InvalidInput);
    assert!(error.to_string().contains("effectiveness baseline"));
}

#[test]
fn guided_research_preserves_an_explicit_zero_signal_baseline() {
    let root = scratch("quick-research-explicit-baseline");
    let mut spec = experiment(&root);
    spec.baselines = vec!["zero-signal".into()];

    let prepared = prepare_quick_research_spec_v1(&serde_json::to_string(&spec).unwrap())
        .expect("valid explicit baseline");
    let prepared = RunExperimentSpec::from_json(&prepared).expect("prepared spec schema");

    assert_eq!(prepared.baselines, vec!["zero-signal"]);
}

#[test]
fn guided_multi_market_research_requires_oos_and_keeps_its_strict_schema() {
    let root = scratch("quick-research-multi-market");
    let bars = write_frame(&root, "market.json", "BTCUSDT.BINANCE", 60);
    let strategy = BuiltinStrategySpec::new(BuiltinStrategyKind::SmaCross, "slot-1");
    let mut spec = RunMultiMarketExperimentSpec {
        schema_version: 1,
        experiment_id: "quick-multi-market".into(),
        target_strategy_id: "slot-1".into(),
        base: MultiMarketBacktestSpec {
            schema_version: 1,
            run_id: "unused-quick-multi-market".into(),
            account_id: "quick-account".into(),
            settlement_currency: "USDT".into(),
            initial_cash_raw: 100_000 * 1_000_000_000,
            initial_cash_exact: None,
            seed: 7,
            output_dir: root.join("out").to_string_lossy().into_owned(),
            markets: vec![MultiMarketBacktestMarket {
                instrument: "BTCUSDT.BINANCE".into(),
                bars_path: bars,
                dataset_manifest_path: None,
                priority: 0,
                market_spec: None,
                asset_class: None,
                fee_schedule: None,
                order_policy: None,
                margin_model: None,
                funding_bps: 0,
                funding_events: Vec::new(),
                strategies: vec![PortfolioStrategySpec {
                    priority: 0,
                    reserved_capital_raw: 100_000 * 1_000_000_000,
                    reserved_capital_exact: None,
                    strategy,
                }],
            }],
        },
        parameter_space: vec![ExperimentParameterSpace {
            name: "fast_window".into(),
            values: vec![2],
        }],
        strategy_kinds: Vec::new(),
        callback_parameter_space: Vec::new(),
        split_plan: Some(split_plan()),
        search: None,
    };
    let original = serde_json::to_string(&spec).unwrap();
    let prepared = prepare_quick_research_spec_v1(&original).expect("valid multi-market OOS spec");
    let prepared_spec = RunMultiMarketExperimentSpec::from_json(&prepared).unwrap();
    assert_eq!(prepared_spec.split_plan, Some(split_plan()));
    assert!(!prepared.contains("baselines"));

    let mut with_unsupported_baseline: serde_json::Value = serde_json::from_str(&original).unwrap();
    with_unsupported_baseline["baselines"] = serde_json::json!(["buy-and-hold"]);
    let error = prepare_quick_research_spec_v1(&with_unsupported_baseline.to_string())
        .expect_err("multi-market schema must reject undeclared baseline field");
    assert_eq!(error.category(), AppErrorCategory::InvalidInput);

    spec.split_plan = None;
    let error = prepare_quick_research_spec_v1(&serde_json::to_string(&spec).unwrap())
        .expect_err("multi-market guided research still requires OOS");
    assert_eq!(error.category(), AppErrorCategory::InvalidInput);
}
