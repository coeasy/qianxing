//! # qx-zhenlu — 针路
//!
//! 执行与路由：风控门禁 → OMS → 路由 → 适配器。
//!
//! 原则：**RiskGate 可以拒绝，但不得静默改写业务含义。**

pub use qx_oms::Oms;

use qx_core::{
    Fill, InstrumentId, Order, OrderStatus, OrderTrace, Price, Quantity, QxError, QxResult, Side,
    TradingInstrumentSpec,
};
use qx_guanxing::QuoteTick;
use qx_risk::{OrderRiskContext as CanonicalOrderRiskContext, OrderRiskPosition};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};

/// 持仓快照：风控判定所需的最小信息。
#[derive(Clone, Copy, Default, Debug)]
pub struct PositionSnapshot {
    /// One-way/net position only. Hedge-mode long/short legs are stored separately.
    pub net_qty: i128,
    /// Gross notional across one-way and both hedge legs at the snapshot mark.
    pub gross_notional: i128,
    pub multiplier: i128,
    pub long_qty: i128,
    pub short_qty: i128,
}

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
    fn canonical_context(&self, position: &PositionSnapshot) -> CanonicalOrderRiskContext {
        CanonicalOrderRiskContext {
            available_margin_raw: self.available_margin_raw,
            reference_price: self.reference_price,
            instrument_spec: self.instrument_spec.clone(),
            max_order_notional_raw: self.max_order_notional_raw,
            max_position_notional_raw: self.max_position_notional_raw,
            position: OrderRiskPosition {
                net_qty: position.net_qty,
                gross_notional: position.gross_notional,
                multiplier: position.multiplier,
                long_qty: position.long_qty,
                short_qty: position.short_qty,
            },
        }
    }

    /// 兼容快照入口对应的统一可审计订单级决定。
    pub fn evaluate_order(
        &self,
        order: &Order,
        position: &PositionSnapshot,
    ) -> qx_risk::OrderRiskDecision {
        let canonical = self.canonical_context(position);
        qx_risk::RiskEngine::evaluate_order(&canonical, order)
    }

    pub fn validate_order(&self, order: &Order, position: &PositionSnapshot) -> QxResult<()> {
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

impl PositionSnapshot {
    pub fn new(net_qty: i128, gross_notional: i128) -> Self {
        Self {
            net_qty,
            gross_notional,
            multiplier: 1,
            long_qty: 0,
            short_qty: 0,
        }
    }

    pub fn new_with_multiplier(net_qty: i128, gross_notional: i128, multiplier: i128) -> Self {
        Self {
            net_qty,
            gross_notional,
            multiplier: multiplier.max(1),
            long_qty: 0,
            short_qty: 0,
        }
    }

    pub fn with_hedge_legs(mut self, long_qty: i128, short_qty: i128) -> Self {
        self.long_qty = long_qty;
        self.short_qty = short_qty;
        self
    }

    pub fn active_qty_for(&self, order: &Order) -> i128 {
        let policy = order.policy.unwrap_or_default();
        if policy.position_mode == qx_core::PositionMode::Hedge {
            match policy.position_side {
                qx_core::PositionSide::Long => self.long_qty,
                qx_core::PositionSide::Short => self.short_qty,
                qx_core::PositionSide::Net => self.net_qty,
            }
        } else {
            self.net_qty
        }
    }
}

fn validate_reduce_only(order: &Order, position: &PositionSnapshot) -> QxResult<()> {
    let policy = order.policy.unwrap_or_default();
    if !policy.reduce_only {
        return Ok(());
    }
    let current = position.active_qty_for(order);
    let current_abs = current
        .checked_abs()
        .ok_or_else(|| QxError::Invariant("当前持仓绝对值溢出".into()))?;
    let reduces_direction =
        (current > 0 && order.side == Side::Sell) || (current < 0 && order.side == Side::Buy);
    if current_abs == 0 || !reduces_direction || order.qty.raw() > current_abs {
        return Err(QxError::BusinessViolation(
            "reduce_only 订单必须只减少目标持仓腿且不得反向穿仓".into(),
        ));
    }
    Ok(())
}

pub trait RiskRule {
    fn name(&self) -> &'static str;
    fn check(&self, o: &Order, pos: &PositionSnapshot) -> QxResult<()>;

    fn check_with_price(
        &self,
        o: &Order,
        pos: &PositionSnapshot,
        reference_price: Option<Price>,
    ) -> QxResult<()> {
        let _ = reference_price;
        self.check(o, pos)
    }
}

/// 单笔最大数量。
pub struct MaxQtyRule {
    pub max_qty: i128,
}

impl RiskRule for MaxQtyRule {
    fn name(&self) -> &'static str {
        "MaxQty"
    }
    fn check(&self, o: &Order, _pos: &PositionSnapshot) -> QxResult<()> {
        if o.qty.raw() > self.max_qty {
            return Err(QxError::BusinessViolation(format!(
                "单笔数量 {} 超过上限 {}",
                o.qty.raw(),
                self.max_qty
            )));
        }
        Ok(())
    }
}

