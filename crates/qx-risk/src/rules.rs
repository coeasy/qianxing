//! 配置化订单级风控规则集。
//!
//! 规则集只承载“静态上限”类规则（单笔数量、投影名义额、禁止卖空）。账户级
//! 的规格、保证金、reduce-only 与投影持仓校验始终复用
//! [`OrderRiskContext::validate_order`] 这一份实现；名义额一律通过
//! `TradingInstrumentSpec::notional` 折算，禁止在任何上层复制 `/1e9` 手算公式。

use crate::{
    OrderRiskContext, OrderRiskDecision, MAX_POSITION_NOTIONAL_MESSAGE, ORDER_RISK_RULE_SET_VERSION,
};
use qx_core::{
    InstrumentId, Order, QxError, QxResult, Side, TradingInstrumentSpec, TradingProduct, SCALE,
};

/// 单条静态风控规则。
///
/// 判定输入是规范化的 [`OrderRiskContext`]，因此同一条规则在回测、Paper 和
/// 实盘三条路径上看到的是同一份持仓、产品规格与参考价快照。
pub trait RiskRule {
    fn name(&self) -> &'static str;
    fn check(&self, context: &OrderRiskContext, order: &Order) -> QxResult<()>;
}

/// 单笔最大数量。
pub struct MaxQtyRule {
    pub max_qty: i128,
}

impl RiskRule for MaxQtyRule {
    fn name(&self) -> &'static str {
        "MaxQty"
    }
    fn check(&self, _context: &OrderRiskContext, order: &Order) -> QxResult<()> {
        if order.qty.raw() > self.max_qty {
            return Err(QxError::BusinessViolation(format!(
                "单笔数量 {} 超过上限 {}",
                order.qty.raw(),
                self.max_qty
            )));
        }
        Ok(())
    }
}

/// 投影持仓最大名义额（结算币种定点原始值）。
///
/// 它不自己折算名义额，而是复用 [`OrderRiskContext::project_exposure`]，
/// 因此与账户级 `max_position_notional_raw` 限额完全同一份判定与同一条消息。
pub struct MaxNotionalRule {
    pub max_notional: i128,
}

