//! A 股涨停板的引擎级用例：分钟线封板必须以**上一交易日收价**判板，并且真的挡下成交。
//!
//! 涨跌停的锚一旦取成"上一根 Bar 的收价"，±10% 的板就被窄化成"最近几分钟 ±10%"：
//! 一根已经封死在涨停价上的分钟线照样成交，回测把买不到的东西记成买到了。这里两个
//! 用例一起构成红绿对 —— 同一份行情，锚取昨收时零成交，锚被覆盖成上一根收价时成交。

use qx_core::{InstrumentId, Money, Order, OrderStatus, Quantity, Side, SCALE};
use qx_guanxing::Bar;
use qx_xingban::ashare::AshareRuleConfig;
use qx_xingban::{
    BacktestConfig, BacktestEngine, BacktestReport, BarStrategy, DataTier, MakerTakerFeeModel,
    NextBarOpenFillModel, NoMargin, VirtualTradingConfig, ZeroLatency,
};
use qx_zhenlu::RiskGate;
use std::collections::BTreeMap;

const INSTRUMENT: &str = "000001.SZSE";
const DAY_MS: u64 = 86_400_000;
const MINUTE: u64 = 60_000;

fn yuan(cents: i128) -> i128 {
    cents * SCALE / 100
}

/// 昨天 14:55 收在 10.00；今天 09:30 那根收在 10.50，10:00 那根封死在涨停价 11.00。
fn bars() -> Vec<Bar> {
    let day_a = 100 * DAY_MS;
    let day_b = day_a + DAY_MS;
    vec![
        Bar::new(
            day_a + 555 * MINUTE,
            yuan(990),
            yuan(995),
            yuan(985),
            yuan(990),
            1_000,
        ),
        Bar::new(
            day_a + 560 * MINUTE,
            yuan(995),
            yuan(1_000),
            yuan(990),
            yuan(1_000),
            1_000,
        ),
        Bar::new(
            day_b,
            yuan(1_050),
            yuan(1_055),
            yuan(1_045),
            yuan(1_050),
            1_000,
        ),
        Bar::new(
            day_b + 5 * MINUTE,
            yuan(1_100),
            yuan(1_100),
            yuan(1_100),
            yuan(1_100),
            1_000,
        ),
    ]
}

/// 只在封死的那根下单：引擎在本根 Bar 内就撮合本根提交的挂单（`backtest.rs:834/840`），
/// 所以涨停能否挡住成交，取决于**这根 Bar** 的锚算出来的板。
struct BuyOnSealedBar {
    trigger: u64,
    done: bool,
}

impl BarStrategy for BuyOnSealedBar {
    fn on_bar(
        &mut self,
        _history: &[Bar],
        instrument: &InstrumentId,
        ts: u64,
        position: i128,
    ) -> Option<Order> {
        if self.done || position != 0 || ts != self.trigger {
            return None;
        }
        self.done = true;
        Some(Order {
            client_id: 0,
            instrument: instrument.clone(),
            side: Side::Buy,
            qty: Quantity::from_i64(100),
            limit: None,
            status: OrderStatus::Submitted,
            filled: Quantity::ZERO,
            account_id: "main".into(),
            trace: None,
            policy: None,
        })
    }
}

fn run(previous_close_raw: BTreeMap<u64, i128>) -> BacktestReport {
    let rules = AshareRuleConfig {
        enabled: true,
        previous_close_raw,
        ..AshareRuleConfig::default()
    };
    rules.validate().expect("规则快照必须自洽");
    let market_bars = bars();
    let config = BacktestConfig {
        instrument: InstrumentId::parse(INSTRUMENT).unwrap(),
        instrument_spec: None,
        account_id: "main".into(),
        currency: "CNY".into(),
        initial_cash: Money::from_i64(100_000),
        multiplier: 1,
        fill: Box::new(NextBarOpenFillModel),
        fee: Box::new(MakerTakerFeeModel {
            maker_bp: 0,
            taker_bp: 0,
        }),
        data_tier: DataTier::Bar,
        latency: Box::new(ZeroLatency),
        margin: Box::new(NoMargin),
        seed: 1,
        risk: RiskGate::conservative_default(),
        virtual_trading: VirtualTradingConfig {
            ashare_rules: Some(rules),
            ..VirtualTradingConfig::default()
        },
    };
    BacktestEngine::new(config)
        .run(
            &market_bars,
            &mut BuyOnSealedBar {
                trigger: market_bars[3].ts,
                done: false,
            },
        )
        .expect("A 股分钟线回测必须跑通")
}

#[test]
fn a_sealed_intraday_limit_up_blocks_the_fill_when_anchored_to_the_session_close() {
    let report = run(BTreeMap::new());
    assert!(
        report.fills.is_empty(),
        "涨停封死的分钟线不该成交，实得 {:?}",
        report.fills
    );
    assert_eq!(
        report.ledger.cash_for("main", "CNY"),
        Money::from_i64(100_000).raw(),
        "没成交就不该动现金"
    );
}

#[test]
fn anchoring_to_the_previous_bar_would_fill_on_that_same_board() {
    // 本轮修掉的旧口径 = 锚取上一根 Bar 的收价。用覆盖表把它原样注入同一份行情：
    // 板被推到 11.55，这根 11.00 的封板线就成交了 —— 上面那条断言的红/绿全靠这一格。
    let market_bars = bars();
    let mut stale_anchor = BTreeMap::new();
    stale_anchor.insert(market_bars[3].ts, market_bars[2].close);
    let report = run(stale_anchor);
    assert_eq!(report.fills.len(), 1, "旧口径下这根封板线照样成交");
    assert_eq!(report.fills[0].price.raw(), yuan(1_100));
}
