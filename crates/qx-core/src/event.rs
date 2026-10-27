//! 事件模型：因果优先级是回测真实性的核心。
//!
//! **同时间戳不是任意排序**，而是因果优先级。若把"策略决策"排到"行情到达"之前，
//! 就等价于偷看未来——这是最常见也最隐蔽的前视偏差。

use crate::clock::Ts;
use crate::identity::InstrumentId;
use crate::ledger::{LedgerEntry, LedgerEntryKind};
use crate::numeric::{DecimalRounding, ExactDecimal};
use crate::numeric::{Money, Price, Quantity};
use crate::order::{Fill, Side};
use crate::trading::OrderPolicy;
use serde::{Deserialize, Serialize};

/// Exchange-precision OHLCV retained beside the legacy 1e-9 Bar projection.
/// The exact values are authoritative when present; raw fields remain for older
/// strategy and Arrow consumers and must equal the checked 1e-9 projection.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExactOhlcv {
    pub open: ExactDecimal,
    pub high: ExactDecimal,
    pub low: ExactDecimal,
    pub close: ExactDecimal,
    pub volume: ExactDecimal,
}

/// One exact depth level stored in the market EventLog. Raw values remain the
/// toward-zero 1e-9 projection for legacy readers; exact values are authoritative.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarketDepthLevel {
    pub price: Price,
    pub quantity: Quantity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price_exact: Option<ExactDecimal>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quantity_exact: Option<ExactDecimal>,
}

impl MarketDepthLevel {
    pub fn validate(&self) -> Result<(), String> {
        match (self.price_exact, self.quantity_exact) {
            (Some(price), Some(quantity)) => {
                if !price.is_positive() || !quantity.is_positive() {
                    return Err("盘口精确价格和数量必须为正".into());
                }
                let project = |value: ExactDecimal| {
                    value
                        .checked_mul_at_scale(ExactDecimal::ONE, 9, DecimalRounding::TowardZero)
                        .and_then(|scaled| scaled.to_raw(9).ok())
                };
                if project(price) != Some(self.price.raw())
                    || project(quantity) != Some(self.quantity.raw())
                {
                    return Err("盘口精确值与 raw 投影不一致".into());
                }
            }
            (None, None) if self.price.raw() > 0 && self.quantity.raw() > 0 => {}
            (None, None) => return Err("盘口价格和数量必须为正".into()),
            _ => return Err("盘口精确价格和数量必须同时提供".into()),
        }
        Ok(())
    }

    pub fn exact_price(&self) -> Result<ExactDecimal, String> {
        match self.price_exact {
            Some(price) => Ok(price),
            None => ExactDecimal::from_parts(self.price.raw(), 9)
                .map_err(|error| format!("盘口 legacy 价格无法转换为精确数值: {error}")),
        }
    }
}

impl ExactOhlcv {
    /// Project each source value onto the legacy 1e-9 columns, truncating toward zero.
    /// The exact sidecar remains authoritative; projection only keeps old consumers usable.
    pub fn raw_projection(&self) -> Result<[i128; 5], String> {
        let project = |value: ExactDecimal| {
            value
                .checked_mul_at_scale(ExactDecimal::ONE, 9, DecimalRounding::TowardZero)
                .and_then(|projected| projected.to_raw(9).ok())
                .ok_or_else(|| "ExactOhlcv raw projection is out of range".to_string())
        };
        Ok([
            project(self.open)?,
            project(self.high)?,
            project(self.low)?,
            project(self.close)?,
            project(self.volume)?,
        ])
    }

    pub fn validate(&self) -> Result<(), String> {
        if !self.open.is_positive()
            || !self.high.is_positive()
            || !self.low.is_positive()
            || !self.close.is_positive()
            || self.volume.is_negative()
        {
            return Err("ExactOhlcv prices must be positive and volume non-negative".into());
        }
        if self
            .high
            .checked_cmp(self.low)
            .is_none_or(|ordering| ordering.is_lt())
            || self
                .high
                .checked_cmp(self.open)
                .is_none_or(|ordering| ordering.is_lt())
            || self
                .high
                .checked_cmp(self.close)
                .is_none_or(|ordering| ordering.is_lt())
            || self
                .low
                .checked_cmp(self.open)
                .is_none_or(|ordering| ordering.is_gt())
            || self
                .low
                .checked_cmp(self.close)
                .is_none_or(|ordering| ordering.is_gt())
        {
            return Err("ExactOhlcv high/low range does not contain open and close".into());
        }
        self.raw_projection()?;
        Ok(())
    }
}

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

/// Scale-aware values are appended only when present so legacy event digests remain unchanged.
fn digest_exact_decimal(h: &mut crate::sourcing::Fnv1a, value: ExactDecimal) {
    h.write_text(&value.coefficient().to_string());
    h.write_u64(u64::from(value.scale()));
}

