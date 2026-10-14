use super::*;
use crate::{DepthBarStrategy, NativeOrderBookStrategy, OrderBookActions, RunManifestIdentity};
use qx_core::{
    InstrumentId, MarginMode, Money, Order, OrderPolicy, OrderStatus, PositionMode, PositionSide,
    Quantity, Side, TradingInstrumentSpec, TradingProduct, SCALE,
};
use qx_strategy::{
    Strategy, StrategyContext, StrategyDecision, StrategyOrderIntent, STRATEGY_API_VERSION,
};
use std::sync::{Arc, Mutex};

#[path = "tests/depth_bar_adapter.rs"]
mod depth_bar_adapter;
#[path = "tests/fill_feedback.rs"]
mod fill_feedback;
#[path = "tests/funding.rs"]
mod funding;
#[path = "tests/pending_risk.rs"]
mod pending_risk;

struct NativeBuy;

impl Strategy for NativeBuy {
    fn on_event(
        &mut self,
        context: &StrategyContext,
        event: &qx_strategy::MarketEvent,
    ) -> Result<StrategyDecision, String> {
        let instrument = event.instrument().cloned().ok_or("missing instrument")?;
        Ok(StrategyDecision {
            schema_version: STRATEGY_API_VERSION,
            request_id: format!("native:{}", event.ts()),
            strategy_id: context.strategy_id.clone(),
            signal_id: event.ts(),
            confidence: 1,
            priority: 0,
            expires_at: event.ts(),
            intents: vec![StrategyOrderIntent {
                intent_id: event.ts(),
                instrument,
                side: Side::Buy,
                qty: Quantity::from_i64(1),
                limit: None,
                policy: None,
                reduce_only: false,
                post_only: false,
            }],
        })
    }
}
#[path = "tests/margin.rs"]
mod margin;
#[path = "tests/tier.rs"]
mod tier;

struct BuyOnce {
    emitted: bool,
}

impl OrderBookStrategy for BuyOnce {
    fn on_order_book(
        &mut self,
        snapshot: &OrderBookSnapshot,
        _position: i128,
    ) -> Result<Vec<Order>, String> {
        if self.emitted {
            return Ok(Vec::new());
        }
        self.emitted = true;
        Ok(vec![Order {
            client_id: 1,
            instrument: snapshot.instrument.clone(),
            side: Side::Buy,
            qty: Quantity::from_i64(1),
            limit: None,
            status: OrderStatus::PendingSubmit,
            filled: Quantity::ZERO,
            account_id: "main".into(),
            trace: None,
            policy: None,
        }])
    }
}

struct DerivativeBuy {
    emitted: bool,
}

impl OrderBookStrategy for DerivativeBuy {
    fn on_order_book(
        &mut self,
        snapshot: &OrderBookSnapshot,
        _position: i128,
    ) -> Result<Vec<Order>, String> {
        if self.emitted {
            return Ok(Vec::new());
        }
        self.emitted = true;
        Ok(vec![Order {
            client_id: 99,
            instrument: snapshot.instrument.clone(),
            side: Side::Buy,
            qty: Quantity::from_i64(1),
            limit: None,
            status: OrderStatus::PendingSubmit,
            filled: Quantity::ZERO,
            account_id: "main".into(),
            trace: None,
            policy: Some(OrderPolicy {
                margin_mode: MarginMode::Cross,
                position_mode: PositionMode::OneWay,
                position_side: PositionSide::Net,
                leverage: 10,
                ..OrderPolicy::default()
            }),
        }])
    }
}

fn snapshot(ts: u64, ask: i64) -> OrderBookSnapshot {
    OrderBookSnapshot {
        instrument: InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
        ts,
        sequence: ts,
        bids: vec![crate::BookLevel {
            price: Price::from_i64(99),
            qty: Quantity::from_i64(2),
        }],
        asks: vec![crate::BookLevel {
            price: Price::from_i64(ask),
            qty: Quantity::from_i64(2),
        }],
    }
}

