//! 成交事实归约：现货与合约成交、合约乘数、逐仓与 Hedge 分仓的成本和已实现盈亏。

use super::{Ledger, LedgerEntry, LedgerEntryKind};
use crate::error::{QxError, QxResult};
use crate::numeric::{Money, Quantity, SCALE};
use crate::order::{Fill, Order, Side};
use crate::trading::TradingInstrumentSpec;

impl Ledger {
    /// 用订单的方向和工具信息应用一笔不可变成交事实。
    pub fn apply_fill(&mut self, order: &Order, fill: &Fill, currency: &str) -> QxResult<Vec<u64>> {
        self.apply_fill_with_multiplier(order, fill, currency, 1)
    }

    /// 应用成交并显式传入合约乘数。兼容的 `apply_fill` 仅适用于乘数为 1 的现货。
    pub fn apply_fill_with_multiplier(
        &mut self,
        order: &Order,
        fill: &Fill,
        currency: &str,
        multiplier: i128,
    ) -> QxResult<Vec<u64>> {
        if fill.order_id != order.client_id {
            return Err(QxError::Invariant("成交 order_id 与订单不一致".into()));
        }
        if !fill.account_id.is_empty() && fill.account_id != order.account_id {
            return Err(QxError::Invariant("成交账户与订单账户不一致".into()));
        }
        if fill.fee.raw() < 0 {
            return Err(QxError::BusinessViolation("成交费用不能为负".into()));
        }
        if fill.price.raw() <= 0 {
            return Err(QxError::BusinessViolation("成交价格必须为正".into()));
        }
        if matches!(
            order.status,
            crate::order::OrderStatus::Rejected
                | crate::order::OrderStatus::Cancelled
                | crate::order::OrderStatus::Expired
        ) {
            return Err(QxError::BusinessViolation("终态订单不能记入成交".into()));
        }
        let mut staged = self.clone();
        let ids = staged.apply_fill_with_multiplier_inner(order, fill, currency, multiplier)?;
        *self = staged;
        Ok(ids)
    }

    /// 按产品规格应用衍生成交。与现货/兼容 multiplier 路径不同，开仓不扣除
    /// 全额名义本金，平仓只归约已实现 PnL；合约数量和 contract_size 由 spec
    /// 决定，因而支持 fractional contract size 与 inverse PnL。
    pub fn apply_fill_with_spec(
        &mut self,
        order: &Order,
        fill: &Fill,
        currency: &str,
        spec: &TradingInstrumentSpec,
    ) -> QxResult<Vec<u64>> {
        spec.validate()?;
        order.policy.unwrap_or_default().validate_for(spec)?;
        if order.instrument != spec.instrument {
            return Err(QxError::BusinessViolation(
                "成交订单与产品规格 instrument 不一致".into(),
            ));
        }
        // `TradingInstrumentSpec` 统一覆盖现货与衍生品，但现货仍必须按
        // 买入扣款/卖出入账的现金语义处理；只有保证金、永续和期货使用
        // “持仓 + 已实现 PnL”路径。
        if spec.product == crate::TradingProduct::Spot {
            return self.apply_fill_with_multiplier(order, fill, currency, 1);
        }
        if fill.order_id != order.client_id
            || (!fill.account_id.is_empty() && fill.account_id != order.account_id)
            || fill.fee.raw() < 0
            || fill.price.raw() <= 0
            || fill.qty.is_zero()
            || fill.qty.raw() > order.remaining().raw()
        {
            return Err(QxError::BusinessViolation("衍生成交事实非法".into()));
        }
        if matches!(
            order.status,
            crate::order::OrderStatus::Rejected
                | crate::order::OrderStatus::Cancelled
                | crate::order::OrderStatus::Expired
        ) {
            return Err(QxError::BusinessViolation("终态订单不能记入成交".into()));
        }
        let mut staged = self.clone();
        let ids = staged.apply_derivative_fill_inner(order, fill, currency, spec)?;
        *self = staged;
        Ok(ids)
    }

