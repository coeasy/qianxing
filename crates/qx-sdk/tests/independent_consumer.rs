//! 独立消费者（方案 §6.1「examples 和独立消费者构建」/ §7 阶段 4 退出门）。
//!
//! 这个文件是门面的**实物证据**：它的 import 只有 `qx_sdk::` 一个前缀——不 import `qx-app`、
//! 不 import 任何领域 crate、不起进程。如果哪天门面漏掉一块（比如忘了重导出 `BuiltinStrategyKind`），
//! 这里会**编译不过**，而不是等到集成方才发现"这个方法不存在"。
//!
//! 它做的事与任何第三方嵌入方完全相同：建会话 → 校验数据 → 同步回测 → 复核产物 →
//! 再用异步句柄跑一次、等它、取结果。
//!
//! 数据集用仓库里那份公开样例（`deploy/qianxing.bar-frame.example.json`，70 根 Bar），
//! 而不是在测试里现造：独立消费者的第一件事本来就是"拿到一份别人的数据"。

use qx_sdk::{
    AlgoExecutionMethod, AlgoExecutionSpec, AlgoExecutionState, AlgoExecutionStatus,
    AlgoParentOrder, BacktestSpec, BuiltinStrategyKind, BuiltinStrategySpec, CallerCapability,
    Client, DatasetSpec, DepthBacktestSpec, EventAppender, ExecutionEvent, ExecutionEventEnvelope,
    ExecutionGateway, ExperimentParameterSpace, ExperimentSplitPlan, InstrumentId,
    MakerTakerFeeSchedule, MultiLegBacktestSpec, MultiMarketBacktestMarket,
    MultiMarketBacktestSpec, Order, OrderStatus, OrderStore, PortfolioBacktestConfig,
    PortfolioStrategySpec, Price, Quantity, RiskParityAssetSpec, RiskParitySpec, RiskPort,
    RiskVerdict, RunDepthExperimentSpec, RunEventKind, RunExperimentSpec,
    RunMultiLegExperimentSpec, RunMultiMarketExperimentSpec, RunStatus, Side, VenuePort,
    VwapVolumeBucket, ALGO_EXECUTION_SPEC_SCHEMA_VERSION, BACKTEST_SPEC_SCHEMA_VERSION,
};
use std::path::{Path, PathBuf};

#[path = "consumer/custom_strategy.rs"]
mod custom_strategy_consumer;
#[path = "consumer/multi_leg_sizing.rs"]
mod multi_leg_sizing_consumer;
#[path = "consumer/strategy_catalog.rs"]
mod strategy_catalog_consumer;
#[path = "consumer/strategy_selection.rs"]
mod strategy_selection_consumer;

/// 仓库根：`crates/qx-sdk/tests/<file>` 往上三层。
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/qx-sdk 一定有仓库根")
        .to_path_buf()
}

/// 仓库自带的样例 BarFrame（70 根 BTCUSDT.BINANCE Bar）。
fn sample_bars() -> PathBuf {
    repo_root()
        .join("deploy")
        .join("qianxing.bar-frame.example.json")
}

/// 本用例独享的产物目录（并发跑不互相踩）。
fn scratch(label: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("qx-sdk-consumer/{label}-{}", std::process::id()));
    std::fs::create_dir_all(&root).expect("建临时目录");
    root
}

fn spec(run_id: &str, output_dir: &Path) -> BacktestSpec {
    let mut strategy = BuiltinStrategySpec::new(BuiltinStrategyKind::SmaCross, "sdk-sma");
    strategy.fast_window = 2;
    strategy.slow_window = 3;
    BacktestSpec::new(
        run_id,
        "BTCUSDT.BINANCE",
        sample_bars().to_string_lossy().into_owned(),
        "USDT",
        // 10 万 USDT 本金（定点 1e9）。写死标度而不是引内部常量：这一份是**独立消费者的输入**，
        // 它只承诺与三个门面同形的 JSON，不承诺与某个内部常量同源。
        100_000 * 1_000_000_000,
        20261010,
        output_dir.to_string_lossy().into_owned(),
        strategy,
    )
}

