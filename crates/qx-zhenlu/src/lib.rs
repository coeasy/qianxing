//! # qx-zhenlu — 针路
//!
//! 执行与路由：风控门禁 → OMS → 路由 → 适配器。
//!
//! 原则：**RiskGate 可以拒绝，但不得静默改写业务含义。**

pub use qx_oms::Oms;
// 目标仓位是全仓唯一概念（定义在 qx-core，V10 §4.8）；此处只做出口别名。
pub use qx_core::TargetPosition;

use qx_core::{
    Fill, InstrumentId, Order, OrderStatus, OrderTrace, Price, Quantity, QxError, QxResult, Side,
    TradingInstrumentSpec,
};
use qx_guanxing::QuoteTick;
#[cfg(test)]
use qx_risk::{MaxNotionalRule, MaxQtyRule, NoShortRule};
use qx_risk::{
    OrderRiskContext as CanonicalOrderRiskContext, OrderRiskPosition, RiskRule, RuleSet,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// 订单进入 OMS/Venue 前的账户级风控上下文。
///
/// `RiskRule` 适合表达单条静态规则；`RiskContext` 把账户可用保证金、市场参考价、
/// 产品规格和账户限额放到同一个不可变快照中，确保回测、Paper 和实盘预检可以
/// 复用同一套规格/杠杆/保证金判定。缺少必要的规格或市价参考价时拒绝订单，
/// 不把未知资金状态当成“通过”。
#[derive(Clone, Debug, Default)]
pub struct RiskContext {
    pub available_margin_raw: Option<i128>,
    pub reference_price: Option<Price>,
    pub instrument_spec: Option<TradingInstrumentSpec>,
    pub max_order_notional_raw: Option<i128>,
    pub max_position_notional_raw: Option<i128>,
}

impl RiskContext {
    fn canonical_context(&self, position: &OrderRiskPosition) -> CanonicalOrderRiskContext {
        CanonicalOrderRiskContext {
            available_margin_raw: self.available_margin_raw,
            reference_price: self.reference_price,
            instrument_spec: self.instrument_spec.clone(),
            max_order_notional_raw: self.max_order_notional_raw,
            max_position_notional_raw: self.max_position_notional_raw,
            position: *position,
        }
    }

    /// 兼容快照入口对应的统一可审计订单级决定。
    pub fn evaluate_order(
        &self,
        order: &Order,
        position: &OrderRiskPosition,
    ) -> qx_risk::OrderRiskDecision {
        let canonical = self.canonical_context(position);
        qx_risk::RiskEngine::evaluate_order(&canonical, order)
    }

    /// 与 [`Self::evaluate_order`] 同一份判定，但叠加配置化静态规则集。
    ///
    /// 三条执行路径（回测、Paper、实盘）都应经由本方法或
    /// `qx_risk::RiskEngine::evaluate_order_with_rules`，不要再各自解释规则。
    pub fn evaluate_order_with_rules(
        &self,
        order: &Order,
        position: &OrderRiskPosition,
        rules: &RuleSet,
    ) -> qx_risk::OrderRiskDecision {
        let canonical = self.canonical_context(position);
        rules.evaluate(&canonical, order)
    }

    pub fn validate_order(&self, order: &Order, position: &OrderRiskPosition) -> QxResult<()> {
        self.canonical_context(position)
            .validate_order(order)
            .map_err(|error| match error {
                QxError::BusinessViolation(message) => {
                    QxError::BusinessViolation(format!("RiskContext: {message}"))
                }
                other => other,
            })
    }
}

/// 旧 `RiskGate` 调用形状到规范风控上下文的转换。
///
/// 兼容入口只携带持仓快照和参考价，因此构造出的上下文没有产品规格与账户限额；
/// reduce-only 不变式与静态规则链在这种上下文下的语义与迁移前一致。
// deprecated compat: 新代码请构造 `qx_risk::OrderRiskContext` 并调用 `RuleSet::evaluate`。
fn legacy_gate_context(
    position: &OrderRiskPosition,
    reference_price: Option<Price>,
) -> CanonicalOrderRiskContext {
    CanonicalOrderRiskContext {
        reference_price,
        position: *position,
        ..CanonicalOrderRiskContext::default()
    }
}

/// 风控门禁。**不短路**：收集全部拒绝原因，策略需要知道所有问题。
//
// deprecated compat: 规则实现已上收到 `qx-risk::RuleSet`。本类型仅保留旧
// `(Order, OrderRiskPosition)` 调用形状。构造必须显式命名规则集
// （[`RiskGate::from_rule_set`] 或 [`RiskGate::conservative_default`]），
// 不再提供零规则即放行的 `new()`/`Default`（V10 §4.5：静默的宽松门禁就是双轨）。
// 生产路径请改为构造 `qx_risk::OrderRiskContext` 并调用
// `qx_risk::RiskEngine::evaluate_order_with_rules` 传入配置化 `RuleSet`。
pub struct RiskGate {
    rule_set: RuleSet,
}

impl RiskGate {
    /// 具名保守默认门禁：回落路径唯一允许的零配置形状，判定来自
    /// [`RuleSet::conservative_default`]，绝不静默放行。
    pub fn conservative_default() -> Self {
        Self {
            rule_set: RuleSet::conservative_default(),
        }
    }

    /// 用配置化规则集构造门禁；`rule_set.version` 会出现在判定结果中。
    pub fn from_rule_set(rule_set: RuleSet) -> Self {
        Self { rule_set }
    }

    /// 借用内部规则集，便于上层把同一份配置传给统一判定入口。
    pub fn rule_set(&self) -> &RuleSet {
        &self.rule_set
    }

    pub fn add(&mut self, r: Box<dyn RiskRule>) {
        self.rule_set.add(r);
    }

    pub fn check(&self, o: &Order, pos: &OrderRiskPosition) -> QxResult<()> {
        self.check_with_price(o, pos, o.limit)
    }

    pub fn check_with_price(
        &self,
        o: &Order,
        pos: &OrderRiskPosition,
        reference_price: Option<Price>,
    ) -> QxResult<()> {
        let decision = self
            .rule_set
            .evaluate_rules_only(&legacy_gate_context(pos, reference_price), o);
        if decision.allowed {
            Ok(())
        } else {
            Err(QxError::BusinessViolation(decision.violations.join("; ")))
        }
    }
}

/// 策略输出的不可变信号。策略不直接提交订单。
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Signal {
    pub strategy_id: String,
    pub signal_id: u64,
    pub instrument: InstrumentId,
    pub target_qty: i128,
    pub confidence: i128,
    pub priority: i32,
    pub expires_at: u64,
}

/// 组合层净额后的目标仓位 `TargetPosition` 唯一定义在 `qx-core`（本 crate 顶部
/// 已重导出）；`SignalMerger` 与 `rebalance_intent` 直接消费该类型。

#[derive(Default)]
pub struct SignalMerger;

impl SignalMerger {
    pub fn merge(&self, mut signals: Vec<Signal>, now: u64) -> Vec<TargetPosition> {
        signals.retain(|s| s.expires_at == 0 || s.expires_at >= now);
        signals.sort_by(|a, b| {
            b.priority
                .cmp(&a.priority)
                .then_with(|| a.strategy_id.cmp(&b.strategy_id))
                .then_with(|| a.signal_id.cmp(&b.signal_id))
        });
        let mut seen = std::collections::BTreeSet::new();
        signals.retain(|signal| seen.insert((signal.strategy_id.clone(), signal.signal_id)));
        let mut merged: BTreeMap<InstrumentId, TargetPosition> = BTreeMap::new();
        for s in signals {
            let entry = merged
                .entry(s.instrument.clone())
                .or_insert_with(|| TargetPosition {
                    instrument: s.instrument.clone(),
                    target_qty: 0,
                    source_signals: Vec::new(),
                });
            entry.target_qty = entry.target_qty.saturating_add(s.target_qty);
            entry.source_signals.push(s.signal_id);
        }
        merged.into_values().collect()
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct OrderIntent {
    pub intent_id: u64,
    pub strategy_id: String,
    pub signal_id: Option<u64>,
    pub account_id: String,
    pub instrument: InstrumentId,
    pub side: Side,
    pub qty: Quantity,
    pub limit: Option<Price>,
    pub created_ts: u64,
    pub rule_version: String,
}

impl OrderIntent {
    pub fn into_order(self) -> Order {
        Order {
            client_id: self.intent_id,
            instrument: self.instrument,
            side: self.side,
            qty: self.qty,
            limit: self.limit,
            status: OrderStatus::PendingSubmit,
            filled: Quantity::ZERO,
            account_id: self.account_id,
            trace: Some(OrderTrace {
                strategy_id: Some(self.strategy_id),
                signal_id: self.signal_id,
                intent_id: Some(self.intent_id),
                rule_version: Some(self.rule_version),
            }),
            policy: None,
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum VenueEvent {
    Accepted {
        client_order_id: u64,
        venue_order_id: String,
        ts: u64,
    },
    Fill(Fill),
    Cancelled {
        client_order_id: u64,
        ts: u64,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ConnectorState {
    Disconnected,
    Connecting,
    Snapshotting,
    Live,
    Degraded,
    ReconcileRequired,
}

/// 轻量确定性限频器：按请求权重消耗 token，不读系统时间。
#[derive(Clone, Copy, Debug)]
pub struct RateLimiter {
    capacity: u64,
    tokens: u64,
    refill_per_second: u64,
    last_ts: u64,
}

impl RateLimiter {
    pub fn new(capacity: u64, refill_per_second: u64, now: u64) -> Self {
        Self {
            capacity,
            tokens: capacity,
            refill_per_second,
            last_ts: now,
        }
    }
    pub fn try_acquire(&mut self, weight: u64, now: u64) -> bool {
        let elapsed = now.saturating_sub(self.last_ts);
        let refill = elapsed.saturating_mul(self.refill_per_second) / 1_000_000_000;
        self.tokens = self.capacity.min(self.tokens.saturating_add(refill));
        self.last_ts = now;
        if weight > self.tokens {
            return false;
        }
        self.tokens -= weight;
        true
    }
    pub fn available(&self) -> u64 {
        self.tokens
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct VenueOrderSnapshot {
    pub client_order_id: u64,
    pub status: OrderStatus,
    pub filled: Quantity,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum VenueReconcileIssue {
    MissingLocally {
        client_order_id: u64,
    },
    MissingAtVenue {
        client_order_id: u64,
    },
    StatusMismatch {
        client_order_id: u64,
        local: OrderStatus,
        venue: OrderStatus,
    },
    FilledMismatch {
        client_order_id: u64,
        local: Quantity,
        venue: Quantity,
    },
}

/// Venue 适配器的最小执行契约。适配器只返回事实事件，不直接改 OMS 或账簿。
pub trait Venue {
    fn id(&self) -> &str;
    fn submit(&mut self, order: Order, ts: u64) -> QxResult<Vec<VenueEvent>>;
    fn cancel(&mut self, client_order_id: u64, ts: u64) -> QxResult<Vec<VenueEvent>>;
    fn snapshot(&self) -> Vec<VenueOrderSnapshot>;
    fn connected(&self) -> bool;
}

/// 多腿订单组的生命周期。两条独立订单只有在该状态机确认所有腿完成后，
/// 才能被策略视为套利完成；部分成交、取消和未知回报必须显式进入恢复路径。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpreadOrderGroupStatus {
    Planned,
    Submitting,
    PartiallyFilled,
    HedgeRequired,
    /// Compensation orders have fully offset all confirmed exposure.
    Hedged,
    Filled,
    Failed,
    Cancelled,
    ReconcileRequired,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct SpreadOrderLeg {
    pub leg_id: String,
    /// 路由到的 Venue；空值仅兼容旧快照，新的多 Venue 编排必须显式填写。
    #[serde(default)]
    pub venue_id: String,
    pub order: Order,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct SpreadCompensationTarget {
    pub leg_id: String,
    pub instrument: InstrumentId,
    pub account_id: String,
    pub side: Side,
    pub qty: Quantity,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct SpreadOrderGroup {
    pub group_id: String,
    pub strategy_id: String,
    pub status: SpreadOrderGroupStatus,
    pub legs: Vec<SpreadOrderLeg>,
}

impl SpreadOrderGroup {
    pub fn new(
        group_id: impl Into<String>,
        strategy_id: impl Into<String>,
        legs: Vec<SpreadOrderLeg>,
    ) -> QxResult<Self> {
        let group = Self {
            group_id: group_id.into(),
            strategy_id: strategy_id.into(),
            status: SpreadOrderGroupStatus::Planned,
            legs,
        };
        group.validate()?;
        Ok(group)
    }

    pub fn validate(&self) -> QxResult<()> {
        if self.group_id.trim().is_empty() || self.strategy_id.trim().is_empty() {
            return Err(QxError::BusinessViolation(
                "SpreadOrderGroup group_id/strategy_id 不能为空".into(),
            ));
        }
        if self.legs.len() < 2 {
            return Err(QxError::BusinessViolation(
                "SpreadOrderGroup 至少需要两条腿".into(),
            ));
        }
        let mut ids = std::collections::BTreeSet::new();
        for leg in &self.legs {
            if leg.leg_id.trim().is_empty() || !ids.insert(leg.leg_id.clone()) {
                return Err(QxError::BusinessViolation(
                    "SpreadOrderGroup leg_id 不能为空且不能重复".into(),
                ));
            }
            leg.order.validate().map_err(QxError::BusinessViolation)?;
            if leg.order.filled != Quantity::ZERO {
                return Err(QxError::BusinessViolation(
                    "SpreadOrderGroup 初始订单不得带有已成交数量".into(),
                ));
            }
        }
        Ok(())
    }

    /// 校验已经进入执行生命周期的订单组。与创建时的 `validate` 不同，
    /// 持久化状态允许腿带有部分/全部成交数量，但仍禁止超量成交和非法订单。
    pub fn validate_persisted(&self) -> QxResult<()> {
        let mut structure = self.clone();
        for leg in &mut structure.legs {
            if leg.order.filled.raw() < 0 || leg.order.filled.raw() > leg.order.qty.raw() {
                return Err(QxError::BusinessViolation(
                    "持久化多腿状态的成交数量超出订单范围".into(),
                ));
            }
            leg.order.filled = Quantity::ZERO;
        }
        structure.validate()
    }

    pub fn begin_submission(&mut self) -> QxResult<()> {
        if self.status != SpreadOrderGroupStatus::Planned {
            return Err(QxError::VenueState("SpreadOrderGroup 不能重复提交".into()));
        }
        self.status = SpreadOrderGroupStatus::Submitting;
        Ok(())
    }

    pub fn record_accepted(&mut self, leg_id: &str) -> QxResult<()> {
        let leg = self.leg_mut(leg_id)?;
        Self::accept_leg_order(leg)?;
        self.recompute_status();
        Ok(())
    }

    pub fn record_fill(&mut self, leg_id: &str, fill: &Fill) -> QxResult<()> {
        let leg = self.leg_mut(leg_id)?;
        if fill.order_id != leg.order.client_id || fill.qty.raw() <= 0 {
            return Err(QxError::BusinessViolation(
                "SpreadOrderGroup Fill 与腿订单不匹配".into(),
            ));
        }
        let next = leg
            .order
            .filled
            .raw()
            .checked_add(fill.qty.raw())
            .ok_or_else(|| QxError::Invariant("SpreadOrderGroup 成交数量溢出".into()))?;
        if next > leg.order.qty.raw() {
            return Err(QxError::BusinessViolation(
                "SpreadOrderGroup 成交数量超过腿订单数量".into(),
            ));
        }
        Self::accept_leg_order(leg)?;
        leg.order.filled = Quantity::from_raw(next);
        leg.order.status = if next == leg.order.qty.raw() {
            OrderStatus::Filled
        } else {
            OrderStatus::PartiallyFilled
        };
        self.recompute_status();
        Ok(())
    }

    pub fn record_rejected(&mut self, leg_id: &str) -> QxResult<()> {
        let leg = self.leg_mut(leg_id)?;
        if leg.order.filled.raw() > 0 {
            self.status = SpreadOrderGroupStatus::HedgeRequired;
            return Ok(());
        }
        if !leg.order.status.is_terminal() {
            leg.order.status = OrderStatus::Rejected;
        }
        self.recompute_status();
        Ok(())
    }

    pub fn record_cancelled(&mut self, leg_id: &str) -> QxResult<()> {
        let leg = self.leg_mut(leg_id)?;
        if leg.order.filled.raw() > 0 {
            self.status = SpreadOrderGroupStatus::HedgeRequired;
            return Ok(());
        }
        if !leg.order.status.is_terminal() {
            leg.order.status = OrderStatus::Cancelled;
        }
        self.recompute_status();
        Ok(())
    }

    pub fn record_unknown(&mut self, leg_id: &str) -> QxResult<()> {
        let leg = self.leg_mut(leg_id)?;
        if leg.order.status.is_terminal() {
            return Err(QxError::VenueState("终态腿不能标记 Unknown".into()));
        }
        leg.order.status = OrderStatus::Unknown;
        self.status = SpreadOrderGroupStatus::ReconcileRequired;
        Ok(())
    }

    /// 跨腿屏障：该状态是否禁止继续提交同组其余腿。
    ///
    /// `ReconcileRequired` 表示已有腿离开本地但结果未知，`HedgeRequired` 表示已有
    /// 确认敞口等待补偿；两者都不能靠"再下一腿"掩盖，必须先由对账/补偿链路得到
    /// 确定事实。多腿编排的唯一安全口径由本函数持有，任何提交入口都必须遵守。
    pub fn blocks_new_leg_submission(&self) -> bool {
        matches!(
            self.status,
            SpreadOrderGroupStatus::ReconcileRequired | SpreadOrderGroupStatus::HedgeRequired
        )
    }

    /// 返回已经产生风险敞口的腿的反向补偿目标。执行器必须将其作为
    /// `reduce_only`/对账恢复命令处理，不能自动把未确认的另一腿补下去。
    pub fn compensation_targets(&self) -> Vec<SpreadCompensationTarget> {
        if !matches!(
            self.status,
            SpreadOrderGroupStatus::PartiallyFilled
                | SpreadOrderGroupStatus::HedgeRequired
                | SpreadOrderGroupStatus::ReconcileRequired
        ) {
            return Vec::new();
        }
        self.legs
            .iter()
            .filter(|leg| leg.order.filled.raw() > 0)
            .map(|leg| SpreadCompensationTarget {
                leg_id: leg.leg_id.clone(),
                instrument: leg.order.instrument.clone(),
                account_id: leg.order.account_id.clone(),
                side: match leg.order.side {
                    Side::Buy => Side::Sell,
                    Side::Sell => Side::Buy,
                },
                qty: leg.order.filled,
            })
            .collect()
    }

    /// Mark a group as recovered only after every deterministic compensation
    /// order has reached Filled. Unknown venue state must never use this path.
    pub fn mark_hedged(&mut self) -> QxResult<()> {
        if self.status != SpreadOrderGroupStatus::HedgeRequired {
            return Err(QxError::VenueState(format!(
                "只有 HedgeRequired 订单组可以标记为 Hedged，当前为 {:?}",
                self.status
            )));
        }
        self.status = SpreadOrderGroupStatus::Hedged;
        Ok(())
    }

    pub fn leg(&self, leg_id: &str) -> QxResult<&SpreadOrderLeg> {
        self.legs
            .iter()
            .find(|leg| leg.leg_id == leg_id)
            .ok_or_else(|| {
                QxError::BusinessViolation(format!("SpreadOrderGroup 不存在腿: {leg_id}"))
            })
    }

    /// 把每腿成本事实汇总成组级归因。`legs` 必须与订单组的腿集合一一对应；
    /// 缺失或多余的腿都视为归因数据错误，而不是补 0。
    pub fn attribute(
        &self,
        legs: Vec<SpreadLegAttribution>,
    ) -> Result<SpreadGroupAttribution, String> {
        let expected: std::collections::BTreeSet<String> =
            self.legs.iter().map(|leg| leg.leg_id.clone()).collect();
        let provided: std::collections::BTreeSet<String> =
            legs.iter().map(|leg| leg.leg_id.clone()).collect();
        if provided != expected || legs.len() != self.legs.len() {
            return Err(format!(
                "多腿归因腿集合与订单组不一致: group={:?} attribution={:?}",
                expected.into_iter().collect::<Vec<_>>(),
                provided.into_iter().collect::<Vec<_>>()
            ));
        }
        let mut legs = legs;
        legs.sort_by(|left, right| left.leg_id.cmp(&right.leg_id));
        let (
            total_filled_qty_raw,
            total_turnover_raw,
            total_fees_raw,
            total_margin_raw,
            total_funding_raw,
        ) = SpreadGroupAttribution::sum(&legs);
        let net_cost_raw = total_fees_raw
            .checked_add(total_funding_raw)
            .ok_or("多腿归因净成本溢出")?;
        let attribution = SpreadGroupAttribution {
            schema_version: SpreadGroupAttribution::SUPPORTED_SCHEMA_VERSION,
            group_id: self.group_id.clone(),
            strategy_id: self.strategy_id.clone(),
            legs,
            total_filled_qty_raw,
            total_turnover_raw,
            total_fees_raw,
            total_margin_raw,
            total_funding_raw,
            net_cost_raw,
            cost_bps: SpreadGroupAttribution::cost_bps(net_cost_raw, total_turnover_raw),
        };
        attribution.validate()?;
        Ok(attribution)
    }

    fn leg_mut(&mut self, leg_id: &str) -> QxResult<&mut SpreadOrderLeg> {
        self.legs
            .iter_mut()
            .find(|leg| leg.leg_id == leg_id)
            .ok_or_else(|| {
                QxError::BusinessViolation(format!("SpreadOrderGroup 不存在腿: {leg_id}"))
            })
    }

    fn accept_leg_order(leg: &mut SpreadOrderLeg) -> QxResult<()> {
        match leg.order.status {
            OrderStatus::PendingSubmit => {
                leg.order
                    .status
                    .transition(OrderStatus::Submitted)
                    .map_err(QxError::Invariant)?;
                leg.order
                    .status
                    .transition(OrderStatus::Accepted)
                    .map_err(QxError::Invariant)?;
            }
            OrderStatus::Submitted => leg
                .order
                .status
                .transition(OrderStatus::Accepted)
                .map_err(QxError::Invariant)?,
            OrderStatus::Accepted
            | OrderStatus::Working
            | OrderStatus::PartiallyFilled
            | OrderStatus::Filled => {}
            status => {
                return Err(QxError::VenueState(format!(
                    "SpreadOrderGroup leg 不能从 {status:?} 接受"
                )))
            }
        }
        Ok(())
    }

    fn recompute_status(&mut self) {
        if self
            .legs
            .iter()
            .any(|leg| leg.order.status == OrderStatus::Unknown)
        {
            self.status = SpreadOrderGroupStatus::ReconcileRequired;
            return;
        }
        let any_filled = self.legs.iter().any(|leg| leg.order.filled.raw() > 0);
        if self
            .legs
            .iter()
            .all(|leg| leg.order.status == OrderStatus::Filled)
        {
            self.status = SpreadOrderGroupStatus::Filled;
        } else if any_filled
            && self.legs.iter().any(|leg| {
                matches!(
                    leg.order.status,
                    OrderStatus::Rejected | OrderStatus::Cancelled | OrderStatus::Expired
                )
            })
        {
            self.status = SpreadOrderGroupStatus::HedgeRequired;
        } else if any_filled {
            self.status = SpreadOrderGroupStatus::PartiallyFilled;
        } else if self.legs.iter().all(|leg| leg.order.status.is_terminal()) {
            self.status = if self
                .legs
                .iter()
                .all(|leg| leg.order.status == OrderStatus::Cancelled)
            {
                SpreadOrderGroupStatus::Cancelled
            } else {
                SpreadOrderGroupStatus::Failed
            };
        } else {
            self.status = SpreadOrderGroupStatus::Submitting;
        }
    }
}

/// 多腿归因里的定点金额/数量字段。raw 值以 1e9 缩放，两条腿的名义额可以超过
/// i64 范围，因此 JSON 承载统一使用十进制字符串，避免静默溢出。
mod spread_raw_number {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(value: &i128, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(value)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<i128, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse::<i128>()
            .map_err(|error| serde::de::Error::custom(format!("多腿归因 raw 数值非法: {error}")))
    }
}

/// 多腿组归因里的单腿成本事实。金额一律是结算币定点 raw 值，由回测或执行层
/// 按腿汇总后交给 [`SpreadOrderGroup::attribute`]，避免下游各自重算口径。
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpreadLegAttribution {
    pub leg_id: String,
    #[serde(with = "spread_raw_number")]
    pub filled_qty_raw: i128,
    #[serde(with = "spread_raw_number")]
    pub turnover_raw: i128,
    #[serde(with = "spread_raw_number")]
    pub fees_raw: i128,
    #[serde(with = "spread_raw_number")]
    pub margin_raw: i128,
    /// 资金费净额；正数表示支付，负数表示收取。
    #[serde(with = "spread_raw_number")]
    pub funding_raw: i128,
}

impl SpreadLegAttribution {
    pub fn new(
        leg_id: impl Into<String>,
        filled_qty_raw: i128,
        turnover_raw: i128,
        fees_raw: i128,
        margin_raw: i128,
        funding_raw: i128,
    ) -> Result<Self, String> {
        let leg = Self {
            leg_id: leg_id.into(),
            filled_qty_raw,
            turnover_raw,
            fees_raw,
            margin_raw,
            funding_raw,
        };
        leg.validate()?;
        Ok(leg)
    }

    fn validate(&self) -> Result<(), String> {
        if self.leg_id.trim().is_empty() {
            return Err("SpreadLegAttribution leg_id 不能为空".into());
        }
        if self.filled_qty_raw < 0 || self.turnover_raw < 0 || self.fees_raw < 0 {
            return Err("SpreadLegAttribution 成交数量、成交额和费用不得为负".into());
        }
        if self.margin_raw < 0 {
            return Err("SpreadLegAttribution 保证金占用不得为负".into());
        }
        Ok(())
    }
}

/// [`SpreadOrderGroup`] 级别的成本汇总，用于把两条腿的费用、保证金和资金费
/// 合并成一个可审计的套利成本口径。
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpreadGroupAttribution {
    pub schema_version: u32,
    pub group_id: String,
    pub strategy_id: String,
    pub legs: Vec<SpreadLegAttribution>,
    #[serde(with = "spread_raw_number")]
    pub total_filled_qty_raw: i128,
    #[serde(with = "spread_raw_number")]
    pub total_turnover_raw: i128,
    #[serde(with = "spread_raw_number")]
    pub total_fees_raw: i128,
    #[serde(with = "spread_raw_number")]
    pub total_margin_raw: i128,
    #[serde(with = "spread_raw_number")]
    pub total_funding_raw: i128,
    /// 费用 + 资金费；正数代表完成该组套利付出的成本。
    #[serde(with = "spread_raw_number")]
    pub net_cost_raw: i128,
    /// 成本占成交额比重（bps）；成交额为 0 时记 0。
    pub cost_bps: i64,
}

impl SpreadGroupAttribution {
    pub const SUPPORTED_SCHEMA_VERSION: u32 = 1;

    pub fn from_json(input: &str) -> Result<Self, String> {
        let attribution: Self =
            serde_json::from_str(input).map_err(|error| format!("多腿归因 JSON 无效: {error}"))?;
        attribution.validate()?;
        Ok(attribution)
    }

    pub fn to_json(&self) -> Result<String, String> {
        self.validate()?;
        serde_json::to_string_pretty(self).map_err(|error| format!("多腿归因序列化失败: {error}"))
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != Self::SUPPORTED_SCHEMA_VERSION {
            return Err(format!(
                "多腿归因 schema_version={} 不受支持（本构建支持 {}）",
                self.schema_version,
                Self::SUPPORTED_SCHEMA_VERSION
            ));
        }
        if self.group_id.trim().is_empty() || self.strategy_id.trim().is_empty() {
            return Err("多腿归因 group_id/strategy_id 不能为空".into());
        }
        if self.legs.len() < 2 {
            return Err("多腿归因至少需要两条腿".into());
        }
        let mut ids = std::collections::BTreeSet::new();
        for leg in &self.legs {
            leg.validate()?;
            if !ids.insert(leg.leg_id.clone()) {
                return Err(format!("多腿归因 leg_id 重复: {}", leg.leg_id));
            }
        }
        let totals = Self::sum(&self.legs);
        if self.total_filled_qty_raw != totals.0
            || self.total_turnover_raw != totals.1
            || self.total_fees_raw != totals.2
            || self.total_margin_raw != totals.3
            || self.total_funding_raw != totals.4
        {
            return Err("多腿归因合计值与腿级明细不一致".into());
        }
        if self.net_cost_raw
            != self
                .total_fees_raw
                .checked_add(self.total_funding_raw)
                .ok_or("多腿归因净成本溢出")?
        {
            return Err("多腿归因净成本与费用/资金费之和不一致".into());
        }
        if self.cost_bps != Self::cost_bps(self.net_cost_raw, self.total_turnover_raw) {
            return Err("多腿归因成本 bps 与明细不一致".into());
        }
        Ok(())
    }

    fn sum(legs: &[SpreadLegAttribution]) -> (i128, i128, i128, i128, i128) {
        legs.iter().fold((0, 0, 0, 0, 0), |mut acc, leg| {
            acc.0 = acc.0.saturating_add(leg.filled_qty_raw);
            acc.1 = acc.1.saturating_add(leg.turnover_raw);
            acc.2 = acc.2.saturating_add(leg.fees_raw);
            acc.3 = acc.3.saturating_add(leg.margin_raw);
            acc.4 = acc.4.saturating_add(leg.funding_raw);
            acc
        })
    }

    fn cost_bps(net_cost_raw: i128, turnover_raw: i128) -> i64 {
        if turnover_raw <= 0 {
            return 0;
        }
        i64::try_from(
            net_cost_raw
                .saturating_mul(10_000)
                .checked_div(turnover_raw)
                .unwrap_or(0),
        )
        .unwrap_or(i64::MAX)
    }
}

/// 多腿订单组的持久化端口。实现必须以 `group_id` 幂等保存完整状态，
/// 使执行 worker 重启后可以继续对账/补偿，而不是重新提交已确认的腿。
pub trait SpreadOrderGroupStore {
    fn load(&self, group_id: &str) -> Result<Option<SpreadOrderGroup>, String>;
    fn save(&mut self, group: &SpreadOrderGroup) -> Result<(), String>;
    fn delete(&mut self, group_id: &str) -> Result<(), String>;

    /// Claim a group for a single recovery owner. Backends that do not yet
    /// provide a durable lease keep the historical no-op behavior; the file
    /// backend implements an expiring atomic claim below.
    fn try_claim(
        &mut self,
        _group_id: &str,
        _owner: &str,
        _now: u64,
        _lease_ms: u64,
    ) -> Result<Option<u64>, String> {
        Ok(Some(0))
    }

    fn verify_claim(
        &mut self,
        _group_id: &str,
        _owner: &str,
        _token: u64,
        _now: u64,
    ) -> Result<bool, String> {
        Ok(true)
    }

    fn save_claimed(
        &mut self,
        group: &SpreadOrderGroup,
        owner: &str,
        token: u64,
        now: u64,
    ) -> Result<(), String> {
        if !self.verify_claim(&group.group_id, owner, token, now)? {
            return Err("多腿恢复 claim 已过期或已被其他 owner 接管".into());
        }
        self.save(group)
    }

    fn release_claim(&mut self, _group_id: &str, _owner: &str, _token: u64) -> Result<(), String> {
        Ok(())
    }
}

/// 单机多腿状态文件存储。它只保存订单组状态，不替代 EventLog；每次状态
/// 变化仍必须先由 EventLog 记录单腿事实，再更新该恢复快照。
#[derive(Clone, Debug)]
pub struct FileSpreadOrderGroupStore {
    root: std::path::PathBuf,
}

impl FileSpreadOrderGroupStore {
    pub fn new(root: impl Into<std::path::PathBuf>) -> Result<Self, String> {
        let root = root.into();
        std::fs::create_dir_all(&root).map_err(|error| format!("创建多腿状态目录失败: {error}"))?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &std::path::Path {
        &self.root
    }

    /// 返回当前状态目录中的订单组标识。临时文件和锁文件不会被暴露；
    /// 每个 JSON 在返回前仍由调用方通过 `load` 做完整结构校验。
    pub fn group_ids(&self) -> Result<Vec<String>, String> {
        let mut ids = Vec::new();
        let entries = std::fs::read_dir(&self.root)
            .map_err(|error| format!("读取多腿状态目录失败: {error}"))?;
        for entry in entries {
            let entry = entry.map_err(|error| format!("读取多腿状态目录项失败: {error}"))?;
            let path = entry.path();
            if !path.is_file() || path.extension().and_then(|value| value.to_str()) != Some("json")
            {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|value| value.to_str()) else {
                continue;
            };
            if !stem.trim().is_empty() {
                ids.push(stem.to_string());
            }
        }
        ids.sort();
        ids.dedup();
        Ok(ids)
    }

    fn path_for(&self, group_id: &str) -> Result<std::path::PathBuf, String> {
        if group_id.trim().is_empty()
            || group_id.contains('/')
            || group_id.contains('\\')
            || group_id.contains("..")
        {
            return Err("多腿 group_id 包含非法路径字符".into());
        }
        Ok(self.root.join(format!("{group_id}.json")))
    }

    fn lock_path(&self) -> std::path::PathBuf {
        self.root.join(".spread-groups.lock")
    }

    fn lease_path_for(&self, group_id: &str) -> Result<std::path::PathBuf, String> {
        self.path_for(group_id)
            .map(|path| path.with_extension("lease"))
    }
}

impl SpreadOrderGroupStore for FileSpreadOrderGroupStore {
    fn load(&self, group_id: &str) -> Result<Option<SpreadOrderGroup>, String> {
        let path = self.path_for(group_id)?;
        let payload = match std::fs::read_to_string(&path) {
            Ok(payload) => payload,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("读取多腿状态 {} 失败: {error}", path.display())),
        };
        let group: SpreadOrderGroup = serde_json::from_str(&payload)
            .map_err(|error| format!("解析多腿状态 {} 失败: {error}", path.display()))?;
        group
            .validate_persisted()
            .map_err(|error| format!("多腿状态 {} 校验失败: {error:?}", path.display()))?;
        Ok(Some(group))
    }

    fn save(&mut self, group: &SpreadOrderGroup) -> Result<(), String> {
        group
            .validate_persisted()
            .map_err(|error| format!("保存多腿状态前校验失败: {error:?}"))?;
        let path = self.path_for(&group.group_id)?;
        let lock_path = self.lock_path();
        let _lock = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock_path)
            .map_err(|error| format!("获取多腿状态写锁失败: {error}"))?;
        let result = (|| {
            let payload = serde_json::to_vec_pretty(group)
                .map_err(|error| format!("序列化多腿状态失败: {error}"))?;
            let temporary = path.with_extension(format!("json.tmp.{}", std::process::id()));
            std::fs::write(&temporary, payload)
                .map_err(|error| format!("写入多腿状态临时文件失败: {error}"))?;
            if let Err(error) = std::fs::rename(&temporary, &path) {
                let _ = std::fs::remove_file(&path);
                std::fs::rename(&temporary, &path)
                    .map_err(|replacement| format!("替换多腿状态失败: {error}; {replacement}"))?;
            }
            Ok::<(), String>(())
        })();
        drop(_lock);
        let _ = std::fs::remove_file(lock_path);
        result
    }

    fn delete(&mut self, group_id: &str) -> Result<(), String> {
        let path = self.path_for(group_id)?;
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!("删除多腿状态失败: {error}")),
        }
    }

    fn try_claim(
        &mut self,
        group_id: &str,
        owner: &str,
        now: u64,
        lease_ms: u64,
    ) -> Result<Option<u64>, String> {
        if owner.trim().is_empty() || lease_ms == 0 {
            return Err("多腿恢复 claim 要求非空 owner 和正 lease_ms".into());
        }
        let path = self.lease_path_for(group_id)?;
        let mut next_token = 1_u64;
        for _ in 0..3 {
            let payload = serde_json::json!({
                "owner": owner,
                "token": next_token,
                "expires_at": now.saturating_add(lease_ms),
            });
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(mut file) => {
                    use std::io::Write;
                    let encoded = serde_json::to_vec(&payload)
                        .map_err(|error| format!("序列化多腿恢复 claim 失败: {error}"))?;
                    file.write_all(&encoded)
                        .map_err(|error| format!("写入多腿恢复 claim 失败: {error}"))?;
                    return Ok(Some(next_token));
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    let existing = match std::fs::read_to_string(&path) {
                        Ok(value) => value,
                        Err(read_error) if read_error.kind() == std::io::ErrorKind::NotFound => {
                            continue
                        }
                        Err(read_error) => {
                            return Err(format!("读取多腿恢复 claim 失败: {read_error}"));
                        }
                    };
                    let current: serde_json::Value = serde_json::from_str(&existing)
                        .map_err(|parse_error| format!("解析多腿恢复 claim 失败: {parse_error}"))?;
                    let expires_at = current
                        .get("expires_at")
                        .and_then(serde_json::Value::as_u64)
                        .ok_or_else(|| "多腿恢复 claim 缺少 expires_at".to_string())?;
                    let current_token = current
                        .get("token")
                        .and_then(serde_json::Value::as_u64)
                        .ok_or_else(|| "多腿恢复 claim 缺少 fencing token".to_string())?;
                    let current_owner = current
                        .get("owner")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default();
                    if current_owner == owner && expires_at > now {
                        return Ok(Some(current_token));
                    }
                    if expires_at > now {
                        return Ok(None);
                    }
                    next_token = current_token.saturating_add(1).max(1);
                    match std::fs::remove_file(&path) {
                        Ok(()) => continue,
                        Err(remove_error)
                            if remove_error.kind() == std::io::ErrorKind::NotFound =>
                        {
                            continue
                        }
                        Err(remove_error) => {
                            return Err(format!("清理过期多腿恢复 claim 失败: {remove_error}"));
                        }
                    }
                }
                Err(error) => return Err(format!("创建多腿恢复 claim 失败: {error}")),
            }
        }
        Ok(None)
    }

    fn verify_claim(
        &mut self,
        group_id: &str,
        owner: &str,
        token: u64,
        now: u64,
    ) -> Result<bool, String> {
        let path = self.lease_path_for(group_id)?;
        let existing = match std::fs::read_to_string(&path) {
            Ok(value) => value,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(format!("读取多腿恢复 claim 失败: {error}")),
        };
        let current: serde_json::Value = serde_json::from_str(&existing)
            .map_err(|error| format!("解析多腿恢复 claim 失败: {error}"))?;
        let current_owner = current.get("owner").and_then(serde_json::Value::as_str);
        let current_token = current.get("token").and_then(serde_json::Value::as_u64);
        let expires_at = current
            .get("expires_at")
            .and_then(serde_json::Value::as_u64);
        Ok(current_owner == Some(owner) && current_token == Some(token) && expires_at > Some(now))
    }

    fn release_claim(&mut self, group_id: &str, owner: &str, token: u64) -> Result<(), String> {
        let path = self.lease_path_for(group_id)?;
        let existing = match std::fs::read_to_string(&path) {
            Ok(value) => value,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(format!("读取多腿恢复 claim 失败: {error}")),
        };
        let current: serde_json::Value = serde_json::from_str(&existing)
            .map_err(|error| format!("解析多腿恢复 claim 失败: {error}"))?;
        // An expired owner may clean up its own lease, but it must never
        // remove a newer owner's lease after the token has advanced.
        if current.get("owner").and_then(serde_json::Value::as_str) != Some(owner)
            || current.get("token").and_then(serde_json::Value::as_u64) != Some(token)
        {
            return Ok(());
        }
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!("释放多腿恢复 claim 失败: {error}")),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct ConnectorCapabilities {
    pub market_data: bool,
    pub user_stream: bool,
    pub submit: bool,
    pub cancel: bool,
    pub replace: bool,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct AdapterHealth {
    pub state: ConnectorState,
    pub last_event_ts: u64,
    pub reconnects: u64,
}

pub trait VenueAdapter: Venue {
    fn capabilities(&self) -> ConnectorCapabilities;
    fn health(&self) -> AdapterHealth;
}

/// 内存 PaperVenue：用真实的订单生命周期和 L1 报价做联调，不连接外部市场。
pub struct PaperVenue {
    id: String,
    connected: bool,
    orders: BTreeMap<u64, Order>,
    next_venue_id: u64,
    state: ConnectorState,
    last_event_ts: u64,
    last_source_seq: u64,
    reconnects: u64,
}

impl PaperVenue {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            connected: true,
            orders: BTreeMap::new(),
            next_venue_id: 1,
            state: ConnectorState::Live,
            last_event_ts: 0,
            last_source_seq: 0,
            reconnects: 0,
        }
    }

    /// 从 EventLog 恢复本地虚拟 Venue 的订单索引。Paper 不把内存 Venue
    /// 当作事实来源；重启后由运行时先恢复 EventLog，再恢复此索引，随后
    /// 仍只能通过新的行情事实推进补偿单成交。
    pub fn restore_orders<I>(&mut self, orders: I) -> QxResult<()>
    where
        I: IntoIterator<Item = Order>,
    {
        for order in orders {
            order.validate().map_err(QxError::BusinessViolation)?;
            if let Some(existing) = self.orders.get(&order.client_id) {
                if existing != &order {
                    return Err(QxError::Invariant(format!(
                        "Paper 恢复订单 {} 与 EventLog 不一致",
                        order.client_id
                    )));
                }
                continue;
            }
            self.orders.insert(order.client_id, order);
        }
        Ok(())
    }

    pub fn disconnect(&mut self) {
        self.connected = false;
        self.state = ConnectorState::ReconcileRequired;
    }
    pub fn reconnect(&mut self) {
        self.connected = true;
        self.state = ConnectorState::Snapshotting;
        self.reconnects += 1;
    }

    pub fn complete_reconcile(&mut self) -> QxResult<()> {
        if !self.connected || !matches!(self.state, ConnectorState::Snapshotting) {
            return Err(QxError::VenueState("当前不在重连对账阶段".into()));
        }
        self.state = ConnectorState::Live;
        Ok(())
    }

    pub fn reconcile_snapshot(
        &mut self,
        remote: &[VenueOrderSnapshot],
    ) -> QxResult<Vec<VenueReconcileIssue>> {
        if !self.connected || !matches!(self.state, ConnectorState::Snapshotting) {
            return Err(QxError::VenueState("当前不在重连对账阶段".into()));
        }
        let remote = remote
            .iter()
            .map(|item| (item.client_order_id, item))
            .collect::<BTreeMap<_, _>>();
        let mut issues = Vec::new();
        for (id, local) in &self.orders {
            match remote.get(id) {
                None => issues.push(VenueReconcileIssue::MissingAtVenue {
                    client_order_id: *id,
                }),
                Some(remote) => {
                    if local.status != remote.status {
                        issues.push(VenueReconcileIssue::StatusMismatch {
                            client_order_id: *id,
                            local: local.status,
                            venue: remote.status,
                        });
                    }
                    if local.filled != remote.filled {
                        issues.push(VenueReconcileIssue::FilledMismatch {
                            client_order_id: *id,
                            local: local.filled,
                            venue: remote.filled,
                        });
                    }
                }
            }
        }
        for id in remote.keys() {
            if !self.orders.contains_key(id) {
                issues.push(VenueReconcileIssue::MissingLocally {
                    client_order_id: *id,
                });
            }
        }
        self.state = if issues.is_empty() {
            ConnectorState::Live
        } else {
            ConnectorState::ReconcileRequired
        };
        Ok(issues)
    }
    pub fn state(&self) -> ConnectorState {
        self.state
    }

    /// 连接中断期间只返回 Ambiguous，调用者必须走 snapshot/reconcile。
    pub fn on_quote(&mut self, instrument: &InstrumentId, quote: QuoteTick) -> Vec<VenueEvent> {
        if !self.connected || !matches!(self.state, ConnectorState::Live) {
            return Vec::new();
        }
        if quote.source_seq != 0 && quote.source_seq <= self.last_source_seq {
            return Vec::new();
        }
        self.last_event_ts = quote.ts;
        if quote.source_seq != 0 {
            self.last_source_seq = quote.source_seq;
        }
        let ids: Vec<u64> = self
            .orders
            .iter()
            .filter(|(_, o)| &o.instrument == instrument && !o.status.is_terminal())
            .map(|(id, _)| *id)
            .collect();
        let mut out = Vec::new();
        let mut remaining_buy = quote.ask_qty.raw();
        let mut remaining_sell = quote.bid_qty.raw();
        for id in ids {
            let o = self.orders.get_mut(&id).unwrap();
            let px = match o.side {
                Side::Buy => quote.ask,
                Side::Sell => quote.bid,
            };
            let price_ok = o
                .limit
                .map(|limit| match o.side {
                    Side::Buy => px.raw() <= limit.raw(),
                    Side::Sell => px.raw() >= limit.raw(),
                })
                .unwrap_or(true);
            let available_raw = match o.side {
                Side::Buy => &mut remaining_buy,
                Side::Sell => &mut remaining_sell,
            };
            if !price_ok || *available_raw <= 0 {
                continue;
            }
            let qty = Quantity::from_raw(o.remaining().raw().min(*available_raw));
            if qty.is_zero() {
                continue;
            }
            *available_raw -= qty.raw();
            if matches!(o.status, OrderStatus::Accepted) {
                let _ = o.status.transition(OrderStatus::Working);
            }
            o.filled = Quantity::from_raw(o.filled.raw() + qty.raw());
            let next = if o.filled.raw() >= o.qty.raw() {
                OrderStatus::Filled
            } else {
                OrderStatus::PartiallyFilled
            };
            let _ = o.status.transition(next);
            let mut fill = Fill {
                order_id: id,
                qty,
                price: px,
                fee: qx_core::Money::ZERO,
                ts: quote.ts,
                account_id: o.account_id.clone(),
                ..Fill::default()
            };
            o.trace_fill(&mut fill, Some(&self.id), None);
            out.push(VenueEvent::Fill(fill));
        }
        out
    }
}

impl Venue for PaperVenue {
    fn id(&self) -> &str {
        &self.id
    }

    fn submit(&mut self, mut order: Order, ts: u64) -> QxResult<Vec<VenueEvent>> {
        order.validate().map_err(QxError::BusinessViolation)?;
        if !self.connected {
            return Err(QxError::Ambiguous(
                "PaperVenue 连接中断，订单结果未知".into(),
            ));
        }
        if !matches!(self.state, ConnectorState::Live) {
            return Err(QxError::VenueState(
                "PaperVenue 正在对账，暂不可下单".into(),
            ));
        }
        if self.orders.contains_key(&order.client_id) {
            return Err(QxError::Invariant("重复 client_order_id".into()));
        }
        order.status = OrderStatus::Accepted;
        self.orders.insert(order.client_id, order.clone());
        let venue_id = format!("{}-{}", self.id, self.next_venue_id);
        self.next_venue_id += 1;
        Ok(vec![VenueEvent::Accepted {
            client_order_id: order.client_id,
            venue_order_id: venue_id,
            ts,
        }])
    }

    fn cancel(&mut self, client_order_id: u64, ts: u64) -> QxResult<Vec<VenueEvent>> {
        if !self.connected {
            return Err(QxError::Ambiguous("取消结果未知，必须对账".into()));
        }
        if !matches!(self.state, ConnectorState::Live) {
            return Err(QxError::VenueState(
                "PaperVenue 正在对账，暂不可撤单".into(),
            ));
        }
        let o = self
            .orders
            .get_mut(&client_order_id)
            .ok_or_else(|| QxError::Permanent("订单不存在".into()))?;
        if o.status.is_terminal() {
            return Ok(Vec::new());
        }
        o.status
            .transition(OrderStatus::CancelPending)
            .map_err(QxError::Invariant)?;
        o.status
            .transition(OrderStatus::Cancelled)
            .map_err(QxError::Invariant)?;
        Ok(vec![VenueEvent::Cancelled {
            client_order_id,
            ts,
        }])
    }

    fn snapshot(&self) -> Vec<VenueOrderSnapshot> {
        self.orders
            .values()
            .map(|o| VenueOrderSnapshot {
                client_order_id: o.client_id,
                status: o.status,
                filled: o.filled,
            })
            .collect()
    }

    fn connected(&self) -> bool {
        self.connected
    }
}

impl VenueAdapter for PaperVenue {
    fn capabilities(&self) -> ConnectorCapabilities {
        ConnectorCapabilities {
            market_data: true,
            user_stream: true,
            submit: true,
            cancel: true,
            replace: false,
        }
    }
    fn health(&self) -> AdapterHealth {
        AdapterHealth {
            state: self.state,
            last_event_ts: self.last_event_ts,
            reconnects: self.reconnects,
        }
    }
}

/// 将组合目标转换为订单意图，显式保留策略和信号来源。
pub fn rebalance_intent(
    target: &TargetPosition,
    current_qty: i128,
    strategy_id: &str,
    account_id: &str,
    intent_id: u64,
    ts: u64,
) -> Option<OrderIntent> {
    let delta = target.target_qty.checked_sub(current_qty)?;
    if delta == 0 {
        return None;
    }
    let qty = delta.checked_abs()?;
    Some(OrderIntent {
        intent_id,
        strategy_id: strategy_id.into(),
        signal_id: target.source_signals.first().copied(),
        account_id: account_id.into(),
        instrument: target.instrument.clone(),
        side: if delta > 0 { Side::Buy } else { Side::Sell },
        qty: Quantity::from_raw(qty),
        limit: None,
        created_ts: ts,
        rule_version: "default".into(),
    })
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StrategyState {
    Registered,
    Initialized,
    Running,
    Paused,
    Stopping,
    Stopped,
}

/// 每个策略实例独立拥有生命周期和交易配额，默认不共享可变状态。
pub struct StrategyRuntime {
    pub strategy_id: String,
    pub version: String,
    pub state: StrategyState,
    pub max_orders: u64,
    pub orders_used: u64,
}

impl StrategyRuntime {
    pub fn new(
        strategy_id: impl Into<String>,
        version: impl Into<String>,
        max_orders: u64,
    ) -> Self {
        Self {
            strategy_id: strategy_id.into(),
            version: version.into(),
            state: StrategyState::Registered,
            max_orders,
            orders_used: 0,
        }
    }

    pub fn initialize(&mut self) -> QxResult<()> {
        if self.state != StrategyState::Registered {
            return Err(QxError::Invariant("策略重复初始化".into()));
        }
        self.state = StrategyState::Initialized;
        Ok(())
    }

    pub fn start(&mut self) -> QxResult<()> {
        if !matches!(
            self.state,
            StrategyState::Initialized | StrategyState::Paused
        ) {
            return Err(QxError::Invariant("策略不能启动".into()));
        }
        self.state = StrategyState::Running;
        Ok(())
    }

    pub fn pause(&mut self) -> QxResult<()> {
        if self.state != StrategyState::Running {
            return Err(QxError::Invariant("只有运行中的策略可暂停".into()));
        }
        self.state = StrategyState::Paused;
        Ok(())
    }

    pub fn stop(&mut self) -> QxResult<()> {
        if matches!(
            self.state,
            StrategyState::Stopped | StrategyState::Registered
        ) {
            return Err(QxError::Invariant("策略不能停止".into()));
        }
        self.state = StrategyState::Stopped;
        Ok(())
    }

    pub fn reserve_order(&mut self) -> QxResult<()> {
        if self.state != StrategyState::Running {
            return Err(QxError::VenueState("策略未运行，禁止产生新订单".into()));
        }
        if self.orders_used >= self.max_orders {
            return Err(QxError::ResourceExhausted("策略订单配额已用尽".into()));
        }
        self.orders_used += 1;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qx_core::{InstrumentId, Money, Price, Side, TradingProduct, SCALE};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn order(qty: i64, side: Side) -> Order {
        Order {
            client_id: 1,
            instrument: InstrumentId::parse("T.V").unwrap(),
            side,
            qty: Quantity::from_i64(qty),
            limit: Some(Price::from_i64(100)),
            status: OrderStatus::Submitted,
            filled: Quantity::ZERO,
            account_id: "a".into(),
            trace: None,
            policy: None,
        }
    }

    fn two_leg_group() -> SpreadOrderGroup {
        let mut buy = order(10, Side::Buy);
        buy.client_id = 201;
        let mut sell = order(10, Side::Sell);
        sell.client_id = 202;
        sell.instrument = InstrumentId::parse("T2.V2").unwrap();
        SpreadOrderGroup::new(
            "spread-attr",
            "pairs-v1",
            vec![
                SpreadOrderLeg {
                    leg_id: "primary".into(),
                    venue_id: "venue-a".into(),
                    order: buy,
                },
                SpreadOrderLeg {
                    leg_id: "reference".into(),
                    venue_id: "venue-b".into(),
                    order: sell,
                },
            ],
        )
        .unwrap()
    }

    #[test]
    fn spread_group_attribution_sums_leg_costs_and_round_trips_json() {
        let group = two_leg_group();
        let attribution = group
            .attribute(vec![
                SpreadLegAttribution::new("reference", 5, 500, 3, 0, -2).unwrap(),
                SpreadLegAttribution::new("primary", 5, 500, 7, 120, 0).unwrap(),
            ])
            .unwrap();
        assert_eq!(
            attribution
                .legs
                .iter()
                .map(|leg| leg.leg_id.as_str())
                .collect::<Vec<_>>(),
            vec!["primary", "reference"]
        );
        assert_eq!(attribution.total_fees_raw, 10);
        assert_eq!(attribution.total_margin_raw, 120);
        assert_eq!(attribution.total_funding_raw, -2);
        assert_eq!(attribution.net_cost_raw, 8);
        assert_eq!(attribution.cost_bps, 80);
        let payload = attribution.to_json().unwrap();
        assert_eq!(
            SpreadGroupAttribution::from_json(&payload).unwrap(),
            attribution
        );
    }

    #[test]
    fn spread_group_attribution_rejects_leg_mismatch_and_tampered_totals() {
        let group = two_leg_group();
        let error = group
            .attribute(vec![
                SpreadLegAttribution::new("primary", 1, 1, 1, 1, 1).unwrap()
            ])
            .unwrap_err();
        assert!(error.contains("腿集合与订单组不一致"), "{error}");

        let mut attribution = group
            .attribute(vec![
                SpreadLegAttribution::new("primary", 5, 500, 7, 120, 0).unwrap(),
                SpreadLegAttribution::new("reference", 5, 500, 3, 0, -2).unwrap(),
            ])
            .unwrap();
        attribution.total_fees_raw += 1;
        assert!(attribution
            .validate()
            .unwrap_err()
            .contains("合计值与腿级明细不一致"));

        let payload = attribution.to_json().unwrap_err();
        assert!(payload.contains("合计值与腿级明细不一致"), "{payload}");
    }

    #[test]
    fn spread_group_requires_explicit_compensation_after_partial_fill() {
        let mut first = order(10, Side::Buy);
        first.client_id = 101;
        let mut second = order(10, Side::Sell);
        second.client_id = 102;
        second.instrument = InstrumentId::parse("T2.V2").unwrap();
        let mut group = SpreadOrderGroup::new(
            "spread-1",
            "basis-v1",
            vec![
                SpreadOrderLeg {
                    leg_id: "buy-leg".into(),
                    venue_id: "venue-a".into(),
                    order: first,
                },
                SpreadOrderLeg {
                    leg_id: "sell-leg".into(),
                    venue_id: "venue-b".into(),
                    order: second,
                },
            ],
        )
        .unwrap();
        group.begin_submission().unwrap();
        group.record_accepted("buy-leg").unwrap();
        group
            .record_fill(
                "buy-leg",
                &Fill {
                    order_id: 101,
                    qty: Quantity::from_i64(4),
                    price: Price::from_i64(100),
                    ..Fill::default()
                },
            )
            .unwrap();
        assert_eq!(group.status, SpreadOrderGroupStatus::PartiallyFilled);
        assert_eq!(group.compensation_targets().len(), 1);
        assert_eq!(group.compensation_targets()[0].side, Side::Sell);
        assert_eq!(group.compensation_targets()[0].qty, Quantity::from_i64(4));

        let mut unknown_group = group.clone();
        unknown_group.record_unknown("sell-leg").unwrap();
        assert_eq!(
            unknown_group.status,
            SpreadOrderGroupStatus::ReconcileRequired
        );

        group.record_cancelled("sell-leg").unwrap();
        assert_eq!(group.status, SpreadOrderGroupStatus::HedgeRequired);
    }

    #[test]
    fn spread_group_file_store_round_trips_in_progress_fills() {
        let root = std::env::temp_dir().join(format!(
            "qianxing-spread-store-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut first = order(10, Side::Buy);
        first.client_id = 301;
        let mut second = order(10, Side::Sell);
        second.client_id = 302;
        second.instrument = InstrumentId::parse("T2.V2").unwrap();
        let mut group = SpreadOrderGroup::new(
            "spread-persisted",
            "basis-v2",
            vec![
                SpreadOrderLeg {
                    leg_id: "buy".into(),
                    venue_id: "venue-a".into(),
                    order: first,
                },
                SpreadOrderLeg {
                    leg_id: "sell".into(),
                    venue_id: "venue-b".into(),
                    order: second,
                },
            ],
        )
        .unwrap();
        group.begin_submission().unwrap();
        group.record_accepted("buy").unwrap();
        group
            .record_fill(
                "buy",
                &Fill {
                    order_id: 301,
                    qty: Quantity::from_i64(3),
                    price: Price::from_i64(100),
                    ..Fill::default()
                },
            )
            .unwrap();
        let mut store = FileSpreadOrderGroupStore::new(&root).unwrap();
        store.save(&group).unwrap();
        store.save(&group).unwrap();
        let restored = store.load("spread-persisted").unwrap().unwrap();
        assert_eq!(restored.status, SpreadOrderGroupStatus::PartiallyFilled);
        assert_eq!(restored.legs[0].order.filled, Quantity::from_i64(3));
        assert_eq!(restored.compensation_targets().len(), 1);
        store.delete("spread-persisted").unwrap();
        assert!(store.load("spread-persisted").unwrap().is_none());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn spread_group_file_store_claim_is_exclusive_and_expires() {
        let root = std::env::temp_dir().join(format!(
            "qianxing-spread-claim-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut first = FileSpreadOrderGroupStore::new(&root).unwrap();
        let mut second = FileSpreadOrderGroupStore::new(&root).unwrap();
        let first_token = first
            .try_claim("spread-claim", "worker-a", 100, 1_000)
            .unwrap()
            .expect("first worker should acquire claim");
        assert!(second
            .try_claim("spread-claim", "worker-b", 200, 1_000)
            .unwrap()
            .is_none());
        assert_eq!(
            first
                .try_claim("spread-claim", "worker-a", 500, 1_000)
                .unwrap(),
            Some(first_token)
        );
        assert!(first
            .verify_claim("spread-claim", "worker-a", first_token, 500)
            .unwrap());
        let second_token = second
            .try_claim("spread-claim", "worker-b", 1_101, 1_000)
            .unwrap()
            .expect("second worker should acquire the expired claim");
        assert!(second_token > first_token);
        assert!(!first
            .verify_claim("spread-claim", "worker-a", first_token, 1_101)
            .unwrap());
        assert!(second
            .verify_claim("spread-claim", "worker-b", second_token, 1_101)
            .unwrap());
        assert!(first
            .release_claim("spread-claim", "worker-a", first_token)
            .is_ok());
        assert!(second
            .release_claim("spread-claim", "worker-b", second_token)
            .is_ok());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn spread_group_is_filled_only_when_every_leg_is_filled() {
        let mut first = order(1, Side::Buy);
        first.client_id = 201;
        let mut second = order(1, Side::Sell);
        second.client_id = 202;
        let mut group = SpreadOrderGroup::new(
            "spread-2",
            "pairs-v1",
            vec![
                SpreadOrderLeg {
                    leg_id: "a".into(),
                    venue_id: "venue-a".into(),
                    order: first,
                },
                SpreadOrderLeg {
                    leg_id: "b".into(),
                    venue_id: "venue-b".into(),
                    order: second,
                },
            ],
        )
        .unwrap();
        group.begin_submission().unwrap();
        for (leg_id, order_id, side) in [("a", 201, Side::Buy), ("b", 202, Side::Sell)] {
            group.record_accepted(leg_id).unwrap();
            group
                .record_fill(
                    leg_id,
                    &Fill {
                        order_id,
                        qty: Quantity::from_i64(1),
                        price: Price::from_i64(100),
                        ..Fill::default()
                    },
                )
                .unwrap();
            assert!(matches!(side, Side::Buy | Side::Sell));
        }
        assert_eq!(group.status, SpreadOrderGroupStatus::Filled);
        assert!(group.compensation_targets().is_empty());
    }

    #[test]
    fn risk_gate_collects_all_reasons() {
        let mut g = RiskGate::from_rule_set(RuleSet::account_limits_only());
        g.add(Box::new(MaxQtyRule {
            max_qty: 5_000_000_000,
        }));
        g.add(Box::new(NoShortRule));
        let o = order(100, Side::Sell);
        let e = g.check(&o, &OrderRiskPosition::default()).unwrap_err();
        let msg = format!("{}", e);
        assert!(msg.contains("MaxQty"));
        assert!(msg.contains("NoShort"));
    }

    #[test]
    fn no_short_blocks_overselling() {
        let g = {
            let mut g = RiskGate::from_rule_set(RuleSet::account_limits_only());
            g.add(Box::new(NoShortRule));
            g
        };
        let o = order(10, Side::Sell);
        assert!(g
            .check(&o, &OrderRiskPosition::new(5_000_000_000, 0))
            .is_err());
        assert!(g
            .check(&o, &OrderRiskPosition::new(10_000_000_000, 0))
            .is_ok());
    }

    #[test]
    fn market_order_requires_reference_price_for_notional_risk() {
        let mut gate = RiskGate::from_rule_set(RuleSet::account_limits_only());
        gate.add(Box::new(MaxNotionalRule {
            max_notional: 50_000_000_000,
        }));
        let mut market = order(1, Side::Buy);
        market.limit = None;
        assert!(gate.check(&market, &OrderRiskPosition::new(0, 0)).is_err());
        assert!(gate
            .check_with_price(
                &market,
                &OrderRiskPosition::new_with_multiplier(0, 0, 10),
                Some(Price::from_i64(100)),
            )
            .is_err());
    }

    #[test]
    fn duplicate_client_id_rejected() {
        let mut oms = Oms::new();
        oms.submit(order(1, Side::Buy)).unwrap();
        assert!(oms.submit(order(1, Side::Buy)).is_err());
    }

    #[test]
    fn fill_updates_status() {
        let mut oms = Oms::new();
        oms.submit(order(10, Side::Buy)).unwrap();
        oms.accept(1).unwrap();
        assert_eq!(oms.get(1).unwrap().status, OrderStatus::Accepted);
        oms.apply_fill(&Fill {
            order_id: 1,
            qty: Quantity::from_i64(10),
            price: Price::from_i64(100),
            fee: Money::ZERO,
            ts: 1,
            ..Fill::default()
        })
        .unwrap();
        assert_eq!(oms.get(1).unwrap().status, OrderStatus::Filled);
        assert!(oms.open_orders().is_empty());
    }

    #[test]
    fn opposing_signals_are_netted_deterministically() {
        let instrument = InstrumentId::parse("T.V").unwrap();
        let merger = SignalMerger;
        let targets = merger.merge(
            vec![
                Signal {
                    strategy_id: "b".into(),
                    signal_id: 2,
                    instrument: instrument.clone(),
                    target_qty: -3,
                    confidence: 1,
                    priority: 1,
                    expires_at: 0,
                },
                Signal {
                    strategy_id: "a".into(),
                    signal_id: 1,
                    instrument: instrument.clone(),
                    target_qty: 10,
                    confidence: 1,
                    priority: 1,
                    expires_at: 0,
                },
            ],
            1,
        );
        assert_eq!(targets[0].target_qty, 7);
        assert_eq!(targets[0].source_signals, vec![1, 2]);
    }

    #[test]
    fn duplicate_signal_replay_does_not_change_target() {
        let instrument = InstrumentId::parse("T.V").unwrap();
        let signal = Signal {
            strategy_id: "s1".into(),
            signal_id: 7,
            instrument: instrument.clone(),
            target_qty: 10,
            confidence: 1,
            priority: 1,
            expires_at: 100,
        };
        let targets = SignalMerger.merge(vec![signal.clone(), signal], 10);
        assert_eq!(targets[0].target_qty, 10);
        assert_eq!(targets[0].source_signals, vec![7]);
    }

    #[test]
    fn paper_venue_accepts_and_fills_from_l1() {
        let instrument = InstrumentId::parse("T.V").unwrap();
        let mut venue = PaperVenue::new("paper");
        let o = order(1, Side::Buy);
        let accepted = venue.submit(o, 10).unwrap();
        assert!(matches!(accepted[0], VenueEvent::Accepted { .. }));
        let events = venue.on_quote(
            &instrument,
            QuoteTick::new(
                20,
                Price::from_i64(99),
                Quantity::from_i64(10),
                Price::from_i64(100),
                Quantity::from_i64(10),
                1,
            ),
        );
        assert!(matches!(events[0], VenueEvent::Fill(_)));
        assert_eq!(venue.snapshot()[0].status, OrderStatus::Filled);
    }

    #[test]
    fn paper_venue_consumes_quote_capacity_once() {
        let instrument = InstrumentId::parse("T.V").unwrap();
        let mut venue = PaperVenue::new("paper");
        let mut first = order(6, Side::Buy);
        first.client_id = 1;
        let mut second = order(6, Side::Buy);
        second.client_id = 2;
        venue.submit(first, 1).unwrap();
        venue.submit(second, 1).unwrap();
        let events = venue.on_quote(
            &instrument,
            QuoteTick::new(
                2,
                Price::from_i64(99),
                Quantity::from_i64(10),
                Price::from_i64(100),
                Quantity::from_i64(10),
                1,
            ),
        );
        let filled: i128 = events
            .iter()
            .map(|event| match event {
                VenueEvent::Fill(fill) => fill.qty.raw(),
                _ => 0,
            })
            .sum();
        assert_eq!(filled, Quantity::from_i64(10).raw());
        assert_eq!(
            venue.snapshot()[0].filled.raw(),
            Quantity::from_i64(6).raw()
        );
        assert_eq!(
            venue.snapshot()[1].filled.raw(),
            Quantity::from_i64(4).raw()
        );
        assert!(venue
            .on_quote(
                &instrument,
                QuoteTick::new(
                    3,
                    Price::from_i64(99),
                    Quantity::from_i64(10),
                    Price::from_i64(100),
                    Quantity::from_i64(10),
                    1,
                )
            )
            .is_empty());
    }

    #[test]
    fn disconnected_submit_is_ambiguous() {
        let mut venue = PaperVenue::new("paper");
        venue.disconnect();
        assert!(matches!(
            venue.submit(order(1, Side::Buy), 1),
            Err(QxError::Ambiguous(_))
        ));
    }

    #[test]
    fn reconnect_requires_snapshot_reconciliation() {
        let mut venue = PaperVenue::new("paper");
        venue.submit(order(1, Side::Buy), 1).unwrap();
        let remote = venue.snapshot();
        venue.disconnect();
        venue.reconnect();
        assert_eq!(venue.state(), ConnectorState::Snapshotting);
        assert!(venue.reconcile_snapshot(&remote).unwrap().is_empty());
        assert_eq!(venue.state(), ConnectorState::Live);
    }

    #[test]
    fn rate_limit_is_weighted_and_deterministic() {
        let mut l = RateLimiter::new(3, 1_000_000_000, 0);
        assert!(l.try_acquire(2, 0));
        assert!(!l.try_acquire(2, 0));
        assert!(l.try_acquire(2, 1_000_000_000));
    }

    #[test]
    fn strategy_lifecycle_and_quota_are_separate_from_risk() {
        let mut s = StrategyRuntime::new("s1", "v1", 1);
        s.initialize().unwrap();
        s.start().unwrap();
        s.reserve_order().unwrap();
        assert!(matches!(
            s.reserve_order(),
            Err(QxError::ResourceExhausted(_))
        ));
        s.pause().unwrap();
        assert!(matches!(s.reserve_order(), Err(QxError::VenueState(_))));
    }

    #[test]
    fn rebalance_intent_rejects_quantity_overflow() {
        let instrument = InstrumentId::parse("T.V").unwrap();
        let target = TargetPosition {
            instrument,
            target_qty: i128::MIN,
            source_signals: vec![1],
        };
        assert!(rebalance_intent(&target, 0, "s", "a", 1, 1).is_none());
    }

    #[test]
    fn risk_context_enforces_spec_leverage_and_available_margin() {
        let instrument = InstrumentId::parse("T.V").unwrap();
        let spec = TradingInstrumentSpec {
            instrument: instrument.clone(),
            product: TradingProduct::Perpetual,
            base_currency: "T".into(),
            quote_currency: "V".into(),
            settlement_currency: "V".into(),
            contract_size: SCALE,
            linear: true,
            inverse: false,
            price_tick: 1,
            qty_step: 1,
            min_qty: 1,
            max_leverage: 20,
            maintenance_margin_bps: 500,
            valid_from: 1,
            valid_to: None,
        };
        let mut candidate = order(1, Side::Buy);
        candidate.instrument = instrument;
        candidate.policy = Some(qx_core::OrderPolicy {
            leverage: 10,
            margin_mode: qx_core::MarginMode::Cross,
            ..qx_core::OrderPolicy::default()
        });
        let price = Price::from_i64(100);
        let required = spec
            .initial_margin(candidate.qty.raw(), price.raw(), 10)
            .unwrap();
        let context = RiskContext {
            available_margin_raw: Some(required - 1),
            reference_price: Some(price),
            instrument_spec: Some(spec.clone()),
            ..RiskContext::default()
        };
        assert!(context
            .validate_order(&candidate, &OrderRiskPosition::default())
            .is_err());
        let context = RiskContext {
            available_margin_raw: Some(required),
            ..context
        };
        assert!(context
            .validate_order(&candidate, &OrderRiskPosition::default())
            .is_ok());
        let mut missing_spec = context;
        missing_spec.instrument_spec = None;
        assert!(missing_spec
            .validate_order(&candidate, &OrderRiskPosition::default())
            .is_err());
    }
}
