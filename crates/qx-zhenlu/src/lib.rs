//! # qx-zhenlu — 针路
//!
//! 执行与路由：风控门禁 → OMS → 路由 → 适配器。
//!
//! 原则：**RiskGate 可以拒绝，但不得静默改写业务含义。**
pub mod algo;
pub use algo::{
    plan_twap, plan_vwap, AlgoChildSlice, AlgoExecutionError, VwapVolumeBucket, MAX_ALGO_CHILDREN,
};
pub mod oms;
pub mod portfolio;
pub mod venue_capabilities;
pub use oms::Oms;
// 目标仓位是全仓唯一概念（定义在 qx-core，V10 §4.8）；此处只做出口别名。
pub use qx_core::TargetPosition;
use qx_core::{
    DecimalRounding, ExactDecimal, FeeModel, Fill, InstrumentId, Order, OrderStatus, OrderTrace,
    Price, Quantity, QxError, QxResult, Side, TradingInstrumentSpec,
};
use qx_guanxing::QuoteTick;
use qx_risk::{
    ExactOrderRiskFacts, OrderRiskContext as CanonicalOrderRiskContext, OrderRiskPosition,
    RiskRule, RuleSet,
};
#[cfg(test)]
use qx_risk::{MaxNotionalRule, MaxQtyRule, NoShortRule};
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
    pub exact: Option<ExactOrderRiskFacts>,
}