impl RiskRule for MaxNotionalRule {
    fn name(&self) -> &'static str {
        "MaxNotional"
    }
    fn check(&self, context: &OrderRiskContext, order: &Order) -> QxResult<()> {
        let price = order
            .limit
            .or(context.reference_price)
            .ok_or_else(|| QxError::BusinessViolation("市价单缺少名义额风控参考价".into()))?;
        let fallback_spec = if context.instrument_spec.is_some() {
            None
        } else {
            // 兼容旧 `RiskGate` 无规格回测路径；仍然只走 `notional` 一份公式。
            Some(legacy_spot_spec(
                &order.instrument,
                context.position.multiplier,
            ))
        };
        let spec = context
            .instrument_spec
            .as_ref()
            .or(fallback_spec.as_ref())
            .ok_or_else(|| QxError::Invariant("名义额规则缺少产品规格".into()))?;
        let (_, projected_gross_notional) = context.project_exposure(spec, order, price)?;
        if projected_gross_notional > self.max_notional {
            return Err(QxError::BusinessViolation(
                MAX_POSITION_NOTIONAL_MESSAGE.into(),
            ));
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
    fn check(&self, context: &OrderRiskContext, order: &Order) -> QxResult<()> {
        if matches!(order.side, Side::Sell) && order.qty.raw() > context.position.net_qty {
            return Err(QxError::BusinessViolation("禁止卖空".into()));
        }
        Ok(())
    }
}

/// 旧 `RiskGate` 无规格回测路径的名义额折算：以现货 1:1（乘数写进
/// `contract_size`）构造临时规格，仍然只走 `TradingInstrumentSpec::notional`，
/// 不引入第二套公式。
///
/// TODO: 回测与 CLI 全部显式接入真实 `TradingInstrumentSpec` 后删除该兼容分支，
/// 缺少规格时应直接按 fail-closed 拒绝。
fn legacy_spot_spec(instrument: &InstrumentId, multiplier: i128) -> TradingInstrumentSpec {
    let currency = instrument.symbol.clone();
    TradingInstrumentSpec {
        instrument: instrument.clone(),
        product: TradingProduct::Spot,
        base_currency: currency.clone(),
        quote_currency: currency.clone(),
        settlement_currency: currency,
        contract_size: multiplier.max(1).saturating_mul(SCALE),
        linear: false,
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

/// 规则集：一次判定所需的全部静态规则，外加规则集版本标识。
///
/// 版本会写进 [`OrderRiskDecision::rule_set_version`]，使任何一条风控结论都能
/// 回溯到当时的规则配置；`evaluate` 不短路，收集全部拒绝原因。
pub struct RuleSet {
    version: String,
    rules: Vec<Box<dyn RiskRule>>,
}

impl Default for RuleSet {
    fn default() -> Self {
        Self::new()
    }
}

impl RuleSet {
    /// 空规则集：只有账户级判定，语义与 `RiskEngine::evaluate_order` 一致。
    pub fn new() -> Self {
        Self {
            version: ORDER_RISK_RULE_SET_VERSION.into(),
            rules: Vec::new(),
        }
    }

    /// 以配置来源（例如运行配置或 RunManifest 中的规则摘要）指定版本号。
    pub fn with_version(version: impl Into<String>) -> Self {
        Self {
            version: version.into(),
            rules: Vec::new(),
        }
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    pub fn add(&mut self, rule: Box<dyn RiskRule>) {
        self.rules.push(rule);
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    pub fn rule_count(&self) -> usize {
        self.rules.len()
    }

    /// 统一判定入口：账户级唯一实现（含 reduce-only、规格、保证金、投影限额）
    /// 叠加静态规则链。空规则集时与 [`crate::RiskEngine::evaluate_order`] 完全等价。
    pub fn evaluate(&self, context: &OrderRiskContext, order: &Order) -> OrderRiskDecision {
        let mut violations = self.rule_violations(context, order);
        if let Err(error) = context.validate_order(order) {
            push_violation(&mut violations, error.to_string());
        }
        self.decision(context, order, violations)
    }

    /// 兼容旧 `RiskGate` 的规则链判定：只跑 reduce-only 不变式与静态规则，
    /// 不引入账户级结构/规格/保证金校验。
    ///
    /// TODO: 该入口保留至 qx-xingban 回测与 CLI 改为显式传入配置化 `RuleSet`
    /// 之后删除；新代码应使用 [`RuleSet::evaluate`]。
    pub fn evaluate_rules_only(
        &self,
        context: &OrderRiskContext,
        order: &Order,
    ) -> OrderRiskDecision {
        let violations = self.rule_violations(context, order);
        self.decision(context, order, violations)
    }

    fn rule_violations(&self, context: &OrderRiskContext, order: &Order) -> Vec<String> {
        let mut violations = Vec::new();
        // reduce-only 是不依赖产品规格的账户不变式：上下文缺少规格时也必须成立。
        if let Err(error) = context.validate_reduce_only(order) {
            push_violation(&mut violations, error.to_string());
        }
        for rule in &self.rules {
            if let Err(error) = rule.check(context, order) {
                push_violation(&mut violations, format!("{}: {}", rule.name(), error));
            }
        }
        violations
    }

    fn decision(
        &self,
        context: &OrderRiskContext,
        order: &Order,
        violations: Vec<String>,
    ) -> OrderRiskDecision {
        let policy = order.policy.unwrap_or_default();
        let reference_price = order.limit.or(context.reference_price);
        let signed_delta = match order.side {
            Side::Buy => Some(order.qty.raw()),
            Side::Sell => order.qty.raw().checked_neg(),
        };
        let projected_position_raw = signed_delta
            .and_then(|delta| context.position.active_qty_for(order).checked_add(delta));
        let projected_margin_raw = context
            .instrument_spec
            .as_ref()
            .zip(reference_price)
            .and_then(|(spec, price)| {
                spec.initial_margin(order.qty.raw(), price.raw(), policy.leverage)
                    .ok()
            });
        OrderRiskDecision {
            allowed: violations.is_empty(),
            violations,
            projected_position_raw,
            projected_margin_raw,
            reference_price_raw: reference_price.map(|price| price.raw()),
            rule_set_version: self.version.clone(),
        }
    }
}

/// 同一个账户级原因可能被多条规则重复报出，只保留首次出现，避免决策噪声。
fn push_violation(violations: &mut Vec<String>, violation: String) {
    if !violations.contains(&violation) {
        violations.push(violation);
    }
}
