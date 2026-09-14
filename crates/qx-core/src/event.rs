//! 事件模型：因果优先级是回测真实性的核心。
//!
//! **同时间戳不是任意排序**，而是因果优先级。若把"策略决策"排到"行情到达"之前，
//! 就等价于偷看未来——这是最常见也最隐蔽的前视偏差。

use crate::clock::Ts;
use crate::identity::InstrumentId;
use crate::ledger::{LedgerEntry, LedgerEntryKind};
use crate::numeric::{Money, Price, Quantity};
use crate::order::Fill;
use serde::{Deserialize, Serialize};

/// 因果优先级。数值小者先处理。
pub mod prio {
    /// 时钟/定时器到期：改变内部状态，但**不应偷看当前 bar 收盘**。
    pub const TIMER: u8 = 0;
    /// 前一时刻提交、且延迟已到期的回报/控制命令。
    pub const FEEDBACK: u8 = 1;
    /// 本时点市场数据到达：新行情成为策略可见输入。
    pub const MARKET: u8 = 2;
    /// 策略/组合/风控命令：使用本时点可见数据决策。
    pub const COMMAND: u8 = 3;
    /// 仿真交易所接收、排队、撮合。
    pub const MATCH: u8 = 4;
    /// Fill、订单状态、账户、组合更新——保证策略**在下一事件才看到结果**。
    pub const APPLY: u8 = 5;
    /// 结算、盘后风控、指标快照：防止盘中估值混入当日成交结果。
    pub const POST: u8 = 9;
}

pub use self::prio as Priority;

/// 外部账户余额快照中的单一资产事实。它用于对账与恢复观察，不能绕过
/// Fill/LedgerEntry 直接修改内核账簿。
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct AccountBalance {
    pub asset: String,
    pub free: Money,
    pub locked: Money,
    /// 保证金/借贷账户的负债，净现金按 free + locked - borrowed 计算。
    #[serde(default)]
    pub borrowed: Money,
}

/// 交易所返回的持仓观察快照。它用于实盘恢复、保证金/风险对账和查询，
/// 不会绕过成交事实直接修改 Ledger；Ledger 仍只由 Fill/Settlement 等事实驱动。
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct AccountPositionSnapshot {
    pub instrument: InstrumentId,
    pub quantity: Quantity,
    pub average_price: Option<Price>,
    pub mark_price: Option<Price>,
    /// 交易所估算的强平价格；它是风险观察值，不直接驱动 Ledger。
    #[serde(default)]
    pub liquidation_price: Option<Price>,
    pub unrealized_pnl: Money,
    pub initial_margin: Money,
    pub maintenance_margin: Money,
    pub leverage: Option<u32>,
    pub margin_mode: Option<String>,
    pub position_side: Option<String>,
}

/// 合约资金费率观察快照。它是市场/账户风险输入，不是已结算资金费事实；
/// 实际资金费入账仍必须由成交所账单或明确结算事实驱动。
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct FundingRateSnapshot {
    pub instrument: InstrumentId,
    pub funding_rate_bps: i64,
    pub next_funding_timestamp_ms: Option<u64>,
}

/// 交易所账单/资金流水中的一笔可归约现金事实。
///
/// 与余额快照不同，Cashflow 是带外部唯一标识的增量事实，可以安全地进入
/// Ledger；同一笔账单重复拉取时由 `correlation_id`/`external_id` 幂等去重。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CashflowKind {
    Funding,
    Interest,
    Settlement,
    Transfer,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct AccountCashflow {
    pub account_id: String,
    pub venue_id: String,
    pub currency: String,
    pub kind: CashflowKind,
    /// 正数表示账户收到，负数表示账户支出。
    pub amount: Money,
    pub external_id: String,
}