impl RiskContext {
    fn canonical_context(&self, position: &OrderRiskPosition) -> CanonicalOrderRiskContext {
        CanonicalOrderRiskContext {
            available_margin_raw: self.available_margin_raw,
            projected_initial_margin_raw: None,
            reference_price: self.reference_price,
            instrument_spec: self.instrument_spec.clone(),
            max_order_notional_raw: self.max_order_notional_raw,
            max_position_notional_raw: self.max_position_notional_raw,
            position: *position,
            exact: self.exact,
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
    /// 规则的解释实现只有一份（`qx_risk::RuleSet`），三条路径按各自缺的输入选不同方法（逐个 grep
    /// 生产调用点确认，旧注释说"三条路都经由本方法"是错的）：意图闸门走 `RiskGate::check*` →
    /// `RuleSet::evaluate`，提交端口走 `evaluate_order` → `account_limits_only`，
    /// 本方法这条组合口径只有 `risk_parity.rs` 拿它做三入口一致性取证。
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

/// 旧 `RiskGate` 调用形状到规范风控上下文的转换：只带持仓快照与参考价；
/// `check_with_spec` 可补产品规格，但旧形状仍无法提供可用保证金与账户级限额。
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
// 规则实现已上收到 `qx-risk::RuleSet`；旧 check/check_with_price/check_with_spec 入口仍
// 保留 `(Order, OrderRiskPosition)` 形状，`check_with_context` 供完整风控快照路径调用。
// 构造必须显式命名规则集（[`RiskGate::from_rule_set`] / [`RiskGate::conservative_default`]），
// 不再提供零规则即放行的 `new()`/`Default`（V10 §4.5）。
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
        self.check_with_spec(o, pos, reference_price, None)
    }

    /// 带产品规格的判定入口（V13 R26）：`MaxNotionalRule` 缺规格即 fail-closed，
    /// 配置了名义额上限的调用方必须走这里传真实规格；`check_with_price` 传 `None`。
    pub fn check_with_spec(
        &self,
        o: &Order,
        pos: &OrderRiskPosition,
        reference_price: Option<Price>,
        instrument_spec: Option<&TradingInstrumentSpec>,
    ) -> QxResult<()> {
        let mut context = legacy_gate_context(pos, reference_price);
        context.instrument_spec = instrument_spec.cloned();
        let decision = self.rule_set.evaluate(&context, o);
        if decision.allowed {
            Ok(())
        } else {
            Err(QxError::BusinessViolation(decision.violations.join("; ")))
        }
    }

    /// Evaluate a complete risk snapshot through the canonical account and static rule path.
    /// Portfolio backtests can supply their projected initial margin so the facade does not
    /// recompute it from only the candidate order quantity.
    pub fn check_with_context(
        &self,
        o: &Order,
        context: &CanonicalOrderRiskContext,
    ) -> QxResult<()> {
        let decision = self.rule_set.evaluate(context, o);
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
    /// Net strategy targets deterministically. Quantity overflow is rejected instead of
    /// silently changing the requested portfolio exposure through integer saturation.
    pub fn merge(&self, mut signals: Vec<Signal>, now: u64) -> QxResult<Vec<TargetPosition>> {
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
            entry.target_qty = entry.target_qty.checked_add(s.target_qty).ok_or_else(|| {
                QxError::BusinessViolation(format!(
                    "策略信号目标仓位溢出: instrument={}, strategy={}, signal_id={}",
                    s.instrument, s.strategy_id, s.signal_id
                ))
            })?;
            entry.source_signals.push(s.signal_id);
        }
        Ok(merged.into_values().collect())
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

            exact_amounts: None,
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
    FeeAdjustment {
        account_id: String,
        venue_id: String,
        order_id: u64,
        instrument: InstrumentId,
        fees: Vec<qx_core::FillFee>,
        ts: u64,
    },
    Cancelled {
        client_order_id: u64,
        ts: u64,
    },
}

/// 连接器的健康档位。断开不是一档状态而是一条义务：`disconnect()` 直接把档位钉成
/// `ReconcileRequired`，`reconnect()` 立刻进 `Snapshotting`（没有异步握手期），"降级"则由
/// 监管侧的 `ServiceStatus::Degraded` 承担 —— 故 `Disconnected`/`Connecting`/`Degraded` 已删。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ConnectorState {
    Snapshotting,
    Live,
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
    /// Exact remote cumulative fill when the exchange returns it as a decimal string.
    pub filled_exact: Option<ExactDecimal>,
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
    ExactFilledMismatch {
        client_order_id: u64,
        local: Option<ExactDecimal>,
        venue: Option<ExactDecimal>,
    },
}

/// Venue 适配器的最小执行契约。适配器只返回事实事件，不直接改 OMS 或账簿。
pub trait Venue {
    fn id(&self) -> &str;
    fn capabilities(&self) -> &'static venue_capabilities::VenueCapabilities;
    /// Check that the venue's current product metadata still agrees with the frozen risk spec.
    /// Most venues do not have a live metadata RPC at this boundary; adapters that do should
    /// override this and fail closed before `submit` when the contract has drifted.
    fn validate_instrument_spec(&mut self, _spec: &TradingInstrumentSpec) -> QxResult<()> {
        Ok(())
    }
    /// Validate order-kind-specific rules that do not fit in the frozen instrument spec,
    /// such as a venue's separate market-order lot size. The common live risk path calls this
    /// after metadata validation and before committing the order intent or writing remotely.
    fn validate_order_constraints(
        &mut self,
        _spec: &TradingInstrumentSpec,
        _order: &Order,
    ) -> QxResult<()> {
        Ok(())
    }
    /// Reject execution while the connector is disconnected or still recovering.
    /// Adapters with richer synchronization state should override this check.
    fn execution_readiness(&self) -> QxResult<()> {
        if self.connected() {
            Ok(())
        } else {
            Err(QxError::VenueState(format!(
                "Venue {} is disconnected",
                self.id()
            )))
        }
    }
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
    pub venue_id: String,
    pub instrument: InstrumentId,
    pub account_id: String,
    pub side: Side,
    pub qty: Quantity,
    /// Exact base quantity when the original order uses scale-aware amounts.
    /// `qty` remains populated for legacy 1e9 orders and is zero for exact orders.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qty_exact: Option<ExactDecimal>,
    pub attempt: u32,
}

impl SpreadCompensationTarget {
    pub fn quantity_decimal(&self) -> ExactDecimal {
        self.qty_exact.unwrap_or_else(|| {
            ExactDecimal::from_parts(self.qty.raw(), 9).unwrap_or(ExactDecimal::ZERO)
        })
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct SpreadHedgeProgress {
    // The containing map defaults for legacy groups; once a progress record exists,
    // require its quantity so truncated state cannot masquerade as zero compensation.
    pub hedged_qty: Quantity,
    /// Scale-aware compensation progress. Legacy snapshots omit this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hedged_qty_exact: Option<ExactDecimal>,
    #[serde(default)]
    pub attempt: u32,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
/// In-memory reducer for a multi-leg order group.
///
/// The `record_*` methods are public for external execution coordinators that own their
/// event/persistence boundary. Durable hosts in this workspace use `apply_order_snapshot`
/// with authoritative OMS/EventLog state so duplicate replay remains idempotent.
pub struct SpreadOrderGroup {
    pub group_id: String,
    pub strategy_id: String,
    pub status: SpreadOrderGroupStatus,
    pub legs: Vec<SpreadOrderLeg>,
    /// The order whose submit request has crossed the local durable boundary but has not yet
    /// produced an authoritative venue outcome. This is deliberately persisted separately
    /// from the aggregate status so a restart cannot mistake a still-Planned group for idle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub submission_in_flight_order_id: Option<u64>,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub hedge_progress: std::collections::BTreeMap<String, SpreadHedgeProgress>,
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
            submission_in_flight_order_id: None,
            hedge_progress: std::collections::BTreeMap::new(),
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
            if !leg.order.filled_decimal().is_zero() {
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
        if let Some(client_order_id) = self.submission_in_flight_order_id {
            if !self
                .legs
                .iter()
                .any(|leg| leg.order.client_id == client_order_id)
            {
                return Err(QxError::BusinessViolation(format!(
                    "持久化多腿提交意图引用了未知订单腿: {client_order_id}"
                )));
            }
        }
        for (leg_id, progress) in &self.hedge_progress {
            let leg = self.leg(leg_id)?;
            let hedged = progress.hedged_qty_exact.unwrap_or_else(|| {
                ExactDecimal::from_parts(progress.hedged_qty.raw(), 9).unwrap_or(ExactDecimal::ZERO)
            });
            if hedged.is_negative()
                || hedged
                    .checked_cmp(leg.order.filled_decimal())
                    .is_none_or(|ordering| ordering.is_gt())
            {
                return Err(QxError::BusinessViolation(format!(
                    "持久化多腿补偿数量超出原腿已成交范围: leg={leg_id}"
                )));
            }
        }
        let mut structure = self.clone();
        structure.hedge_progress.clear();
        for leg in &mut structure.legs {
            let exceeds_quantity = if let Some(amounts) = leg.order.exact_amounts {
                amounts.quote_budget.is_none()
                    && leg
                        .order
                        .filled_decimal()
                        .checked_cmp(amounts.quantity)
                        .is_none_or(|ordering| ordering.is_gt())
            } else {
                leg.order.filled.raw() < 0 || leg.order.filled.raw() > leg.order.qty.raw()
            };
            if exceeds_quantity {
                return Err(QxError::BusinessViolation(
                    "持久化多腿状态的成交数量超出订单范围".into(),
                ));
            }
            leg.order.filled = Quantity::ZERO;
            if let Some(amounts) = leg.order.exact_amounts.as_mut() {
                amounts.filled = ExactDecimal::ZERO;
                amounts.quote_filled = ExactDecimal::ZERO;
            }
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

    /// Mark a leg accepted in an external coordinator's in-memory group projection.
    pub fn record_accepted(&mut self, leg_id: &str) -> QxResult<()> {
        let leg = self.leg_mut(leg_id)?;
        Self::accept_leg_order(leg)?;
        self.recompute_status();
        Ok(())
    }

    /// Apply one confirmed fill to an in-memory leg projection.
    pub fn record_fill(&mut self, leg_id: &str, fill: &Fill) -> QxResult<()> {
        fill.validate_exact_amounts()
            .map_err(QxError::BusinessViolation)?;
        let leg = self.leg_mut(leg_id)?;
        if fill.order_id != leg.order.client_id || !fill.quantity_decimal().is_positive() {
            return Err(QxError::BusinessViolation(
                "SpreadOrderGroup Fill 与腿订单不匹配".into(),
            ));
        }
        if leg.order.exact_amounts.is_some() != fill.exact_amounts.is_some() {
            return Err(QxError::BusinessViolation(
                "SpreadOrderGroup 腿订单与成交的金额精度不一致".into(),
            ));
        }
        let next = leg
            .order
            .filled_decimal()
            .checked_add(fill.quantity_decimal())
            .ok_or_else(|| QxError::Invariant("SpreadOrderGroup 成交数量溢出".into()))?;
        if let Some(amounts) = leg.order.exact_amounts.as_mut() {
            if amounts.quote_budget.is_none()
                && next
                    .checked_cmp(amounts.quantity)
                    .is_none_or(|ordering| ordering.is_gt())
            {
                return Err(QxError::BusinessViolation(
                    "SpreadOrderGroup 成交数量超过腿订单数量".into(),
                ));
            }
            amounts.filled = next;
        } else {
            let raw = next
                .to_raw(9)
                .map_err(|error| QxError::BusinessViolation(error.into()))?;
            if raw > leg.order.qty.raw() {
                return Err(QxError::BusinessViolation(
                    "SpreadOrderGroup 成交数量超过腿订单数量".into(),
                ));
            }
            leg.order.filled = Quantity::from_raw(raw);
        }
        Self::accept_leg_order(leg)?;
        let fully_filled = leg.order.exact_amounts.map_or_else(
            || leg.order.filled == leg.order.qty,
            |amounts| {
                amounts.quote_budget.is_none()
                    && amounts.filled.checked_cmp(amounts.quantity)
                        == Some(std::cmp::Ordering::Equal)
            },
        );
        leg.order.status = if fully_filled {
            OrderStatus::Filled
        } else {
            OrderStatus::PartiallyFilled
        };
        self.recompute_status();
        Ok(())
    }

    /// Replace a leg with the authoritative OMS snapshot. This makes the group reducer
    /// idempotent and preserves exact fills and quote-budget accounting from the EventLog.
    pub fn apply_order_snapshot(&mut self, snapshot: &Order) -> QxResult<()> {
        let leg = self
            .legs
            .iter_mut()
            .find(|leg| leg.order.client_id == snapshot.client_id)
            .ok_or_else(|| {
                QxError::BusinessViolation(format!(
                    "SpreadOrderGroup 不存在订单腿: {}",
                    snapshot.client_id
                ))
            })?;
        let old_status = leg.order.status;
        let new_status = snapshot.status;
        let status_regressed = (old_status.is_terminal() && old_status != new_status)
            || matches!(
                (old_status, new_status),
                (OrderStatus::Submitted, OrderStatus::PendingSubmit)
                    | (
                        OrderStatus::Accepted,
                        OrderStatus::PendingSubmit | OrderStatus::Submitted
                    )
                    | (
                        OrderStatus::Working,
                        OrderStatus::PendingSubmit | OrderStatus::Submitted | OrderStatus::Accepted
                    )
                    | (
                        OrderStatus::PartiallyFilled,
                        OrderStatus::PendingSubmit
                            | OrderStatus::Submitted
                            | OrderStatus::Accepted
                            | OrderStatus::Working
                    )
                    | (
                        OrderStatus::CancelPending,
                        OrderStatus::PendingSubmit
                            | OrderStatus::Submitted
                            | OrderStatus::Accepted
                            | OrderStatus::Working
                            | OrderStatus::PartiallyFilled
                    )
                    | (
                        OrderStatus::Unknown,
                        OrderStatus::PendingSubmit
                            | OrderStatus::Submitted
                            | OrderStatus::CancelPending
                    )
            );
        let exact_terms_match = match (leg.order.exact_amounts, snapshot.exact_amounts) {
            (Some(old), Some(new)) => {
                old.quantity == new.quantity
                    && old.limit_price == new.limit_price
                    && old.quote_budget == new.quote_budget
                    && old.market_reference_price == new.market_reference_price
            }
            (None, None) => leg.order.qty == snapshot.qty && leg.order.limit == snapshot.limit,
            _ => false,
        };
        if leg.order.instrument != snapshot.instrument
            || leg.order.side != snapshot.side
            || leg.order.account_id != snapshot.account_id
            || leg.order.trace != snapshot.trace
            || leg.order.policy != snapshot.policy
            || !exact_terms_match
            || status_regressed
            || leg
                .order
                .filled_decimal()
                .checked_cmp(snapshot.filled_decimal())
                .is_none_or(|ordering| ordering.is_gt())
        {
            return Err(QxError::BusinessViolation(
                "SpreadOrderGroup EventLog 订单快照身份不一致或成交量倒退".into(),
            ));
        }
        snapshot.validate().map_err(QxError::BusinessViolation)?;
        leg.order = snapshot.clone();
        if self.submission_in_flight_order_id == Some(snapshot.client_id)
            && matches!(
                snapshot.status,
                OrderStatus::Accepted
                    | OrderStatus::Working
                    | OrderStatus::PartiallyFilled
                    | OrderStatus::Filled
                    | OrderStatus::Rejected
                    | OrderStatus::Expired
                    | OrderStatus::Cancelled
                    | OrderStatus::Unknown
            )
        {
            // Accepted/terminal facts resolve the submit attempt. Unknown remains fail-closed
            // through the aggregate ReconcileRequired status after recomputation.
            self.submission_in_flight_order_id = None;
        }
        self.recompute_status();
        Ok(())
    }

    /// Record a venue rejection in an external coordinator's group projection.
    pub fn record_rejected(&mut self, leg_id: &str) -> QxResult<()> {
        let leg = self.leg_mut(leg_id)?;
        if matches!(
            leg.order.status,
            OrderStatus::Filled | OrderStatus::Cancelled | OrderStatus::Expired
        ) {
            return Err(QxError::VenueState(
                "终态腿不能被不同终态回报覆盖为 Rejected".into(),
            ));
        }
        if leg.order.status != OrderStatus::Rejected {
            leg.order.status = OrderStatus::Rejected;
        }
        self.recompute_status();
        Ok(())
    }

    /// Record a confirmed cancellation in an external coordinator's group projection.
    pub fn record_cancelled(&mut self, leg_id: &str) -> QxResult<()> {
        let leg = self.leg_mut(leg_id)?;
        if matches!(
            leg.order.status,
            OrderStatus::Filled | OrderStatus::Rejected
        ) {
            return Err(QxError::VenueState(
                "终态腿不能被不同终态回报覆盖为 Cancelled".into(),
            ));
        }
        if !matches!(
            leg.order.status,
            OrderStatus::Cancelled | OrderStatus::Expired
        ) {
            leg.order.status = OrderStatus::Cancelled;
        }
        self.recompute_status();
        Ok(())
    }

    /// Mark an uncertain venue outcome as requiring reconciliation before more legs submit.
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
            SpreadOrderGroupStatus::ReconcileRequired
                | SpreadOrderGroupStatus::HedgeRequired
                | SpreadOrderGroupStatus::Hedged
                | SpreadOrderGroupStatus::Filled
                | SpreadOrderGroupStatus::Failed
                | SpreadOrderGroupStatus::Cancelled
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
            .filter_map(|leg| {
                let progress = self
                    .hedge_progress
                    .get(&leg.leg_id)
                    .copied()
                    .unwrap_or_default();
                let filled = leg.order.filled_decimal();
                let hedged = progress.hedged_qty_exact.unwrap_or_else(|| {
                    ExactDecimal::from_parts(progress.hedged_qty.raw(), 9)
                        .unwrap_or(ExactDecimal::ZERO)
                });
                let outstanding = filled.checked_sub(hedged)?;
                outstanding.is_positive().then(|| SpreadCompensationTarget {
                    leg_id: leg.leg_id.clone(),
                    venue_id: leg.venue_id.clone(),
                    instrument: leg.order.instrument.clone(),
                    account_id: leg.order.account_id.clone(),
                    side: match leg.order.side {
                        Side::Buy => Side::Sell,
                        Side::Sell => Side::Buy,
                    },
                    qty: if leg.order.exact_amounts.is_some() {
                        Quantity::ZERO
                    } else {
                        Quantity::from_raw(outstanding.to_raw(9).unwrap_or(0))
                    },
                    qty_exact: leg.order.exact_amounts.map(|_| outstanding),
                    attempt: progress.attempt,
                })
            })
            .collect()
    }

    /// Record a legacy scale fill for an external in-memory compensation projection.
    pub fn record_compensation_fill(&mut self, leg_id: &str, qty: Quantity) -> QxResult<()> {
        if qty.raw() <= 0 {
            return Err(QxError::BusinessViolation("补偿成交数量必须为正".into()));
        }
        let exact = ExactDecimal::from_parts(qty.raw(), 9)
            .map_err(|error| QxError::BusinessViolation(error.into()))?;
        self.record_compensation_fill_exact(leg_id, exact)
    }

    pub fn record_compensation_fill_exact(
        &mut self,
        leg_id: &str,
        qty: ExactDecimal,
    ) -> QxResult<()> {
        if !qty.is_positive() {
            return Err(QxError::BusinessViolation("补偿成交数量必须为正".into()));
        }
        let leg = self.leg(leg_id)?;
        let current = self.hedge_progress.get(leg_id).copied().unwrap_or_default();
        let next = current
            .hedged_qty_exact
            .unwrap_or_else(|| {
                ExactDecimal::from_parts(current.hedged_qty.raw(), 9).unwrap_or(ExactDecimal::ZERO)
            })
            .checked_add(qty)
            .ok_or_else(|| QxError::Invariant("累计补偿数量溢出".into()))?;
        if next
            .checked_cmp(leg.order.filled_decimal())
            .is_none_or(|ordering| ordering.is_gt())
        {
            return Err(QxError::BusinessViolation(
                "补偿成交数量超过原腿已确认敞口".into(),
            ));
        }
        let (hedged_qty, hedged_qty_exact) = if leg.order.exact_amounts.is_some() {
            (Quantity::ZERO, Some(next))
        } else {
            (
                Quantity::from_raw(
                    next.to_raw(9)
                        .map_err(|error| QxError::BusinessViolation(error.into()))?,
                ),
                None,
            )
        };
        self.hedge_progress.insert(
            leg_id.to_string(),
            SpreadHedgeProgress {
                hedged_qty,
                hedged_qty_exact,
                attempt: current.attempt,
            },
        );
        Ok(())
    }

    pub fn advance_compensation_attempt(&mut self, leg_id: &str) -> QxResult<u32> {
        self.leg(leg_id)?;
        let progress = self.hedge_progress.entry(leg_id.to_string()).or_default();
        progress.attempt = progress
            .attempt
            .checked_add(1)
            .ok_or_else(|| QxError::Invariant("补偿尝试计数溢出".into()))?;
        Ok(progress.attempt)
    }

    pub fn all_confirmed_exposure_hedged(&self) -> bool {
        self.legs.iter().all(|leg| {
            leg.order.status.is_terminal()
                && self
                    .hedge_progress
                    .get(&leg.leg_id)
                    .map_or(ExactDecimal::ZERO, |progress| {
                        progress.hedged_qty_exact.unwrap_or_else(|| {
                            ExactDecimal::from_parts(progress.hedged_qty.raw(), 9)
                                .unwrap_or(ExactDecimal::ZERO)
                        })
                    })
                    .checked_cmp(leg.order.filled_decimal())
                    .is_some_and(|ordering| !ordering.is_lt())
        })
    }

    pub fn require_reconciliation(&mut self) {
        self.status = SpreadOrderGroupStatus::ReconcileRequired;
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
        if !self.all_confirmed_exposure_hedged() {
            return Err(QxError::VenueState(
                "仍有未完成的跨账户/交易所补偿敞口，不能标记为 Hedged".into(),
            ));
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
        ) = SpreadGroupAttribution::sum(&legs)?;
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
            cost_bps: SpreadGroupAttribution::cost_bps(net_cost_raw, total_turnover_raw)?,
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
                )));
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
        let any_filled = self
            .legs
            .iter()
            .any(|leg| leg.order.filled_decimal().is_positive());
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
        let totals = Self::sum(&self.legs)?;
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
        if self.cost_bps != Self::cost_bps(self.net_cost_raw, self.total_turnover_raw)? {
            return Err("多腿归因成本 bps 与明细不一致".into());
        }
        Ok(())
    }

    fn sum(legs: &[SpreadLegAttribution]) -> Result<(i128, i128, i128, i128, i128), String> {
        legs.iter()
            .try_fold((0_i128, 0_i128, 0_i128, 0_i128, 0_i128), |acc, leg| {
                Ok((
                    acc.0
                        .checked_add(leg.filled_qty_raw)
                        .ok_or("多腿归因成交数量合计溢出")?,
                    acc.1
                        .checked_add(leg.turnover_raw)
                        .ok_or("多腿归因成交额合计溢出")?,
                    acc.2
                        .checked_add(leg.fees_raw)
                        .ok_or("多腿归因费用合计溢出")?,
                    acc.3
                        .checked_add(leg.margin_raw)
                        .ok_or("多腿归因保证金合计溢出")?,
                    acc.4
                        .checked_add(leg.funding_raw)
                        .ok_or("多腿归因资金费合计溢出")?,
                ))
            })
    }

    fn cost_bps(net_cost_raw: i128, turnover_raw: i128) -> Result<i64, String> {
        if turnover_raw <= 0 {
            return Ok(0);
        }
        let scaled = net_cost_raw
            .checked_mul(10_000)
            .ok_or("多腿归因成本 bps 缩放溢出")?;
        let bps = scaled
            .checked_div(turnover_raw)
            .ok_or("多腿归因成本 bps 除法溢出")?;
        i64::try_from(bps).map_err(|_| "多腿归因成本 bps 超出 i64 范围".into())
    }
}

/// 多腿订单组的持久化端口。实现必须以 `group_id` 幂等保存完整状态，
/// 使执行 worker 重启后可以继续对账/补偿，而不是重新提交已确认的腿。
pub trait SpreadOrderGroupStore {
    fn load(&self, group_id: &str) -> Result<Option<SpreadOrderGroup>, String>;
    fn save(&mut self, group: &SpreadOrderGroup) -> Result<(), String>;
    /// Atomically reduce an authoritative local OMS order snapshot into its group.
    /// Backends must serialize the read-modify-write so concurrent venue workers cannot
    /// overwrite one another's leg progress with stale snapshots.
    fn apply_order_snapshot(&mut self, group_id: &str, snapshot: &Order) -> Result<(), String>;
    fn delete(&mut self, group_id: &str) -> Result<(), String>;