fn digest_optional_exact_decimal(h: &mut crate::sourcing::Fnv1a, value: Option<ExactDecimal>) {
    h.write_u64(u64::from(value.is_some()));
    if let Some(value) = value {
        digest_exact_decimal(h, value);
    }
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
    /// 保证金/借贷账户的负债；用于可用余额视图和旧版净现金回退。
    #[serde(default)]
    pub borrowed: Money,
    /// Exchange-reported net wallet cash in this asset, excluding derivative unrealized PnL.
    /// `None` keeps the legacy spot formula; derivative adapters must populate this only when
    /// the venue response exposes wallet cash independently from equity/UPnL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub net_cash_balance: Option<Money>,
    /// Scale-aware exchange balance facts. Legacy money fields remain the 1e9 projection.
    /// This sidecar is emitted only when the source supplied exact decimal values.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exact_amounts: Option<ExactAccountBalanceAmounts>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ExactAccountBalanceAmounts {
    pub free: ExactDecimal,
    pub locked: ExactDecimal,
    pub borrowed: ExactDecimal,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub net_cash_balance: Option<ExactDecimal>,
}

impl AccountBalance {
    /// 净现金口径的**唯一**实现：显式 venue 钱包现金优先，否则回退旧公式
    /// `free + locked - borrowed`。
    ///
    /// 上层（对账、余额差异判定、协议线格式）必须经由本方法得到净现金，不得再
    /// 各自抄写这条公式——历史上 `qx-runtime` 与 `qx-cli` worker 两侧抄写时
    /// 已经出现口径分叉（V10 §4.7）。溢出返回 `None`，由调用方决定 fail-closed。
    pub fn net_cash_raw(&self) -> Option<i128> {
        if let Some(net_cash) = self.net_cash_balance {
            return Some(net_cash.raw());
        }
        self.free
            .raw()
            .checked_add(self.locked.raw())?
            .checked_sub(self.borrowed.raw())
    }

    /// Exact wallet cash from an exchange snapshot, falling back to the legacy raw facts.
    pub fn net_cash_exact(&self) -> Option<ExactDecimal> {
        if let Some(exact) = self.exact_amounts {
            return exact.net_cash_balance.or_else(|| {
                exact
                    .free
                    .checked_add(exact.locked)?
                    .checked_sub(exact.borrowed)
            });
        }
        if let Some(net_cash) = self.net_cash_balance {
            return ExactDecimal::from_parts(net_cash.raw(), 9).ok();
        }
        let free = ExactDecimal::from_parts(self.free.raw(), 9).ok()?;
        let locked = ExactDecimal::from_parts(self.locked.raw(), 9).ok()?;
        let borrowed = ExactDecimal::from_parts(self.borrowed.raw(), 9).ok()?;
        free.checked_add(locked)?.checked_sub(borrowed)
    }
}

/// 交易所返回的持仓观察快照。它用于实盘恢复、保证金/风险对账和查询，
/// 不会绕过成交事实直接修改 Ledger；Ledger 仍只由 Fill/Settlement 等事实驱动。
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct AccountPositionSnapshot {
    pub instrument: InstrumentId,
    /// Settlement/PNL currency from the venue market metadata. Required before a portfolio
    /// can aggregate derivative unrealized PnL across instruments without mixing currencies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settlement_currency: Option<String>,
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
    /// Exact exchange-reported amounts corresponding to the 1e9 compatibility projection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exact_amounts: Option<ExactAccountPositionAmounts>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ExactAccountPositionAmounts {
    pub quantity: ExactDecimal,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub average_price: Option<ExactDecimal>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mark_price: Option<ExactDecimal>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub liquidation_price: Option<ExactDecimal>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unrealized_pnl: Option<ExactDecimal>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_margin: Option<ExactDecimal>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maintenance_margin: Option<ExactDecimal>,
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

/// The strategy output that crossed the backtest adapter boundary.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct StrategyIntentFact {
    pub intent_id: u64,
    pub instrument: InstrumentId,
    pub side: Side,
    pub qty: Quantity,
    pub limit: Option<Price>,
    /// Scale-aware quantity when the strategy emitted an exact decimal intent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qty_exact: Option<ExactDecimal>,
    /// Scale-aware limit price when the strategy emitted an exact decimal intent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit_exact: Option<ExactDecimal>,
    pub policy: Option<OrderPolicy>,
    pub reduce_only: bool,
    pub post_only: bool,
}

/// An immutable strategy decision recorded before order admission.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct StrategyDecisionFact {
    pub schema_version: u32,
    pub request_id: String,
    pub strategy_id: String,
    pub signal_id: u64,
    pub confidence: i128,
    pub priority: i32,
    pub expires_at: u64,
    pub intents: Vec<StrategyIntentFact>,
}

/// The exact canonical risk decision evaluated for an order in a backtest.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct OrderRiskDecisionFact {
    pub client_order_id: u64,
    pub instrument: InstrumentId,
    pub strategy_id: Option<String>,
    pub signal_id: Option<u64>,
    pub intent_id: Option<u64>,
    pub allowed: bool,
    pub violations: Vec<String>,
    pub projected_position_raw: Option<i128>,
    pub projected_margin_raw: Option<i128>,
    pub reference_price_raw: Option<i128>,
    pub rule_set_version: String,
}

