mod common;

use qx_app::{
    run_backtest, BacktestFillModel, BacktestMarginConfig, BacktestMarginTier, BuiltinStrategySpec,
    CallerCapability, RunContext,
};

use common::{backtest_spec, scratch, write_frame};
use qx_core::{InstrumentId, TradingInstrumentSpec, TradingProduct, SCALE};
use qx_strategy::BuiltinStrategyKind;
use qx_xingban::{AshareRuleConfig, ExecutionCostRules};

#[test]
fn app_backtest_binds_execution_costs_and_ashare_rules() {
    let root = scratch("execution-binding");
    let bars = write_frame(&root, "bars.json", "600000.SSE", 60);
    let mut spec = backtest_spec("execution-binding", &bars, &root.join("out"));
    spec.instrument = "600000.SSE".into();
    spec.settlement_currency = "CNY".into();
    spec.strategy = BuiltinStrategySpec::new(BuiltinStrategyKind::SmaCross, "app-sma");
    spec.strategy.fast_window = 2;
    spec.strategy.slow_window = 3;
    spec.execution_cost_rules = Some(ExecutionCostRules {
        maker_bp: 3,
        taker_bp: 12,
        latency_base_ns: 1_000_000,
        latency_insert_ns: 0,
        ..ExecutionCostRules::default()
    });
    let mut ashare = AshareRuleConfig::default();
    ashare.enabled = true;
    ashare.t_plus_one = false;
    spec.ashare_rules = Some(ashare);

    let context = RunContext::new(CallerCapability::Research, "execution-binding");
    let outcome = run_backtest(&spec, &context).expect("配置的费率、延迟与 A 股规则应装配进引擎");
    let summary: serde_json::Value =
        serde_json::from_slice(&std::fs::read(outcome.artifacts.summary).expect("读取回测摘要"))
            .expect("摘要 JSON");
    assert!(
        summary["fee_model"]
            .as_str()
            .unwrap()
            .starts_with("AShareFee@v1["),
        "A 股费率应优先于 maker/taker 成本档"
    );
    assert_eq!(summary["fills"].as_u64(), Some(outcome.fills));
    let manifest: serde_json::Value = serde_json::from_slice(
        &std::fs::read(outcome.artifacts.run_manifest).expect("读取运行清单"),
    )
    .expect("运行清单 JSON");

    let mut zero_latency = spec.clone();
    zero_latency.run_id = "execution-binding-zero-latency".into();
    zero_latency.output_dir = root.join("zero-latency").to_string_lossy().into_owned();
    zero_latency
        .execution_cost_rules
        .as_mut()
        .expect("成本规则")
        .latency_base_ns = 0;
    let baseline = run_backtest(
        &zero_latency,
        &RunContext::new(CallerCapability::Research, "execution-binding-zero-latency"),
    )
    .expect("零延迟对照回测");
    let baseline_manifest: serde_json::Value = serde_json::from_slice(
        &std::fs::read(baseline.artifacts.run_manifest).expect("读取对照运行清单"),
    )
    .expect("对照运行清单 JSON");
    assert_ne!(
        manifest["model_fingerprint"], baseline_manifest["model_fingerprint"],
        "延迟 descriptor 必须进入运行模型指纹"
    );

    let mut best_price = zero_latency;
    best_price.run_id = "execution-binding-best-price".into();
    best_price.output_dir = root.join("best-price").to_string_lossy().into_owned();
    best_price.fill_model = Some(BacktestFillModel::BestPrice);
    let alternate = run_backtest(
        &best_price,
        &RunContext::new(CallerCapability::Research, "execution-binding-best-price"),
    )
    .expect("显式 Bar 撮合模型");
    let alternate_summary: serde_json::Value = serde_json::from_slice(
        &std::fs::read(alternate.artifacts.summary).expect("读取 best-price 摘要"),
    )
    .expect("best-price 摘要 JSON");
    assert_eq!(alternate_summary["fill_model"], "BestPrice");
}

#[test]
fn app_backtest_binds_explicit_exchange_tiered_margin() {
    let root = scratch("tiered-margin-binding");
    let bars = write_frame(&root, "bars.json", "BTCUSDT.BINANCE", 60);
    let mut spec = backtest_spec("tiered-margin-binding", &bars, &root.join("out"));
    spec.initial_cash_raw = 150 * SCALE;
    spec.strategy.quantity_raw = 2 * SCALE;
    spec.execution_cost_rules = Some(ExecutionCostRules::default());
    spec.market_spec = Some(TradingInstrumentSpec {
        instrument: InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
        product: TradingProduct::Perpetual,
        base_currency: "BTC".into(),
        quote_currency: "USDT".into(),
        settlement_currency: "USDT".into(),
        contract_size: SCALE,
        linear: true,
        inverse: false,
        price_tick: SCALE / 100,
        qty_step: SCALE / 100,
        min_qty: SCALE / 100,
        max_leverage: 10,
        maintenance_margin_bps: 500,
        valid_from: 1,
        valid_to: None,
    });
    spec.margin_model = Some(BacktestMarginConfig::Tiers {
        tiers: vec![BacktestMarginTier {
            max_notional: 10_000 * SCALE,
            initial_bp: 5_000,
            maintenance_bp: 500,
            max_leverage: Some(10),
        }],
    });

    let outcome = run_backtest(
        &spec,
        &RunContext::new(CallerCapability::Research, "tiered-margin-binding"),
    )
    .expect("exchange tier schedule should reach the shared engine");
    assert!(
        outcome.fills > 0,
        "50% initial margin should admit the 2-unit order with 150 units of cash"
    );
    let mut default_margin = spec;
    default_margin.run_id = "tiered-margin-binding-default".into();
    default_margin.output_dir = root.join("default-margin").to_string_lossy().into_owned();
    default_margin.margin_model = None;
    let default_outcome = run_backtest(
        &default_margin,
        &RunContext::new(CallerCapability::Research, "tiered-margin-binding-default"),
    )
    .expect("default one-times margin model");
    assert_eq!(
        default_outcome.fills, 0,
        "the default 1x margin should reject the same order, proving tier config changed the engine model"
    );
}