    /// Claim a group for a single recovery owner. Backends must implement a
    /// durable lease; silently acting as a single-owner store is unsafe when
    /// recovery workers overlap.
    fn try_claim(
        &mut self,
        group_id: &str,
        owner: &str,
        now: u64,
        lease_ms: u64,
    ) -> Result<Option<u64>, String>;

    fn verify_claim(
        &mut self,
        group_id: &str,
        owner: &str,
        token: u64,
        now: u64,
    ) -> Result<bool, String>;

    fn save_claimed(
        &mut self,
        group: &SpreadOrderGroup,
        owner: &str,
        token: u64,
        now: u64,
    ) -> Result<(), String>;

    fn release_claim(&mut self, group_id: &str, owner: &str, token: u64) -> Result<(), String>;

    /// Persist an in-flight submit intent before the EventLog registration and venue call.
    /// Repeating the same client order ID is idempotent; a different in-flight leg is left
    /// for the queue to retry after the current result is synchronized.
    fn begin_leg_submission(&self, group_id: &str, client_order_id: u64) -> Result<(), String>;
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

    fn claim_matches_locked(
        &self,
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
        Ok(current_owner == Some(owner)
            && current_token == Some(token)
            && expires_at.is_some_and(|expires_at| expires_at > now))
    }

    fn save_locked(&self, group: &SpreadOrderGroup) -> Result<(), String> {
        let path = self.path_for(&group.group_id)?;
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
        Ok(())
    }
}

impl SpreadOrderGroupStore for FileSpreadOrderGroupStore {
    fn load(&self, group_id: &str) -> Result<Option<SpreadOrderGroup>, String> {
        let _lock = qx_core::FileLock::acquire(self.lock_path())
            .map_err(|error| format!("获取多腿状态读锁失败: {error}"))?;
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
        // 写锁交给 qx-core::file_lock：进程被杀留下的 `.spread-groups.lock` 在旧实现里会让
        // 多腿状态此后每一次写入都失败，且错误只有一句 OS 文案（V11 §40 D1）。
        let _lock = qx_core::FileLock::acquire(self.lock_path())
            .map_err(|error| format!("获取多腿状态写锁失败: {error}"))?;
        self.save_locked(group)
    }