/// 事件类型。
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum EventKind {
    Timer {
        name: String,
    },
    /// 一根 bar 到达，携带收盘价（策略此刻才可见）。
    MarketBar {
        instrument: InstrumentId,
        close: Price,
    },
    /// L1 报价到达；深度和数量由上层数据事件保存。
    MarketQuote {
        instrument: InstrumentId,
        bid: Price,
        ask: Price,
    },
    AccountBalanceSnapshot {
        account_id: String,
        venue_id: String,
        balances: Vec<AccountBalance>,
    },
    AccountPositionSnapshot {
        account_id: String,
        venue_id: String,
        positions: Vec<AccountPositionSnapshot>,
    },
    FundingRateSnapshot {
        instrument: InstrumentId,
        funding_rate_bps: i64,
        next_funding_timestamp_ms: Option<u64>,
    },
    AccountCashflow {
        cashflow: AccountCashflow,
    },
    /// 订单完整提交事实。与仅携带 id 的历史 `Submit` 事件并存，供实盘
    /// EventLog 在重启后恢复 OMS 所需的完整订单形状。
    OrderSubmitted {
        order: crate::order::Order,
    },
    Submit {
        client_order_id: u64,
    },
    Accepted {
        client_order_id: u64,
        #[serde(default)]
        venue_order_id: Option<String>,
    },
    Rejected {
        client_order_id: u64,
        reason: String,
    },
    Cancelled {
        client_order_id: u64,
    },
    Filled {
        fill: Fill,
    },
    LedgerApplied {
        entry: LedgerEntry,
    },
    ReconcileRequired {
        client_order_id: u64,
    },
    /// 日终结算。
    Settle,
}

/// 事件信封：append-only、frozen、可重放。
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Event {
    /// 稳定输入序，保证同 (ts, prio) 下顺序确定。
    pub seq: u64,
    /// 业务发生时间（回测按此推进）。
    pub ts: Ts,
    pub prio: u8,
    pub kind: EventKind,
    /// 触发本事件的源事件 seq，用于归因。
    pub causation: Option<u64>,
    /// 外部事实时间；回测中默认为业务时间。
    pub receive_time: Ts,
    pub engine_time: Ts,
    pub source_seq: u64,
    pub correlation_id: String,
}

impl Event {
    pub fn new(seq: u64, ts: Ts, prio: u8, kind: EventKind) -> Self {
        Self {
            seq,
            ts,
            prio,
            kind,
            causation: None,
            receive_time: ts,
            engine_time: ts,
            source_seq: seq,
            correlation_id: String::new(),
        }
    }

    pub fn caused_by(mut self, seq: u64) -> Self {
        self.causation = Some(seq);
        self
    }

    pub fn received_at(mut self, ts: Ts) -> Self {
        self.receive_time = ts;
        self
    }
    pub fn engine_at(mut self, ts: Ts) -> Self {
        self.engine_time = ts;
        self
    }
    pub fn sourced_by(mut self, seq: u64) -> Self {
        self.source_seq = seq;
        self
    }
    pub fn correlated(mut self, id: impl Into<String>) -> Self {
        self.correlation_id = id.into();
        self
    }

