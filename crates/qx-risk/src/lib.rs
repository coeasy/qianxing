use qx_core::{
    DecimalRounding, ExactDecimal, Order, PositionMode, PositionSide, Price, QxError, QxResult,
    Side, TradingInstrumentSpec,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

mod pending_exposure;
pub mod rules;

pub use pending_exposure::{
    pending_order_gross_notional, pending_order_gross_notional_at_reference_mark,
    pending_order_gross_notional_exact, pending_order_gross_notional_exact_at_reference_mark,
};

pub use rules::{
    MaxNotionalRule, MaxQtyRule, NoShortRule, RiskRule, RuleSet,
    CONSERVATIVE_DEFAULT_RULE_SET_VERSION, CONSERVATIVE_MAX_QTY_RAW,
};

/// 组合级风控快照。
///
/// `RiskEngine::evaluate` 只读 `portfolio_id` / `gross_exposure` / `drawdown_bps` /
/// `factor_exposure` 四格；`timestamp` / `net_exposure` / `volatility_bps` 三格在仓库内
/// **没有任何生产读者**（`volatility_bps` 在唯一的仓库内构造点恒为 0）。
/// 按 #118/#171 先例保留不删，并登记在 `maturity/capabilities.yaml` 的
/// `canonical_order_risk_decision` → `risk_snapshot_carries_fields_no_decision_reads`。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RiskSnapshot {
    pub portfolio_id: String,
    pub timestamp: u64,
    pub gross_exposure: i128,
    pub net_exposure: i128,
    pub volatility_bps: u32,
    pub drawdown_bps: i32,
    pub factor_exposure: BTreeMap<String, i32>,
}

impl RiskSnapshot {
    pub fn validate(&self) -> Result<(), String> {
        if self.portfolio_id.trim().is_empty() {
            return Err("portfolio id is required".into());
        }
        if self.gross_exposure < 0 {
            return Err("gross exposure cannot be negative".into());
        }
        if self.drawdown_bps > 0 {
            return Err("drawdown cannot be positive".into());
        }
        if self.factor_exposure.keys().any(|key| key.trim().is_empty()) {
            return Err("factor id cannot be empty".into());
        }
        Ok(())
    }
}

/// 组合级快照风控动作。
///
/// 历史上 `decision::RiskAction` 与本枚举变体完全重复且零调用，已随死代码删除；
/// 本枚举是仓库内唯一的组合级动作表示。`evaluate()` 只产下面三档，第四档
/// `Rebalance` 没有任何生产者也没有任何一条判定臂，随 V12 §23 删除。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum RiskDecision {
    Allow,
    Reject,
    Reduce,
}

pub struct RiskEngine;

impl RiskEngine {
    pub fn evaluate(
        snapshot: &RiskSnapshot,
        max_exposure: i128,
        max_drawdown_bps: i32,
    ) -> RiskDecision {
        if snapshot.validate().is_err() {
            return RiskDecision::Reject;
        }
        if snapshot.gross_exposure > max_exposure {
            return RiskDecision::Reduce;
        }
        if snapshot.drawdown_bps < max_drawdown_bps {
            return RiskDecision::Reject;
        }
        RiskDecision::Allow
    }
}

/// 订单级风控输入的唯一规范表示。
///
/// 回测、Paper 和实盘执行都必须在调用 Venue 前使用同一套规格、保证金、
/// 减仓和投影持仓逻辑。上层可以保留自己的账户快照类型，但应在边界处
/// 转换到这里，避免在不同执行器中复制一套“看起来相同”的风控。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OrderRiskPosition {
    pub net_qty: i128,
    pub gross_notional: i128,
    pub multiplier: i128,
    pub long_qty: i128,
    pub short_qty: i128,
}

impl OrderRiskPosition {
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

