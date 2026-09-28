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

/// 交易事实的最小血缘元数据。
///
/// 该结构只描述事实来源和规则版本，不携带任何凭据。旧 EventLog 缺少该
/// 字段时由 serde 使用默认值恢复；新事实会把它纳入 Event digest，保证
/// 来源或规则变化不会被错误地视为同一条可重放事实。
pub const EVENT_METADATA_SCHEMA_VERSION: u32 = 2;

/// 事件的统一运行作用域。空字段表示该维度不适用于当前事件（例如全局
/// 市场事件），但一旦事件属于订单/账户/策略链路，生产入口必须填充相应身份。
#[derive(Clone, Default, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct EventContext {
    #[serde(default)]
    pub tenant_id: String,
    #[serde(default)]
    pub run_id: String,
    #[serde(default)]
    pub account_id: String,
    #[serde(default)]
    pub portfolio_id: String,
    #[serde(default)]
    pub strategy_id: String,
    #[serde(default)]
    pub signal_id: String,
    #[serde(default)]
    pub intent_id: String,
}

impl EventContext {
    pub fn validate_for_trading(&self) -> Result<(), String> {
        if self.run_id.trim().is_empty()
            || self.account_id.trim().is_empty()
            || self.strategy_id.trim().is_empty()
        {
            return Err("交易事件上下文必须包含 run_id、account_id 和 strategy_id".into());
        }
        Ok(())
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct EventMetadata {
    #[serde(default = "default_event_metadata_schema_version")]
    pub schema_version: u32,
    #[serde(default)]
    pub source_id: String,
    /// 稳定来源分类，例如 `market_data`、`venue`、`control` 或 `internal`。
    /// 具体供应商/worker 仍由 `source_id` 表达；分类用于跨适配器审计和
    /// 回放筛选，旧事件缺失时恢复为 `internal`。
    #[serde(default = "default_event_source_kind")]
    pub source_kind: String,
    #[serde(default)]
    pub dedup_key: String,
    #[serde(default = "default_event_rule_version")]
    pub rule_version: String,
    #[serde(default)]
    pub context: EventContext,
}

fn default_event_metadata_schema_version() -> u32 {
    EVENT_METADATA_SCHEMA_VERSION
}

fn default_event_rule_version() -> String {
    "runtime-v1".into()
}

fn default_event_source_kind() -> String {
    "internal".into()
}

impl Default for EventMetadata {
    fn default() -> Self {
        Self {
            schema_version: EVENT_METADATA_SCHEMA_VERSION,
            source_id: String::new(),
            source_kind: default_event_source_kind(),
            dedup_key: String::new(),
            rule_version: default_event_rule_version(),
            context: EventContext::default(),
        }
    }
}

impl EventMetadata {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version == 0 {
            return Err("EventMetadata schema_version 必须大于 0".into());
        }
        if self.rule_version.trim().is_empty() {
            return Err("EventMetadata rule_version 不能为空".into());
        }
        if self.source_kind.trim().is_empty() {
            return Err("EventMetadata source_kind 不能为空".into());
        }
        Ok(())
    }

    pub fn derived(&self, suffix: impl AsRef<str>) -> Self {
        let suffix = suffix.as_ref();
        let mut derived = self.clone();
        if !derived.dedup_key.is_empty() {
            derived.dedup_key = format!("{}:{suffix}", derived.dedup_key);
        }
        derived
    }
}

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

impl AccountBalance {
    /// 净现金口径的**唯一**实现：`free + locked - borrowed`。
    ///
    /// 上层（对账、余额差异判定、协议线格式）必须经由本方法得到净现金，不得再
    /// 各自抄写这条公式——历史上 `qx-runtime` 与 `qx-cli` worker 两侧抄写时
    /// 已经出现口径分叉（V10 §4.7）。溢出返回 `None`，由调用方决定 fail-closed。
    pub fn net_cash_raw(&self) -> Option<i128> {
        self.free
            .raw()
            .checked_add(self.locked.raw())?
            .checked_sub(self.borrowed.raw())
    }
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
    /// 这三个量是"交易所报了才有"的观察值：`None` 表示这一份回报根本没带该字段，
    /// `Some(0)` 表示交易所明确说了是零。此前它们是不可区分的 `Money`，于是现货交易所
    /// 或省略字段的连接器会让持仓行长期以"0"的身份被发布，读侧把"没报"当成"没有浮亏/
    /// 没有保证金"。老日志缺这三个键按 `None` 读，不补一个伪造的零。
    #[serde(default)]
    pub unrealized_pnl: Option<Money>,
    #[serde(default)]
    pub initial_margin: Option<Money>,
    #[serde(default)]
    pub maintenance_margin: Option<Money>,
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
    /// L1 报价到达；保存最优买卖价及对应数量，供 Paper/回放保持成交容量一致。
    MarketQuote {
        instrument: InstrumentId,
        bid: Price,
        ask: Price,
        #[serde(default)]
        bid_qty: Quantity,
        #[serde(default)]
        ask_qty: Quantity,
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
    /// 外部事实时间；回测中默认为业务时间。
    pub receive_time: Ts,
    pub engine_time: Ts,
    /// 归因锚点：本事件所对应的外部事实序；`correlation_id` 给跨进程关联。
    pub source_seq: u64,
    pub correlation_id: String,
    /// 来源、去重和规则版本；`receive_time`/`ts` 分别对应 observed/effective 时间。
    #[serde(default)]
    pub metadata: EventMetadata,
}

impl Event {
    pub fn new(seq: u64, ts: Ts, prio: u8, kind: EventKind) -> Self {
        Self {
            seq,
            ts,
            prio,
            kind,
            receive_time: ts,
            engine_time: ts,
            source_seq: seq,
            correlation_id: String::new(),
            metadata: EventMetadata::default(),
        }
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

    pub fn with_metadata(mut self, metadata: EventMetadata) -> Self {
        self.metadata = metadata;
        self
    }

    /// 事实业务生效时间，即 metadata 语义中的 effective_at。
    pub fn effective_at(&self) -> Ts {
        self.ts
    }

    /// 稳定摘要：用于重放校验。**不得依赖 Debug 输出**（格式可能随版本变化）。
    pub fn digest(&self, h: &mut crate::sourcing::Fnv1a) {
        h.write_u64(self.seq);
        h.write_u64(self.ts);
        h.write_u64(self.prio as u64);
        h.write_u64(self.receive_time);
        h.write_u64(self.engine_time);
        h.write_u64(self.source_seq);
        h.write_text(&self.correlation_id);
        h.write_u64(self.metadata.schema_version as u64);
        h.write_text(&self.metadata.source_id);
        h.write_text(&self.metadata.source_kind);
        h.write_text(&self.metadata.dedup_key);
        h.write_text(&self.metadata.rule_version);
        h.write_text(&self.metadata.context.tenant_id);
        h.write_text(&self.metadata.context.run_id);
        h.write_text(&self.metadata.context.account_id);
        h.write_text(&self.metadata.context.portfolio_id);
        h.write_text(&self.metadata.context.strategy_id);
        h.write_text(&self.metadata.context.signal_id);
        h.write_text(&self.metadata.context.intent_id);
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
                bid_qty,
                ask_qty,
            } => {
                h.write_u64(7);
                h.write_text(&instrument.to_string());
                h.write_i128(bid.raw());
                h.write_i128(ask.raw());
                h.write_i128(bid_qty.raw());
                h.write_i128(ask_qty.raw());
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
                    for value in [
                        position.unrealized_pnl,
                        position.initial_margin,
                        position.maintenance_margin,
                    ] {
                        // 与账户快照的 state_hash 同一条纪律：未报与报为零必须是两份内容，
                        // 否则把日志里的 `null` 改成 `0` 不会改动摘要。
                        h.write_u64(u64::from(value.is_some()));
                        h.write_i128(value.map(Money::raw).unwrap_or_default());
                    }
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
                    LedgerEntryKind::RightsEntitlement => 10,
                    LedgerEntryKind::CashDividendEntitlement => 11,
                    LedgerEntryKind::ConvertibleBondInterestEntitlement => 12,
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

    #[test]
    fn metadata_is_part_of_event_identity_and_round_trips() {
        let event = Event::new(1, 20, Priority::MARKET, EventKind::Settle)
            .received_at(30)
            .with_metadata(EventMetadata {
                source_id: "okx-market".into(),
                dedup_key: "okx:quote:7".into(),
                rule_version: "market-v2".into(),
                ..EventMetadata::default()
            });
        assert_eq!(event.effective_at(), 20);
        assert_eq!(event.receive_time, 30);
        let restored: Event =
            serde_json::from_str(&serde_json::to_string(&event).unwrap()).unwrap();
        assert_eq!(restored, event);

        let mut legacy = serde_json::to_value(&event).unwrap();
        legacy.as_object_mut().unwrap().remove("metadata");
        let restored_legacy: Event = serde_json::from_value(legacy).unwrap();
        assert_eq!(restored_legacy.metadata, EventMetadata::default());

        let mut first = crate::sourcing::Fnv1a::new();
        event.digest(&mut first);
        let changed = event
            .clone()
            .with_metadata(event.metadata.derived("ledger:1"));
        let mut second = crate::sourcing::Fnv1a::new();
        changed.digest(&mut second);
        assert_ne!(first.finish(), second.finish());
    }

    /// 交易所"没报这一项"和"报了且为零"是两份不同的内容。把两者哈希成同一个摘要，
    /// 等于让日志里把 `null` 悄悄改成 `0` 也能通过重放校验（V11 Q68）。
    #[test]
    fn unreported_position_money_is_not_hashed_as_zero() {
        let position = |pnl: Option<Money>| AccountPositionSnapshot {
            instrument: InstrumentId::parse("BTC/USDT:USDT.OKX").unwrap(),
            quantity: Quantity::from_i64(2),
            average_price: None,
            mark_price: None,
            liquidation_price: None,
            unrealized_pnl: pnl,
            initial_margin: None,
            maintenance_margin: None,
            leverage: None,
            margin_mode: None,
            position_side: None,
        };
        let digest_of = |pnl: Option<Money>| {
            let event = Event::new(
                1,
                1,
                Priority::APPLY,
                EventKind::AccountPositionSnapshot {
                    account_id: "main".into(),
                    venue_id: "OKX".into(),
                    positions: vec![position(pnl)],
                },
            );
            let mut hasher = crate::sourcing::Fnv1a::new();
            event.digest(&mut hasher);
            hasher.finish()
        };
        assert_ne!(digest_of(None), digest_of(Some(Money::ZERO)));

        let reported: AccountPositionSnapshot =
            serde_json::from_value(serde_json::to_value(position(Some(Money::ZERO))).unwrap())
                .unwrap();
        assert_eq!(reported, position(Some(Money::ZERO)));
        // 老日志没有这三个键：读出来必须是"没报"，而不是补一个伪造的零。
        let mut legacy = serde_json::to_value(position(Some(Money::ZERO))).unwrap();
        for key in ["unrealized_pnl", "initial_margin", "maintenance_margin"] {
            assert!(
                legacy
                    .as_object_mut()
                    .expect("持仓观察值是 JSON 对象")
                    .remove(key)
                    .is_some(),
                "键 {key} 必须真的写进过线格式，否则这条用例没在测缺席回读"
            );
        }
        let legacy: AccountPositionSnapshot = serde_json::from_value(legacy).unwrap();
        assert_eq!(
            (
                legacy.unrealized_pnl,
                legacy.initial_margin,
                legacy.maintenance_margin
            ),
            (None, None, None)
        );
    }
}