    fn apply_order_snapshot(&mut self, group_id: &str, snapshot: &Order) -> Result<(), String> {
        let _lock = qx_core::FileLock::acquire(self.lock_path())
            .map_err(|error| format!("获取多腿订单快照归约锁失败: {error}"))?;
        let path = self.path_for(group_id)?;
        let payload = std::fs::read_to_string(&path)
            .map_err(|error| format!("读取多腿状态 {} 失败: {error}", path.display()))?;
        let mut group: SpreadOrderGroup = serde_json::from_str(&payload)
            .map_err(|error| format!("解析多腿状态 {} 失败: {error}", path.display()))?;
        group
            .validate_persisted()
            .map_err(|error| format!("多腿状态 {} 校验失败: {error:?}", path.display()))?;
        if group.group_id != group_id {
            return Err(format!("多腿状态文件标识与请求不一致: {group_id}"));
        }
        group
            .apply_order_snapshot(snapshot)
            .map_err(|error| format!("归约多腿订单快照失败: {error:?}"))?;
        group
            .validate_persisted()
            .map_err(|error| format!("归约后多腿状态无效: {error:?}"))?;
        self.save_locked(&group)
    }

    fn begin_leg_submission(&self, group_id: &str, client_order_id: u64) -> Result<(), String> {
        let _lock = qx_core::FileLock::acquire(self.lock_path())
            .map_err(|error| format!("获取多腿提交意图锁失败: {error}"))?;
        let path = self.path_for(group_id)?;
        let payload = std::fs::read_to_string(&path)
            .map_err(|error| format!("读取多腿状态 {} 失败: {error}", path.display()))?;
        let mut group: SpreadOrderGroup = serde_json::from_str(&payload)
            .map_err(|error| format!("解析多腿状态 {} 失败: {error}", path.display()))?;
        group
            .validate_persisted()
            .map_err(|error| format!("多腿状态 {} 校验失败: {error:?}", path.display()))?;
        if !group
            .legs
            .iter()
            .any(|leg| leg.order.client_id == client_order_id)
        {
            return Err(format!(
                "多腿订单组 {group_id} 不包含订单 {client_order_id}"
            ));
        }
        if group.blocks_new_leg_submission() {
            return Err(format!(
                "FAIL_CLOSED: 多腿订单组 {group_id} 状态为 {:?}，禁止开始新腿提交",
                group.status
            ));
        }
        match group.submission_in_flight_order_id {
            Some(existing) if existing != client_order_id => {
                return Err(format!(
                    "RETRYABLE: 多腿订单组 {group_id} 的订单 {existing} 正在提交，等待其结果同步后重试"
                ));
            }
            Some(_) => return Ok(()),
            None => {}
        }
        group.submission_in_flight_order_id = Some(client_order_id);
        group
            .validate_persisted()
            .map_err(|error| format!("多腿提交意图无效: {error:?}"))?;
        self.save_locked(&group)
    }

    fn delete(&mut self, group_id: &str) -> Result<(), String> {
        let _lock = qx_core::FileLock::acquire(self.lock_path())
            .map_err(|error| format!("获取多腿状态写锁失败: {error}"))?;
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
        let _lock = qx_core::FileLock::acquire(self.lock_path())
            .map_err(|error| format!("获取多腿恢复 claim 锁失败: {error}"))?;
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
                            continue;
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
                            continue;
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
        let _lock = qx_core::FileLock::acquire(self.lock_path())
            .map_err(|error| format!("获取多腿恢复 claim 锁失败: {error}"))?;
        self.claim_matches_locked(group_id, owner, token, now)
    }

    fn save_claimed(
        &mut self,
        group: &SpreadOrderGroup,
        owner: &str,
        token: u64,
        now: u64,
    ) -> Result<(), String> {
        group
            .validate_persisted()
            .map_err(|error| format!("保存多腿状态前校验失败: {error:?}"))?;
        let _lock = qx_core::FileLock::acquire(self.lock_path())
            .map_err(|error| format!("获取多腿恢复 claim 锁失败: {error}"))?;
        if !self.claim_matches_locked(&group.group_id, owner, token, now)? {
            return Err("多腿恢复 claim 已过期或已被其他 owner 接管".into());
        }
        self.save_locked(group)
    }