    pub fn active_qty_for(self, order: &Order) -> i128 {
        let policy = order.policy.unwrap_or_default();
        if policy.position_mode == PositionMode::Hedge {
            match policy.position_side {
                PositionSide::Long => self.long_qty,
                PositionSide::Short => self.short_qty,
                PositionSide::Net => self.net_qty,
            }
        } else {
            self.net_qty
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct OrderRiskContext {
    pub available_margin_raw: Option<i128>,
    /// Portfolio-derived initial margin after applying the candidate order. When present,
    /// this replaces the isolated single-order estimate in `validate_order`.
    pub projected_initial_margin_raw: Option<i128>,
    pub reference_price: Option<Price>,
    pub instrument_spec: Option<TradingInstrumentSpec>,
    pub max_order_notional_raw: Option<i128>,
    pub max_position_notional_raw: Option<i128>,
    pub position: OrderRiskPosition,
    /// Complete scale-aware account facts are required before the risk layer can evaluate an
    /// exact order. Raw-only live snapshots deliberately do not populate this field.
    pub exact: Option<ExactOrderRiskFacts>,
}

/// Position facts used when evaluating an order whose quantity or price exceeds the legacy
/// 1e-9 grid. Gross notional includes all positions in the account/portfolio represented by the
/// enclosing risk context; leg quantities identify the position affected by this order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExactOrderRiskPosition {
    pub net_qty: ExactDecimal,
    pub gross_notional: ExactDecimal,
    pub long_qty: ExactDecimal,
    pub short_qty: ExactDecimal,
}

impl ExactOrderRiskPosition {
    pub fn active_qty_for(self, order: &Order) -> ExactDecimal {
        let policy = order.policy.unwrap_or_default();
        if policy.position_mode == PositionMode::Hedge {
            match policy.position_side {
                PositionSide::Long => self.long_qty,
                PositionSide::Short => self.short_qty,
                PositionSide::Net => self.net_qty,
            }
        } else {
            self.net_qty
        }
    }
}

/// Scale-aware account and limit facts. `available_margin` and `position` must be populated
/// from the same account snapshot/event sequence as the legacy risk context; stale or partial
/// exact facts must not be used to admit an order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExactOrderRiskFacts {
    pub available_margin: Option<ExactDecimal>,
    pub projected_initial_margin: Option<ExactDecimal>,
    pub reference_price: Option<ExactDecimal>,
    pub max_order_notional: Option<ExactDecimal>,
    pub max_position_notional: Option<ExactDecimal>,
    pub position: ExactOrderRiskPosition,
}

/// 统一订单级风控输出。
///
/// 生产消费面只有三格：`allowed` 决定放行、`violations` 是全部拒绝原因（不短路）、
/// `rule_set_version` 让任何一条结论都能回溯到规则集 —— `qx-execution` 的两个
/// `RiskPort` 实现读的就是这三格。
///
/// 另外三格投影（`projected_position_raw` / `projected_margin_raw` /
/// `reference_price_raw`）由 Bar 回测的风险事件持久化，并通过已复核的 research-artifact
/// 读模型展示原始定点值。Paper/Live 的 `RiskPort` 目前仍只消费前述三格，未把这些投影
/// 接入其运行产物或研究读模型；这项剩余边界登记在
/// `maturity/capabilities.yaml` 的 `canonical_order_risk_decision.limitations`。
///
/// Venue 只能在 `allowed=true` 且调用方完成副作用前被调用。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct OrderRiskDecision {
    pub allowed: bool,
    pub violations: Vec<String>,
    pub projected_position_raw: Option<i128>,
    pub projected_margin_raw: Option<i128>,
    pub reference_price_raw: Option<i128>,
    pub rule_set_version: String,
}

pub const ORDER_RISK_RULE_SET_VERSION: &str = "qx-order-risk-v1";

/// 投影名义额超限时唯一使用的拒绝消息。
///
/// `OrderRiskContext::validate_order` 与规则链的 `MaxNotionalRule` 共用该常量，
/// 保证回测、Paper 和实盘三条路径的 violations 集合可直接逐条比对。
pub const MAX_POSITION_NOTIONAL_MESSAGE: &str = "投影持仓名义额超过账户限额";

/// 名义额规则缺少产品规格时的拒绝消息（fail-closed，V13 R26）。
///
/// `MaxNotionalRule` 此前在无 `instrument_spec` 时合成一份临时现货规格，与
/// `OrderRiskContext::validate_order`「账户限额在场即拒绝」的口径自相矛盾。
/// 现在两侧同一条纪律：缺规格即拒绝，绝不替交易所猜乘数/合约面值。
pub const MAX_POSITION_NOTIONAL_MISSING_SPEC_MESSAGE: &str =
    "名义额规则缺少 TradingInstrumentSpec，拒绝按未知规格折算";

impl RiskEngine {
    /// 统一订单级入口。底层校验仍只有 `OrderRiskContext::validate_order` 一份，
    /// 本方法只负责把结果投影为跨应用层可审计的决定，避免不同执行器复制
    /// “是否允许下单”的判断。
    ///
    /// 需要叠加配置化静态规则时调用 [`Self::evaluate_order_with_rules`]；两者共用
    /// [`RuleSet::evaluate`]，因此投影字段与 `rule_set_version` 语义完全一致。
    pub fn evaluate_order(context: &OrderRiskContext, order: &Order) -> OrderRiskDecision {
        RuleSet::account_limits_only().evaluate(context, order)
    }

    /// 带规则集的统一订单级入口：账户级唯一实现 + 配置化静态规则。
    pub fn evaluate_order_with_rules(
        context: &OrderRiskContext,
        order: &Order,
        rules: &RuleSet,
    ) -> OrderRiskDecision {
        rules.evaluate(context, order)
    }
}

