use qx_core::{
    Order, PositionMode, PositionSide, Price, QxError, QxResult, Side, TradingInstrumentSpec,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub mod decision;
pub mod exposure;
pub mod volatility;

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

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum RiskDecision {
    Allow,
    Reject,
    Reduce,
    Rebalance,
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
    pub reference_price: Option<Price>,
    pub instrument_spec: Option<TradingInstrumentSpec>,
    pub max_order_notional_raw: Option<i128>,
    pub max_position_notional_raw: Option<i128>,
    pub position: OrderRiskPosition,
}

/// 统一订单级风控输出。它把允许/拒绝和投影结果放在同一个可审计对象中，
/// 供回测、Paper、CCXT 和实盘执行端口使用；Venue 仍只能在 `allowed=true`
/// 且调用方完成副作用前被调用。
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

impl RiskEngine {
    /// 统一订单级入口。底层校验仍只有 `OrderRiskContext::validate_order` 一份，
    /// 本方法只负责把结果投影为跨应用层可审计的决定，避免不同执行器复制
    /// “是否允许下单”的判断。
    pub fn evaluate_order(context: &OrderRiskContext, order: &Order) -> OrderRiskDecision {
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
        match context.validate_order(order) {
            Ok(()) => OrderRiskDecision {
                allowed: true,
                violations: Vec::new(),
                projected_position_raw,
                projected_margin_raw,
                reference_price_raw: reference_price.map(|price| price.raw()),
                rule_set_version: ORDER_RISK_RULE_SET_VERSION.into(),
            },
            Err(error) => OrderRiskDecision {
                allowed: false,
                violations: vec![error.to_string()],
                projected_position_raw,
                projected_margin_raw,
                reference_price_raw: reference_price.map(|price| price.raw()),
                rule_set_version: ORDER_RISK_RULE_SET_VERSION.into(),
            },
        }
    }
}

impl OrderRiskContext {
    pub fn validate_order(&self, order: &Order) -> QxResult<()> {
        order.validate().map_err(QxError::BusinessViolation)?;
        let Some(spec) = self.instrument_spec.as_ref() else {
            if self.available_margin_raw.is_some()
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
        if let Some(limit) = self.max_position_notional_raw {
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
            let snapshot = RiskSnapshot {
                portfolio_id: order.account_id.clone(),
                timestamp: 0,
                gross_exposure: projected_gross_notional,
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
                    "投影持仓名义额超过账户限额".into(),
                ));
            }
        }
        if let Some(available) = self.available_margin_raw {
            if available < 0 {
                return Err(QxError::BusinessViolation("可用保证金不能为负".into()));
            }
            if !policy.reduce_only {
                let required =
                    spec.initial_margin(order.qty.raw(), price.raw(), policy.leverage)?;
                if required > available {
                    return Err(QxError::BusinessViolation(
                        "订单初始保证金超过账户可用保证金".into(),
                    ));
                }
            }
        }
        Ok(())
    }

    fn validate_reduce_only(&self, order: &Order) -> QxResult<()> {
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

#[cfg(test)]
mod tests {
    use super::{
        OrderRiskContext, RiskDecision, RiskEngine, RiskSnapshot, ORDER_RISK_RULE_SET_VERSION,
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
}