#[test]
fn an_independent_consumer_runs_and_verifies_a_backtest_through_the_facade_only() {
    let output_dir = scratch("sync");
    let mut backtest = spec("sdk-run", &output_dir);
    backtest.account_id = Some("acct.rust-sdk".into());
    assert_eq!(backtest.schema_version, BACKTEST_SPEC_SCHEMA_VERSION);

    let client = Client::new(CallerCapability::Research, "sdk-run");

    // ① 数据可用性是**裁定**：usable=false 是结论，不是错误。
    let dataset = DatasetSpec::new("sdk-dataset", sample_bars().to_string_lossy().into_owned());
    let verdict = client.validate_dataset(&dataset).expect("数据集可读");
    assert!(verdict.usable, "样例数据集必须可用: {:?}", verdict.gaps);
    assert_eq!(verdict.rows, 70);

    // ② 同步回测，四份产物落盘。
    let outcome = client.run_backtest(&backtest).expect("回测跑通");
    assert_eq!(outcome.account_id.as_deref(), Some("acct.rust-sdk"));
    assert_eq!(outcome.equity_points, 70);
    assert!(outcome.fills > 0, "样例数据必须真跑出成交");
    for path in [
        &outcome.artifacts.run_manifest,
        &outcome.artifacts.summary,
        &outcome.artifacts.equity,
        &outcome.artifacts.fills,
    ] {
        assert!(Path::new(path).is_file(), "产物缺失: {path}");
    }

    // ③ 独立复核：篡改过的读数必须判失败而不是报错。
    let verification = client.verify_run(&outcome).expect("复核跑得起来");
    assert!(
        verification.verified,
        "独立消费者必须能自己复核产物: {:?}",
        verification.mismatches
    );
    assert_eq!(verification.result_hash, outcome.result_hash);
}

#[test]
fn an_independent_consumer_drives_the_async_handle_and_sees_the_same_hash() {
    let output_dir = scratch("async");
    let backtest = spec("sdk-async", &output_dir);
    let client = Client::new(CallerCapability::Research, "sdk-async");

    // 句柄式异步：立刻拿到 handle，由调用方决定等多久（缺省 30 s，有限）。
    let mut handle = client.start_backtest(backtest.clone());
    assert_eq!(handle.run_id(), "sdk-async");
    assert_eq!(handle.wait(), RunStatus::Succeeded);
    let outcome = handle
        .try_take_result()
        .expect("终态必须能取到结果")
        .expect("成功的运行必须是 Ok");
    assert_eq!(
        handle
            .events_after(0)
            .iter()
            .map(|event| event.kind)
            .collect::<Vec<_>>(),
        vec![RunEventKind::Started, RunEventKind::Succeeded]
    );
    // 一次性读取：再取一次就是 None（结果不会被执行两次）。
    assert!(handle.try_take_result().is_none());

    // 同一身份重跑逐位同哈希——"可复现"这条承诺在独立消费者手上同样成立。
    let again = client.run_backtest(&backtest).expect("重跑");
    assert_eq!(again.result_hash, outcome.result_hash);
}

#[test]
fn an_independent_consumer_uses_shared_fixed_notional_sizing() {
    let output_dir = scratch("notional-sizing");
    let mut backtest = spec("sdk-notional", &output_dir);
    backtest.strategy.target_notional_raw = Some(1_000 * 1_000_000_000);
    let client = Client::new(CallerCapability::Research, "sdk-notional");
    let outcome = client
        .run_backtest(&backtest)
        .expect("notional sizing backtest");
    assert!(outcome.fills > 0);
    let fills = std::fs::read_to_string(&outcome.artifacts.fills).expect("fill facts");
    let quantities = fills
        .lines()
        .skip(1)
        .filter_map(|row| row.split(',').nth(2)?.parse::<i128>().ok())
        .collect::<Vec<_>>();
    assert!(!quantities.is_empty());
    assert!(quantities
        .iter()
        .all(|quantity| *quantity > 0 && *quantity != 1_000_000_000));
    assert!(client.verify_run(&outcome).unwrap().verified);
}