impl OrderRiskContext {
    pub fn validate_order(&self, order: &Order) -> QxResult<()> {
        order.validate().map_err(QxError::BusinessViolation)?;
        if order.exact_amounts.is_some() {
            return self.validate_exact_order(order);
        }
        order
            .require_legacy_amounts()
            .map_err(QxError::BusinessViolation)?;
        let Some(spec) = self.instrument_spec.as_ref() else {
            if self.available_margin_raw.is_some()
                || self.projected_initial_margin_raw.is_some()
                || self.max_order_notional_raw.is_some()
                || self.max_position_notional_raw.is_some()
            {
                return Err(QxError::BusinessViolation(
                    "订单级风控缺少 TradingInstrumentSpec".into(),
                ));
            }
            return Ok(());
        };
        if order.instrument != spec.instrument {
            return Err(QxError::BusinessViolation(
                "订单标的与 TradingInstrumentSpec 不一致".into(),
            ));
        }
        let policy = order.policy.unwrap_or_default();
        policy.validate_for(spec)?;
        spec.validate_order(order.qty.raw(), order.limit.map(|price| price.raw()))?;
        let price = order
            .limit
            .or(self.reference_price)
            .ok_or_else(|| QxError::BusinessViolation("订单级风控缺少市价参考价".into()))?;
        let notional = spec.notional(order.qty.raw(), price.raw())?;
        if self
            .max_order_notional_raw
            .is_some_and(|limit| notional > limit)
        {
            return Err(QxError::BusinessViolation("订单名义额超过账户限额".into()));
        }

        self.validate_reduce_only(order)?;
        if let Some(limit) = self.max_position_notional_raw {
            let (projected_qty, projected_gross) = self.project_exposure(spec, order, price)?;
            let snapshot = RiskSnapshot {
                portfolio_id: order.account_id.clone(),
                timestamp: 0,
                gross_exposure: projected_gross,
                net_exposure: projected_qty,
                volatility_bps: 0,
                drawdown_bps: 0,
                factor_exposure: BTreeMap::new(),
            };
            if !matches!(
                RiskEngine::evaluate(&snapshot, limit, i32::MIN),
                RiskDecision::Allow
            ) {
                return Err(QxError::BusinessViolation(
                    MAX_POSITION_NOTIONAL_MESSAGE.into(),
                ));
            }
        }
        let spot_sell_needs_no_cash_margin =
            spec.product == qx_core::TradingProduct::Spot && order.side == Side::Sell;
        if let Some(available) = self
            .available_margin_raw
            .filter(|_| !spot_sell_needs_no_cash_margin)
        {
            if available < 0 {
                return Err(QxError::BusinessViolation("可用保证金不能为负".into()));
            }
            if !policy.reduce_only {
                let required = match self.projected_initial_margin_raw {
                    Some(required) if required < 0 => {
                        return Err(QxError::BusinessViolation("投影初始保证金不能为负".into()));
                    }
                    Some(required) => required,
                    None => spec.initial_margin(order.qty.raw(), price.raw(), policy.leverage)?,
                };
                if required > available {
                    return Err(QxError::BusinessViolation(
                        "订单初始保证金超过账户可用保证金".into(),
                    ));
                }
            }
        }
        Ok(())
    }