    fn release_claim(&mut self, group_id: &str, owner: &str, token: u64) -> Result<(), String> {
        let _lock = qx_core::FileLock::acquire(self.lock_path())
            .map_err(|error| format!("获取多腿恢复 claim 锁失败: {error}"))?;
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
    fee_model: Box<dyn FeeModel + Send>,
    instrument_specs: BTreeMap<InstrumentId, TradingInstrumentSpec>,
}

fn exact_from_legacy_raw(raw: i128) -> QxResult<ExactDecimal> {
    ExactDecimal::from_parts(raw, 9).map_err(|error| QxError::Invariant(error.into()))
}

fn exact_projection_raw(value: ExactDecimal) -> QxResult<i128> {
    value
        .checked_mul_at_scale(ExactDecimal::ONE, 9, DecimalRounding::TowardZero)
        .and_then(|projection| projection.to_raw(9).ok())
        .ok_or_else(|| QxError::Invariant("精确数量无法投影到 1e-9 兼容字段".into()))
}

fn exact_quote_value(
    raw: i128,
    exact: Option<ExactDecimal>,
    label: &str,
) -> QxResult<ExactDecimal> {
    let value = exact.unwrap_or(exact_from_legacy_raw(raw)?);
    if !value.is_positive() || exact.is_some_and(|_| exact_projection_raw(value).ok() != Some(raw))
    {
        return Err(QxError::BusinessViolation(format!(
            "Paper L1 {label} 非正数或与兼容投影不一致"
        )));
    }
    Ok(value)
}

fn exact_min(left: ExactDecimal, right: ExactDecimal) -> QxResult<ExactDecimal> {
    match left.checked_cmp(right) {
        Some(std::cmp::Ordering::Greater) => Ok(right),
        Some(_) => Ok(left),
        None => Err(QxError::Invariant("Paper 精确金额比较失败".into())),
    }
}

fn floor_exact_to_step(value: ExactDecimal, step: ExactDecimal) -> QxResult<ExactDecimal> {
    let units = value
        .checked_div_at_scale(step, 0, DecimalRounding::TowardZero)
        .ok_or_else(|| QxError::Invariant("Paper 数量步进除法失败".into()))?;
    units
        .checked_mul_at_scale(step, ExactDecimal::MAX_SCALE, DecimalRounding::TowardZero)
        .ok_or_else(|| QxError::Invariant("Paper 数量步进回乘溢出".into()))
}

impl PaperVenue {
    /// 构造 Paper 虚拟 Venue。费用模型**必须显式给出**：Paper 的 `Fill.fee` 要与
    /// 回测、实盘同一口径，"默认零费"会让同一条成交在执行平面上悄悄变便宜
    /// （V11 §4.1 的缺陷形状）。不计费的冒烟测试显式传 `Box::new(ZeroFeeModel)`。
    pub fn new(id: impl Into<String>, fee_model: Box<dyn FeeModel + Send>) -> Self {
        Self {
            id: id.into(),
            connected: true,
            orders: BTreeMap::new(),
            next_venue_id: 1,
            state: ConnectorState::Live,
            last_event_ts: 0,
            last_source_seq: 0,
            reconnects: 0,
            fee_model,
            instrument_specs: BTreeMap::new(),
        }
    }

    /// Register the frozen product rules used for exact Paper admission and matching.
    pub fn set_instrument_spec(&mut self, spec: TradingInstrumentSpec) -> QxResult<()> {
        spec.validate()?;
        if let Some(existing) = self.instrument_specs.get(&spec.instrument) {
            if existing != &spec {
                return Err(QxError::ReconcileRequired(format!(
                    "Paper Venue 收到冲突的冻结规格: {}",
                    spec.instrument
                )));
            }
            return Ok(());
        }
        self.instrument_specs.insert(spec.instrument.clone(), spec);
        Ok(())
    }

