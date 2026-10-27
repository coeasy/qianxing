//! Strategy-owned position and pending-order exposure for portfolio capital caps.

use crate::cost::MarginRule;
use qx_core::{ExactDecimal, Fill, Order, QxError, QxResult, Side, TradingInstrumentSpec};
use std::collections::BTreeMap;
use std::sync::Arc;

#[derive(Clone)]
pub(crate) struct PortfolioBudgetMarket {
    pub(crate) spec: TradingInstrumentSpec,
    pub(crate) mark: ExactDecimal,
    pub(crate) margin: Arc<dyn MarginRule>,
    pub(crate) multiplier: i128,
}

#[derive(Clone, Copy, Default)]
struct PositionExposure {
    quantity: ExactDecimal,
    leverage: u32,
}

#[derive(Clone)]
struct PendingExposure {
    strategy_id: String,
    instrument: qx_core::InstrumentId,
    signed_qty: ExactDecimal,
    reference_price: ExactDecimal,
    leverage: u32,
}

#[derive(Default)]
pub(crate) struct PortfolioBudgetState {
    positions: BTreeMap<(String, qx_core::InstrumentId), PositionExposure>,
    pending_orders: BTreeMap<u64, PendingExposure>,
}

impl PortfolioBudgetState {
    pub(crate) fn position(&self, strategy_id: &str) -> QxResult<i128> {
        let mut positions = self
            .positions
            .iter()
            .filter(|((owner, _), exposure)| owner == strategy_id && !exposure.quantity.is_zero());
        let Some(((_, _), exposure)) = positions.next() else {
            return Ok(0);
        };
        if positions.next().is_some() {
            return Err(QxError::BusinessViolation(
                "订单簿组合策略不能把多个标的的持仓合并成一个数量".into(),
            ));
        }
        exposure.quantity.to_raw(9).map_err(|_| {
            QxError::BusinessViolation("组合策略精确持仓超出订单簿策略的 1e-9 位置投影精度".into())
        })
    }