/// 最大名义额。
pub struct MaxNotionalRule {
    pub max_notional: i128,
}

impl RiskRule for MaxNotionalRule {
    fn name(&self) -> &'static str {
        "MaxNotional"
    }
    fn check(&self, o: &Order, pos: &PositionSnapshot) -> QxResult<()> {
        self.check_with_price(o, pos, o.limit)
    }
    fn check_with_price(
        &self,
        o: &Order,
        pos: &PositionSnapshot,
        reference_price: Option<Price>,
    ) -> QxResult<()> {
        let px = o
            .limit
            .or(reference_price)
            .ok_or_else(|| QxError::BusinessViolation("市价单缺少名义额风控参考价".into()))?;
        let current_qty = pos.active_qty_for(o);
        let signed_delta = match o.side {
            Side::Buy => o.qty.raw(),
            Side::Sell => o.qty.raw().saturating_neg(),
        };
        let projected_qty = current_qty.saturating_add(signed_delta);
        let current_abs = current_qty.saturating_abs();
        let projected_abs = projected_qty.saturating_abs();
        let multiplier = pos.multiplier.max(1);
        let current_notional =
            (current_abs.saturating_mul(px.raw()) / 1_000_000_000).saturating_mul(multiplier);
        let projected_notional =
            (projected_abs.saturating_mul(px.raw()) / 1_000_000_000).saturating_mul(multiplier);
        let other_notional = pos.gross_notional.saturating_sub(current_notional);
        if other_notional.saturating_add(projected_notional) > self.max_notional {
            return Err(QxError::BusinessViolation("投影持仓超过最大名义额".into()));
        }
        Ok(())
    }
}

/// 禁止卖空（现货账户常见）。
pub struct NoShortRule;

impl RiskRule for NoShortRule {
    fn name(&self) -> &'static str {
        "NoShort"
    }
    fn check(&self, o: &Order, pos: &PositionSnapshot) -> QxResult<()> {
        let is_sell = matches!(o.side, qx_core::Side::Sell);
        if is_sell && o.qty.raw() > pos.net_qty {
            return Err(QxError::BusinessViolation("禁止卖空".into()));
        }
        Ok(())
    }
}

/// 风控门禁。**不短路**：收集全部拒绝原因，策略需要知道所有问题。
pub struct RiskGate {
    rules: Vec<Box<dyn RiskRule>>,
}

impl Default for RiskGate {
    fn default() -> Self {
        Self::new()
    }
}

impl RiskGate {
    pub fn new() -> Self {
        Self { rules: Vec::new() }
    }

    pub fn add(&mut self, r: Box<dyn RiskRule>) {
        self.rules.push(r);
    }

    pub fn check(&self, o: &Order, pos: &PositionSnapshot) -> QxResult<()> {
        self.check_with_price(o, pos, o.limit)
    }