    /// 当前费用模型描述子，用于运行清单与配置校验的可观测性。
    /// 冻结的执行平面成本口径：只有本文件的 golden 用例读它，生产侧一律走 `fee_model.descriptor()`。
    /// 把它当公共面会让"descriptor 变了就是指纹变了"看起来像在保护一条并不存在的外部契约（V12 §16）。
    #[cfg(test)]
    pub(crate) fn fee_descriptor(&self) -> String {
        self.fee_model.descriptor()
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

    /// Restore orders and the next deterministic venue sequence from the
    /// account's durable EventLog. Workers that recreate PaperVenue per bounded
    /// execution cycle must use this to avoid reusing a venue order ID.
    pub fn restore_orders_with_next_venue_id<I>(
        &mut self,
        orders: I,
        next_venue_id: u64,
    ) -> QxResult<()>
    where
        I: IntoIterator<Item = Order>,
    {
        self.restore_orders(orders)?;
        if next_venue_id == 0 {
            return Err(QxError::BusinessViolation(
                "PaperVenue next_venue_id 必须为正数".into(),
            ));
        }
        self.next_venue_id = self.next_venue_id.max(next_venue_id);
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
                    let local_exact = local.exact_amounts.map(|amounts| amounts.filled);
                    if local_exact.is_some() || remote.filled_exact.is_some() {
                        if local_exact != remote.filled_exact {
                            issues.push(VenueReconcileIssue::ExactFilledMismatch {
                                client_order_id: *id,
                                local: local_exact,
                                venue: remote.filled_exact,
                            });
                        }
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

    /// Legacy convenience wrapper. Production execution paths use `try_on_quote` so an exact
    /// amount, fee or specification error cannot be mistaken for an empty matching result.
    pub fn on_quote(&mut self, instrument: &InstrumentId, quote: QuoteTick) -> Vec<VenueEvent> {
        self.try_on_quote(instrument, quote).unwrap_or_default()
    }

    /// Match one L1 quote. Exact orders use the quote's scale-bearing price and displayed size,
    /// frozen instrument rules, and exact fee path; legacy orders remain on the 1e-9 grid.
    /// State is staged on a cloned order map and committed only after all fills validate.
    pub fn try_on_quote(
        &mut self,
        instrument: &InstrumentId,
        quote: QuoteTick,
    ) -> QxResult<Vec<VenueEvent>> {
        if !self.connected || !matches!(self.state, ConnectorState::Live) {
            return Ok(Vec::new());
        }
        if quote.source_seq != 0 && quote.source_seq <= self.last_source_seq {
            return Ok(Vec::new());
        }
        let bid = exact_quote_value(quote.bid.raw(), quote.bid_exact, "bid")?;
        let ask = exact_quote_value(quote.ask.raw(), quote.ask_exact, "ask")?;
        if bid.checked_cmp(ask).is_none_or(|ordering| ordering.is_gt()) {
            return Err(QxError::BusinessViolation("Paper 行情买卖价交叉".into()));
        }
        let bid_size = exact_quote_value(quote.bid_qty.raw(), quote.bid_qty_exact, "bid quantity")?;
        let ask_size = exact_quote_value(quote.ask_qty.raw(), quote.ask_qty_exact, "ask quantity")?;
        for order in self
            .orders
            .values()
            .filter(|order| &order.instrument == instrument && !order.status.is_terminal())
            .filter(|order| order.exact_amounts.is_some())
        {
            if !self.instrument_specs.contains_key(&order.instrument) {
                return Err(QxError::BusinessViolation(format!(
                    "Paper 精确订单缺少冻结 market spec: {}",
                    order.instrument
                )));
            }
        }

        let mut orders = self.orders.clone();
        let mut remaining_buy = ask_size;
        let mut remaining_sell = bid_size;
        let mut out = Vec::new();
        // BTreeMap order gives deterministic FIFO by client order id.
        for (id, order) in &mut orders {
            if &order.instrument != instrument || order.status.is_terminal() {
                continue;
            }
            let price = match order.side {
                Side::Buy => ask,
                Side::Sell => bid,
            };
            let price_ok = match order.limit_price_decimal() {
                Some(limit) => match price.checked_cmp(limit) {
                    Some(std::cmp::Ordering::Less | std::cmp::Ordering::Equal)
                        if order.side == Side::Buy =>
                    {
                        true
                    }
                    Some(std::cmp::Ordering::Greater | std::cmp::Ordering::Equal)
                        if order.side == Side::Sell =>
                    {
                        true
                    }
                    Some(_) => false,
                    None => {
                        return Err(QxError::Invariant("Paper 限价与最新价精确比较失败".into()));
                    }
                },
                None => true,
            };
            if !price_ok {
                continue;
            }
            let available = match order.side {
                Side::Buy => &mut remaining_buy,
                Side::Sell => &mut remaining_sell,
            };
            if !available.is_positive() {
                continue;
            }

            if let Some(mut amounts) = order.exact_amounts {
                let spec = self
                    .instrument_specs
                    .get(&order.instrument)
                    .ok_or_else(|| QxError::BusinessViolation("Paper exact spec missing".into()))?;
                let quantity_step = spec
                    .exact_rules
                    .as_ref()
                    .map(|rules| rules.quantity_step)
                    .unwrap_or(exact_from_legacy_raw(spec.qty_step)?);
                let max_by_order = if let Some(budget) = amounts.quote_budget {
                    let remaining_budget = budget
                        .checked_sub(amounts.quote_filled)
                        .ok_or_else(|| QxError::Invariant("Paper quote budget 下溢".into()))?;
                    remaining_budget
                        .checked_div_at_scale(
                            price,
                            ExactDecimal::MAX_SCALE,
                            DecimalRounding::TowardZero,
                        )
                        .ok_or_else(|| QxError::Invariant("Paper quote budget 除法失败".into()))?
                } else {
                    amounts
                        .quantity
                        .checked_sub(amounts.filled)
                        .ok_or_else(|| QxError::Invariant("Paper exact remaining 下溢".into()))?
                };
                let raw_cap = exact_min(max_by_order, *available)?;
                let quantity = floor_exact_to_step(raw_cap, quantity_step)?;
                if !quantity.is_positive() {
                    continue;
                }
                spec.validate_exact_fill(quantity, price)?;
                let quote_cost = spec.quote_cash_exact(
                    quantity,
                    price,
                    if order.side == Side::Buy {
                        DecimalRounding::AwayFromZero
                    } else {
                        DecimalRounding::TowardZero
                    },
                )?;
                if let Some(budget) = amounts.quote_budget {
                    let remaining_budget = budget
                        .checked_sub(amounts.quote_filled)
                        .ok_or_else(|| QxError::Invariant("Paper quote budget 下溢".into()))?;
                    if quote_cost
                        .checked_cmp(remaining_budget)
                        .is_none_or(|ordering| ordering.is_gt())
                    {
                        // A conservative buy debit can round up by one unit at scale 18.
                        // Skip this quote instead of overspending or looping over tiny lots.
                        continue;
                    }
                    amounts.quote_filled = amounts
                        .quote_filled
                        .checked_add(quote_cost)
                        .ok_or_else(|| QxError::Invariant("Paper quote fill 累计溢出".into()))?;
                }
                amounts.filled = amounts
                    .filled
                    .checked_add(quantity)
                    .ok_or_else(|| QxError::Invariant("Paper exact fill 累计溢出".into()))?;
                let fee_basis = spec.notional_exact(ExactDecimal::ONE, price)?;
                // Maker/taker classification matches the Bar matcher: only an explicit
                // post-only order receives maker treatment.
                let is_maker = order.policy.as_ref().is_some_and(|policy| policy.post_only);
                let fee = self
                    .fee_model
                    .commission_exact(quantity, fee_basis, order.side, is_maker)?;
                if fee.is_negative() {
                    return Err(QxError::BusinessViolation(
                        "Paper 精确费用模型返回负手续费".into(),
                    ));
                }
                let fee_currency = (!fee.is_zero()).then(|| spec.settlement_currency.clone());
                order.exact_amounts = Some(amounts);
                if matches!(order.status, OrderStatus::Accepted) {
                    order
                        .status
                        .transition(OrderStatus::Working)
                        .map_err(QxError::Invariant)?;
                }
                let fully_filled = amounts.quote_budget.map_or_else(
                    || {
                        amounts
                            .filled
                            .checked_cmp(amounts.quantity)
                            .is_some_and(|ordering| !ordering.is_lt())
                    },
                    |budget| {
                        amounts
                            .quote_filled
                            .checked_cmp(budget)
                            .is_some_and(|ordering| !ordering.is_lt())
                    },
                );
                order
                    .status
                    .transition(if fully_filled {
                        OrderStatus::Filled
                    } else {
                        OrderStatus::PartiallyFilled
                    })
                    .map_err(QxError::Invariant)?;
                *available = available
                    .checked_sub(quantity)
                    .ok_or_else(|| QxError::Invariant("Paper exact quote depth 下溢".into()))?;
                let mut fill = Fill {
                    order_id: *id,
                    qty: Quantity::ZERO,
                    price: Price::ZERO,
                    fee: qx_core::Money::ZERO,
                    ts: quote.ts,
                    account_id: order.account_id.clone(),
                    fee_currency: fee_currency.clone(),
                    exact_amounts: Some(qx_core::ExactFillAmounts {
                        quantity,
                        price,
                        quote_cost: Some(quote_cost),
                        quote_budget_complete: false,
                    }),
                    ..Fill::default()
                };
                if !fee.is_zero() {
                    fill.exact_fees.push(qx_core::ExactFillFee {
                        amount: fee,
                        currency: fee_currency,
                        source_trade_ids: Vec::new(),
                    });
                }
                order.trace_fill(&mut fill, Some(&self.id), None);
                out.push(VenueEvent::Fill(fill));
            } else {
                let px = match order.side {
                    Side::Buy => quote.ask,
                    Side::Sell => quote.bid,
                };
                if px.raw() <= 0 {
                    continue;
                }
                let available_raw = exact_projection_raw(*available)?.max(0);
                let qty = Quantity::from_raw(order.remaining().raw().min(available_raw));
                if qty.is_zero() {
                    continue;
                }
                let exact_qty = exact_from_legacy_raw(qty.raw())?;
                *available = available
                    .checked_sub(exact_qty)
                    .ok_or_else(|| QxError::Invariant("Paper legacy quote depth 下溢".into()))?;
                if matches!(order.status, OrderStatus::Accepted) {
                    order
                        .status
                        .transition(OrderStatus::Working)
                        .map_err(QxError::Invariant)?;
                }
                order.filled = Quantity::from_raw(
                    order
                        .filled
                        .raw()
                        .checked_add(qty.raw())
                        .ok_or_else(|| QxError::Invariant("Paper raw fill 累计溢出".into()))?,
                );
                order
                    .status
                    .transition(if order.filled.raw() >= order.qty.raw() {
                        OrderStatus::Filled
                    } else {
                        OrderStatus::PartiallyFilled
                    })
                    .map_err(QxError::Invariant)?;
                let is_maker = order.policy.as_ref().is_some_and(|policy| policy.post_only);
                let fee = self.fee_model.commission_for_side(
                    qty.raw(),
                    px.raw(),
                    order.side,
                    is_maker,
                )?;
                let mut fill = Fill {
                    order_id: *id,
                    qty,
                    price: px,
                    fee: qx_core::Money::from_raw(fee),
                    ts: quote.ts,
                    account_id: order.account_id.clone(),
                    ..Fill::default()
                };
                order.trace_fill(&mut fill, Some(&self.id), None);
                out.push(VenueEvent::Fill(fill));
            }
        }
        self.orders = orders;
        self.last_event_ts = quote.ts;
        if quote.source_seq != 0 {
            self.last_source_seq = quote.source_seq;
        }
        Ok(out)
    }
}

impl Venue for PaperVenue {
    fn id(&self) -> &str {
        &self.id
    }

    fn capabilities(&self) -> &'static venue_capabilities::VenueCapabilities {
        &venue_capabilities::PAPER_VENUE_CAPABILITIES
    }

    fn submit(&mut self, mut order: Order, ts: u64) -> QxResult<Vec<VenueEvent>> {
        self.capabilities()
            .validate_submit(&order, self.id())
            .map_err(QxError::BusinessViolation)?;
        order.validate().map_err(QxError::BusinessViolation)?;
        if let Some(amounts) = order.exact_amounts {
            let spec = self
                .instrument_specs
                .get(&order.instrument)
                .ok_or_else(|| {
                    QxError::BusinessViolation(format!(
                        "Paper exact submit 缺少冻结 market spec: {}",
                        order.instrument
                    ))
                })?;
            if spec.instrument != order.instrument {
                return Err(QxError::BusinessViolation(
                    "Paper exact submit 的 market spec 标的不匹配".into(),
                ));
            }
            if let Some(budget) = amounts.quote_budget {
                spec.validate_exact_quote_budget(budget)?;
            } else {
                spec.validate_exact_order_with_reference(
                    amounts.quantity,
                    amounts.limit_price,
                    amounts.market_reference_price,
                )?;
            }
        }
        self.fee_model.validate()?;
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
        let next_venue_id = self
            .next_venue_id
            .checked_add(1)
            .ok_or_else(|| QxError::Invariant("PaperVenue venue_order_id 序号溢出".into()))?;
        order.status = OrderStatus::Accepted;
        self.orders.insert(order.client_id, order.clone());
        let venue_id = format!("{}-{}", self.id, self.next_venue_id);
        self.next_venue_id = next_venue_id;
        Ok(vec![VenueEvent::Accepted {
            client_order_id: order.client_id,
            venue_order_id: venue_id,
            ts,
        }])
    }

    fn cancel(&mut self, client_order_id: u64, ts: u64) -> QxResult<Vec<VenueEvent>> {
        self.capabilities()
            .validate_cancel()
            .map_err(QxError::BusinessViolation)?;
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
                filled_exact: o.exact_amounts.map(|amounts| amounts.filled),
            })
            .collect()
    }