    fn validate_exact_order(&self, order: &Order) -> QxResult<()> {
        let exact = self.exact.as_ref().ok_or_else(|| {
            QxError::BusinessViolation(
                "精确订单金额风控缺少同一账户快照的精确余额与持仓事实，拒绝提交".into(),
            )
        })?;
        if exact.position.gross_notional.is_negative()
            || exact.position.long_qty.is_negative()
            || exact.position.short_qty.is_positive()
        {
            return Err(QxError::BusinessViolation(
                "精确风险快照的总名义额或多空持仓方向非法".into(),
            ));
        }
        let active_exact_position = exact.position.active_qty_for(order);
        if self.position.active_qty_for(order)
            != exact_to_raw_projection(active_exact_position, DecimalRounding::TowardZero)?
        {
            return Err(QxError::BusinessViolation(
                "精确与兼容持仓快照不一致，拒绝使用精确订单风控".into(),
            ));
        }
        if let (Some(raw), Some(available)) = (self.available_margin_raw, exact.available_margin) {
            if raw != exact_to_raw_projection(available, DecimalRounding::TowardZero)? {
                return Err(QxError::BusinessViolation(
                    "精确与兼容可用保证金快照不一致".into(),
                ));
            }
        }
        if let (Some(raw), Some(projected)) = (
            self.projected_initial_margin_raw,
            exact.projected_initial_margin,
        ) {
            if raw != exact_to_raw_projection(projected, DecimalRounding::Ceiling)? {
                return Err(QxError::BusinessViolation(
                    "精确与兼容组合初始保证金投影不一致".into(),
                ));
            }
        }
        let spec = self.instrument_spec.as_ref().ok_or_else(|| {
            QxError::BusinessViolation("精确订单风控缺少 TradingInstrumentSpec".into())
        })?;
        if order.instrument != spec.instrument {
            return Err(QxError::BusinessViolation(
                "精确订单标的与 TradingInstrumentSpec 不一致".into(),
            ));
        }
        let policy = order.policy.unwrap_or_default();
        policy.validate_for(spec)?;
        let amounts = order
            .exact_amounts
            .ok_or_else(|| QxError::Invariant("精确风控订单缺少 exact_amounts".into()))?;
        let limit_price = amounts.limit_price;
        let price = limit_price
            .or(exact.reference_price)
            .ok_or_else(|| QxError::BusinessViolation("精确市价单风控缺少精确参考价".into()))?;
        let rule_reference = exact.reference_price.or(Some(price));
        let notional = if let Some(quote_budget) = amounts.quote_budget {
            if order.side != Side::Buy
                || spec.product != qx_core::TradingProduct::Spot
                || policy.reduce_only
                || !amounts.quantity.is_zero()
                || limit_price.is_some()
            {
                return Err(QxError::BusinessViolation(
                    "计价币预算订单仅支持非 reduce-only 的现货市价买入".into(),
                ));
            }
            spec.validate_exact_quote_budget(quote_budget)?;
            quote_budget
        } else {
            spec.validate_exact_order_with_reference(
                amounts.quantity,
                limit_price,
                rule_reference,
            )?;
            spec.notional_exact(amounts.quantity, price)?
        };
        let raw_max_order = self
            .max_order_notional_raw
            .map(exact_from_raw)
            .transpose()?;
        let max_order = stricter_limit(exact.max_order_notional, raw_max_order)?;
        if max_order.is_some_and(|limit| {
            notional
                .checked_cmp(limit)
                .is_some_and(|ordering| ordering.is_gt())
        }) {
            return Err(QxError::BusinessViolation("订单名义额超过账户限额".into()));
        }

        self.validate_exact_reduce_only(order, exact.position)?;
        let raw_max_position = self
            .max_position_notional_raw
            .map(exact_from_raw)
            .transpose()?;
        let max_position = stricter_limit(exact.max_position_notional, raw_max_position)?;
        if let Some(limit) = max_position {
            let (_, gross) = self.project_exposure_exact(spec, order, price, exact.position)?;
            if gross
                .checked_cmp(limit)
                .is_some_and(|ordering| ordering.is_gt())
            {
                return Err(QxError::BusinessViolation(
                    MAX_POSITION_NOTIONAL_MESSAGE.into(),
                ));
            }
        }

        let spot_sell_needs_no_cash_margin =
            spec.product == qx_core::TradingProduct::Spot && order.side == Side::Sell;
        if !spot_sell_needs_no_cash_margin && !policy.reduce_only {
            let available = exact.available_margin.ok_or_else(|| {
                QxError::BusinessViolation("精确订单缺少精确可用保证金快照".into())
            })?;
            if available.is_negative() {
                return Err(QxError::BusinessViolation("可用保证金不能为负".into()));
            }
            let projected = match exact.projected_initial_margin {
                Some(projected) => projected,
                None if self.projected_initial_margin_raw.is_some() => {
                    return Err(QxError::BusinessViolation(
                        "精确订单缺少组合投影后的精确初始保证金".into(),
                    ));
                }
                None => match amounts.quote_budget {
                    Some(quote_budget) => quote_budget,
                    None => spec.initial_margin_exact(amounts.quantity, price, policy.leverage)?,
                },
            };
            if projected.is_negative() {
                return Err(QxError::BusinessViolation("投影初始保证金不能为负".into()));
            }
            if projected
                .checked_cmp(available)
                .is_some_and(|ordering| ordering.is_gt())
            {
                return Err(QxError::BusinessViolation(
                    "订单初始保证金超过账户可用保证金".into(),
                ));
            }
        }
        Ok(())
    }

    fn project_exposure_exact(
        &self,
        spec: &TradingInstrumentSpec,
        order: &Order,
        price: ExactDecimal,
        position: ExactOrderRiskPosition,
    ) -> QxResult<(ExactDecimal, ExactDecimal)> {
        if spec.instrument != order.instrument {
            return Err(QxError::BusinessViolation(
                "精确风控标的与 TradingInstrumentSpec 不一致".into(),
            ));
        }
        let amounts = order
            .exact_amounts
            .ok_or_else(|| QxError::Invariant("精确风控订单缺少 exact_amounts".into()))?;
        let current = position.active_qty_for(order);
        if let Some(quote_budget) = amounts.quote_budget {
            if order.side != Side::Buy || spec.product != qx_core::TradingProduct::Spot {
                return Err(QxError::BusinessViolation(
                    "计价币预算敞口只支持现货买入".into(),
                ));
            }
            let current_notional = spec.notional_exact(exact_abs(current)?, price)?;
            let other_notional = position
                .gross_notional
                .checked_sub(current_notional)
                .ok_or_else(|| {
                    QxError::BusinessViolation(
                        "精确持仓总名义额小于目标标的当前名义额，风险快照不一致".into(),
                    )
                })?;
            let projected_notional = current_notional
                .checked_add(quote_budget)
                .ok_or_else(|| QxError::Invariant("计价币预算投影名义额溢出".into()))?;
            let gross = other_notional
                .checked_add(projected_notional)
                .ok_or_else(|| QxError::Invariant("精确投影总名义额溢出".into()))?;
            return Ok((current, gross));
        }
        let delta = match order.side {
            Side::Buy => amounts.quantity,
            Side::Sell => amounts
                .quantity
                .checked_neg()
                .ok_or_else(|| QxError::Invariant("精确订单数量取反溢出".into()))?,
        };
        let projected = current
            .checked_add(delta)
            .ok_or_else(|| QxError::Invariant("精确投影持仓数量溢出".into()))?;
        let current_notional = spec.notional_exact(exact_abs(current)?, price)?;
        let projected_notional = spec.notional_exact(exact_abs(projected)?, price)?;
        let other_notional = position
            .gross_notional
            .checked_sub(current_notional)
            .ok_or_else(|| {
                QxError::BusinessViolation(
                    "精确持仓总名义额小于目标标的当前名义额，风险快照不一致".into(),
                )
            })?;
        let gross = other_notional
            .checked_add(projected_notional)
            .ok_or_else(|| QxError::Invariant("精确投影总名义额溢出".into()))?;
        Ok((projected, gross))
    }