    pub fn check_with_price(
        &self,
        o: &Order,
        pos: &PositionSnapshot,
        reference_price: Option<Price>,
    ) -> QxResult<()> {
        let mut errs = Vec::new();
        if let Err(error) = validate_reduce_only(o, pos) {
            errs.push(format!("ReduceOnly: {error}"));
        }
        for r in &self.rules {
            if let Err(e) = r.check_with_price(o, pos, reference_price) {
                errs.push(format!("{}: {}", r.name(), e));
            }
        }
        if errs.is_empty() {
            Ok(())
        } else {
            Err(QxError::BusinessViolation(errs.join("; ")))
        }
    }
}

/// 路由候选：附带完整证据，调用方据此决策。
#[derive(Clone, Debug)]
pub struct Candidate {
    pub venue: String,
    pub expected_cost: i128,
    pub available_qty: i128,
    pub reason: String,
}

/// 跨 Venue 路由所需的最小可解释证据。所有费率字段均为基点，延迟为纳秒。
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct VenueQuote {
    pub venue: String,
    pub price: Price,
    pub available_qty: Quantity,
    pub fee_bps: u32,
    pub slippage_bps: u32,
    pub latency_ns: u64,
    pub inventory_penalty_bps: u32,
    pub failure_bps: u32,
    pub settlement_risk_bps: u32,
    pub ready: bool,
    pub margin_available: bool,
}

/// 路由决策：准入过滤通过后的候选集。
pub struct Router {
    default_venue: String,
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

/// 组合层净额后的目标仓位。
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TargetPosition {
    pub instrument: InstrumentId,
    pub target_qty: i128,
    pub source_signals: Vec<u64>,
}

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
    pub fn validate(&self) -> QxResult<()> {
        if self.intent_id == 0
            || self.strategy_id.trim().is_empty()
            || self.account_id.trim().is_empty()
            || self.rule_version.trim().is_empty()
            || self.qty.raw() <= 0
        {
            return Err(QxError::BusinessViolation(
                "OrderIntent 缺少必填字段或数量非法".into(),
            ));
        }
        if self.limit.is_some_and(|price| price.raw() <= 0) {
            return Err(QxError::BusinessViolation(
                "OrderIntent 限价必须为正".into(),
            ));
        }
        Ok(())
    }

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

