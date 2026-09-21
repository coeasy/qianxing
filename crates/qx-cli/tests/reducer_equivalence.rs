//! 回测链路与实盘链路的成交归约等价性契约。
//!
//! 两条链路只在"事实从哪来"上不同：星板按 Bar 撮合，运行时按 Venue 回报归约。
//! 同一笔成交事实一旦被两侧接受，现金、持仓、费用和订单状态必须逐条一致，否则
//! 回测结论不能外推到实盘。本测试用真实的 `BacktestEngine` 产出成交，再把同一份
//! 成交事实喂给真实的 `LiveEventPipeline`，因此任何一侧重新引入私有归约分支、
//! 换掉 `FillTerms` 的条款选择或改掉传入的币种与账户，都会在这里立刻失败。
//! 归约顺序本身的原子性由 `qx-core` 的 `fill_apply` 用例锁定。

use qx_core::{
    Fill, InstrumentId, Ledger, LedgerEntry, LedgerEntryKind, MakerTakerFeeModel, Money, Order,
    OrderStatus, Quantity, Side, TradingInstrumentSpec, TradingProduct, VenueId, SCALE,
};
use qx_guanxing::Bar;
use qx_runtime::{LiveEventPipeline, RuntimeEventEnvelope, RuntimeExternalEvent};
use qx_xingban::{
    BacktestConfig, BacktestEngine, BacktestReport, BarStrategy, DataTier, NextBarOpenFillModel,
    NoMargin, VirtualTradingConfig, ZeroLatency,
};
use qx_zhenlu::RiskGate;

const ACCOUNT: &str = "main";
const CURRENCY: &str = "USDT";

fn initial_cash() -> Money {
    Money::from_i64(1_000)
}

/// `Bar` 的价格与量是定点原始值，需要自己乘上 `SCALE`。
fn bar(ts: u64, open: i128, high: i128, low: i128, close: i128) -> Bar {
    Bar::new(
        ts,
        open * SCALE,
        high * SCALE,
        low * SCALE,
        close * SCALE,
        10 * SCALE,
    )
}

// Bar t 决策、Bar t+1 开盘成交，因此两笔委托分别落在 110 与 120 两个不同价格上。
fn bars() -> Vec<Bar> {
    vec![
        bar(1, 100, 105, 100, 105),
        bar(2, 110, 115, 108, 112),
        bar(3, 120, 125, 118, 122),
        bar(4, 130, 135, 128, 132),
    ]
}

fn spot_spec() -> TradingInstrumentSpec {
    TradingInstrumentSpec {
        instrument: InstrumentId::parse("BTC/USDT.BINANCE").unwrap(),
        product: TradingProduct::Spot,
        base_currency: "BTC".into(),
        quote_currency: CURRENCY.into(),
        settlement_currency: CURRENCY.into(),
        contract_size: SCALE,
        linear: true,
        inverse: false,
        price_tick: 1,
        qty_step: 1,
        min_qty: 1,
        max_leverage: 1,
        maintenance_margin_bps: 0,
        valid_from: 1,
        valid_to: None,
    }
}

/// 线性永续：每张合约 0.01 BTC，与现货的现金语义路径不同。
fn perpetual_spec() -> TradingInstrumentSpec {
    TradingInstrumentSpec {
        instrument: InstrumentId::new("BTCUSDT", VenueId::new("BINANCE")),
        product: TradingProduct::Perpetual,
        base_currency: "BTC".into(),
        quote_currency: CURRENCY.into(),
        settlement_currency: CURRENCY.into(),
        contract_size: SCALE / 100,
        linear: true,
        inverse: false,
        price_tick: 1,
        qty_step: 1,
        min_qty: 1,
        max_leverage: 10,
        maintenance_margin_bps: 50,
        valid_from: 1,
        valid_to: None,
    }
}

/// 在前两根可见 Bar 上各下一笔一手的市价买单。
struct BuyTwice {
    orders: usize,
}

impl BarStrategy for BuyTwice {
    fn on_bar(
        &mut self,
        _history: &[Bar],
        instrument: &InstrumentId,
        _ts: u64,
        _position: i128,
    ) -> Option<Order> {
        if self.orders >= 2 {
            return None;
        }
        self.orders += 1;
        Some(Order {
            client_id: self.orders as u64,
            instrument: instrument.clone(),
            side: Side::Buy,
            qty: Quantity::from_i64(1),
            limit: None,
            status: OrderStatus::Submitted,
            filled: Quantity::ZERO,
            account_id: ACCOUNT.into(),
            trace: None,
            policy: None,
        })
    }
}

fn run_backtest(spec: TradingInstrumentSpec) -> BacktestReport {
    let config = BacktestConfig {
        instrument: spec.instrument.clone(),
        instrument_spec: Some(spec),
        account_id: ACCOUNT.into(),
        currency: CURRENCY.into(),
        initial_cash: initial_cash(),
        multiplier: 1,
        fill: Box::new(NextBarOpenFillModel),
        fee: Box::new(MakerTakerFeeModel {
            maker_bp: 0,
            taker_bp: 25,
        }),
        data_tier: DataTier::Bar,
        latency: Box::new(ZeroLatency),
        margin: Box::new(NoMargin),
        seed: 7,
        risk: RiskGate::from_rule_set(qx_risk::RuleSet::account_limits_only()),
        virtual_trading: VirtualTradingConfig::default(),
    };
    let report = BacktestEngine::new(config)
        .run(&bars(), &mut BuyTwice { orders: 0 })
        .expect("回测链路应跑完");
    assert_eq!(report.fills.len(), 2, "样例数据必须产出两笔成交");
    assert!(
        report.fills[0].price != report.fills[1].price
            && report.fills[0].fee != report.fills[1].fee
            && !report.fills[0].fee.is_zero(),
        "两笔成交必须价格与费用都不同，否则等价性没有约束力: {:?}",
        report.fills
    );
    report
}