    fn validate_exact_reduce_only(
        &self,
        order: &Order,
        position: ExactOrderRiskPosition,
    ) -> QxResult<()> {
        let policy = order.policy.unwrap_or_default();
        if !policy.reduce_only {
            return Ok(());
        }
        let current = position.active_qty_for(order);
        let current_abs = exact_abs(current)?;
        let reduces_direction = (current.is_positive() && order.side == Side::Sell)
            || (current.is_negative() && order.side == Side::Buy);
        let quantity = order
            .exact_amounts
            .ok_or_else(|| QxError::Invariant("精确 reduce_only 缺少订单数量".into()))?
            .quantity;
        if current.is_zero()
            || !reduces_direction
            || quantity
                .checked_cmp(current_abs)
                .is_none_or(|ordering| ordering.is_gt())
        {
            return Err(QxError::BusinessViolation(
                "reduce_only 订单必须只减少目标持仓腿且不得反向穿仓".into(),
            ));
        }
        Ok(())
    }

    /// 投影持仓名义额的唯一实现：返回 `(投影后净数量, 投影后组合 gross 名义额)`。
    ///
    /// `validate_order` 与规则链的 `MaxNotionalRule` 都调用本方法，名义额一律走
    /// `TradingInstrumentSpec::notional`，不允许任何上层再写一份 `/1e9` 折算。
    pub fn project_exposure(
        &self,
        spec: &TradingInstrumentSpec,
        order: &Order,
        price: Price,
    ) -> QxResult<(i128, i128)> {
        order
            .require_legacy_amounts()
            .map_err(QxError::BusinessViolation)?;
        let current_qty = self.position.active_qty_for(order);
        let signed_delta = match order.side {
            Side::Buy => order.qty.raw(),
            Side::Sell => order
                .qty
                .raw()
                .checked_neg()
                .ok_or_else(|| QxError::Invariant("订单数量取反溢出".into()))?,
        };
        let projected_qty = current_qty
            .checked_add(signed_delta)
            .ok_or_else(|| QxError::Invariant("投影持仓数量溢出".into()))?;
        let current_abs = current_qty
            .checked_abs()
            .ok_or_else(|| QxError::Invariant("当前持仓绝对值溢出".into()))?;
        let projected_abs = projected_qty
            .checked_abs()
            .ok_or_else(|| QxError::Invariant("投影持仓绝对值溢出".into()))?;
        let current_leg_notional = spec.notional(current_abs, price.raw())?;
        let projected_leg_notional = spec.notional(projected_abs, price.raw())?;
        let other_notional = self
            .position
            .gross_notional
            .saturating_sub(current_leg_notional);
        let projected_gross_notional = other_notional
            .checked_add(projected_leg_notional)
            .ok_or_else(|| QxError::Invariant("投影持仓名义额溢出".into()))?;
        Ok((projected_qty, projected_gross_notional))
    }