#[test]
fn an_independent_consumer_configures_fees_through_the_stable_facade() {
    let output_dir = scratch("fee-schedule");
    let mut backtest = spec("sdk-fee-schedule", &output_dir);
    backtest.fee_schedule = Some(MakerTakerFeeSchedule {
        maker_fee_bps: 13,
        taker_fee_bps: 41,
    });
    let client = Client::new(CallerCapability::Research, "sdk-fee-schedule");
    let outcome = client
        .run_backtest(&backtest)
        .expect("explicit fee schedule");
    let summary: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&outcome.artifacts.summary).unwrap()).unwrap();
    assert_eq!(
        summary["fee_model"],
        "MakerTaker@v1[params=maker_bp=13;taker_bp=41]"
    );
    assert!(summary["fees_raw"].as_i64().unwrap() > 0);
    assert!(client.verify_run(&outcome).unwrap().verified);
}

#[test]
fn an_independent_consumer_builds_a_shared_risk_parity_plan() {
    let client = Client::new(CallerCapability::Research, "sdk-risk-parity");
    let spec = RiskParitySpec {
        schema_version: 1,
        portfolio_id: "sdk-risk-parity".into(),
        available_cash_raw: 1_000_000_000_000,
        target_gross_notional_raw: 600_000_000_000,
        current_positions: Default::default(),
        assets: vec![
            RiskParityAssetSpec {
                instrument: "BTCUSDT.BINANCE".into(),
                mark_price_raw: 20_000_000_000,
                returns_bps: vec![-200, 100, -100, 200, -100, 100],
            },
            RiskParityAssetSpec {
                instrument: "ETHUSDT.BINANCE".into(),
                mark_price_raw: 1_000_000_000,
                returns_bps: vec![-100, 50, -50, 100, -50, 50],
            },
        ],
        max_turnover_bps: 10_000,
        min_trade_notional_raw: 0,
    };
    let result = client
        .plan_risk_parity(&spec)
        .expect("shared risk parity plan");
    assert_eq!(result.plan.allocations.len(), 2);
    assert_eq!(
        result
            .plan
            .allocations
            .iter()
            .map(|row| row.weight_bps)
            .sum::<u32>(),
        10_000
    );
    assert_eq!(result.plan.observations, 6);
    assert_eq!(result.plan_hash.len(), 16);
}

#[test]
fn an_independent_consumer_plans_twap_and_vwap_through_the_stable_facade() {
    let client = Client::new(CallerCapability::Research, "algo-plan");
    let twap = client
        .plan_algo_execution(&AlgoExecutionSpec {
            schema_version: ALGO_EXECUTION_SPEC_SCHEMA_VERSION,
            parent_id: "parent-1".into(),
            total_quantity_raw: 11,
            start_ts: 10,
            end_ts: 40,
            method: AlgoExecutionMethod::Twap,
            child_count: Some(4),
            volume_profile: vec![],
        })
        .unwrap();
    assert_eq!(
        twap.children
            .iter()
            .map(|child| child.quantity_raw)
            .sum::<i128>(),
        11
    );
    assert!(twap
        .children
        .iter()
        .all(|child| child.parent_id == "parent-1"));

    let vwap = client
        .plan_algo_execution(&AlgoExecutionSpec {
            schema_version: ALGO_EXECUTION_SPEC_SCHEMA_VERSION,
            parent_id: "parent-2".into(),
            total_quantity_raw: 10,
            start_ts: 100,
            end_ts: 300,
            method: AlgoExecutionMethod::Vwap,
            child_count: None,
            volume_profile: vec![
                VwapVolumeBucket {
                    ts: 100,
                    market_volume_raw: 1,
                },
                VwapVolumeBucket {
                    ts: 200,
                    market_volume_raw: 2,
                },
                VwapVolumeBucket {
                    ts: 300,
                    market_volume_raw: 3,
                },
            ],
        })
        .unwrap();
    assert_eq!(
        vwap.children
            .iter()
            .map(|child| child.quantity_raw)
            .sum::<i128>(),
        10
    );
    assert_eq!(
        vwap.children
            .iter()
            .map(|child| child.quantity_raw)
            .collect::<Vec<_>>(),
        [1, 3, 6]
    );
}