#[test]
fn order_book_backtest_is_causal_and_replays_fill() {
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let config = OrderBookBacktestConfig {
        instrument: instrument.clone(),
        account_id: "main".into(),
        currency: "USDT".into(),
        initial_cash: Money::from_i64(10_000),
        fee_bps: 1,
        instrument_spec: None,
        risk: RiskGate::conservative_default(),
        data_tier: DataTier::L2L3,
    };
    let mut strategy = BuyOnce { emitted: false };
    let report = OrderBookBacktestEngine::new(config)
        .run(&[snapshot(1, 100), snapshot(2, 101)], &mut strategy)
        .unwrap();
    assert_eq!(report.fills.len(), 1);
    assert_eq!(report.fills[0].ts, 2);
    assert_eq!(
        report
            .event_log
            .events()
            .iter()
            .filter(|event| matches!(&event.kind, EventKind::OrderSubmitted { .. }))
            .count(),
        1
    );
    // 深度链同一条纪律（V11 Q62）：事实源要被重新接受一遍并逐条还原出同一本账簿，
    // 而不是把同一段事件切片再哈希一遍。
    let (log, replayed) = ReplayVerifier::replay(report.event_log.events()).unwrap();
    assert_eq!(log.digest(), report.result_hash());
    assert_eq!(replayed.entries(), report.ledger.entries());
    let facts = ReplayVerifier::verify(report.event_log.events(), &report.ledger).unwrap();
    assert_eq!(facts.events, report.event_log.events().len());
    assert_eq!(facts.ledger_entries, report.ledger.entries().len());
}

/// 名义额远超可用现金的现货买单：用来验证深度链不会把账簿现金透支成负数。
struct BigBuyOnce {
    emitted: bool,
}

impl OrderBookStrategy for BigBuyOnce {
    fn on_order_book(
        &mut self,
        snapshot: &OrderBookSnapshot,
        _position: i128,
    ) -> Result<Vec<Order>, String> {
        if self.emitted {
            return Ok(Vec::new());
        }
        self.emitted = true;
        Ok(vec![Order {
            client_id: 7,
            instrument: snapshot.instrument.clone(),
            side: Side::Buy,
            qty: Quantity::from_i64(1_000),
            limit: None,
            status: OrderStatus::PendingSubmit,
            filled: Quantity::ZERO,
            account_id: "main".into(),
            trace: None,
            policy: None,
        }])
    }
}

/// 没有 market spec 的深度回测同样是"现货买入只能用可用现金"。Bar 链的
/// `cash_funded_spot_buy` 分支一直这么挡；这条用例钉住两链同口径，否则一次超买
/// 会静默把账簿现金透支成负数，同一份信号在两条链上给出两种结论。
#[test]
fn specless_spot_buy_beyond_available_cash_is_rejected_not_overdrawn() {
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let config = OrderBookBacktestConfig {
        instrument,
        account_id: "main".into(),
        currency: "USDT".into(),
        initial_cash: Money::from_i64(10_000),
        fee_bps: 50,
        instrument_spec: None,
        risk: RiskGate::conservative_default(),
        data_tier: DataTier::L2L3,
    };
    let mut strategy = BigBuyOnce { emitted: false };
    let report = OrderBookBacktestEngine::new(config)
        .run(&[snapshot(1, 100), snapshot(2, 101)], &mut strategy)
        .unwrap();
    assert!(
        report.fills.is_empty(),
        "1 万现金成交了 10 万名义额: {:?}",
        report.fills
    );
    assert_eq!(report.turnover_raw, 0);
    assert_eq!(report.fees_raw, 0);
    assert_eq!(
        report.ledger.cash_for("main", "USDT"),
        Money::from_i64(10_000).raw(),
        "现金被透支，说明买入没有过现金门"
    );
    let reasons: Vec<String> = report
        .event_log
        .events()
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::Rejected { reason, .. } => Some(reason.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        reasons,
        vec!["买入成本（含预估手续费）超过订单簿回测可用现金".to_string()],
        "拒单事实没有进入事件日志"
    );
}