    pub(super) fn apply_derivative_fill_inner(
        &mut self,
        order: &Order,
        fill: &Fill,
        currency: &str,
        spec: &TradingInstrumentSpec,
    ) -> QxResult<Vec<u64>> {
        let policy = order.policy.unwrap_or_default();
        let hedge = policy.position_mode == crate::trading::PositionMode::Hedge;
        let existing = if hedge {
            self.position_for_side(&order.account_id, &order.instrument, policy.position_side)
        } else {
            self.position_for(&order.account_id, &order.instrument)
        };
        let current = existing.quantity.raw();
        let delta = match order.side {
            Side::Buy => fill.qty.raw(),
            Side::Sell => -fill.qty.raw(),
        };
        let close_qty = if current != 0 && current.signum() != delta.signum() {
            current
                .checked_abs()
                .ok_or_else(|| QxError::Invariant("衍生持仓绝对值溢出".into()))?
                .min(
                    delta
                        .checked_abs()
                        .ok_or_else(|| QxError::Invariant("衍生成交数量绝对值溢出".into()))?,
                )
        } else {
            0
        };
        let mut ids = Vec::new();
        if close_qty > 0 {
            let pnl_qty = if current > 0 { close_qty } else { -close_qty };
            let pnl =
                spec.unrealized_pnl(pnl_qty, existing.average_entry.raw(), fill.price.raw())?;
            if pnl != 0 {
                ids.push(self.append(LedgerEntry {
                    id: 0,
                    account_id: order.account_id.clone(),
                    currency: currency.into(),
                    kind: LedgerEntryKind::TradeCash,
                    amount: Money::from_raw(pnl),
                    instrument: Some(order.instrument.clone()),
                    quantity: Quantity::ZERO,
                    price: None,
                    order_id: Some(fill.order_id),
                    ts: fill.ts,
                    multiplier: 1,
                    position_side: None,
                })?);
            }
        }
        ids.push(self.append(LedgerEntry {
            id: 0,
            account_id: order.account_id.clone(),
            currency: currency.into(),
            kind: LedgerEntryKind::TradePosition,
            amount: Money::ZERO,
            instrument: Some(order.instrument.clone()),
            quantity: Quantity::from_raw(delta),
            price: Some(fill.price),
            order_id: Some(fill.order_id),
            ts: fill.ts,
            multiplier: 1,
            position_side: hedge.then_some(policy.position_side),
        })?);
        if !fill.fee.is_zero() {
            ids.push(self.append(LedgerEntry {
                id: 0,
                account_id: order.account_id.clone(),
                currency: currency.into(),
                kind: LedgerEntryKind::Fee,
                amount: Money::from_raw(-fill.fee.raw()),
                instrument: Some(order.instrument.clone()),
                quantity: Quantity::ZERO,
                price: None,
                order_id: Some(fill.order_id),
                ts: fill.ts,
                multiplier: 1,
                position_side: None,
            })?);
        }
        Ok(ids)
    }

    pub(super) fn apply_fill_with_multiplier_inner(
        &mut self,
        order: &Order,
        fill: &Fill,
        currency: &str,
        multiplier: i128,
    ) -> QxResult<Vec<u64>> {
        if multiplier <= 0 {
            return Err(QxError::BusinessViolation("合约乘数必须为正".into()));
        }
        if fill.qty.is_zero() || fill.qty.raw() > order.remaining().raw() {
            return Err(QxError::Invariant("成交数量非法".into()));
        }
        let notional = fill
            .qty
            .raw()
            .checked_mul(fill.price.raw())
            .and_then(|v| v.checked_div(SCALE))
            .and_then(|v| v.checked_mul(multiplier))
            .ok_or_else(|| QxError::Invariant("成交名义额溢出".into()))?;
        let signed_cash = match order.side {
            Side::Buy => -notional,
            Side::Sell => notional,
        };

        let cash_id = self.append(LedgerEntry {
            id: 0,
            account_id: order.account_id.clone(),
            currency: currency.into(),
            kind: LedgerEntryKind::TradeCash,
            amount: Money::from_raw(signed_cash),
            instrument: Some(order.instrument.clone()),
            quantity: Quantity::ZERO,
            price: None,
            order_id: Some(fill.order_id),
            ts: fill.ts,
            multiplier,
            position_side: None,
        })?;
        let position_delta = match order.side {
            Side::Buy => fill.qty.raw(),
            Side::Sell => -fill.qty.raw(),
        };
        let pos_id = self.append(LedgerEntry {
            id: 0,
            account_id: order.account_id.clone(),
            currency: currency.into(),
            kind: LedgerEntryKind::TradePosition,
            amount: Money::ZERO,
            instrument: Some(order.instrument.clone()),
            quantity: Quantity::from_raw(position_delta),
            price: Some(fill.price),
            order_id: Some(fill.order_id),
            ts: fill.ts,
            multiplier,
            position_side: order
                .policy
                .filter(|policy| policy.position_mode == crate::trading::PositionMode::Hedge)
                .map(|policy| policy.position_side),
        })?;
        let mut ids = vec![cash_id, pos_id];
        if !fill.fee.is_zero() {
            let fee_id = self.append(LedgerEntry {
                id: 0,
                account_id: order.account_id.clone(),
                currency: currency.into(),
                kind: LedgerEntryKind::Fee,
                amount: Money::from_raw(-fill.fee.raw()),
                instrument: Some(order.instrument.clone()),
                quantity: Quantity::ZERO,
                price: None,
                order_id: Some(fill.order_id),
                ts: fill.ts,
                multiplier,
                position_side: None,
            })?;
            ids.push(fee_id);
        }
        Ok(ids)
    }
}