/// Immutable execution capability preflight evidence bound to one exact order.
/// `preflight_json` is the versioned app-layer request and decision snapshot; its hash is included
/// here together with the order/catalog fingerprints so replay can detect substitution.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ExecutionCapabilityDecisionFact {
    pub schema_version: u32,
    pub kind: ExecutionCapabilityDecisionKind,
    pub decision_id: String,
    pub command_id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order_group_id: Option<String>,
    pub client_order_id: u64,
    pub account_id: String,
    pub venue_id: String,
    pub order_fingerprint: String,
    pub catalog_fingerprint: String,
    pub market_rule_fingerprint: String,
    pub preflight_json: String,
}

pub const EXECUTION_CAPABILITY_DECISION_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionCapabilityDecisionKind {
    Submit,
    RecoveryAdoption,
}

/// 事件类型。
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum EventKind {
    /// L1 报价到达；保存最优买卖价及对应数量，供 Paper/回放保持成交容量一致。
    MarketQuote {
        instrument: InstrumentId,
        bid: Price,
        ask: Price,
        #[serde(default)]
        bid_qty: Quantity,
        #[serde(default)]
        ask_qty: Quantity,
        /// Exchange-native quote precision; absent for historical/legacy market facts.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bid_exact: Option<ExactDecimal>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ask_exact: Option<ExactDecimal>,
        /// Exchange-native displayed quantities; absent on legacy and historical quotes.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bid_qty_exact: Option<ExactDecimal>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ask_qty_exact: Option<ExactDecimal>,
    },
    /// Sequenced L2 snapshot from the market-data source. The arrays retain exact
    /// source precision and are replayable as a durable strategy input.
    MarketOrderBook {
        instrument: InstrumentId,
        sequence: u64,
        bids: Vec<MarketDepthLevel>,
        asks: Vec<MarketDepthLevel>,
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
    StrategyDecision {
        decision: StrategyDecisionFact,
    },
    OrderRiskDecision {
        decision: OrderRiskDecisionFact,
    },
    ExecutionCapabilityDecision {
        decision: ExecutionCapabilityDecisionFact,
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
    /// Durable cancel request intent. If the process exits while the venue call is in flight,
    /// restart recovery sees CancelPending and reconciles instead of treating the order as idle.
    CancelPending {
        client_order_id: u64,
    },
    Cancelled {
        client_order_id: u64,
    },
    Filled {
        fill: Fill,
    },
    /// A venue fee/rebate adjustment discovered after its fill quantity was already applied.
    FeeAdjustment {
        account_id: String,
        venue_id: String,
        order_id: u64,
        instrument: InstrumentId,
        fees: Vec<crate::order::FillFee>,
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
            EventKind::MarketQuote {
                instrument,
                bid,
                ask,
                bid_qty,
                ask_qty,
                bid_exact,
                ask_exact,
                bid_qty_exact,
                ask_qty_exact,
            } => {
                h.write_u64(7);
                h.write_text(&instrument.to_string());
                h.write_i128(bid.raw());
                h.write_i128(ask.raw());
                h.write_i128(bid_qty.raw());
                h.write_i128(ask_qty.raw());
                if bid_exact.is_some() || ask_exact.is_some() {
                    h.write_u64(0x4558_4143_545f_5154);
                    digest_optional_exact_decimal(h, *bid_exact);
                    digest_optional_exact_decimal(h, *ask_exact);
                }
                if bid_qty_exact.is_some() || ask_qty_exact.is_some() {
                    h.write_u64(0x4558_4143_545f_5153);
                    digest_optional_exact_decimal(h, *bid_qty_exact);
                    digest_optional_exact_decimal(h, *ask_qty_exact);
                }
            }
            EventKind::MarketOrderBook {
                instrument,
                sequence,
                bids,
                asks,
            } => {
                h.write_u64(22);
                h.write_text(&instrument.to_string());
                h.write_u64(*sequence);
                for levels in [bids, asks] {
                    h.write_u64(levels.len() as u64);
                    for level in levels {
                        h.write_i128(level.price.raw());
                        h.write_i128(level.quantity.raw());
                        digest_optional_exact_decimal(h, level.price_exact);
                        digest_optional_exact_decimal(h, level.quantity_exact);
                    }
                }
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
                    if let Some(net_cash) = balance.net_cash_balance {
                        h.write_u64(1);
                        h.write_i128(net_cash.raw());
                    }
                    if let Some(exact) = balance.exact_amounts {
                        h.write_u64(0x4558_4143_545f_4241);
                        digest_exact_decimal(h, exact.free);
                        digest_exact_decimal(h, exact.locked);
                        digest_exact_decimal(h, exact.borrowed);
                        digest_optional_exact_decimal(h, exact.net_cash_balance);
                    }
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
                    if let Some(currency) = position.settlement_currency.as_deref() {
                        h.write_u64(1);
                        h.write_text(currency);
                    }
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
                    if let Some(exact) = position.exact_amounts {
                        h.write_u64(0x4558_4143_545f_504f);
                        digest_exact_decimal(h, exact.quantity);
                        digest_optional_exact_decimal(h, exact.average_price);
                        digest_optional_exact_decimal(h, exact.mark_price);
                        digest_optional_exact_decimal(h, exact.liquidation_price);
                        digest_optional_exact_decimal(h, exact.unrealized_pnl);
                        digest_optional_exact_decimal(h, exact.initial_margin);
                        digest_optional_exact_decimal(h, exact.maintenance_margin);
                    }
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
                if let Some(exact) = order.exact_amounts {
                    h.write_u64(0x4558_4143_545f_4f52);
                    digest_exact_decimal(h, exact.quantity);
                    h.write_u64(u64::from(exact.limit_price.is_some()));
                    if let Some(price) = exact.limit_price {
                        digest_exact_decimal(h, price);
                    }
                    digest_exact_decimal(h, exact.filled);
                }
            }
            EventKind::StrategyDecision { decision } => {
                h.write_u64(17);
                h.write_u64(decision.schema_version as u64);
                h.write_text(&decision.request_id);
                h.write_text(&decision.strategy_id);
                h.write_u64(decision.signal_id);
                h.write_i128(decision.confidence);
                h.write_u64(decision.priority as i64 as u64);
                h.write_u64(decision.expires_at);
                h.write_u64(decision.intents.len() as u64);
                for intent in &decision.intents {
                    h.write_u64(intent.intent_id);
                    h.write_text(&intent.instrument.to_string());
                    h.write_u64(match intent.side {
                        Side::Buy => 1,
                        Side::Sell => 2,
                    });
                    h.write_i128(intent.qty.raw());
                    h.write_u64(u64::from(intent.limit.is_some()));
                    h.write_i128(intent.limit.map(Price::raw).unwrap_or_default());
                    if intent.qty_exact.is_some() || intent.limit_exact.is_some() {
                        h.write_u64(0x4558_4143_545f_5349);
                        digest_optional_exact_decimal(h, intent.qty_exact);
                        digest_optional_exact_decimal(h, intent.limit_exact);
                    }
                    h.write_u64(u64::from(intent.policy.is_some()));
                    if let Some(policy) = intent.policy {
                        h.write_u64(u64::from(policy.reduce_only));
                        h.write_u64(match policy.position_side {
                            crate::trading::PositionSide::Long => 1,
                            crate::trading::PositionSide::Short => 2,
                            crate::trading::PositionSide::Net => 3,
                        });
                        h.write_u64(match policy.margin_mode {
                            crate::trading::MarginMode::Cash => 1,
                            crate::trading::MarginMode::Cross => 2,
                            crate::trading::MarginMode::Isolated => 3,
                        });
                        h.write_u64(match policy.position_mode {
                            crate::trading::PositionMode::OneWay => 1,
                            crate::trading::PositionMode::Hedge => 2,
                        });
                        h.write_u64(policy.leverage as u64);
                        h.write_u64(u64::from(policy.post_only));
                    }
                    h.write_u64(u64::from(intent.reduce_only));
                    h.write_u64(u64::from(intent.post_only));
                }
            }
            EventKind::OrderRiskDecision { decision } => {
                h.write_u64(18);
                h.write_u64(decision.client_order_id);
                h.write_text(&decision.instrument.to_string());
                h.write_u64(u64::from(decision.strategy_id.is_some()));
                h.write_text(decision.strategy_id.as_deref().unwrap_or_default());
                h.write_u64(u64::from(decision.signal_id.is_some()));
                h.write_u64(decision.signal_id.unwrap_or_default());
                h.write_u64(u64::from(decision.intent_id.is_some()));
                h.write_u64(decision.intent_id.unwrap_or_default());
                h.write_u64(u64::from(decision.allowed));
                h.write_u64(decision.violations.len() as u64);
                for violation in &decision.violations {
                    h.write_text(violation);
                }
                for value in [
                    decision.projected_position_raw,
                    decision.projected_margin_raw,
                    decision.reference_price_raw,
                ] {
                    h.write_u64(u64::from(value.is_some()));
                    h.write_i128(value.unwrap_or_default());
                }
                h.write_text(&decision.rule_set_version);
            }
            EventKind::ExecutionCapabilityDecision { decision } => {
                h.write_u64(20);
                h.write_u64(decision.schema_version as u64);
                h.write_u64(match decision.kind {
                    ExecutionCapabilityDecisionKind::Submit => 1,
                    ExecutionCapabilityDecisionKind::RecoveryAdoption => 2,
                });
                h.write_text(&decision.decision_id);
                h.write_u64(u64::from(decision.command_id.is_some()));
                h.write_u64(decision.command_id.unwrap_or_default());
                h.write_u64(u64::from(decision.order_group_id.is_some()));
                h.write_text(decision.order_group_id.as_deref().unwrap_or_default());
                h.write_u64(decision.client_order_id);
                h.write_text(&decision.account_id);
                h.write_text(&decision.venue_id);
                h.write_text(&decision.order_fingerprint);
                h.write_text(&decision.catalog_fingerprint);
                h.write_text(&decision.market_rule_fingerprint);
                h.write_text(&decision.preflight_json);
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
            EventKind::CancelPending { client_order_id } => {
                h.write_u64(21);
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
                // Keep the digest of legacy scalar-only Fill facts stable. Explicit fee rows are
                // sorted before hashing so equivalent venue arrays cannot create replay drift.
                if !fill.fees.is_empty() {
                    let mut fees = fill.fees.clone();
                    fees.sort_by(|left, right| {
                        left.currency
                            .as_deref()
                            .unwrap_or_default()
                            .cmp(right.currency.as_deref().unwrap_or_default())
                            .then_with(|| left.amount.raw().cmp(&right.amount.raw()))
                    });
                    h.write_u64(1);
                    h.write_u64(fees.len() as u64);
                    for fee in fees {
                        h.write_i128(fee.amount.raw());
                        h.write_text(fee.currency.as_deref().unwrap_or_default());
                        if !fee.source_trade_ids.is_empty() {
                            let mut trade_ids = fee.source_trade_ids;
                            trade_ids.sort();
                            h.write_u64(2);
                            h.write_u64(trade_ids.len() as u64);
                            for trade_id in trade_ids {
                                h.write_text(&trade_id);
                            }
                        }
                    }
                }
                if let Some(exact) = fill.exact_amounts {
                    h.write_u64(0x4558_4143_545f_4649);
                    digest_exact_decimal(h, exact.quantity);
                    digest_exact_decimal(h, exact.price);
                    digest_optional_exact_decimal(h, exact.quote_cost);
                    h.write_u64(u64::from(exact.quote_budget_complete));
                }
                if !fill.exact_fees.is_empty() {
                    let mut fees = fill.exact_fees.clone();
                    fees.sort_by(|left, right| {
                        left.currency
                            .as_deref()
                            .unwrap_or_default()
                            .cmp(right.currency.as_deref().unwrap_or_default())
                            .then_with(|| left.amount.to_string().cmp(&right.amount.to_string()))
                    });
                    h.write_u64(0x4558_4143_545f_4645);
                    h.write_u64(fees.len() as u64);
                    for fee in fees {
                        digest_exact_decimal(h, fee.amount);
                        h.write_text(fee.currency.as_deref().unwrap_or_default());
                        let mut trade_ids = fee.source_trade_ids;
                        trade_ids.sort();
                        h.write_u64(trade_ids.len() as u64);
                        for trade_id in trade_ids {
                            h.write_text(&trade_id);
                        }
                    }
                }
            }
            EventKind::FeeAdjustment {
                account_id,
                venue_id,
                order_id,
                instrument,
                fees,
            } => {
                h.write_u64(19);
                h.write_text(account_id);
                h.write_text(venue_id);
                h.write_u64(*order_id);
                h.write_text(&instrument.to_string());
                let mut fees = fees.clone();
                fees.sort_by(|left, right| {
                    left.currency
                        .as_deref()
                        .unwrap_or_default()
                        .cmp(right.currency.as_deref().unwrap_or_default())
                        .then_with(|| left.amount.raw().cmp(&right.amount.raw()))
                });
                h.write_u64(fees.len() as u64);
                for fee in fees {
                    h.write_i128(fee.amount.raw());
                    h.write_text(fee.currency.as_deref().unwrap_or_default());
                    if !fee.source_trade_ids.is_empty() {
                        let mut trade_ids = fee.source_trade_ids;
                        trade_ids.sort();
                        h.write_u64(2);
                        h.write_u64(trade_ids.len() as u64);
                        for trade_id in trade_ids {
                            h.write_text(&trade_id);
                        }
                    }
                }
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
                // Preserve pre-leverage event hashes when the optional fact is absent.
                // Once present, bind the opening policy to the replay digest.
                if let Some(leverage) = entry.leverage {
                    h.write_u64(1);
                    h.write_u64(u64::from(leverage));
                }
                if let Some(exact) = entry.exact_amounts {
                    h.write_u64(0x4558_4143_545f_4c45);
                    digest_exact_decimal(h, exact.amount);
                    digest_exact_decimal(h, exact.quantity);
                    h.write_u64(u64::from(exact.price.is_some()));
                    if let Some(price) = exact.price {
                        digest_exact_decimal(h, price);
                    }
                    if let Some(contract_size) = exact.contract_size {
                        h.write_u64(0x4558_4143_545f_4353);
                        digest_exact_decimal(h, contract_size);
                    }
                    if let Some(inverse) = exact.inverse {
                        h.write_u64(0x4558_4143_545f_494e);
                        h.write_u64(u64::from(inverse));
                    }
                }
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
    use crate::{ExactFillAmounts, ExactOrderAmounts, InstrumentId, Order, OrderStatus};

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
        let left = Event::new(
            1,
            1,
            Priority::TIMER,
            EventKind::Rejected {
                client_order_id: 7,
                reason: "c".into(),
            },
        )
        .correlated("ab");
        let right = Event::new(
            1,
            1,
            Priority::TIMER,
            EventKind::Rejected {
                client_order_id: 7,
                reason: "bc".into(),
            },
        )
        .correlated("a");
        let mut left_hash = crate::sourcing::Fnv1a::new();
        let mut right_hash = crate::sourcing::Fnv1a::new();
        left.digest(&mut left_hash);
        right.digest(&mut right_hash);
        assert_ne!(left_hash.finish(), right_hash.finish());
    }

    #[test]
    fn market_quote_digest_binds_exact_displayed_sizes_only_when_present() {
        let instrument = InstrumentId::parse("TINYUSDT.OKX").unwrap();
        let legacy = Event::new(
            1,
            10,
            Priority::MARKET,
            EventKind::MarketQuote {
                instrument: instrument.clone(),
                bid: Price::from_i64(1),
                ask: Price::from_i64(2),
                bid_qty: Quantity::ZERO,
                ask_qty: Quantity::ZERO,
                bid_exact: None,
                ask_exact: None,
                bid_qty_exact: None,
                ask_qty_exact: None,
            },
        );
        let exact = Event::new(
            1,
            10,
            Priority::MARKET,
            EventKind::MarketQuote {
                instrument,
                bid: Price::from_i64(1),
                ask: Price::from_i64(2),
                bid_qty: Quantity::ZERO,
                ask_qty: Quantity::ZERO,
                bid_exact: None,
                ask_exact: None,
                bid_qty_exact: Some(ExactDecimal::parse("0.0000000001").unwrap()),
                ask_qty_exact: Some(ExactDecimal::parse("0.0000000002").unwrap()),
            },
        );
        let digest = |event: &Event| {
            let mut hash = crate::sourcing::Fnv1a::new();
            event.digest(&mut hash);
            hash.finish()
        };
        assert_ne!(digest(&legacy), digest(&exact));
        let restored: Event = serde_json::from_str(&serde_json::to_string(&exact).unwrap())
            .expect("exact displayed sizes round-trip through EventLog JSON");
        assert_eq!(restored, exact);
    }

    #[test]
    fn strategy_and_risk_decision_fields_are_bound_to_replay_digest() {
        let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
        let strategy_event = Event::new(
            1,
            20,
            Priority::COMMAND,
            EventKind::StrategyDecision {
                decision: StrategyDecisionFact {
                    schema_version: 1,
                    request_id: "request-1".into(),
                    strategy_id: "strategy-1".into(),
                    signal_id: 7,
                    confidence: 9_500,
                    priority: -2,
                    expires_at: 25,
                    intents: vec![StrategyIntentFact {
                        intent_id: 8,
                        instrument,
                        side: Side::Buy,
                        qty: Quantity::from_i64(2),
                        limit: None,
                        qty_exact: None,
                        limit_exact: None,
                        policy: None,
                        reduce_only: false,
                        post_only: false,
                    }],
                },
            },
        );
        let changed_confidence = match &strategy_event.kind {
            EventKind::StrategyDecision { decision } => {
                let mut decision = decision.clone();
                decision.confidence += 1;
                Event::new(
                    strategy_event.seq,
                    strategy_event.ts,
                    strategy_event.prio,
                    EventKind::StrategyDecision { decision },
                )
            }
            _ => unreachable!(),
        };
        let exact_quantity = match &strategy_event.kind {
            EventKind::StrategyDecision { decision } => {
                let mut decision = decision.clone();
                decision.intents[0].qty_exact = Some(ExactDecimal::parse("0.0000000001").unwrap());
                Event::new(
                    strategy_event.seq,
                    strategy_event.ts,
                    strategy_event.prio,
                    EventKind::StrategyDecision { decision },
                )
            }
            _ => unreachable!(),
        };
        let risk_event = Event::new(
            2,
            20,
            Priority::COMMAND,
            EventKind::OrderRiskDecision {
                decision: OrderRiskDecisionFact {
                    client_order_id: 9,
                    instrument: InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
                    strategy_id: Some("strategy-1".into()),
                    signal_id: Some(7),
                    intent_id: Some(8),
                    allowed: true,
                    violations: Vec::new(),
                    projected_position_raw: Some(0),
                    projected_margin_raw: None,
                    reference_price_raw: Some(100),
                    rule_set_version: "risk-v1".into(),
                },
            },
        );
        let changed_projection = match &risk_event.kind {
            EventKind::OrderRiskDecision { decision } => {
                let mut decision = decision.clone();
                decision.projected_margin_raw = Some(0);
                Event::new(
                    risk_event.seq,
                    risk_event.ts,
                    risk_event.prio,
                    EventKind::OrderRiskDecision { decision },
                )
            }
            _ => unreachable!(),
        };
        let changed_instrument = match &risk_event.kind {
            EventKind::OrderRiskDecision { decision } => {
                let mut decision = decision.clone();
                decision.instrument = InstrumentId::parse("ETHUSDT.BINANCE").unwrap();
                Event::new(
                    risk_event.seq,
                    risk_event.ts,
                    risk_event.prio,
                    EventKind::OrderRiskDecision { decision },
                )
            }
            _ => unreachable!(),
        };
        let digest = |event: &Event| {
            let mut hash = crate::sourcing::Fnv1a::new();
            event.digest(&mut hash);
            hash.finish()
        };
        assert_ne!(digest(&strategy_event), digest(&changed_confidence));
        assert_ne!(digest(&strategy_event), digest(&exact_quantity));
        assert_ne!(digest(&risk_event), digest(&changed_projection));
        assert_ne!(digest(&risk_event), digest(&changed_instrument));
        let restored: Event = serde_json::from_str(&serde_json::to_string(&risk_event).unwrap())
            .expect("risk decision fact round-trip");
        assert_eq!(restored, risk_event);
    }

    #[test]
    fn ledger_event_digest_includes_persisted_opening_leverage() {
        let mut entry = LedgerEntry {
            id: 1,
            account_id: "main".into(),
            currency: "USDT".into(),
            kind: LedgerEntryKind::TradePosition,
            amount: Money::ZERO,
            instrument: Some(InstrumentId::parse("BTCUSDT.BINANCE").unwrap()),
            quantity: Quantity::from_i64(1),
            price: Some(Price::from_i64(100)),
            order_id: Some(7),
            ts: 1,
            multiplier: 1,
            position_side: None,
            leverage: None,
            exact_amounts: None,
        };
        let legacy = Event::new(
            1,
            1,
            Priority::APPLY,
            EventKind::LedgerApplied {
                entry: entry.clone(),
            },
        );
        entry.leverage = Some(10);
        let leveraged = Event::new(1, 1, Priority::APPLY, EventKind::LedgerApplied { entry });
        let digest = |event: &Event| {
            let mut hash = crate::sourcing::Fnv1a::new();
            event.digest(&mut hash);
            hash.finish()
        };
        assert_ne!(digest(&legacy), digest(&leveraged));
    }

    #[test]
    fn exact_ledger_amounts_are_included_in_event_identity() {
        let mut entry = LedgerEntry {
            id: 1,
            account_id: "main".into(),
            currency: "USDT".into(),
            kind: LedgerEntryKind::TradeCash,
            amount: Money::ZERO,
            instrument: Some(InstrumentId::parse("TINY/USDT.BINANCE").unwrap()),
            quantity: Quantity::ZERO,
            price: None,
            order_id: Some(7),
            ts: 1,
            multiplier: 1,
            position_side: None,
            leverage: None,
            exact_amounts: None,
        };
        let legacy = Event::new(
            1,
            1,
            Priority::APPLY,
            EventKind::LedgerApplied {
                entry: entry.clone(),
            },
        );
        entry.exact_amounts = Some(crate::ExactLedgerAmounts {
            amount: ExactDecimal::parse("-0.000000000000000001").unwrap(),
            quantity: ExactDecimal::ZERO,
            price: None,
            contract_size: None,
            inverse: None,
        });
        let exact = Event::new(
            1,
            1,
            Priority::APPLY,
            EventKind::LedgerApplied {
                entry: entry.clone(),
            },
        );
        let digest = |event: &Event| {
            let mut hash = crate::sourcing::Fnv1a::new();
            event.digest(&mut hash);
            hash.finish()
        };
        assert_ne!(digest(&legacy), digest(&exact));
        let restored: Event =
            serde_json::from_str(&serde_json::to_string(&exact).unwrap()).unwrap();
        assert_eq!(restored, exact);

        let mut derivative_entry = entry;
        derivative_entry.kind = LedgerEntryKind::TradePosition;
        let derivative_amounts = derivative_entry
            .exact_amounts
            .as_mut()
            .expect("exact amount payload");
        derivative_amounts.amount = ExactDecimal::ZERO;
        derivative_amounts.quantity = ExactDecimal::parse("1").unwrap();
        derivative_amounts.price = Some(ExactDecimal::parse("10000").unwrap());
        derivative_amounts.contract_size = Some(ExactDecimal::parse("0.001").unwrap());
        derivative_amounts.inverse = Some(false);
        let derivative = Event::new(
            1,
            1,
            Priority::APPLY,
            EventKind::LedgerApplied {
                entry: derivative_entry,
            },
        );
        assert_ne!(digest(&exact), digest(&derivative));
        let restored: Event =
            serde_json::from_str(&serde_json::to_string(&derivative).unwrap()).unwrap();
        assert_eq!(restored, derivative);
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
            settlement_currency: None,
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
            exact_amounts: None,
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

    #[test]
    fn exact_account_balance_and_position_amounts_are_part_of_event_identity() {
        let digest = |event: &Event| {
            let mut hash = crate::sourcing::Fnv1a::new();
            event.digest(&mut hash);
            hash.finish()
        };
        let balance_event = |amount: ExactDecimal| {
            Event::new(
                1,
                1,
                Priority::APPLY,
                EventKind::AccountBalanceSnapshot {
                    account_id: "main".into(),
                    venue_id: "BINANCE".into(),
                    balances: vec![AccountBalance {
                        asset: "PEPE".into(),
                        free: Money::ZERO,
                        locked: Money::ZERO,
                        borrowed: Money::ZERO,
                        net_cash_balance: None,
                        exact_amounts: Some(ExactAccountBalanceAmounts {
                            free: amount,
                            locked: ExactDecimal::ZERO,
                            borrowed: ExactDecimal::ZERO,
                            net_cash_balance: None,
                        }),
                    }],
                },
            )
        };
        let balance_a = balance_event(ExactDecimal::parse("0.0000000001").unwrap());
        let balance_b = balance_event(ExactDecimal::parse("0.0000000002").unwrap());
        assert_ne!(digest(&balance_a), digest(&balance_b));

        let position_event = |quantity: ExactDecimal| {
            Event::new(
                1,
                1,
                Priority::APPLY,
                EventKind::AccountPositionSnapshot {
                    account_id: "main".into(),
                    venue_id: "BINANCE".into(),
                    positions: vec![AccountPositionSnapshot {
                        instrument: InstrumentId::parse("PEPEUSDT.BINANCE").unwrap(),
                        settlement_currency: Some("USDT".into()),
                        quantity: Quantity::ZERO,
                        average_price: None,
                        mark_price: None,
                        liquidation_price: None,
                        unrealized_pnl: None,
                        initial_margin: None,
                        maintenance_margin: None,
                        leverage: None,
                        margin_mode: None,
                        position_side: Some("short".into()),
                        exact_amounts: Some(ExactAccountPositionAmounts {
                            quantity,
                            average_price: None,
                            mark_price: None,
                            liquidation_price: None,
                            unrealized_pnl: None,
                            initial_margin: None,
                            maintenance_margin: None,
                        }),
                    }],
                },
            )
        };
        let position_a = position_event(ExactDecimal::parse("-0.0000000001").unwrap());
        let position_b = position_event(ExactDecimal::parse("-0.0000000002").unwrap());
        assert_ne!(digest(&position_a), digest(&position_b));
        assert_eq!(
            serde_json::from_str::<Event>(&serde_json::to_string(&position_a).unwrap()).unwrap(),
            position_a
        );
    }

    #[test]
    fn exact_order_amounts_are_round_tripped_and_included_in_event_identity() {
        let mut order = Order {
            client_id: 88,
            instrument: InstrumentId::parse("PEPE/USDT.OKX").unwrap(),
            side: Side::Buy,
            qty: Quantity::ZERO,
            limit: None,
            status: OrderStatus::Submitted,
            filled: Quantity::ZERO,
            account_id: "main".into(),
            trace: None,
            policy: None,
            exact_amounts: None,
        };
        let legacy = Event::new(
            1,
            1,
            Priority::APPLY,
            EventKind::OrderSubmitted {
                order: order.clone(),
            },
        );
        order.exact_amounts = Some(ExactOrderAmounts {
            quantity: ExactDecimal::parse("0.000000000123456789").unwrap(),
            limit_price: Some(ExactDecimal::parse("0.00000001").unwrap()),
            filled: ExactDecimal::ZERO,
            market_reference_price: None,
            quote_budget: None,
            quote_filled: Default::default(),
        });
        let exact = Event::new(1, 1, Priority::APPLY, EventKind::OrderSubmitted { order });

        let digest = |event: &Event| {
            let mut hash = crate::sourcing::Fnv1a::new();
            event.digest(&mut hash);
            hash.finish()
        };
        assert_ne!(digest(&legacy), digest(&exact));
        let restored: Event =
            serde_json::from_str(&serde_json::to_string(&exact).unwrap()).unwrap();
        assert_eq!(restored, exact);
    }

    #[test]
    fn exact_fill_amounts_are_included_in_event_identity() {
        let mut fill = Fill {
            order_id: 89,
            account_id: "main".into(),
            ..Fill::default()
        };
        let legacy = Event::new(
            1,
            1,
            Priority::APPLY,
            EventKind::Filled { fill: fill.clone() },
        );
        fill.exact_amounts = Some(ExactFillAmounts {
            quantity: ExactDecimal::parse("0.0000000001").unwrap(),
            price: ExactDecimal::parse("0.00000001").unwrap(),
            quote_cost: None,
            quote_budget_complete: false,
        });
        let exact = Event::new(1, 1, Priority::APPLY, EventKind::Filled { fill });
        let digest = |event: &Event| {
            let mut hash = crate::sourcing::Fnv1a::new();
            event.digest(&mut hash);
            hash.finish()
        };
        assert_ne!(digest(&legacy), digest(&exact));
        let completed_quote_budget = match exact.kind.clone() {
            EventKind::Filled { mut fill } => {
                fill.exact_amounts.as_mut().unwrap().quote_cost =
                    Some(ExactDecimal::parse("0.0000000001").unwrap());
                fill.exact_amounts.as_mut().unwrap().quote_budget_complete = true;
                Event::new(1, 1, Priority::APPLY, EventKind::Filled { fill })
            }
            _ => unreachable!(),
        };
        assert_ne!(digest(&exact), digest(&completed_quote_budget));
        let with_exact_fee = match exact.kind.clone() {
            EventKind::Filled { mut fill } => {
                fill.exact_fees.push(crate::ExactFillFee {
                    amount: ExactDecimal::parse("0.0000000001").unwrap(),
                    currency: Some("PEPE".into()),
                    source_trade_ids: vec!["fee-trade-1".into()],
                });
                Event::new(1, 1, Priority::APPLY, EventKind::Filled { fill })
            }
            _ => unreachable!(),
        };
        assert_ne!(digest(&exact), digest(&with_exact_fee));
        let restored: Event =
            serde_json::from_str(&serde_json::to_string(&with_exact_fee).unwrap()).unwrap();
        assert_eq!(restored, with_exact_fee);
    }
}