    /// reduce-only 不变式：不依赖产品规格与账户限额，规则链即使上下文缺规格
    /// 也必须先满足该条件，因此单独暴露给 [`crate::RuleSet`] 与上层门面复用。
    pub fn validate_reduce_only(&self, order: &Order) -> QxResult<()> {
        if order.exact_amounts.is_some() {
            let exact = self.exact.as_ref().ok_or_else(|| {
                QxError::BusinessViolation("精确 reduce_only 缺少精确持仓风险事实".into())
            })?;
            return self.validate_exact_reduce_only(order, exact.position);
        }
        order
            .require_legacy_amounts()
            .map_err(QxError::BusinessViolation)?;
        let policy = order.policy.unwrap_or_default();
        if !policy.reduce_only {
            return Ok(());
        }
        let current = self.position.active_qty_for(order);
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
}

fn exact_from_raw(raw: i128) -> QxResult<ExactDecimal> {
    ExactDecimal::from_parts(raw, 9).map_err(|error| QxError::Invariant(error.into()))
}

fn exact_to_raw_projection(value: ExactDecimal, rounding: DecimalRounding) -> QxResult<i128> {
    let one = ExactDecimal::from_parts(1, 0).map_err(|error| QxError::Invariant(error.into()))?;
    value
        .checked_mul_at_scale(one, 9, rounding)
        .and_then(|projected| projected.to_raw(9).ok())
        .ok_or_else(|| QxError::Invariant("精确风险金额无法投影到兼容 raw 字段".into()))
}

fn stricter_limit(
    exact_limit: Option<ExactDecimal>,
    raw_limit: Option<ExactDecimal>,
) -> QxResult<Option<ExactDecimal>> {
    match (exact_limit, raw_limit) {
        (Some(exact), Some(raw)) => exact
            .checked_cmp(raw)
            .map(|ordering| Some(if ordering.is_gt() { raw } else { exact }))
            .ok_or_else(|| QxError::Invariant("精确风险限额比较失败".into())),
        (Some(limit), None) | (None, Some(limit)) => Ok(Some(limit)),
        (None, None) => Ok(None),
    }
}

fn exact_abs(value: ExactDecimal) -> QxResult<ExactDecimal> {
    if value.is_negative() {
        value
            .checked_neg()
            .ok_or_else(|| QxError::Invariant("精确持仓绝对值溢出".into()))
    } else {
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ExactOrderRiskFacts, ExactOrderRiskPosition, OrderRiskContext, RiskDecision, RiskEngine,
        RiskSnapshot, ORDER_RISK_RULE_SET_VERSION,
    };
    use qx_core::{InstrumentId, Order, OrderStatus, Price, Quantity, Side};
    use std::collections::BTreeMap;

    fn snapshot() -> RiskSnapshot {
        RiskSnapshot {
            portfolio_id: "portfolio-1".into(),
            timestamp: 1,
            gross_exposure: 100,
            net_exposure: 100,
            volatility_bps: 10,
            drawdown_bps: 0,
            factor_exposure: BTreeMap::new(),
        }
    }

    #[test]
    fn invalid_snapshot_fails_closed() {
        let mut value = snapshot();
        value.gross_exposure = -1;
        assert_eq!(
            RiskEngine::evaluate(&value, 1_000, -500),
            RiskDecision::Reject
        );
    }

    #[test]
    fn exposure_and_drawdown_limits_are_ordered() {
        assert_eq!(
            RiskEngine::evaluate(&snapshot(), 50, -500),
            RiskDecision::Reduce
        );
        assert_eq!(
            RiskEngine::evaluate(&snapshot(), 1_000, 100),
            RiskDecision::Reject
        );
        assert_eq!(
            RiskEngine::evaluate(&snapshot(), 1_000, -500),
            RiskDecision::Allow
        );
    }

    #[test]
    fn order_risk_engine_returns_auditable_decision_without_duplicate_rules() {
        let order = Order {
            client_id: 1,
            instrument: InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
            side: Side::Buy,
            qty: Quantity::from_i64(1),
            limit: Some(Price::from_i64(100)),
            status: OrderStatus::PendingSubmit,
            filled: Quantity::ZERO,
            account_id: "main".into(),
            trace: None,
            policy: None,
            exact_amounts: None,
        };
        let allowed = RiskEngine::evaluate_order(&OrderRiskContext::default(), &order);
        assert!(allowed.allowed);
        assert!(allowed.violations.is_empty());
        assert_eq!(allowed.projected_position_raw, Some(order.qty.raw()));
        assert_eq!(
            allowed.reference_price_raw,
            Some(order.limit.unwrap().raw())
        );
        assert_eq!(allowed.rule_set_version, ORDER_RISK_RULE_SET_VERSION);

        let rejected_context = OrderRiskContext {
            max_order_notional_raw: Some(1),
            ..OrderRiskContext::default()
        };
        let rejected = RiskEngine::evaluate_order(&rejected_context, &order);
        assert!(!rejected.allowed);
        assert_eq!(rejected.violations.len(), 1);
        assert!(rejected.violations[0].contains("TradingInstrumentSpec"));
    }

    #[test]
    fn legacy_risk_boundary_rejects_exact_amounts_instead_of_treating_them_as_zero() {
        use qx_core::{
            ExactDecimal, ExactOrderAmounts, TradingInstrumentSpec, TradingProduct, SCALE,
        };

        let order = Order {
            client_id: 4,
            instrument: InstrumentId::parse("PEPE/USDT.OKX").unwrap(),
            side: Side::Buy,
            qty: Quantity::ZERO,
            limit: None,
            status: OrderStatus::PendingSubmit,
            filled: Quantity::ZERO,
            account_id: "main".into(),
            trace: None,
            policy: None,
            exact_amounts: Some(ExactOrderAmounts {
                quantity: ExactDecimal::parse("1000.0000000001").unwrap(),
                limit_price: Some(ExactDecimal::parse("0.00000001").unwrap()),
                filled: ExactDecimal::ZERO,
                market_reference_price: None,
                quote_budget: None,
                quote_filled: Default::default(),
            }),
        };
        let rejected = RiskEngine::evaluate_order(&OrderRiskContext::default(), &order);
        assert!(!rejected.allowed);
        assert!(rejected
            .violations
            .iter()
            .any(|error| error.contains("精确订单金额")));
        assert!(OrderRiskContext::default()
            .project_exposure(
                &TradingInstrumentSpec {
                    instrument: order.instrument.clone(),
                    product: TradingProduct::Spot,
                    base_currency: "PEPE".into(),
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
                    valid_from: 1,
                    valid_to: None,
                    exact_rules: None,
                },
                &order,
                Price::from_i64(1),
            )
            .is_err());
    }

    #[test]
    fn exact_order_risk_validates_sub_nano_lot_tick_notional_and_margin() {
        use qx_core::{
            ExactDecimal, ExactOrderAmounts, ExactTradingRules, TradingInstrumentSpec,
            TradingProduct, SCALE,
        };

        let instrument = InstrumentId::parse("PEPE/USDT.OKX").unwrap();
        let price = ExactDecimal::parse("0.00000004").unwrap();
        let order = Order {
            client_id: 9,
            instrument: instrument.clone(),
            side: Side::Buy,
            qty: Quantity::ZERO,
            limit: None,
            status: OrderStatus::PendingSubmit,
            filled: Quantity::ZERO,
            account_id: "main".into(),
            trace: None,
            policy: None,
            exact_amounts: Some(ExactOrderAmounts {
                quantity: ExactDecimal::parse("0.0000000003").unwrap(),
                limit_price: Some(price),
                filled: ExactDecimal::ZERO,
                market_reference_price: None,
                quote_budget: None,
                quote_filled: Default::default(),
            }),
        };
        let spec = TradingInstrumentSpec {
            instrument,
            product: TradingProduct::Spot,
            base_currency: "PEPE".into(),
            quote_currency: "USDT".into(),
            settlement_currency: "USDT".into(),
            contract_size: SCALE,
            linear: true,
            inverse: false,
            price_tick: 1,
            qty_step: 1,
            min_qty: 1,
            min_notional_raw: 0,
            max_leverage: 1,
            maintenance_margin_bps: 0,
            valid_from: 1,
            valid_to: None,
            exact_rules: Some(ExactTradingRules {
                contract_size: ExactDecimal::parse("1").unwrap(),
                price_tick: ExactDecimal::parse("0.00000001").unwrap(),
                quantity_step: ExactDecimal::parse("0.0000000001").unwrap(),
                minimum_quantity: ExactDecimal::parse("0.0000000001").unwrap(),
                minimum_notional: ExactDecimal::ZERO,
            }),
        };
        let facts = ExactOrderRiskFacts {
            available_margin: Some(ExactDecimal::parse("1").unwrap()),
            projected_initial_margin: None,
            reference_price: Some(price),
            max_order_notional: Some(ExactDecimal::parse("0.00000000000000002").unwrap()),
            max_position_notional: Some(ExactDecimal::parse("0.00000000000000002").unwrap()),
            position: ExactOrderRiskPosition {
                net_qty: ExactDecimal::ZERO,
                gross_notional: ExactDecimal::ZERO,
                long_qty: ExactDecimal::ZERO,
                short_qty: ExactDecimal::ZERO,
            },
        };
        let context = OrderRiskContext {
            instrument_spec: Some(spec),
            exact: Some(facts),
            ..OrderRiskContext::default()
        };

        let allowed = RiskEngine::evaluate_order(&context, &order);
        assert!(allowed.allowed, "{allowed:?}");
        assert_eq!(allowed.reference_price_raw, Some(40));
        assert_eq!(allowed.projected_position_raw, None);
        assert_eq!(allowed.projected_margin_raw, None);

        use super::{MaxQtyRule, NoShortRule, RiskRule};
        assert!(MaxQtyRule { max_qty: 1 }.check(&context, &order).is_ok());
        assert!(MaxQtyRule { max_qty: 0 }.check(&context, &order).is_err());
        let mut sell = order.clone();
        sell.side = Side::Sell;
        assert!(NoShortRule.check(&context, &sell).is_err());
        let mut held = context.clone();
        held.exact.as_mut().unwrap().position.net_qty =
            ExactDecimal::parse("0.0000000005").unwrap();
        assert!(NoShortRule.check(&held, &sell).is_ok());
        let mut reduce_only_close = sell.clone();
        reduce_only_close.policy = Some(qx_core::OrderPolicy {
            reduce_only: true,
            ..qx_core::OrderPolicy::default()
        });
        assert!(held.validate_reduce_only(&reduce_only_close).is_ok());
        reduce_only_close.exact_amounts.as_mut().unwrap().quantity =
            ExactDecimal::parse("0.0000000006").unwrap();
        assert!(held.validate_reduce_only(&reduce_only_close).is_err());

        let mut mismatched_position = context.clone();
        mismatched_position.position.net_qty = 1;
        let rejected = RiskEngine::evaluate_order(&mismatched_position, &order);
        assert!(!rejected.allowed);
        assert!(rejected
            .violations
            .iter()
            .any(|message| message.contains("持仓快照不一致")));

        let mut insufficient = context.clone();
        insufficient.exact.as_mut().unwrap().available_margin =
            Some(ExactDecimal::parse("0.000000000000000001").unwrap());
        let rejected = RiskEngine::evaluate_order(&insufficient, &order);
        assert!(!rejected.allowed);
        assert!(rejected
            .violations
            .iter()
            .any(|message| message.contains("账户可用保证金")));

        let mut stricter_legacy_limit = context;
        stricter_legacy_limit.max_order_notional_raw = Some(0);
        let rejected = RiskEngine::evaluate_order(&stricter_legacy_limit, &order);
        assert!(!rejected.allowed);
        assert!(rejected
            .violations
            .iter()
            .any(|message| message.contains("订单名义额超过账户限额")));
    }

    /// V13 R26：名义额规则缺规格即 fail-closed，不再合成临时现货规格。
    #[test]
    fn max_notional_rule_fails_closed_without_a_product_spec() {
        use super::{MaxNotionalRule, RiskRule};
        use qx_core::{TradingInstrumentSpec, TradingProduct, SCALE};
        let rule = MaxNotionalRule {
            max_notional: 150 * SCALE,
        };
        let order = Order {
            client_id: 1,
            instrument: InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
            side: Side::Buy,
            qty: Quantity::from_i64(1),
            limit: Some(Price::from_raw(100 * SCALE)),
            status: OrderStatus::PendingSubmit,
            filled: Quantity::ZERO,
            account_id: "main".into(),
            trace: None,
            policy: None,
            exact_amounts: None,
        };
        // 缺规格：拒绝，且理由点名缺的是 TradingInstrumentSpec。
        let bare = OrderRiskContext {
            reference_price: order.limit,
            ..OrderRiskContext::default()
        };
        let error = rule.check(&bare, &order).unwrap_err();
        assert!(
            error.to_string().contains("TradingInstrumentSpec"),
            "缺规格时必须 fail-closed 且点名缺什么：{error}"
        );
        // 给规格：走真实折算（qty * contract_size / SCALE * price / SCALE = 100 * SCALE）。
        let with_spec = OrderRiskContext {
            reference_price: order.limit,
            instrument_spec: Some(TradingInstrumentSpec {
                instrument: order.instrument.clone(),
                product: TradingProduct::Spot,
                base_currency: "BTC".into(),
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
                valid_from: 1,
                valid_to: None,
                exact_rules: None,
            }),
            ..OrderRiskContext::default()
        };
        assert!(rule.check(&with_spec, &order).is_ok());
    }

    #[test]
    fn projected_initial_margin_overrides_isolated_order_estimate() {
        use qx_core::{TradingInstrumentSpec, TradingProduct, SCALE};
        let order = Order {
            client_id: 1,
            instrument: InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
            side: Side::Buy,
            qty: Quantity::from_i64(1),
            limit: Some(Price::from_i64(100)),
            status: OrderStatus::PendingSubmit,
            filled: Quantity::ZERO,
            account_id: "main".into(),
            trace: None,
            policy: None,
            exact_amounts: None,
        };
        let context = OrderRiskContext {
            available_margin_raw: Some(50 * SCALE),
            projected_initial_margin_raw: Some(25 * SCALE),
            reference_price: order.limit,
            instrument_spec: Some(TradingInstrumentSpec {
                instrument: order.instrument.clone(),
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
                min_notional_raw: 0,
                max_leverage: 20,
                maintenance_margin_bps: 500,
                valid_from: 1,
                valid_to: None,
                exact_rules: None,
            }),
            ..OrderRiskContext::default()
        };
        assert!(
            RiskEngine::evaluate_order(&context, &order).allowed,
            "portfolio-projected requirement must override the isolated order estimate"
        );
    }

    #[test]
    fn spot_sell_does_not_require_quote_cash_margin() {
        use qx_core::{TradingInstrumentSpec, TradingProduct, SCALE};
        let order = Order {
            client_id: 2,
            instrument: InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
            side: Side::Sell,
            qty: Quantity::from_i64(1),
            limit: Some(Price::from_i64(100)),
            status: OrderStatus::PendingSubmit,
            filled: Quantity::ZERO,
            account_id: "main".into(),
            trace: None,
            policy: None,
            exact_amounts: None,
        };
        let context = OrderRiskContext {
            available_margin_raw: Some(0),
            reference_price: order.limit,
            instrument_spec: Some(TradingInstrumentSpec {
                instrument: order.instrument.clone(),
                product: TradingProduct::Spot,
                base_currency: "BTC".into(),
                quote_currency: "USDT".into(),
                settlement_currency: "USDT".into(),
                contract_size: SCALE,
                linear: false,
                inverse: false,
                price_tick: 1,
                qty_step: SCALE,
                min_qty: SCALE,
                min_notional_raw: 0,
                max_leverage: 1,
                maintenance_margin_bps: 0,
                valid_from: 1,
                valid_to: None,
                exact_rules: None,
            }),
            ..OrderRiskContext::default()
        };
        assert!(
            RiskEngine::evaluate_order(&context, &order).allowed,
            "spot inventory is checked separately; a sell must not need quote cash"
        );
    }
}