    /// 稳定摘要：用于重放校验。**不得依赖 Debug 输出**（格式可能随版本变化）。
    pub fn digest(&self, h: &mut crate::sourcing::Fnv1a) {
        h.write_u64(self.seq);
        h.write_u64(self.ts);
        h.write_u64(self.prio as u64);
        h.write_u64(self.causation.unwrap_or(0));
        h.write_u64(self.receive_time);
        h.write_u64(self.engine_time);
        h.write_u64(self.source_seq);
        h.write_text(&self.correlation_id);
        match &self.kind {
            EventKind::Timer { name } => {
                h.write_u64(1);
                h.write_text(name);
            }
            EventKind::MarketBar { instrument, close } => {
                h.write_u64(2);
                h.write_text(&instrument.to_string());
                h.write_i128(close.raw());
            }
            EventKind::MarketQuote {
                instrument,
                bid,
                ask,
            } => {
                h.write_u64(7);
                h.write_text(&instrument.to_string());
                h.write_i128(bid.raw());
                h.write_i128(ask.raw());
            }
            EventKind::AccountBalanceSnapshot {
                account_id,
                venue_id,
                balances,
            } => {
                h.write_u64(13);
                h.write_text(account_id);
                h.write_text(venue_id);
                h.write_u64(balances.len() as u64);
                for balance in balances {
                    h.write_text(&balance.asset);
                    h.write_i128(balance.free.raw());
                    h.write_i128(balance.locked.raw());
                    h.write_i128(balance.borrowed.raw());
                }
            }
            EventKind::AccountPositionSnapshot {
                account_id,
                venue_id,
                positions,
            } => {
                h.write_u64(14);
                h.write_text(account_id);
                h.write_text(venue_id);
                h.write_u64(positions.len() as u64);
                for position in positions {
                    h.write_text(&position.instrument.to_string());
                    h.write_i128(position.quantity.raw());
                    h.write_i128(position.average_price.map(|price| price.raw()).unwrap_or(0));
                    h.write_i128(position.mark_price.map(|price| price.raw()).unwrap_or(0));
                    h.write_i128(
                        position
                            .liquidation_price
                            .map(|price| price.raw())
                            .unwrap_or(0),
                    );
                    h.write_i128(position.unrealized_pnl.raw());
                    h.write_i128(position.initial_margin.raw());
                    h.write_i128(position.maintenance_margin.raw());
                    h.write_u64(position.leverage.unwrap_or(0) as u64);
                    h.write_text(position.margin_mode.as_deref().unwrap_or_default());
                    h.write_text(position.position_side.as_deref().unwrap_or_default());
                }
            }
            EventKind::FundingRateSnapshot {
                instrument,
                funding_rate_bps,
                next_funding_timestamp_ms,
            } => {
                h.write_u64(15);
                h.write_text(&instrument.to_string());
                h.write_i128(i128::from(*funding_rate_bps));
                h.write_u64(next_funding_timestamp_ms.unwrap_or(0));
            }
            EventKind::AccountCashflow { cashflow } => {
                h.write_u64(16);
                h.write_text(&cashflow.account_id);
                h.write_text(&cashflow.venue_id);
                h.write_text(&cashflow.currency);
                h.write_u64(match cashflow.kind {
                    CashflowKind::Funding => 1,
                    CashflowKind::Interest => 2,
                    CashflowKind::Settlement => 3,
                    CashflowKind::Transfer => 4,
                });
                h.write_i128(cashflow.amount.raw());
                h.write_text(&cashflow.external_id);
            }
            EventKind::OrderSubmitted { order } => {
                h.write_u64(12);
                h.write_u64(order.client_id);
                h.write_text(&order.instrument.to_string());
                h.write_u64(match order.side {
                    crate::order::Side::Buy => 1,
                    crate::order::Side::Sell => 2,
                });
                h.write_i128(order.qty.raw());
                h.write_i128(order.limit.map(|price| price.raw()).unwrap_or(0));
                h.write_u64(match order.status {
                    crate::order::OrderStatus::PendingSubmit => 1,
                    crate::order::OrderStatus::Submitted => 2,
                    crate::order::OrderStatus::Accepted => 3,
                    crate::order::OrderStatus::Working => 4,
                    crate::order::OrderStatus::PartiallyFilled => 5,
                    crate::order::OrderStatus::Filled => 6,
                    crate::order::OrderStatus::CancelPending => 7,
                    crate::order::OrderStatus::Cancelled => 8,
                    crate::order::OrderStatus::Rejected => 9,
                    crate::order::OrderStatus::Expired => 10,
                    crate::order::OrderStatus::Unknown => 11,
                });
                h.write_i128(order.filled.raw());
                h.write_text(&order.account_id);
                if let Some(trace) = &order.trace {
                    h.write_text(trace.strategy_id.as_deref().unwrap_or_default());
                    h.write_u64(trace.signal_id.unwrap_or(0));
                    h.write_u64(trace.intent_id.unwrap_or(0));
                    h.write_text(trace.rule_version.as_deref().unwrap_or_default());
                } else {
                    h.write_text("");
                    h.write_u64(0);
                    h.write_u64(0);
                    h.write_text("");
                }
            }
            EventKind::Submit { client_order_id } => {
                h.write_u64(3);
                h.write_u64(*client_order_id);
            }
            EventKind::Accepted {
                client_order_id,
                venue_order_id,
            } => {
                h.write_u64(4);
                h.write_u64(*client_order_id);
                h.write_text(venue_order_id.as_deref().unwrap_or_default());
            }
            EventKind::Rejected {
                client_order_id,
                reason,
            } => {
                h.write_u64(8);
                h.write_u64(*client_order_id);
                h.write_text(reason);
            }
            EventKind::Cancelled { client_order_id } => {
                h.write_u64(9);
                h.write_u64(*client_order_id);
            }
            EventKind::Filled { fill } => {
                h.write_u64(5);
                h.write_u64(fill.order_id);
                h.write_i128(fill.qty.raw());
                h.write_i128(fill.price.raw());
                h.write_i128(fill.fee.raw());
                h.write_u64(fill.ts);
                h.write_text(&fill.account_id);
                h.write_text(fill.strategy_id.as_deref().unwrap_or_default());
                h.write_u64(fill.signal_id.unwrap_or(0));
                h.write_u64(fill.intent_id.unwrap_or(0));
                h.write_text(fill.venue_id.as_deref().unwrap_or_default());
                h.write_text(fill.venue_order_id.as_deref().unwrap_or_default());
                h.write_text(fill.rule_version.as_deref().unwrap_or_default());
                h.write_text(fill.fee_currency.as_deref().unwrap_or_default());
            }
            EventKind::LedgerApplied { entry } => {
                h.write_u64(10);
                h.write_u64(entry.id);
                h.write_text(&entry.account_id);
                h.write_text(&entry.currency);
                h.write_u64(match &entry.kind {
                    LedgerEntryKind::TradeCash => 1,
                    LedgerEntryKind::TradePosition => 2,
                    LedgerEntryKind::Fee => 3,
                    LedgerEntryKind::Funding => 4,
                    LedgerEntryKind::Settlement => 5,
                    LedgerEntryKind::Adjustment => 6,
                    LedgerEntryKind::Interest => 7,
                    LedgerEntryKind::Liquidation => 8,
                    LedgerEntryKind::CorporateAction => 9,
                });
                h.write_i128(entry.amount.raw());
                h.write_text(
                    &entry
                        .instrument
                        .as_ref()
                        .map(ToString::to_string)
                        .unwrap_or_default(),
                );
                h.write_i128(entry.quantity.raw());
                h.write_i128(entry.price.map(|p| p.raw()).unwrap_or(0));
                h.write_u64(entry.order_id.unwrap_or(0));
                h.write_u64(entry.ts);
                h.write_i128(entry.multiplier);
                h.write_u64(match entry.position_side {
                    Some(crate::trading::PositionSide::Long) => 1,
                    Some(crate::trading::PositionSide::Short) => 2,
                    Some(crate::trading::PositionSide::Net) => 3,
                    None => 0,
                });
            }
            EventKind::ReconcileRequired { client_order_id } => {
                h.write_u64(11);
                h.write_u64(*client_order_id);
            }
            EventKind::Settle => h.write_u64(6),
        }
    }
}

/// 仅用于让上面 `Fill` 字段类型保持显式引用，避免 unused import 误判。
#[allow(dead_code)]
fn _assert_types(_: Option<(Quantity, Money)>) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn priority_order_is_deterministic() {
        const {
            assert!(Priority::MARKET < Priority::COMMAND);
            assert!(Priority::COMMAND < Priority::MATCH);
            assert!(Priority::MATCH < Priority::APPLY);
            assert!(Priority::APPLY < Priority::POST);
        }
    }

    #[test]
    fn digest_preserves_text_boundaries() {
        let left = Event::new(1, 1, Priority::TIMER, EventKind::Timer { name: "c".into() })
            .correlated("ab");
        let right = Event::new(
            1,
            1,
            Priority::TIMER,
            EventKind::Timer { name: "bc".into() },
        )
        .correlated("a");
        let mut left_hash = crate::sourcing::Fnv1a::new();
        let mut right_hash = crate::sourcing::Fnv1a::new();
        left.digest(&mut left_hash);
        right.digest(&mut right_hash);
        assert_ne!(left_hash.finish(), right_hash.finish());
    }
}