#[test]
fn an_independent_consumer_prepares_restartable_algo_state_through_the_facade() {
    let client = Client::new(CallerCapability::Research, "algo-state");
    let spec = AlgoExecutionSpec {
        schema_version: ALGO_EXECUTION_SPEC_SCHEMA_VERSION,
        parent_id: "parent-state".into(),
        total_quantity_raw: 11,
        start_ts: 10,
        end_ts: 40,
        method: AlgoExecutionMethod::Twap,
        child_count: Some(4),
        volume_profile: vec![],
    };
    let plan = client.plan_algo_execution(&spec).unwrap();
    let mut state = AlgoExecutionState::new(AlgoParentOrder {
        parent_id: spec.parent_id.clone(),
        plan: plan.children,
        child_order_id_base: 100,
        parent_order: Order {
            client_id: 90,
            instrument: InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
            side: Side::Buy,
            qty: Quantity::from_raw(11),
            limit: Some(Price::from_i64(100)),
            status: OrderStatus::PendingSubmit,
            filled: Quantity::ZERO,
            account_id: "main".into(),
            trace: None,
            policy: None,
        },
    })
    .expect("prepare Rust SDK algo state");

    assert_eq!(state.status, AlgoExecutionStatus::Ready);
    assert_eq!(state.plan.len(), 4);
    let checkpoint = state.to_json().unwrap();
    assert_eq!(
        qx_sdk::AlgoExecutionState::from_json(&checkpoint).unwrap(),
        state
    );

    let mut venue = ConsumerVenue;
    let mut events = ConsumerEvents::default();
    let mut source_seq = 0;
    let risk = ConsumerRisk;
    let mut gateway = ExecutionGateway::new(
        &mut venue,
        &mut events,
        "independent-consumer",
        10,
        &mut source_seq,
    );
    let step = state.step(10, &mut gateway, &risk).unwrap();
    assert!(matches!(step, qx_sdk::AlgoExecutionStep::Submitted { .. }));
    assert_eq!(state.next_slice_position, 1);
}

struct ConsumerVenue;

impl VenuePort for ConsumerVenue {
    fn venue_id(&self) -> &str {
        "consumer-paper"
    }

    fn submit_order(&mut self, order: Order, _ts: u64) -> Result<Vec<ExecutionEvent>, String> {
        Ok(vec![ExecutionEvent::Accepted {
            client_order_id: order.client_id,
            venue_order_id: format!("consumer-{}", order.client_id),
        }])
    }

    fn cancel_order(
        &mut self,
        client_order_id: u64,
        _ts: u64,
    ) -> Result<Vec<ExecutionEvent>, String> {
        Ok(vec![ExecutionEvent::Cancelled { client_order_id }])
    }
}

#[derive(Default)]
struct ConsumerEvents(Vec<Order>);

impl OrderStore for ConsumerEvents {
    fn orders(&self) -> Vec<Order> {
        self.0.clone()
    }

    fn register_order(
        &mut self,
        order: Order,
        _ts: u64,
        _correlation_id: Option<String>,
    ) -> Result<(), String> {
        self.0.push(order);
        Ok(())
    }
}