#[test]
fn derivative_order_book_uses_margin_ledger_semantics() {
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let spec = TradingInstrumentSpec {
        instrument: instrument.clone(),
        product: TradingProduct::Perpetual,
        base_currency: "BTC".into(),
        quote_currency: "USDT".into(),
        settlement_currency: "USDT".into(),
        contract_size: SCALE,
        linear: true,
        inverse: false,
        price_tick: 1,
        qty_step: 1,
        min_qty: 1,
        max_leverage: 100,
        maintenance_margin_bps: 500,
        valid_from: 1,
        valid_to: None,
    };
    let config = OrderBookBacktestConfig {
        instrument,
        account_id: "main".into(),
        currency: "USDT".into(),
        initial_cash: Money::from_i64(10_000),
        fee_bps: 0,
        instrument_spec: Some(spec),
        risk: RiskGate::conservative_default(),
        data_tier: DataTier::L2L3,
    };
    let mut strategy = DerivativeBuy { emitted: false };
    let report = OrderBookBacktestEngine::new(config)
        .run(&[snapshot(1, 100), snapshot(2, 101)], &mut strategy)
        .unwrap();
    assert_eq!(report.fills.len(), 1);
    assert_eq!(
        report.ledger.cash_for("main", "USDT"),
        Money::from_i64(10_000).raw()
    );
    assert_eq!(
        report
            .ledger
            .position_for("main", &InstrumentId::parse("BTCUSDT.BINANCE").unwrap())
            .quantity
            .raw(),
        Quantity::from_i64(1).raw()
    );
}

/// 小额合约乘数的盘口回测必须按真实名义额计费：Ledger 用
/// `spec.notional` 记账，若费用仍按 `qty * price` 计，0.001 合约的
/// 手续费会被高估 1000 倍。
#[test]
fn fractional_contract_sizes_fee_on_spec_notional() {
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let spec = TradingInstrumentSpec {
        instrument: instrument.clone(),
        product: TradingProduct::Perpetual,
        base_currency: "BTC".into(),
        quote_currency: "USDT".into(),
        settlement_currency: "USDT".into(),
        contract_size: SCALE / 1000,
        linear: true,
        inverse: false,
        price_tick: 1,
        qty_step: 1,
        min_qty: 1,
        max_leverage: 100,
        maintenance_margin_bps: 500,
        valid_from: 1,
        valid_to: None,
    };
    let config = OrderBookBacktestConfig {
        instrument,
        account_id: "main".into(),
        currency: "USDT".into(),
        initial_cash: Money::from_i64(10_000),
        fee_bps: 100,
        instrument_spec: Some(spec),
        risk: RiskGate::conservative_default(),
        data_tier: DataTier::L2L3,
    };
    let mut strategy = DerivativeBuy { emitted: false };
    let report = OrderBookBacktestEngine::new(config)
        .run(&[snapshot(1, 100), snapshot(2, 100)], &mut strategy)
        .unwrap();
    assert_eq!(report.fills.len(), 1);
    // 1 张 * 0.001 BTC * 100 USDT = 0.1 USDT 名义额，1% 费率 = 0.001
    assert_eq!(report.fills[0].fee.raw(), SCALE / 1000);
}

/// 执行模型不能静默覆盖配置里的费率，否则成本口径与声明不一致。
#[test]
fn conflicting_execution_model_fee_is_rejected_not_silently_applied() {
    let config = OrderBookBacktestConfig {
        instrument: InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
        account_id: "main".into(),
        currency: "USDT".into(),
        initial_cash: Money::from_i64(10_000),
        fee_bps: 5,
        instrument_spec: None,
        risk: RiskGate::conservative_default(),
        data_tier: DataTier::L2L3,
    };
    let mut strategy = DerivativeBuy { emitted: false };
    let error = OrderBookBacktestEngine::new(config)
        .with_execution_model(OrderBookExecutionModel::new(7).unwrap())
        .run(&[snapshot(1, 100), snapshot(2, 100)], &mut strategy)
        .err()
        .expect("费率冲突必须报错");
    assert!(
        matches!(error, qx_core::QxError::BusinessViolation(_))
            && error.to_string().contains("fee_bps"),
        "错误信息应指出冲突字段: {error}"
    );
}