    fn strategy_exposures(
        &self,
        strategy_id: &str,
    ) -> BTreeMap<qx_core::InstrumentId, PositionExposure> {
        self.positions
            .iter()
            .filter(|((owner, _), exposure)| owner == strategy_id && !exposure.quantity.is_zero())
            .map(|((_, instrument), exposure)| (instrument.clone(), *exposure))
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn order_budget_check_exact(
        &self,
        budget_for: impl Fn(&str) -> Option<i128>,
        order: &Order,
        order_quantity: ExactDecimal,
        reference_price: ExactDecimal,
        position_mark: ExactDecimal,
        instrument_spec: Option<&TradingInstrumentSpec>,
        multiplier: i128,
        margin: &dyn MarginRule,
        portfolio_markets: Option<&BTreeMap<qx_core::InstrumentId, PortfolioBudgetMarket>>,
        currency: &str,
    ) -> QxResult<Option<(ExactDecimal, ExactDecimal)>> {
        self.order_budget_check_with_exact_budget(
            |strategy_id| {
                budget_for(strategy_id).and_then(|raw| ExactDecimal::from_parts(raw, 9).ok())
            },
            order,
            order_quantity,
            reference_price,
            position_mark,
            instrument_spec,
            multiplier,
            margin,
            portfolio_markets,
            currency,
        )
    }

    pub(crate) fn order_budget_check_with_exact_budget(
        &self,
        budget_for: impl Fn(&str) -> Option<ExactDecimal>,
        order: &Order,
        order_quantity: ExactDecimal,
        reference_price: ExactDecimal,
        position_mark: ExactDecimal,
        instrument_spec: Option<&TradingInstrumentSpec>,
        multiplier: i128,
        margin: &dyn MarginRule,
        portfolio_markets: Option<&BTreeMap<qx_core::InstrumentId, PortfolioBudgetMarket>>,
        currency: &str,
    ) -> QxResult<Option<(ExactDecimal, ExactDecimal)>> {
        let Some(strategy_id) = strategy_id(order) else {
            return Ok(None);
        };
        let Some(budget) = budget_for(strategy_id) else {
            return Ok(None);
        };
        let leverage = order.policy.unwrap_or_default().leverage;
        let mut exposures = self.strategy_exposures(strategy_id);
        let mut reference_prices = BTreeMap::new();
        let mut maximum_required = required_total_capital(
            &exposures,
            &reference_prices,
            &order.instrument,
            position_mark,
            instrument_spec,
            multiplier,
            margin,
            portfolio_markets,
            currency,
        )?;
        for pending in self
            .pending_orders
            .values()
            .filter(|pending| pending.strategy_id == strategy_id)
        {
            update_exposure(
                &mut exposures,
                &pending.instrument,
                pending.signed_qty,
                pending.leverage,
            )?;
            retain_max_reference_price(
                &mut reference_prices,
                pending.instrument.clone(),
                pending.reference_price,
            )?;
            maximum_required = exact_max(
                maximum_required,
                required_total_capital(
                    &exposures,
                    &reference_prices,
                    &order.instrument,
                    position_mark,
                    instrument_spec,
                    multiplier,
                    margin,
                    portfolio_markets,
                    currency,
                )?,
            )?;
        }
        update_exposure(
            &mut exposures,
            &order.instrument,
            signed_qty(order.side, order_quantity)?,
            leverage,
        )?;
        retain_max_reference_price(
            &mut reference_prices,
            order.instrument.clone(),
            reference_price,
        )?;
        maximum_required = exact_max(
            maximum_required,
            required_total_capital(
                &exposures,
                &reference_prices,
                &order.instrument,
                position_mark,
                instrument_spec,
                multiplier,
                margin,
                portfolio_markets,
                currency,
            )?,
        )?;
        Ok(Some((budget, maximum_required)))
    }

    pub(crate) fn validate_fill_budget_with_exact_budget(
        &self,
        budget_for: impl Fn(&str) -> Option<ExactDecimal>,
        order: &Order,
        fill: &Fill,
        instrument_spec: Option<&TradingInstrumentSpec>,
        multiplier: i128,
        margin: &dyn MarginRule,
        portfolio_markets: Option<&BTreeMap<qx_core::InstrumentId, PortfolioBudgetMarket>>,
        currency: &str,
    ) -> Result<(), QxError> {
        let Some(owner) = strategy_id(order) else {
            return Ok(());
        };
        let Some(budget) = budget_for(owner) else {
            return Ok(());
        };
        let leverage = order.policy.unwrap_or_default().leverage;
        let fill_delta = signed_qty(order.side, fill_quantity_exact(fill)?)?;
        let fill_price = fill_price_exact(fill)?;
        let mut exposures = self.strategy_exposures(owner);
        update_exposure(&mut exposures, &order.instrument, fill_delta, leverage)?;
        let mut reference_prices = BTreeMap::new();
        retain_max_reference_price(&mut reference_prices, order.instrument.clone(), fill_price)?;
        let mut maximum_required = required_total_capital(
            &exposures,
            &reference_prices,
            &order.instrument,
            fill_price,
            instrument_spec,
            multiplier,
            margin,
            portfolio_markets,
            currency,
        )?;

        for (order_id, pending) in &self.pending_orders {
            if pending.strategy_id != owner {
                continue;
            }
            let remaining = if *order_id == order.client_id {
                pending
                    .signed_qty
                    .checked_sub(fill_delta)
                    .ok_or_else(|| QxError::Invariant("组合策略剩余挂单数量溢出".into()))?
            } else {
                pending.signed_qty
            };
            if remaining.is_zero() {
                continue;
            }
            update_exposure(
                &mut exposures,
                &pending.instrument,
                remaining,
                pending.leverage,
            )?;
            retain_max_reference_price(
                &mut reference_prices,
                pending.instrument.clone(),
                pending.reference_price,
            )?;
            maximum_required = exact_max(
                maximum_required,
                required_total_capital(
                    &exposures,
                    &reference_prices,
                    &order.instrument,
                    fill_price,
                    instrument_spec,
                    multiplier,
                    margin,
                    portfolio_markets,
                    currency,
                )?,
            )?;
        }
        if maximum_required
            .checked_cmp(budget)
            .ok_or_else(|| QxError::Invariant("组合策略预算比较失败".into()))?
            .is_gt()
        {
            return Err(QxError::BusinessViolation(format!(
                "成交价使策略持仓需求超过 reserved_capital_raw: required={maximum_required}, budget={budget}"
            )));
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn accept_if_owned_exact(
        &mut self,
        budget_for: impl Fn(&str) -> Option<i128>,
        order: &Order,
        order_quantity: ExactDecimal,
        reference_price: ExactDecimal,
    ) -> Result<(), QxError> {
        self.accept_if_owned_with_exact_budget(
            |strategy_id| {
                budget_for(strategy_id).and_then(|raw| ExactDecimal::from_parts(raw, 9).ok())
            },
            order,
            order_quantity,
            reference_price,
        )
    }

    pub(crate) fn accept_if_owned_with_exact_budget(
        &mut self,
        budget_for: impl Fn(&str) -> Option<ExactDecimal>,
        order: &Order,
        order_quantity: ExactDecimal,
        reference_price: ExactDecimal,
    ) -> Result<(), QxError> {
        if let Some(owner) =
            strategy_id(order).filter(|strategy_id| budget_for(strategy_id).is_some())
        {
            if !order_quantity.is_positive() || !reference_price.is_positive() {
                return Err(QxError::BusinessViolation(
                    "组合预算订单数量和参考价必须为正".into(),
                ));
            }
            if self.pending_orders.contains_key(&order.client_id) {
                return Err(QxError::Invariant("组合预算重复登记同一挂单 ID".into()));
            }
            self.pending_orders.insert(
                order.client_id,
                PendingExposure {
                    strategy_id: owner.to_string(),
                    instrument: order.instrument.clone(),
                    signed_qty: signed_qty(order.side, order_quantity)?,
                    reference_price,
                    leverage: order.policy.unwrap_or_default().leverage,
                },
            );
        }
        Ok(())
    }

    pub(crate) fn apply_fill_if_owned_with_exact_budget(
        &mut self,
        budget_for: impl Fn(&str) -> Option<ExactDecimal>,
        order: &Order,
        fill: &Fill,
    ) -> Result<(), QxError> {
        if strategy_id(order).is_some_and(|strategy_id| budget_for(strategy_id).is_some()) {
            self.apply_fill(order, fill)?;
        }
        Ok(())
    }

    pub(crate) fn cancel_if_owned_with_exact_budget(
        &mut self,
        budget_for: impl Fn(&str) -> Option<ExactDecimal>,
        client_order_id: u64,
    ) -> Result<(), QxError> {
        if let Some(pending) = self.pending_orders.get(&client_order_id) {
            if budget_for(&pending.strategy_id).is_some() {
                self.pending_orders.remove(&client_order_id);
            }
        }
        Ok(())
    }

    fn apply_fill(&mut self, order: &Order, fill: &Fill) -> Result<(), QxError> {
        let Some(owner) = strategy_id(order) else {
            return Ok(());
        };
        let fill_delta = signed_qty(order.side, fill_quantity_exact(fill)?)?;
        let pending = self
            .pending_orders
            .get(&order.client_id)
            .ok_or_else(|| QxError::ReconcileRequired("组合策略成交没有对应的预算挂单".into()))?;
        if pending.strategy_id != owner
            || pending.instrument != order.instrument
            || pending.signed_qty.coefficient().signum() != fill_delta.coefficient().signum()
        {
            return Err(QxError::ReconcileRequired(
                "组合策略预算挂单归属或方向与成交不一致".into(),
            ));
        }
        let position_key = (owner.to_string(), order.instrument.clone());
        let prior = self
            .positions
            .get(&position_key)
            .copied()
            .unwrap_or_default();
        let new_quantity = prior
            .quantity
            .checked_add(fill_delta)
            .ok_or_else(|| QxError::Invariant("组合策略成交持仓溢出".into()))?;
        if new_quantity.is_zero() {
            self.positions.remove(&position_key);
        } else {
            let order_leverage = order.policy.unwrap_or_default().leverage;
            let leverage = if prior.quantity.is_zero()
                || prior.quantity.is_negative() != new_quantity.is_negative()
            {
                order_leverage
            } else {
                prior.leverage.min(order_leverage)
            };
            self.positions.insert(
                position_key,
                PositionExposure {
                    quantity: new_quantity,
                    leverage,
                },
            );
        }
        let pending = self
            .pending_orders
            .get_mut(&order.client_id)
            .ok_or_else(|| QxError::ReconcileRequired("组合策略预算挂单在状态更新前消失".into()))?;
        pending.signed_qty = pending
            .signed_qty
            .checked_sub(fill_delta)
            .ok_or_else(|| QxError::Invariant("组合策略剩余挂单数量溢出".into()))?;
        if pending.signed_qty.is_zero() {
            self.pending_orders.remove(&order.client_id);
        }
        Ok(())
    }
}

fn strategy_id(order: &Order) -> Option<&str> {
    order
        .trace
        .as_ref()
        .and_then(|trace| trace.strategy_id.as_deref())
}

fn fill_quantity_exact(fill: &Fill) -> QxResult<ExactDecimal> {
    let quantity = fill
        .exact_amounts
        .map(|amounts| amounts.quantity)
        .unwrap_or(raw_decimal(fill.qty.raw())?);
    if !quantity.is_positive() {
        return Err(QxError::BusinessViolation(
            "组合策略成交数量必须为正".into(),
        ));
    }
    Ok(quantity)
}

fn fill_price_exact(fill: &Fill) -> QxResult<ExactDecimal> {
    let price = fill
        .exact_amounts
        .map(|amounts| amounts.price)
        .unwrap_or(raw_decimal(fill.price.raw())?);
    if !price.is_positive() {
        return Err(QxError::BusinessViolation(
            "组合策略成交价格必须为正".into(),
        ));
    }
    Ok(price)
}

fn signed_qty(side: Side, qty: ExactDecimal) -> QxResult<ExactDecimal> {
    if !qty.is_positive() {
        return Err(QxError::BusinessViolation(
            "组合策略订单与成交数量必须为正".into(),
        ));
    }
    match side {
        Side::Buy => Ok(qty),
        Side::Sell => qty
            .checked_neg()
            .ok_or_else(|| QxError::Invariant("组合策略卖出数量溢出".into())),
    }
}

fn exact_abs(value: ExactDecimal) -> QxResult<ExactDecimal> {
    if value.is_negative() {
        value
            .checked_neg()
            .ok_or_else(|| QxError::Invariant("组合策略持仓绝对值溢出".into()))
    } else {
        Ok(value)
    }
}

fn exact_max(left: ExactDecimal, right: ExactDecimal) -> QxResult<ExactDecimal> {
    if left
        .checked_cmp(right)
        .ok_or_else(|| QxError::Invariant("组合策略预算值比较失败".into()))?
        .is_ge()
    {
        Ok(left)
    } else {
        Ok(right)
    }
}

fn retain_max_reference_price(
    reference_prices: &mut BTreeMap<qx_core::InstrumentId, ExactDecimal>,
    instrument: qx_core::InstrumentId,
    reference_price: ExactDecimal,
) -> QxResult<()> {
    match reference_prices.get_mut(&instrument) {
        Some(current) => *current = exact_max(*current, reference_price)?,
        None => {
            reference_prices.insert(instrument, reference_price);
        }
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct BudgetMarketView<'a> {
    spec: Option<&'a TradingInstrumentSpec>,
    mark: ExactDecimal,
    margin: &'a dyn MarginRule,
    multiplier: i128,
}

fn required_total_capital(
    exposures: &BTreeMap<qx_core::InstrumentId, PositionExposure>,
    reference_prices: &BTreeMap<qx_core::InstrumentId, ExactDecimal>,
    fallback_instrument: &qx_core::InstrumentId,
    fallback_mark: ExactDecimal,
    fallback_spec: Option<&TradingInstrumentSpec>,
    fallback_multiplier: i128,
    fallback_margin: &dyn MarginRule,
    portfolio_markets: Option<&BTreeMap<qx_core::InstrumentId, PortfolioBudgetMarket>>,
    currency: &str,
) -> QxResult<ExactDecimal> {
    let mut total = ExactDecimal::ZERO;
    for (instrument, exposure) in exposures {
        if exposure.quantity.is_zero() {
            continue;
        }
        let view =
            if let Some(market) = portfolio_markets.and_then(|markets| markets.get(instrument)) {
                BudgetMarketView {
                    spec: Some(&market.spec),
                    mark: market.mark,
                    margin: market.margin.as_ref(),
                    multiplier: market.multiplier,
                }
            } else if instrument == fallback_instrument {
                BudgetMarketView {
                    spec: fallback_spec,
                    mark: fallback_mark,
                    margin: fallback_margin,
                    multiplier: fallback_multiplier,
                }
            } else {
                return Err(QxError::ReconcileRequired(format!(
                    "组合策略预算缺少标的行情/规则快照 {instrument}"
                )));
            };
        let reference_price = reference_prices
            .get(instrument)
            .copied()
            .unwrap_or(view.mark);
        if let Some(spec) = view.spec {
            let required_currency = if spec.product.supports_leverage() {
                spec.settlement_currency.as_str()
            } else {
                spec.quote_currency.as_str()
            };
            if !currency.is_empty() && !required_currency.eq_ignore_ascii_case(currency) {
                return Err(QxError::BusinessViolation(format!(
                    "策略预算暂不支持跨计价币种: instrument={instrument}, required={required_currency}, budget={currency}"
                )));
            }
        }
        let required = required_capital(
            exposure.quantity,
            reference_price,
            view.spec,
            view.multiplier,
            exposure.leverage,
            view.margin,
        )?;
        total = total
            .checked_add(required)
            .ok_or_else(|| QxError::Invariant("多标的策略预算合计溢出".into()))?;
    }
    Ok(total)
}

fn update_exposure(
    exposures: &mut BTreeMap<qx_core::InstrumentId, PositionExposure>,
    instrument: &qx_core::InstrumentId,
    signed_delta: ExactDecimal,
    leverage: u32,
) -> QxResult<()> {
    let current = exposures.get(instrument).copied().unwrap_or_default();
    let quantity = current
        .quantity
        .checked_add(signed_delta)
        .ok_or_else(|| QxError::Invariant("组合策略预计持仓溢出".into()))?;
    if quantity.is_zero() {
        exposures.remove(instrument);
        return Ok(());
    }
    let effective_leverage = if current.quantity.is_zero()
        || current.quantity.is_negative() != quantity.is_negative()
        || current.leverage == 0
    {
        leverage
    } else {
        current.leverage.min(leverage)
    };
    exposures.insert(
        instrument.clone(),
        PositionExposure {
            quantity,
            leverage: effective_leverage,
        },
    );
    Ok(())
}

fn raw_decimal(raw: i128) -> QxResult<ExactDecimal> {
    ExactDecimal::from_parts(raw, 9)
        .map_err(|error| QxError::Invariant(format!("组合预算 raw 数值无效: {error}")))
}

fn required_capital(
    position: ExactDecimal,
    reference_price: ExactDecimal,
    instrument_spec: Option<&TradingInstrumentSpec>,
    multiplier: i128,
    leverage: u32,
    margin: &dyn MarginRule,
) -> QxResult<ExactDecimal> {
    let quantity = exact_abs(position)?;
    if quantity.is_zero() {
        return Ok(ExactDecimal::ZERO);
    }
    if !reference_price.is_positive() {
        return Err(QxError::BusinessViolation(
            "组合策略参考价格必须为正".into(),
        ));
    }

    // Keep the established raw result bit-for-bit whenever the exact exposure is representable
    // at the legacy 1e-9 scale. Higher-precision exposures use exact spec and margin math below.
    if let (Ok(quantity_raw), Ok(price_raw)) = (quantity.to_raw(9), reference_price.to_raw(9)) {
        return raw_decimal(required_capital_raw(
            quantity_raw,
            price_raw,
            instrument_spec,
            multiplier,
            leverage,
            margin,
        )?);
    }

    let spec = instrument_spec.ok_or_else(|| {
        QxError::BusinessViolation("高精度组合策略预算需要冻结 TradingInstrumentSpec".into())
    })?;
    if spec.product.supports_leverage() {
        margin.instrument_initial_margin_exact(spec, quantity, reference_price, leverage)
    } else {
        spec.notional_exact(quantity, reference_price)
    }
}

fn required_capital_raw(
    position: i128,
    reference_price: i128,
    instrument_spec: Option<&TradingInstrumentSpec>,
    multiplier: i128,
    leverage: u32,
    margin: &dyn MarginRule,
) -> Result<i128, QxError> {
    let qty = position
        .checked_abs()
        .ok_or_else(|| QxError::Invariant("组合策略持仓绝对值溢出".into()))?;
    let price = reference_price
        .checked_abs()
        .ok_or_else(|| QxError::Invariant("组合策略参考价格绝对值溢出".into()))?;
    if qty == 0 {
        return Ok(0);
    }
    if let Some(spec) = instrument_spec.filter(|spec| spec.product.supports_leverage()) {
        margin.instrument_initial_margin(spec, qty, price, leverage)
    } else if let Some(spec) = instrument_spec {
        spec.notional(qty, price)
    } else {
        qx_core::notional(qty, price)?
            .checked_mul(multiplier)
            .ok_or_else(|| QxError::Invariant("组合策略名义额乘数计算溢出".into()))
    }
}