    fn connected(&self) -> bool {
        self.connected
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

/// 策略生命周期档位：`stop()` 直接把 `Running` 收成 `Stopped`，故无"正在停"这一档（V12 §23）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StrategyState {
    Registered,
    Initialized,
    Running,
    Paused,
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
#[path = "paper_venue_sequence_tests.rs"]
mod paper_venue_sequence_tests;

#[cfg(test)]
mod tests {

    use super::*;

    use qx_core::{
        ExactDecimal, ExactTradingRules, InstrumentId, Money, Price, Side, TradingInstrumentSpec,
        TradingProduct, SCALE,
    };
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
            exact_amounts: None,
        }
    }

    /// 与 Bar 回测装配同一默认费率的 Paper Venue；零费只在生命周期冒烟用例里显式出现。
    fn shared_rate_paper() -> PaperVenue {
        PaperVenue::new(
            "paper",
            Box::new(qx_core::MakerTakerFeeModel::default_maker_taker()),
        )
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

    fn exact_order(client_id: u64, instrument: &str, side: Side, quantity: ExactDecimal) -> Order {
        Order {
            client_id,
            instrument: InstrumentId::parse(instrument).unwrap(),
            side,
            qty: Quantity::ZERO,
            limit: None,
            status: OrderStatus::Submitted,
            filled: Quantity::ZERO,
            account_id: "exact-account".into(),
            trace: None,
            policy: None,
            exact_amounts: Some(qx_core::ExactOrderAmounts {
                quantity,
                limit_price: None,
                filled: ExactDecimal::ZERO,
                quote_budget: None,
                quote_filled: ExactDecimal::ZERO,
                market_reference_price: Some(ExactDecimal::parse("100").unwrap()),
            }),
        }
    }

    fn tiny_exact_spot_spec(instrument: &InstrumentId) -> TradingInstrumentSpec {
        TradingInstrumentSpec {
            instrument: instrument.clone(),
            product: TradingProduct::Spot,
            base_currency: "TINY".into(),
            quote_currency: "USDT".into(),
            settlement_currency: "USDT".into(),
            contract_size: SCALE,
            linear: false,
            inverse: false,
            price_tick: 1,
            qty_step: 1,
            min_qty: 1,
            min_notional_raw: 0,
            max_leverage: 1,
            maintenance_margin_bps: 0,
            valid_from: 0,
            valid_to: None,
            exact_rules: Some(ExactTradingRules {
                contract_size: ExactDecimal::ONE,
                price_tick: ExactDecimal::parse("0.0000000001").unwrap(),
                quantity_step: ExactDecimal::parse("0.0000000001").unwrap(),
                minimum_quantity: ExactDecimal::parse("0.0000000001").unwrap(),
                minimum_notional: ExactDecimal::ZERO,
            }),
        }
    }

    #[test]
    fn paper_venue_matches_sub_nano_order_against_exact_l1_size_and_price() {
        let instrument = InstrumentId::parse("TINY/USDT.OKX").unwrap();
        let mut venue = PaperVenue::new("paper", Box::new(qx_core::ZeroFeeModel));
        venue
            .set_instrument_spec(tiny_exact_spot_spec(&instrument))
            .unwrap();
        let quantity = ExactDecimal::parse("0.0000000003").unwrap();
        let mut order = exact_order(9_801, "TINY/USDT.OKX", Side::Buy, quantity);
        order.status = OrderStatus::Submitted;
        venue.submit(order, 1).unwrap();

        let quote = QuoteTick::new(
            2,
            Price::from_raw(9),
            Quantity::ZERO,
            Price::from_raw(10),
            Quantity::ZERO,
            1,
        )
        .with_exact_prices(
            ExactDecimal::parse("0.0000000099").unwrap(),
            ExactDecimal::parse("0.0000000100").unwrap(),
        )
        .with_exact_sizes(
            ExactDecimal::parse("0.0000000005").unwrap(),
            ExactDecimal::parse("0.0000000002").unwrap(),
        );
        let events = venue.try_on_quote(&instrument, quote).unwrap();
        let [VenueEvent::Fill(fill)] = events.as_slice() else {
            panic!("expected one exact partial fill, got {events:?}");
        };
        let exact_fill = fill.exact_amounts.unwrap();
        assert_eq!(
            exact_fill.quantity,
            ExactDecimal::parse("0.0000000002").unwrap()
        );
        assert_eq!(
            exact_fill.price,
            ExactDecimal::parse("0.0000000100").unwrap()
        );
        assert_eq!(
            exact_fill.quote_cost,
            Some(ExactDecimal::parse("0.000000000000000002").unwrap())
        );
        assert_eq!(fill.qty, Quantity::ZERO);
        assert_eq!(fill.price, Price::ZERO);
        assert_eq!(venue.snapshot()[0].status, OrderStatus::PartiallyFilled);
        assert_eq!(
            venue.snapshot()[0].filled_exact,
            Some(ExactDecimal::parse("0.0000000002").unwrap())
        );
    }

    #[test]
    fn paper_venue_records_exact_fee_in_the_instrument_settlement_currency() {
        let instrument = InstrumentId::parse("TINY/USDT.OKX").unwrap();
        let mut venue = PaperVenue::new(
            "paper",
            Box::new(qx_core::MakerTakerFeeModel {
                maker_bp: 2,
                taker_bp: 5,
            }),
        );
        venue
            .set_instrument_spec(tiny_exact_spot_spec(&instrument))
            .unwrap();
        let mut order = exact_order(
            9_803,
            "TINY/USDT.OKX",
            Side::Buy,
            ExactDecimal::parse("0.0000000002").unwrap(),
        );
        order.status = OrderStatus::Submitted;
        venue.submit(order, 1).unwrap();
        let quote = QuoteTick::new(
            2,
            Price::from_i64(99),
            Quantity::ZERO,
            Price::from_i64(100),
            Quantity::ZERO,
            1,
        )
        .with_exact_prices(
            ExactDecimal::parse("99").unwrap(),
            ExactDecimal::parse("100").unwrap(),
        )
        .with_exact_sizes(
            ExactDecimal::parse("0.0000000005").unwrap(),
            ExactDecimal::parse("0.0000000002").unwrap(),
        );

        let events = venue.try_on_quote(&instrument, quote).unwrap();
        let [VenueEvent::Fill(fill)] = events.as_slice() else {
            panic!("expected one exact fee fill, got {events:?}");
        };
        assert_eq!(fill.fee_currency.as_deref(), Some("USDT"));
        assert_eq!(fill.exact_fees.len(), 1);
        assert_eq!(fill.exact_fees[0].currency.as_deref(), Some("USDT"));
        assert_eq!(
            fill.exact_fees[0].amount,
            ExactDecimal::parse("0.00000000001").unwrap()
        );
    }

    #[test]
    fn paper_venue_rejects_exact_submit_without_frozen_spec() {
        let mut venue = PaperVenue::new(
            "paper",
            Box::new(qx_core::MakerTakerFeeModel {
                maker_bp: 2,
                taker_bp: 5,
            }),
        );
        let order = exact_order(
            9_802,
            "TINY/USDT.OKX",
            Side::Buy,
            ExactDecimal::parse("0.0000000002").unwrap(),
        );
        let error = venue.submit(order, 1).unwrap_err();
        assert!(error.to_string().contains("缺少冻结 market spec"));
        assert!(venue.snapshot().is_empty());
    }

    #[test]
    fn spread_group_exact_small_coin_fill_produces_exact_compensation_target() {
        let order_qty = ExactDecimal::parse("0.0000000008").unwrap();
        let fill_qty = ExactDecimal::parse("0.0000000003").unwrap();
        let first = exact_order(801, "TINY.V1", Side::Buy, order_qty);
        let second = exact_order(802, "TINY2.V2", Side::Sell, order_qty);
        let mut group = SpreadOrderGroup::new(
            "spread-exact-small-coin",
            "tiny-pair",
            vec![
                SpreadOrderLeg {
                    leg_id: "buy".into(),
                    venue_id: "V1".into(),
                    order: first.clone(),
                },
                SpreadOrderLeg {
                    leg_id: "sell".into(),
                    venue_id: "V2".into(),
                    order: second.clone(),
                },
            ],
        )
        .unwrap();
        let mut first_snapshot = first;
        first_snapshot.status = OrderStatus::Cancelled;
        first_snapshot.exact_amounts.as_mut().unwrap().filled = fill_qty;
        group.apply_order_snapshot(&first_snapshot).unwrap();
        let mut second_snapshot = second;
        second_snapshot.status = OrderStatus::Cancelled;
        group.apply_order_snapshot(&second_snapshot).unwrap();

        assert_eq!(group.status, SpreadOrderGroupStatus::HedgeRequired);
        let target = group.compensation_targets().pop().unwrap();
        assert_eq!(target.qty, Quantity::ZERO);
        assert_eq!(target.qty_exact, Some(fill_qty));
        group
            .record_compensation_fill_exact("buy", fill_qty)
            .unwrap();
        assert!(group.all_confirmed_exposure_hedged(), "group={group:#?}");
    }

    #[test]
    fn spread_group_file_store_applies_concurrent_leg_snapshots_atomically() {
        let root = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!(
                "qianxing-spread-atomic-snapshots-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
        let group = two_leg_group();
        let mut store = FileSpreadOrderGroupStore::new(&root).unwrap();
        store.save(&group).unwrap();
        let mut first = group.legs[0].order.clone();
        first.status = OrderStatus::Filled;
        first.filled = first.qty;
        let mut second = group.legs[1].order.clone();
        second.status = OrderStatus::Filled;
        second.filled = second.qty;
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let paths = [root.clone(), root.clone()];
        let snapshots = [first, second];
        let handles = paths
            .into_iter()
            .zip(snapshots)
            .map(|(path, snapshot)| {
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    let mut store = FileSpreadOrderGroupStore::new(path).unwrap();
                    barrier.wait();
                    store
                        .apply_order_snapshot("spread-attr", &snapshot)
                        .unwrap();
                })
            })
            .collect::<Vec<_>>();
        for handle in handles {
            handle.join().unwrap();
        }
        let restored = store.load("spread-attr").unwrap().unwrap();
        assert_eq!(restored.status, SpreadOrderGroupStatus::Filled);
        assert!(restored
            .legs
            .iter()
            .all(|leg| leg.order.status == OrderStatus::Filled));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn spread_group_legacy_state_defaults_missing_progress_but_rejects_partial_progress() {
        let group = two_leg_group();
        let mut legacy = serde_json::to_value(&group).unwrap();
        legacy.as_object_mut().unwrap().remove("hedge_progress");
        let restored: SpreadOrderGroup = serde_json::from_value(legacy.clone()).unwrap();
        assert!(restored.hedge_progress.is_empty());

        legacy["hedge_progress"] = serde_json::json!({ "primary": { "attempt": 1 } });
        assert!(serde_json::from_value::<SpreadOrderGroup>(legacy).is_err());
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
    fn spread_group_attribution_rejects_overflowing_leg_totals() {
        let error = two_leg_group()
            .attribute(vec![
                SpreadLegAttribution::new("primary", i128::MAX, 0, 0, 0, 0).unwrap(),
                SpreadLegAttribution::new("reference", 1, 0, 0, 0, 0).unwrap(),
            ])
            .unwrap_err();
        assert!(error.contains("成交数量合计溢出"), "{error}");
    }

    #[test]
    fn spread_group_attribution_rejects_cost_bps_overflow() {
        let error = two_leg_group()
            .attribute(vec![
                SpreadLegAttribution::new("primary", 0, 1, i128::MAX, 0, 0).unwrap(),
                SpreadLegAttribution::new("reference", 0, 0, 0, 0, 0).unwrap(),
            ])
            .unwrap_err();
        assert!(error.contains("成本 bps 缩放溢出"), "{error}");
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
        assert_eq!(group.legs[1].order.status, OrderStatus::Cancelled);
        assert!(!group.all_confirmed_exposure_hedged());
        group.record_cancelled("buy-leg").unwrap();
        group
            .record_compensation_fill("buy-leg", Quantity::from_i64(4))
            .unwrap();
        assert!(group.all_confirmed_exposure_hedged());
        group.mark_hedged().unwrap();
        assert_eq!(group.status, SpreadOrderGroupStatus::Hedged);
        assert!(group.blocks_new_leg_submission());
    }

    #[test]
    fn spread_group_file_store_round_trips_in_progress_fills() {
        let root = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!(
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
        let root = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!(
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
    fn claimed_group_save_is_atomic_with_lease_ownership() {
        let root = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!(
                "qianxing-spread-claim-save-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
        let mut first = FileSpreadOrderGroupStore::new(&root).unwrap();
        let mut second = FileSpreadOrderGroupStore::new(&root).unwrap();
        let group = SpreadOrderGroup::new(
            "spread-claim-save",
            "claim-test",
            vec![
                SpreadOrderLeg {
                    leg_id: "first".into(),
                    venue_id: "venue-a".into(),
                    order: order(1, Side::Buy),
                },
                SpreadOrderLeg {
                    leg_id: "second".into(),
                    venue_id: "venue-b".into(),
                    order: order(1, Side::Sell),
                },
            ],
        )
        .unwrap();
        first.save(&group).unwrap();

        let first_token = first
            .try_claim("spread-claim-save", "worker-a", 100, 10)
            .unwrap()
            .unwrap();
        let second_token = second
            .try_claim("spread-claim-save", "worker-b", 111, 10)
            .unwrap()
            .expect("the second worker may claim after expiry");

        let mut next = group.clone();
        next.begin_submission().unwrap();
        second
            .save_claimed(&next, "worker-b", second_token, 111)
            .unwrap();

        assert!(first
            .save_claimed(&group, "worker-a", first_token, 112)
            .is_err());
        assert_eq!(
            first.load("spread-claim-save").unwrap().unwrap().status,
            SpreadOrderGroupStatus::Submitting
        );
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
        assert!(group.blocks_new_leg_submission());
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
        let missing_spec = gate
            .check_with_price(
                &market,
                &OrderRiskPosition::new_with_multiplier(0, 0, 10),
                Some(Price::from_i64(100)),
            )
            .unwrap_err()
            .to_string();
        assert!(
            missing_spec.contains("TradingInstrumentSpec"),
            "configured notional risk must reject a missing product specification: {missing_spec}"
        );
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
        assert!(oms
            .all_orders()
            .iter()
            .all(|order| order.status.is_terminal()));
    }

    #[test]
    fn paper_venue_accepts_and_fills_from_l1() {
        let instrument = InstrumentId::parse("T.V").unwrap();
        let mut venue = PaperVenue::new("paper", Box::new(qx_core::ZeroFeeModel));
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
    fn paper_venue_charges_fills_with_the_shared_fee_model() {
        let instrument = InstrumentId::parse("T.V").unwrap();
        let fee_of = |post_only: bool, id: u64| {
            let mut venue = shared_rate_paper();
            let mut o = order(1, Side::Buy);
            o.client_id = id;
            if post_only {
                o.policy = Some(qx_core::OrderPolicy {
                    post_only: true,
                    ..qx_core::OrderPolicy::default()
                });
            }
            venue.submit(o, 10).unwrap();
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
            match events.first() {
                Some(VenueEvent::Fill(fill)) => fill.fee.raw(),
                _ => panic!("post_only={post_only} 应产生成交"),
            }
        };
        // 名义 100.0：吃单 5bp = 0.05，挂单 2bp = 0.02
        assert_eq!(fee_of(false, 1), qx_core::SCALE / 20);
        assert_eq!(fee_of(true, 2), qx_core::SCALE / 50);
        // 冻结的执行平面成本口径：descriptor 变了就是指纹变了，须与 Bar 回测装配同步审阅。
        assert_eq!(
            shared_rate_paper().fee_descriptor(),
            "MakerTaker@v1[params=maker_bp=2;taker_bp=5]"
        );
    }

    #[test]
    fn disconnected_submit_is_ambiguous() {
        let mut venue = PaperVenue::new("paper", Box::new(qx_core::ZeroFeeModel));
        venue.disconnect();
        assert!(matches!(
            venue.submit(order(1, Side::Buy), 1),
            Err(QxError::Ambiguous(_))
        ));
    }

    #[test]
    fn reconnect_requires_snapshot_reconciliation() {
        let mut venue = PaperVenue::new("paper", Box::new(qx_core::ZeroFeeModel));
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
            min_notional_raw: 0,
            max_leverage: 20,
            maintenance_margin_bps: 500,
            valid_from: 1,
            valid_to: None,
            exact_rules: None,
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