fn l2_config(fee_bps: i128) -> OrderBookBacktestConfig {
    OrderBookBacktestConfig {
        instrument: InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
        account_id: "main".into(),
        currency: "USDT".into(),
        initial_cash: Money::from_i64(10_000),
        fee_bps,
        instrument_spec: None,
        risk: RiskGate::conservative_default(),
        data_tier: DataTier::L2L3,
    }
}

fn book_context() -> qx_strategy::StrategyContext {
    qx_strategy::StrategyContext {
        strategy_id: "depth-book".into(),
        strategy_version: "v1".into(),
        account_id: "main".into(),
        venue_id: "paper".into(),
        data_fingerprint: "book-1".into(),
        as_of: 1,
        positions: std::collections::BTreeMap::new(),
        cash: std::collections::BTreeMap::new(),
        available_margin_raw: Some(10_000),
        risk_state: "ready".into(),
    }
}

#[test]
fn order_book_report_is_deterministic_and_carries_unified_metrics() {
    let snapshots = [snapshot(1, 100), snapshot(2, 101), snapshot(3, 102)];
    let first = OrderBookBacktestEngine::new(l2_config(1))
        .run(&snapshots, &mut BuyOnce { emitted: false })
        .unwrap();
    let second = OrderBookBacktestEngine::new(l2_config(1))
        .run(&snapshots, &mut BuyOnce { emitted: false })
        .unwrap();
    assert_eq!(first.result_hash(), second.result_hash());
    assert_eq!(first.equity, second.equity);
    assert_eq!(first.snapshot_ts, vec![1, 2, 3]);
    assert_eq!(first.positions.len(), first.equity.len());
    assert_eq!(
        first.fees_raw,
        first.fills.iter().map(|fill| fill.fee.raw()).sum::<i128>()
    );
    assert!(first.turnover_raw > 0);
    assert_eq!(first.clock_start, 1);
    assert_eq!(first.clock_end, 3);
    assert_eq!(first.final_equity(), *first.equity.last().unwrap());
    assert!(first
        .model_descriptors
        .iter()
        .any(|descriptor| descriptor == "data_tier=L2L3"));
    // 缺省口径要逐字写进描述子，非缺省口径要同时改变描述子与结果哈希：命令面那条
    // 断言挡住"配了却不生效"，这一条挡住"生效了却没写进产物"。
    assert!(first.model_descriptors.iter().any(|descriptor| descriptor
        == "orderbook-matching@v1 fee_bps=1 latency_snapshots=0 queue_position_bps=0 market_impact_bps=0"));
    let modeled = OrderBookBacktestEngine::new(l2_config(1))
        .with_execution_model(OrderBookExecutionModel {
            fee_bps: 1,
            latency_snapshots: 2,
            queue_position_bps: 500,
            market_impact_bps: 10,
        })
        .run(&snapshots, &mut BuyOnce { emitted: false })
        .unwrap();
    assert!(modeled.model_descriptors.iter().any(|descriptor| descriptor
        == "orderbook-matching@v1 fee_bps=1 latency_snapshots=2 queue_position_bps=500 market_impact_bps=10"));
    assert_ne!(
        modeled.result_hash(),
        first.result_hash(),
        "延迟与冲击参数没有改变撮合结果"
    );
    let manifest = first
        .run_manifest(
            RunManifestIdentity {
                run_id: "book-test:run",
                code_commit: "workspace",
                config_hash: "config-hash",
                strategy_version: "v1",
                instrument_spec_version: "default-instrument-spec-v1",
                runtime_version: "runtime-schema-1",
            },
            "depth:test",
        )
        .unwrap();
    assert_eq!(manifest.clock_start, 1);
    assert_eq!(manifest.clock_end, 3);
    assert_eq!(
        manifest.input_event_hash,
        format!("{:016x}", first.input_data_hash)
    );
    assert_eq!(
        manifest.result_hash,
        format!("{:016x}", first.result_hash())
    );
    let changed = OrderBookBacktestEngine::new(l2_config(50))
        .run(&snapshots, &mut BuyOnce { emitted: false })
        .unwrap();
    assert_ne!(changed.result_hash(), first.result_hash());
    assert!(changed.fees_raw > first.fees_raw);
}