impl EventAppender for ConsumerEvents {
    fn append_execution_event(&mut self, envelope: ExecutionEventEnvelope) -> Result<(), String> {
        if let ExecutionEvent::Accepted {
            client_order_id, ..
        } = envelope.event
        {
            let order = self
                .0
                .iter_mut()
                .find(|order| order.client_id == client_order_id)
                .ok_or_else(|| "accepted event without order".to_string())?;
            order.status = OrderStatus::Accepted;
        }
        Ok(())
    }
}

struct ConsumerRisk;

impl RiskPort for ConsumerRisk {
    fn evaluate_order(&self, _order: &Order) -> Result<RiskVerdict, String> {
        Ok(RiskVerdict::Allow)
    }
}

#[test]
fn an_independent_consumer_uses_live_available_margin_sizing() {
    let output_dir = scratch("available-margin-sizing");
    let mut backtest = spec("sdk-margin-budget", &output_dir);
    backtest.strategy.target_available_margin_bps = Some(1_000);
    let client = Client::new(CallerCapability::Research, "sdk-margin-budget");
    let outcome = client
        .run_backtest(&backtest)
        .expect("available-margin sizing backtest");
    assert!(outcome.fills > 0);
    let fills = std::fs::read_to_string(&outcome.artifacts.fills).expect("fill facts");
    let quantities = fills
        .lines()
        .skip(1)
        .filter_map(|row| row.split(',').nth(2)?.parse::<i128>().ok())
        .collect::<Vec<_>>();
    assert!(!quantities.is_empty());
    assert!(quantities
        .iter()
        .all(|quantity| *quantity > 0 && *quantity != 1_000_000_000));
    assert!(client.verify_run(&outcome).unwrap().verified);
}

#[test]
fn an_independent_consumer_sees_the_same_error_document_the_facades_do() {
    let output_dir = scratch("error");
    let mut broken = spec("sdk-broken", &output_dir);
    broken.bars_path = output_dir
        .join("missing-bars.json")
        .to_string_lossy()
        .into_owned();
    let client = Client::new(CallerCapability::Research, "sdk-broken");
    let error = client.run_backtest(&broken).expect_err("缺数据必须失败");

    assert_eq!(error.category().as_str(), "DATA_UNAVAILABLE");
    assert_eq!(error.source_code(), Some("io:NotFound"));
    assert_eq!(error.correlation_id(), Some("sdk-broken"));
    assert!(!error.safe_to_retry());
    // 错误文档与三个门面报的是同一份形状——独立消费者不需要另一套解析。
    let payload: serde_json::Value = serde_json::from_str(&error.to_json()).expect("错误可解析");
    assert_eq!(payload["action"], "PROVIDE_DATA");
    assert_eq!(payload["retry"], "NEVER");
}

#[test]
fn the_rust_sdk_facade_executes_walk_forward_and_baseline_comparison() {
    let output_dir = scratch("experiment");
    let client = Client::new(CallerCapability::Research, "sdk-experiment");
    let spec = RunExperimentSpec {
        schema_version: 1,
        experiment_id: "sdk-walk-forward".into(),
        base: spec("unused-experiment-base", &output_dir),
        parameter_space: vec![ExperimentParameterSpace {
            name: "fast_window".into(),
            values: vec![2, 3],
        }],
        strategy_kinds: Vec::new(),
        split_plan: Some(ExperimentSplitPlan {
            train_bars: 30,
            test_bars: 10,
            walk_forward: true,
            step_bars: 10,
        }),
        baselines: vec!["buy-and-hold".into(), "zero-signal".into()],
    };
    let result = client.run_experiment(&spec).expect("Rust SDK experiment");
    let oos = result
        .out_of_sample
        .expect("OOS report comes through facade");
    assert_eq!(oos.folds.len(), 4);
    assert_eq!(result.baselines.len(), 2);
    assert_eq!(result.baselines[1].return_bps, 0);
}