    /// 在订单意图进入 OMS 前绑定不可变合约规格，拒绝无法在 Venue 精度上
    /// 表达的数量或价格；不做静默截断。
    pub fn validate_against(&self, spec: &TradingInstrumentSpec) -> QxResult<()> {
        self.validate()?;
        if self.instrument != spec.instrument {
            return Err(QxError::BusinessViolation(
                "OrderIntent instrument 与 TradingInstrumentSpec 不一致".into(),
            ));
        }
        spec.validate_order(self.qty.raw(), self.limit.map(|price| price.raw()))
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

/// 组合目标到订单意图的规格化入口。目标差额为零时返回 None；非零差额
/// 必须满足市场最小数量、lot step 和限价 tick，否则返回可解释错误。
pub fn rebalance_intent_with_spec(
    target: &TargetPosition,
    current_qty: i128,
    strategy_id: &str,
    account_id: &str,
    intent_id: u64,
    ts: u64,
    spec: &TradingInstrumentSpec,
) -> QxResult<Option<OrderIntent>> {
    let Some(intent) =
        rebalance_intent(target, current_qty, strategy_id, account_id, intent_id, ts)
    else {
        return Ok(None);
    };
    intent.validate_against(spec)?;
    Ok(Some(intent))
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

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TradingAccount {
    pub account_id: String,
    pub venue_id: String,
    pub currency: String,
    pub enabled: bool,
    pub max_orders: u64,
    pub orders_used: u64,
}

impl TradingAccount {
    pub fn new(
        account_id: impl Into<String>,
        venue_id: impl Into<String>,
        currency: impl Into<String>,
        max_orders: u64,
    ) -> Self {
        Self {
            account_id: account_id.into(),
            venue_id: venue_id.into(),
            currency: currency.into(),
            enabled: true,
            max_orders,
            orders_used: 0,
        }
    }
}

/// 多账户路由器：账户是结算与权限边界，策略不能直接写券商余额。
#[derive(Default)]
pub struct AccountRouter {
    accounts: BTreeMap<String, TradingAccount>,
    strategy_allowlist: BTreeMap<String, Vec<String>>,
}

impl AccountRouter {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn register(&mut self, account: TradingAccount) -> QxResult<()> {
        if account.account_id.trim().is_empty()
            || account.venue_id.trim().is_empty()
            || account.currency.trim().is_empty()
            || account.max_orders == 0
        {
            return Err(QxError::BusinessViolation("交易账户配置非法".into()));
        }
        if self
            .accounts
            .insert(account.account_id.clone(), account.clone())
            .is_some()
        {
            return Err(QxError::Invariant("交易账户重复注册".into()));
        }
        Ok(())
    }
    pub fn allow_strategy(&mut self, strategy_id: &str, account_id: &str) -> QxResult<()> {
        if strategy_id.trim().is_empty() {
            return Err(QxError::BusinessViolation("策略标识不能为空".into()));
        }
        if !self.accounts.contains_key(account_id) {
            return Err(QxError::Permanent("账户不存在".into()));
        }
        self.strategy_allowlist
            .entry(strategy_id.into())
            .or_default()
            .push(account_id.into());
        self.strategy_allowlist.get_mut(strategy_id).unwrap().sort();
        self.strategy_allowlist
            .get_mut(strategy_id)
            .unwrap()
            .dedup();
        Ok(())
    }
    pub fn route(&mut self, strategy_id: &str, preferred: Option<&str>) -> QxResult<String> {
        let allowed = self
            .strategy_allowlist
            .get(strategy_id)
            .ok_or_else(|| QxError::Permanent("策略没有账户白名单".into()))?;
        let candidate = if let Some(preferred) = preferred {
            if !allowed.iter().any(|id| id == preferred) {
                return Err(QxError::BusinessViolation(
                    "策略不允许路由到指定账户".into(),
                ));
            }
            preferred.to_string()
        } else {
            allowed
                .iter()
                .find(|id| {
                    self.accounts
                        .get(*id)
                        .map(|a| a.enabled && a.orders_used < a.max_orders)
                        .unwrap_or(false)
                })
                .cloned()
                .ok_or_else(|| QxError::ResourceExhausted("没有可用交易账户".into()))?
        };
        let account = self.accounts.get_mut(&candidate).unwrap();
        if !account.enabled {
            return Err(QxError::VenueState("交易账户已禁用".into()));
        }
        if account.orders_used >= account.max_orders {
            return Err(QxError::ResourceExhausted("交易账户订单配额已用尽".into()));
        }
        account.orders_used += 1;
        Ok(candidate)
    }
    pub fn accounts(&self) -> &BTreeMap<String, TradingAccount> {
        &self.accounts
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Allocation {
    pub account_id: String,
    pub quantity: i128,
}

/// 按权重分配目标 delta，余数按账户 id 字典序分配，保证可重放。
pub fn allocate_target(
    target_qty: i128,
    weights: &BTreeMap<String, i128>,
) -> QxResult<Vec<Allocation>> {
    if weights.is_empty() {
        return Err(QxError::Permanent("没有账户分配权重".into()));
    }
    let total = weights
        .values()
        .copied()
        .filter(|weight| *weight > 0)
        .try_fold(0_i128, |total, weight| total.checked_add(weight))
        .ok_or_else(|| QxError::BusinessViolation("账户分配权重溢出".into()))?;
    if total <= 0 {
        return Err(QxError::BusinessViolation("账户分配权重必须为正".into()));
    }
    let sign = if target_qty < 0 { -1 } else { 1 };
    let abs = target_qty
        .checked_abs()
        .ok_or_else(|| QxError::BusinessViolation("目标仓位绝对值溢出".into()))?;
    let mut out = Vec::new();
    let mut assigned: i128 = 0;
    for (id, weight) in weights {
        if *weight <= 0 {
            continue;
        }
        let q = abs
            .checked_mul(*weight)
            .and_then(|value| value.checked_div(total))
            .ok_or_else(|| QxError::BusinessViolation("账户分配数量溢出".into()))?;
        assigned = assigned
            .checked_add(q)
            .ok_or_else(|| QxError::BusinessViolation("账户分配累计溢出".into()))?;
        out.push(Allocation {
            account_id: id.clone(),
            quantity: q * sign,
        });
    }
    let remainder = abs
        .checked_sub(assigned)
        .ok_or_else(|| QxError::Invariant("账户分配余数非法".into()))?;
    if remainder > 0 {
        if let Some(first) = out.first_mut() {
            first.quantity += remainder * sign;
        }
    }
    Ok(out)
}

#[derive(Default)]
pub struct AccountCommandQueue {
    queues: BTreeMap<String, VecDeque<OrderIntent>>,
    max_pending: usize,
}

impl AccountCommandQueue {
    pub fn new(max_pending: usize) -> Self {
        Self {
            queues: BTreeMap::new(),
            max_pending,
        }
    }
    pub fn push(&mut self, intent: OrderIntent) -> QxResult<()> {
        intent.validate()?;
        let q = self.queues.entry(intent.account_id.clone()).or_default();
        if q.len() >= self.max_pending {
            return Err(QxError::ResourceExhausted("账户命令队列已满".into()));
        }
        q.push_back(intent);
        Ok(())
    }
    pub fn pop(&mut self, account_id: &str) -> Option<OrderIntent> {
        self.queues
            .get_mut(account_id)
            .and_then(VecDeque::pop_front)
    }
    pub fn pending(&self, account_id: &str) -> usize {
        self.queues.get(account_id).map(VecDeque::len).unwrap_or(0)
    }
}

impl Router {
    pub fn new(default_venue: impl Into<String>) -> Self {
        Self {
            default_venue: default_venue.into(),
        }
    }

    /// 准入过滤优先于价格选择：可交易、可结算、可风控是第一约束。
    pub fn candidates(&self, o: &Order, ready: bool) -> Vec<Candidate> {
        if !ready {
            return vec![];
        }
        vec![Candidate {
            venue: self.default_venue.clone(),
            expected_cost: 0,
            available_qty: o.qty.raw(),
            reason: "唯一可用场所".into(),
        }]
    }

    /// 根据可交易性、可结算性和成本证据确定性排序；不可交易候选不会进入价格比较。
    pub fn rank_candidates(&self, o: &Order, quotes: &[VenueQuote]) -> Vec<Candidate> {
        let mut candidates = quotes
            .iter()
            .filter(|quote| {
                quote.ready
                    && quote.margin_available
                    && quote.available_qty.raw() >= o.qty.raw()
                    && quote.price.raw() > 0
            })
            .map(|quote| {
                let risk_bps = quote
                    .fee_bps
                    .saturating_add(quote.slippage_bps)
                    .saturating_add(quote.inventory_penalty_bps)
                    .saturating_add(quote.failure_bps)
                    .saturating_add(quote.settlement_risk_bps)
                    .saturating_add((quote.latency_ns / 1_000_000) as u32);
                let notional = quote
                    .price
                    .raw()
                    .saturating_mul(o.qty.raw())
                    .saturating_mul(risk_bps as i128);
                let base = quote.price.raw().saturating_mul(o.qty.raw());
                let expected_cost = match o.side {
                    Side::Buy => base.saturating_add(notional / 10_000),
                    Side::Sell => base.saturating_neg().saturating_add(notional / 10_000),
                };
                Candidate {
                    venue: quote.venue.clone(),
                    expected_cost,
                    available_qty: quote.available_qty.raw(),
                    reason: format!(
                        "price={} fee={}bps slippage={}bps latency={}ns inventory={}bps failure={}bps settlement={}bps",
                        quote.price.raw(),
                        quote.fee_bps,
                        quote.slippage_bps,
                        quote.latency_ns,
                        quote.inventory_penalty_bps,
                        quote.failure_bps,
                        quote.settlement_risk_bps,
                    ),
                }
            })
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| {
            left.expected_cost
                .cmp(&right.expected_cost)
                .then_with(|| left.venue.cmp(&right.venue))
        });
        candidates
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
    fn router_filters_and_scores_multi_venue_evidence() {
        let router = Router::new("fallback");
        let order = order(1, Side::Buy);
        let candidates = router.rank_candidates(
            &order,
            &[
                VenueQuote {
                    venue: "slow".into(),
                    price: Price::from_i64(101),
                    available_qty: Quantity::from_i64(1),
                    fee_bps: 10,
                    slippage_bps: 20,
                    latency_ns: 20_000_000,
                    inventory_penalty_bps: 0,
                    failure_bps: 0,
                    settlement_risk_bps: 0,
                    ready: true,
                    margin_available: true,
                },
                VenueQuote {
                    venue: "best".into(),
                    price: Price::from_i64(100),
                    available_qty: Quantity::from_i64(1),
                    fee_bps: 5,
                    slippage_bps: 5,
                    latency_ns: 1_000_000,
                    inventory_penalty_bps: 0,
                    failure_bps: 0,
                    settlement_risk_bps: 0,
                    ready: true,
                    margin_available: true,
                },
                VenueQuote {
                    venue: "not-ready".into(),
                    price: Price::from_i64(1),
                    available_qty: Quantity::from_i64(100),
                    fee_bps: 0,
                    slippage_bps: 0,
                    latency_ns: 0,
                    inventory_penalty_bps: 0,
                    failure_bps: 0,
                    settlement_risk_bps: 0,
                    ready: false,
                    margin_available: true,
                },
            ],
        );
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].venue, "best");
        assert_eq!(candidates[1].venue, "slow");
        assert!(candidates[0].reason.contains("fee=5bps"));
    }

    #[test]
    fn risk_gate_collects_all_reasons() {
        let mut g = RiskGate::new();
        g.add(Box::new(MaxQtyRule {
            max_qty: 5_000_000_000,
        }));
        g.add(Box::new(NoShortRule));
        let o = order(100, Side::Sell);
        let e = g.check(&o, &PositionSnapshot::default()).unwrap_err();
        let msg = format!("{}", e);
        assert!(msg.contains("MaxQty"));
        assert!(msg.contains("NoShort"));
    }

    #[test]
    fn no_short_blocks_overselling() {
        let g = {
            let mut g = RiskGate::new();
            g.add(Box::new(NoShortRule));
            g
        };
        let o = order(10, Side::Sell);
        assert!(g
            .check(&o, &PositionSnapshot::new(5_000_000_000, 0))
            .is_err());
        assert!(g
            .check(&o, &PositionSnapshot::new(10_000_000_000, 0))
            .is_ok());
    }

    #[test]
    fn market_order_requires_reference_price_for_notional_risk() {
        let mut gate = RiskGate::new();
        gate.add(Box::new(MaxNotionalRule {
            max_notional: 50_000_000_000,
        }));
        let mut market = order(1, Side::Buy);
        market.limit = None;
        assert!(gate.check(&market, &PositionSnapshot::new(0, 0)).is_err());
        assert!(gate
            .check_with_price(
                &market,
                &PositionSnapshot::new_with_multiplier(0, 0, 10),
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
    fn account_router_enforces_allowlist_and_quota() {
        let mut r = AccountRouter::new();
        r.register(TradingAccount::new("a", "SIM", "USD", 1))
            .unwrap();
        r.allow_strategy("s1", "a").unwrap();
        assert_eq!(r.route("s1", None).unwrap(), "a");
        assert!(matches!(
            r.route("s1", None),
            Err(QxError::ResourceExhausted(_))
        ));
    }

    #[test]
    fn allocation_is_deterministic_and_preserves_total() {
        let mut weights = BTreeMap::new();
        weights.insert("a".into(), 1);
        weights.insert("b".into(), 2);
        let allocations = allocate_target(10, &weights).unwrap();
        assert_eq!(allocations.iter().map(|x| x.quantity).sum::<i128>(), 10);
        assert_eq!(allocations[0].account_id, "a");
        assert!(allocate_target(i128::MIN, &weights).is_err());
    }

    #[test]
    fn account_command_queue_isolated_by_account() {
        let instrument = InstrumentId::parse("T.V").unwrap();
        let intent = OrderIntent {
            intent_id: 1,
            strategy_id: "s".into(),
            signal_id: None,
            account_id: "a".into(),
            instrument,
            side: Side::Buy,
            qty: Quantity::from_i64(1),
            limit: None,
            created_ts: 1,
            rule_version: "r".into(),
        };
        let mut q = AccountCommandQueue::new(1);
        q.push(intent.clone()).unwrap();
        assert!(matches!(q.push(intent), Err(QxError::ResourceExhausted(_))));
        assert_eq!(q.pop("a").unwrap().intent_id, 1);
    }

    #[test]
    fn account_router_does_not_fallback_from_an_explicit_forbidden_account() {
        let mut router = AccountRouter::new();
        router
            .register(TradingAccount::new("a", "SIM", "USD", 1))
            .unwrap();
        router
            .register(TradingAccount::new("b", "SIM", "USD", 1))
            .unwrap();
        router.allow_strategy("s1", "a").unwrap();
        assert!(router.route("s1", Some("b")).is_err());
    }

    #[test]
    fn invalid_order_intent_is_rejected_and_min_integer_is_safe() {
        let instrument = InstrumentId::parse("T.V").unwrap();
        let invalid = OrderIntent {
            intent_id: 0,
            strategy_id: String::new(),
            signal_id: None,
            account_id: "a".into(),
            instrument: instrument.clone(),
            side: Side::Buy,
            qty: Quantity::ZERO,
            limit: None,
            created_ts: 1,
            rule_version: "v1".into(),
        };
        assert!(invalid.validate().is_err());
        let target = TargetPosition {
            instrument,
            target_qty: i128::MIN,
            source_signals: vec![1],
        };
        assert!(rebalance_intent(&target, 0, "s", "a", 1, 1).is_none());
    }

    #[test]
    fn rebalance_with_instrument_spec_rejects_unaligned_delta() {
        let instrument = InstrumentId::parse("T.V").unwrap();
        let target = TargetPosition {
            instrument: instrument.clone(),
            target_qty: 15,
            source_signals: vec![1],
        };
        let spec = TradingInstrumentSpec {
            instrument,
            product: TradingProduct::Perpetual,
            base_currency: "T".into(),
            quote_currency: "V".into(),
            settlement_currency: "V".into(),
            contract_size: SCALE,
            linear: true,
            inverse: false,
            price_tick: 1,
            qty_step: 10,
            min_qty: 10,
            max_leverage: 10,
            maintenance_margin_bps: 500,
            valid_from: 1,
            valid_to: None,
        };
        assert!(rebalance_intent_with_spec(&target, 0, "s", "a", 1, 1, &spec).is_err());
        let aligned = TargetPosition {
            target_qty: 20,
            ..target
        };
        assert!(
            rebalance_intent_with_spec(&aligned, 0, "s", "a", 1, 1, &spec)
                .unwrap()
                .is_some()
        );
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
            .validate_order(&candidate, &PositionSnapshot::default())
            .is_err());
        let context = RiskContext {
            available_margin_raw: Some(required),
            ..context
        };
        assert!(context
            .validate_order(&candidate, &PositionSnapshot::default())
            .is_ok());
        let mut missing_spec = context;
        missing_spec.instrument_spec = None;
        assert!(missing_spec
            .validate_order(&candidate, &PositionSnapshot::default())
            .is_err());
    }
}
