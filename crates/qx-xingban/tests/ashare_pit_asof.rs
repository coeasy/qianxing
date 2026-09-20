//! A 股 PIT 门禁：同一份录制数据集在不同研究截止日（`as_of`）下必须产出可区分的回测结果。
//!
//! 数据集本身逐字节相同，只有信封的 `as_of` 不同。分红公告在截止日之前发布时
//! 才会进入规则快照并产生现金流；否则必须以"看不见"处理，且回测结果必须不同。

use qx_core::{InstrumentId, Money, Order, OrderStatus, Quantity, Side, SCALE};
use qx_guanxing::Bar;
use qx_xingban::ashare::{AshareCorporateActionLoadReport, AshareRuleConfig};
use qx_xingban::{
    BacktestConfig, BacktestEngine, BacktestReport, BarStrategy, DataTier, MakerTakerFeeModel,
    NextBarOpenFillModel, NoMargin, VirtualTradingConfig, ZeroLatency,
};
use qx_zhenlu::RiskGate;

const INSTRUMENT: &str = "000001.SZSE";
const DAY_MS: u64 = 86_400_000;
/// 每股 1 元现金分红，持有 100 股 ⇒ 支付日入账 100 元。
const DIVIDEND_PER_SHARE_RAW: i128 = SCALE;
const SHARES: i64 = 100;

/// 录制数据集（v1 信封）：一次现金分红，公告早于登记日。`{as_of}` 由用例填入，
/// 因此两条分支看到的除权/支付信息完全来自同一份录制行。
fn dataset(as_of: &str) -> String {
    format!(
        r#"{{
  "schema_version": 1,
  "source": "recorded-ashare-dividend-v1",
  "instrument": "{INSTRUMENT}",
  "as_of": "{as_of}",
  "actions": [
    {{
      "instrument": "{INSTRUMENT}",
      "action_type": "cash_dividend",
      "published_at": "2024-05-20T09:00:00+08:00",
      "announcement_date": "2024-05-20",
      "record_date": "2024-05-30",
      "ex_date": "2024-06-03",
      "payment_date": "2024-06-10",
      "cash_dividend_raw": {DIVIDEND_PER_SHARE_RAW},
      "source": "akshare"
    }}
  ]
}}"#
    )
}

fn snapshot(as_of: &str) -> (AshareRuleConfig, AshareCorporateActionLoadReport) {
    let mut rules = AshareRuleConfig {
        enabled: true,
        ..AshareRuleConfig::default()
    };
    let report = rules
        .apply_corporate_actions_json_with_report(INSTRUMENT, &dataset(as_of))
        .expect("录制数据集必须可加载");
    rules.validate().expect("规则快照必须自洽");
    (rules, report)
}

struct BuyBoardLot {
    done: bool,
}

impl BarStrategy for BuyBoardLot {
    fn on_bar(
        &mut self,
        _history: &[Bar],
        instrument: &InstrumentId,
        _ts: u64,
        position: i128,
    ) -> Option<Order> {
        if self.done || position != 0 {
            return None;
        }
        self.done = true;
        Some(Order {
            client_id: 0,
            instrument: instrument.clone(),
            side: Side::Buy,
            qty: Quantity::from_i64(SHARES),
            limit: None,
            status: OrderStatus::Submitted,
            filled: Quantity::ZERO,
            account_id: "main".into(),
            trace: None,
            policy: None,
        })
    }
}

/// 在可见快照上取真实日期时间戳，保证两条分支跑同一组 Bar（次根开放价成交，
/// 因此下单 Bar 与登记日之间还要留出一根成交 Bar）。
fn bars_for(rules: &AshareRuleConfig) -> Vec<Bar> {
    let event = rules
        .corporate_actions
        .first()
        .expect("完整数据集应含一条分红事件");
    let record = event.record_ts.expect("登记日必须透传");
    let price = 10 * SCALE;
    [
        record - 3 * DAY_MS,
        record - 2 * DAY_MS,
        record - DAY_MS,
        record,
        event.ts,
        event.payment_ts.expect("支付日必须透传"),
    ]
    .into_iter()
    .map(|ts| Bar::new(ts, price, price, price, price, 1_000))
    .collect()
}

fn run_backtest(rules: &AshareRuleConfig, bars: &[Bar]) -> BacktestReport {
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
            ashare_rules: Some(rules.clone()),
            ..VirtualTradingConfig::default()
        },
    };
    BacktestEngine::new(config)
        .run(bars, &mut BuyBoardLot { done: false })
        .expect("A 股 PIT 回测必须跑通")
}

#[test]
fn research_cutoff_before_publication_hides_the_recorded_dividend() {
    let (rules, report) = snapshot("2024-05-01T00:00:00+08:00");
    assert!(report.as_of_ms.is_some(), "研究截止日必须登记进 PIT 报告");
    assert_eq!(report.source, "recorded-ashare-dividend-v1");
    assert_eq!(report.row_count, 1);
    assert_eq!(report.applied_actions, 0);
    assert_eq!(report.hidden_actions, 1);
    assert!(rules.corporate_actions.is_empty());
    assert_eq!(
        report.row_count,
        report.applied_actions + report.hidden_actions
    );
}

#[test]
fn same_dataset_under_different_as_of_produces_distinguishable_backtests() {
    let (hidden, hidden_report) = snapshot("2024-05-01T00:00:00+08:00");
    assert_eq!(
        (hidden_report.applied_actions, hidden_report.hidden_actions),
        (0, 1)
    );
    let (visible, visible_report) = snapshot("2024-06-01T00:00:00+08:00");
    assert_eq!(visible_report.applied_actions, 1);
    assert_eq!(visible_report.hidden_actions, 0);

    // Bar 序列取自可见快照，两条分支共用同一份行情输入。
    let bars = bars_for(&visible);
    let hidden_run = run_backtest(&hidden, &bars);
    let visible_run = run_backtest(&visible, &bars);

    assert_eq!(hidden_run.fills.len(), 1);
    assert_eq!(visible_run.fills.len(), 1);
    let dividend = Money::from_raw(DIVIDEND_PER_SHARE_RAW * SHARES as i128);
    assert_eq!(
        visible_run
            .ledger
            .cash_for("main", "CNY")
            .saturating_sub(hidden_run.ledger.cash_for("main", "CNY")),
        dividend.raw(),
        "只有公告已知的分支应收到分红现金"
    );
    assert_eq!(
        visible_run.final_equity() - hidden_run.final_equity(),
        dividend.raw()
    );
    assert_ne!(
        hidden_run.result_hash(),
        visible_run.result_hash(),
        "同一数据集在不同 as_of 下结果指纹必须可区分"
    );
}

#[test]
fn a_single_cutoff_stays_reproducible() {
    let (rules, _) = snapshot("2024-06-01T00:00:00+08:00");
    let bars = bars_for(&rules);
    let first = run_backtest(&rules, &bars);
    let second = run_backtest(&rules, &bars);
    assert_eq!(first.result_hash(), second.result_hash());
    assert_eq!(first.result_hash(), first.replay_hash());
}