fn temp_root(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "qianxing-reducer-equivalence-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

/// 把回测产出的成交事实按实盘链路重放一遍：注册同一委托 → Venue 确认 → 成交。
fn replay_in_live(
    tag: &str,
    fills: &[Fill],
    spec: &TradingInstrumentSpec,
) -> (LiveEventPipeline, std::path::PathBuf) {
    let root = temp_root(tag);
    let mut pipeline =
        LiveEventPipeline::open(&root, format!("{tag}-events"), CURRENCY).expect("事件日志应打开");
    for (index, fill) in fills.iter().enumerate() {
        pipeline
            .register_order(
                Order {
                    client_id: fill.order_id,
                    instrument: spec.instrument.clone(),
                    side: Side::Buy,
                    qty: fill.qty,
                    limit: Some(fill.price),
                    status: OrderStatus::PendingSubmit,
                    filled: Quantity::ZERO,
                    account_id: ACCOUNT.into(),
                    trace: None,
                    policy: None,
                },
                fill.ts,
            )
            .expect("实盘链路应注册同一委托");
        pipeline
            .ingest(RuntimeEventEnvelope::venue(
                RuntimeExternalEvent::Accepted {
                    client_order_id: fill.order_id,
                    venue_order_id: None,
                },
                fill.ts,
                fill.ts,
                (index + 1) as u64,
                format!("{tag}-ack-{index}"),
            ))
            .expect("实盘链路应接受同一确认");
        pipeline
            .ingest(RuntimeEventEnvelope::venue(
                RuntimeExternalEvent::FillWithSpec {
                    fill: Box::new(fill.clone()),
                    spec: Box::new(spec.clone()),
                },
                fill.ts,
                fill.ts + 1,
                (index + 100) as u64,
                format!("{tag}-fill-{index}"),
            ))
            .expect("实盘链路应归约同一成交");
    }
    (pipeline, root)
}

/// 只保留成交派生的账本 entry，并把 `id` 重排为相对序号后比较。
fn booked(ledger: &Ledger) -> Vec<LedgerEntry> {
    ledger
        .entries()
        .iter()
        .filter(|entry| entry.kind != LedgerEntryKind::Adjustment)
        .cloned()
        .enumerate()
        .map(|(index, mut entry)| {
            entry.id = index as u64;
            entry
        })
        .collect()
}

fn assert_equivalent(report: &BacktestReport, live: &LiveEventPipeline, instrument: &InstrumentId) {
    assert_eq!(
        booked(&report.ledger),
        booked(live.ledger()),
        "同一成交事实序列必须在两侧记出逐条相同的账本"
    );
    assert_eq!(
        report.ledger.cash(CURRENCY) - initial_cash().raw(),
        live.ledger().cash(CURRENCY),
        "扣除初始入金后两侧现金必须一致"
    );
    assert_eq!(
        report.ledger.position_for(ACCOUNT, instrument),
        live.ledger().position_for(ACCOUNT, instrument),
        "两侧持仓状态必须一致"
    );
    assert_eq!(
        live.orders()
            .iter()
            .map(|order| (order.client_id, order.status, order.filled))
            .collect::<Vec<(u64, OrderStatus, Quantity)>>(),
        report
            .fills
            .iter()
            .map(|fill| (fill.order_id, OrderStatus::Filled, fill.qty))
            .collect::<Vec<(u64, OrderStatus, Quantity)>>(),
        "归约后的订单状态必须与回测成交一一对应"
    );
    assert_eq!(
        booked(&report.ledger)
            .iter()
            .filter(|entry| entry.kind == LedgerEntryKind::Fee)
            .map(|entry| entry.amount)
            .collect::<Vec<Money>>(),
        report
            .fills
            .iter()
            .map(|fill| Money::from_raw(-fill.fee.raw()))
            .collect::<Vec<Money>>(),
        "两侧共同的账本必须把成交费用逐笔全额入账"
    );
}

/// 现货：规格条款与乘数 1 的历史条款都必须记出现金账。
#[test]
fn spot_fills_reduce_identically_on_both_chains() {
    let spec = spot_spec();
    let report = run_backtest(spec.clone());
    let (live, root) = replay_in_live("spot", &report.fills, &spec);
    assert_equivalent(&report, &live, &spec.instrument);
    assert!(
        booked(&report.ledger)
            .iter()
            .any(|entry| entry.kind == LedgerEntryKind::TradeCash),
        "现货买入必须按全额名义本金出现金流"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// 衍生品：开仓只记持仓与费用、不扣全额名义本金。若某一侧绕过规格条款改用
/// 历史乘数，两侧账本会立刻分叉——这条用例就是为了让条款选择不可静默退化。
#[test]
fn perpetual_fills_reduce_identically_on_both_chains() {
    let spec = perpetual_spec();
    let report = run_backtest(spec.clone());
    let (live, root) = replay_in_live("perp", &report.fills, &spec);
    assert_equivalent(&report, &live, &spec.instrument);
    let entries = booked(&report.ledger);
    assert!(
        entries
            .iter()
            .any(|entry| entry.kind == LedgerEntryKind::TradePosition),
        "衍生开仓必须记持仓变动"
    );
    assert!(
        !entries
            .iter()
            .any(|entry| entry.kind == LedgerEntryKind::TradeCash),
        "衍生开仓不应扣全额名义本金，否则说明某一侧没有走规格条款"
    );
    let _ = std::fs::remove_dir_all(root);
}
