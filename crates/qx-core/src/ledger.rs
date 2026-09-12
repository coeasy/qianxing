//! 事件驱动账簿。
//!
//! 余额和持仓不允许被策略直接写入，只能由 Fill、费用、资金费或结算事实
//! 经过本模块归约。账簿保留追加式 LedgerEntry，便于重放与对账。

use crate::error::{QxError, QxResult};
use crate::identity::InstrumentId;
use crate::numeric::{Money, Price, Quantity, SCALE};
use crate::order::{Fill, Order, Side};
use crate::trading::{PositionSide, TradingInstrumentSpec};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum LedgerEntryKind {
    TradeCash,
    TradePosition,
    Fee,
    Funding,
    Interest,
    Settlement,
    Liquidation,
    Adjustment,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct LedgerEntry {
    pub id: u64,
    pub account_id: String,
    pub currency: String,
    pub kind: LedgerEntryKind,
    pub amount: Money,
    pub instrument: Option<InstrumentId>,
    pub quantity: Quantity,
    /// TradePosition 必须携带成交价，才能在重放时重建平均成本和已实现盈亏。
    pub price: Option<Price>,
    pub order_id: Option<u64>,
    pub ts: u64,
    #[serde(default = "default_multiplier")]
    pub multiplier: i128,
    /// Hedge 模式下记录 Long/Short 分仓；None 表示传统 OneWay 净仓。
    #[serde(default)]
    pub position_side: Option<PositionSide>,
}

fn default_multiplier() -> i128 {
    1
}

#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct PositionState {
    pub quantity: Quantity,
    pub average_entry: Price,
    pub realized_pnl: Money,
}

#[derive(Clone, Debug, Default)]
pub struct Ledger {
    cash: BTreeMap<(String, String), i128>,
    positions: BTreeMap<(String, InstrumentId), PositionState>,
    hedge_positions: BTreeMap<(String, InstrumentId, PositionSide), PositionState>,
    entries: Vec<LedgerEntry>,
    next_id: u64,
}

impl Ledger {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn deposit(
        &mut self,
        account_id: &str,
        currency: &str,
        amount: Money,
        ts: u64,
    ) -> QxResult<u64> {
        if amount.raw() < 0 {
            return Err(QxError::BusinessViolation("入金金额不能为负".into()));
        }
        self.append(LedgerEntry {
            id: 0,
            account_id: account_id.into(),
            currency: currency.into(),
            kind: LedgerEntryKind::Adjustment,
            amount,
            instrument: None,
            quantity: Quantity::ZERO,
            price: None,
            order_id: None,
            ts,
            multiplier: 1,
            position_side: None,
        })
    }

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

    fn apply_derivative_fill_inner(
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

    fn apply_fill_with_multiplier_inner(
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

    pub fn apply_funding(
        &mut self,
        account_id: &str,
        currency: &str,
        amount: Money,
        ts: u64,
    ) -> QxResult<u64> {
        self.apply_cash_entry(account_id, currency, LedgerEntryKind::Funding, amount, ts)
    }

    pub fn apply_interest(
        &mut self,
        account_id: &str,
        currency: &str,
        amount: Money,
        ts: u64,
    ) -> QxResult<u64> {
        self.apply_cash_entry(account_id, currency, LedgerEntryKind::Interest, amount, ts)
    }

    pub fn apply_settlement(
        &mut self,
        account_id: &str,
        currency: &str,
        amount: Money,
        ts: u64,
    ) -> QxResult<u64> {
        self.apply_cash_entry(
            account_id,
            currency,
            LedgerEntryKind::Settlement,
            amount,
            ts,
        )
    }

    /// 应用交易所存取款/划转等外部现金调整。金额可以为正或负，只有已经
    /// 归一化且带外部账单身份的 Cashflow 事实才应调用此方法。
    pub fn apply_adjustment(
        &mut self,
        account_id: &str,
        currency: &str,
        amount: Money,
        ts: u64,
    ) -> QxResult<u64> {
        self.apply_cash_entry(
            account_id,
            currency,
            LedgerEntryKind::Adjustment,
            amount,
            ts,
        )
    }

    pub fn apply_liquidation(
        &mut self,
        account_id: &str,
        currency: &str,
        amount: Money,
        ts: u64,
    ) -> QxResult<u64> {
        self.apply_cash_entry(
            account_id,
            currency,
            LedgerEntryKind::Liquidation,
            amount,
            ts,
        )
    }

    fn apply_cash_entry(
        &mut self,
        account_id: &str,
        currency: &str,
        kind: LedgerEntryKind,
        amount: Money,
        ts: u64,
    ) -> QxResult<u64> {
        self.append(LedgerEntry {
            id: 0,
            account_id: account_id.into(),
            currency: currency.into(),
            kind,
            amount,
            instrument: None,
            quantity: Quantity::ZERO,
            price: None,
            order_id: None,
            ts,
            multiplier: 1,
            position_side: None,
        })
    }

    pub fn cash(&self, currency: &str) -> i128 {
        self.cash
            .iter()
            .filter(|((_, c), _)| c == currency)
            .map(|(_, amount)| *amount)
            .sum()
    }

    pub fn position(&self, instrument: &InstrumentId) -> PositionState {
        let mut out = PositionState::default();
        for ((_, id), state) in self
            .positions
            .iter()
            .filter(|((_, id), _)| id == instrument)
        {
            let _ = id;
            out.quantity =
                Quantity::from_raw(out.quantity.raw().saturating_add(state.quantity.raw()));
            out.realized_pnl = Money::from_raw(
                out.realized_pnl
                    .raw()
                    .saturating_add(state.realized_pnl.raw()),
            );
            if out.quantity.raw() != 0 {
                out.average_entry = state.average_entry;
            }
        }
        for ((_, id, _), state) in self
            .hedge_positions
            .iter()
            .filter(|((_, id, _), _)| id == instrument)
        {
            let _ = id;
            out.quantity =
                Quantity::from_raw(out.quantity.raw().saturating_add(state.quantity.raw()));
            out.realized_pnl = Money::from_raw(
                out.realized_pnl
                    .raw()
                    .saturating_add(state.realized_pnl.raw()),
            );
            if out.quantity.raw() != 0 {
                out.average_entry = state.average_entry;
            }
        }
        out
    }

    pub fn cash_for(&self, account_id: &str, currency: &str) -> i128 {
        *self
            .cash
            .get(&(account_id.to_string(), currency.to_string()))
            .unwrap_or(&0)
    }

    /// 返回账户所有币种的现金余额，按币种排序，供跨币种抵押品估值使用。
    pub fn cash_balances_for(&self, account_id: &str) -> BTreeMap<String, i128> {
        self.cash
            .iter()
            .filter(|((account, _), _)| account == account_id)
            .map(|((_, currency), amount)| (currency.clone(), *amount))
            .collect()
    }

    pub fn position_for(&self, account_id: &str, instrument: &InstrumentId) -> PositionState {
        let mut out = self
            .positions
            .get(&(account_id.to_string(), instrument.clone()))
            .cloned()
            .unwrap_or_default();
        for ((account, id, _), state) in self
            .hedge_positions
            .iter()
            .filter(|((account, id, _), _)| account == account_id && id == instrument)
        {
            let _ = (account, id);
            out.quantity =
                Quantity::from_raw(out.quantity.raw().saturating_add(state.quantity.raw()));
            out.realized_pnl = Money::from_raw(
                out.realized_pnl
                    .raw()
                    .saturating_add(state.realized_pnl.raw()),
            );
            if out.quantity.raw() != 0 {
                out.average_entry = state.average_entry;
            }
        }
        out
    }

    pub fn position_for_side(
        &self,
        account_id: &str,
        instrument: &InstrumentId,
        side: PositionSide,
    ) -> PositionState {
        if side == PositionSide::Net {
            return self.position_for(account_id, instrument);
        }
        self.hedge_positions
            .get(&(account_id.to_string(), instrument.clone(), side))
            .cloned()
            .unwrap_or_default()
    }

    pub fn entries(&self) -> &[LedgerEntry] {
        &self.entries
    }

    pub fn equity(&self, marks: &BTreeMap<InstrumentId, Price>, currency: &str) -> Option<i128> {
        self.equity_with_multiplier(marks, currency, 1)
    }

    pub fn equity_with_multiplier(
        &self,
        marks: &BTreeMap<InstrumentId, Price>,
        currency: &str,
        multiplier: i128,
    ) -> Option<i128> {
        if multiplier <= 0 {
            return None;
        }
        let mut value = self.cash(currency);
        for ((_, instrument), position) in &self.positions {
            let mark = marks.get(instrument)?;
            let n = position
                .quantity
                .raw()
                .checked_mul(mark.raw())?
                .checked_div(SCALE)?
                .checked_mul(multiplier)?;
            value = value.checked_add(n)?;
        }
        for ((_, instrument, _), position) in &self.hedge_positions {
            let mark = marks.get(instrument)?;
            let n = position
                .quantity
                .raw()
                .checked_mul(mark.raw())?
                .checked_div(SCALE)?
                .checked_mul(multiplier)?;
            value = value.checked_add(n)?;
        }
        Some(value)
    }

    pub fn equity_for(
        &self,
        account_id: &str,
        marks: &BTreeMap<InstrumentId, Price>,
        currency: &str,
    ) -> Option<i128> {
        self.equity_for_with_multiplier(account_id, marks, currency, 1)
    }

    pub fn equity_for_with_multiplier(
        &self,
        account_id: &str,
        marks: &BTreeMap<InstrumentId, Price>,
        currency: &str,
        multiplier: i128,
    ) -> Option<i128> {
        if multiplier <= 0 {
            return None;
        }
        let mut value = self.cash_for(account_id, currency);
        for ((account, instrument), position) in &self.positions {
            if account != account_id {
                continue;
            }
            let mark = marks.get(instrument)?;
            let n = position
                .quantity
                .raw()
                .checked_mul(mark.raw())?
                .checked_div(SCALE)?
                .checked_mul(multiplier)?;
            value = value.checked_add(n)?;
        }
        for ((account, instrument, _), position) in &self.hedge_positions {
            if account != account_id {
                continue;
            }
            let mark = marks.get(instrument)?;
            let n = position
                .quantity
                .raw()
                .checked_mul(mark.raw())?
                .checked_div(SCALE)?
                .checked_mul(multiplier)?;
            value = value.checked_add(n)?;
        }
        Some(value)
    }

    pub fn equity_for_with_spec(
        &self,
        account_id: &str,
        marks: &BTreeMap<InstrumentId, Price>,
        currency: &str,
        spec: &TradingInstrumentSpec,
    ) -> QxResult<i128> {
        spec.validate()?;
        let mut value = self.cash_for(account_id, currency);
        for ((account, instrument), position) in &self.positions {
            if account != account_id
                || instrument != &spec.instrument
                || position.quantity.is_zero()
            {
                continue;
            }
            let mark = marks
                .get(instrument)
                .ok_or_else(|| QxError::Invariant("衍生品权益缺少标记价格".into()))?;
            let pnl = spec.unrealized_pnl(
                position.quantity.raw(),
                position.average_entry.raw(),
                mark.raw(),
            )?;
            value = value
                .checked_add(pnl)
                .ok_or_else(|| QxError::Invariant("衍生品权益溢出".into()))?;
        }
        for ((account, instrument, _), position) in &self.hedge_positions {
            if account != account_id
                || instrument != &spec.instrument
                || position.quantity.is_zero()
            {
                continue;
            }
            let mark = marks
                .get(instrument)
                .ok_or_else(|| QxError::Invariant("衍生品权益缺少标记价格".into()))?;
            let pnl = spec.unrealized_pnl(
                position.quantity.raw(),
                position.average_entry.raw(),
                mark.raw(),
            )?;
            value = value
                .checked_add(pnl)
                .ok_or_else(|| QxError::Invariant("衍生品权益溢出".into()))?;
        }
        Ok(value)
    }

    /// 在指定报价币种下计算跨币种保证金权益。
    ///
    /// `fx_rates` 的含义是“1 单位资产币种可兑换多少报价币种”，同币种
    /// 不需要放入表中。没有可靠汇率时返回错误，禁止把不同币种的原始整数
    /// 直接相加造成虚假的可用保证金。
    pub fn equity_for_with_spec_and_fx(
        &self,
        account_id: &str,
        marks: &BTreeMap<InstrumentId, Price>,
        reporting_currency: &str,
        spec: &TradingInstrumentSpec,
        fx_rates: &BTreeMap<String, Price>,
    ) -> QxResult<i128> {
        spec.validate()?;
        let convert = |amount: i128, currency: &str| -> QxResult<i128> {
            let rate = if currency == reporting_currency {
                SCALE
            } else {
                fx_rates
                    .get(currency)
                    .ok_or_else(|| {
                        QxError::ReconcileRequired(format!(
                            "缺少 {currency}->{reporting_currency} 抵押品汇率"
                        ))
                    })?
                    .raw()
            };
            if rate <= 0 {
                return Err(QxError::ReconcileRequired(format!(
                    "{currency}->{reporting_currency} 抵押品汇率必须为正"
                )));
            }
            amount
                .checked_mul(rate)
                .and_then(|value| value.checked_div(SCALE))
                .ok_or_else(|| QxError::Invariant("跨币种权益换算溢出".into()))
        };
        let mut value = 0_i128;
        for ((account, currency), amount) in &self.cash {
            if account == account_id {
                value = value
                    .checked_add(convert(*amount, currency)?)
                    .ok_or_else(|| QxError::Invariant("跨币种现金权益溢出".into()))?;
            }
        }
        let settlement_currency = &spec.settlement_currency;
        for ((account, instrument), position) in &self.positions {
            if account != account_id
                || instrument != &spec.instrument
                || position.quantity.is_zero()
            {
                continue;
            }
            let mark = marks
                .get(instrument)
                .ok_or_else(|| QxError::Invariant("跨币种衍生品权益缺少标记价格".into()))?;
            let pnl = spec.unrealized_pnl(
                position.quantity.raw(),
                position.average_entry.raw(),
                mark.raw(),
            )?;
            value = value
                .checked_add(convert(pnl, settlement_currency)?)
                .ok_or_else(|| QxError::Invariant("跨币种未实现权益溢出".into()))?;
        }
        for ((account, instrument, _), position) in &self.hedge_positions {
            if account != account_id
                || instrument != &spec.instrument
                || position.quantity.is_zero()
            {
                continue;
            }
            let mark = marks
                .get(instrument)
                .ok_or_else(|| QxError::Invariant("跨币种衍生品权益缺少标记价格".into()))?;
            let pnl = spec.unrealized_pnl(
                position.quantity.raw(),
                position.average_entry.raw(),
                mark.raw(),
            )?;
            value = value
                .checked_add(convert(pnl, settlement_currency)?)
                .ok_or_else(|| QxError::Invariant("跨币种未实现权益溢出".into()))?;
        }
        Ok(value)
    }

    /// 以完整 entry 重放账簿。entry id、现金和持仓变更都必须连续且可验证。
    pub fn apply_entry(&mut self, entry: LedgerEntry) -> QxResult<u64> {
        if entry.id != self.next_id {
            return Err(QxError::Invariant(format!(
                "账簿 entry id 不连续: expected={}, actual={}",
                self.next_id, entry.id
            )));
        }
        if entry.account_id.trim().is_empty() || entry.currency.trim().is_empty() {
            return Err(QxError::Invariant("账簿 entry 缺少账户或币种".into()));
        }
        if entry.multiplier <= 0 {
            return Err(QxError::Invariant("账簿 entry 合约乘数必须为正".into()));
        }
        if entry.amount.raw() != 0 {
            let key = (entry.account_id.clone(), entry.currency.clone());
            let current = self.cash.get(&key).copied().unwrap_or(0);
            let next = current
                .checked_add(entry.amount.raw())
                .ok_or_else(|| QxError::Invariant("现金余额溢出".into()))?;
            self.cash.insert(key, next);
        }
        if matches!(entry.kind, LedgerEntryKind::TradePosition) {
            let instrument = entry
                .instrument
                .clone()
                .ok_or_else(|| QxError::Invariant("持仓 entry 缺少 instrument".into()))?;
            let price = entry
                .price
                .ok_or_else(|| QxError::Invariant("持仓 entry 缺少 price".into()))?;
            if let Some(position_side) = entry.position_side {
                if position_side == PositionSide::Net {
                    return Err(QxError::Invariant(
                        "LedgerEntry position_side=Net 必须使用 OneWay 净仓".into(),
                    ));
                }
                self.apply_hedge_position_delta(
                    &entry.account_id,
                    &instrument,
                    position_side,
                    entry.quantity.raw(),
                    price,
                    entry.multiplier,
                )?;
            } else {
                self.apply_position_delta(
                    &entry.account_id,
                    &instrument,
                    entry.quantity.raw(),
                    price,
                    entry.multiplier,
                )?;
            }
        }
        let id = entry.id;
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or_else(|| QxError::Invariant("账簿序号溢出".into()))?;
        self.entries.push(entry);
        Ok(id)
    }

    fn apply_position_delta(
        &mut self,
        account_id: &str,
        instrument: &InstrumentId,
        delta: i128,
        price: Price,
        multiplier: i128,
    ) -> QxResult<()> {
        let key = (account_id.to_string(), instrument.clone());
        let mut state = self.positions.get(&key).cloned().unwrap_or_default();
        apply_position_state_delta(&mut state, delta, price, multiplier)?;
        self.positions.insert(key, state);
        Ok(())
    }

    fn apply_hedge_position_delta(
        &mut self,
        account_id: &str,
        instrument: &InstrumentId,
        position_side: PositionSide,
        delta: i128,
        price: Price,
        multiplier: i128,
    ) -> QxResult<()> {
        let key = (account_id.to_string(), instrument.clone(), position_side);
        let mut state = self.hedge_positions.get(&key).cloned().unwrap_or_default();
        apply_position_state_delta(&mut state, delta, price, multiplier)?;
        self.hedge_positions.insert(key, state);
        Ok(())
    }
}

fn apply_position_state_delta(
    state: &mut PositionState,
    delta: i128,
    price: Price,
    multiplier: i128,
) -> QxResult<()> {
    if delta == 0 {
        return Ok(());
    }
    let current = state.quantity.raw();
    let next = current
        .checked_add(delta)
        .ok_or_else(|| QxError::Invariant("持仓数量溢出".into()))?;
    if current == 0 || current.signum() == delta.signum() {
        let old_abs = current
            .checked_abs()
            .ok_or_else(|| QxError::Invariant("持仓绝对值溢出".into()))?;
        let add_abs = delta
            .checked_abs()
            .ok_or_else(|| QxError::Invariant("持仓变动绝对值溢出".into()))?;
        let weighted = old_abs
            .checked_mul(state.average_entry.raw())
            .and_then(|v| v.checked_add(add_abs.checked_mul(price.raw())?))
            .ok_or_else(|| QxError::Invariant("持仓成本溢出".into()))?;
        let next_abs = next
            .checked_abs()
            .ok_or_else(|| QxError::Invariant("持仓结果绝对值溢出".into()))?;
        state.average_entry = Price::from_raw(weighted / next_abs);
    } else {
        let close_qty = current
            .checked_abs()
            .ok_or_else(|| QxError::Invariant("平仓数量绝对值溢出".into()))?
            .min(
                delta
                    .checked_abs()
                    .ok_or_else(|| QxError::Invariant("平仓变动绝对值溢出".into()))?,
            );
        let pnl_raw = if current > 0 {
            (price.raw() - state.average_entry.raw())
                .checked_mul(close_qty)
                .and_then(|v| v.checked_div(SCALE))
        } else {
            (state.average_entry.raw() - price.raw())
                .checked_mul(close_qty)
                .and_then(|v| v.checked_div(SCALE))
        }
        .ok_or_else(|| QxError::Invariant("已实现盈亏溢出".into()))?;
        let pnl_raw = pnl_raw
            .checked_mul(multiplier)
            .ok_or_else(|| QxError::Invariant("合约乘数下已实现盈亏溢出".into()))?;
        state.realized_pnl = Money::from_raw(
            state
                .realized_pnl
                .raw()
                .checked_add(pnl_raw)
                .ok_or_else(|| QxError::Invariant("累计已实现盈亏溢出".into()))?,
        );
        if next == 0 {
            state.average_entry = Price::ZERO;
        } else if current.signum() != next.signum() {
            state.average_entry = price;
        }
    }
    state.quantity = Quantity::from_raw(next);
    Ok(())
}

impl Ledger {
    fn append(&mut self, mut entry: LedgerEntry) -> QxResult<u64> {
        entry.id = self.next_id;
        self.apply_entry(entry)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{InstrumentId, OrderStatus};

    fn order(side: Side) -> Order {
        Order {
            client_id: 7,
            instrument: InstrumentId::parse("BTC-USDT.BINANCE").unwrap(),
            side,
            qty: Quantity::from_i64(2),
            limit: None,
            status: OrderStatus::Accepted,
            filled: Quantity::ZERO,
            account_id: "main".into(),
            trace: None,
            policy: None,
        }
    }

    #[test]
    fn fill_changes_cash_and_position() {
        let mut l = Ledger::new();
        l.deposit("main", "USDT", Money::from_i64(1000), 1).unwrap();
        let o = order(Side::Buy);
        let f = Fill {
            order_id: 7,
            qty: Quantity::from_i64(2),
            price: Price::from_i64(100),
            fee: Money::from_i64(1),
            ts: 2,
            ..Fill::default()
        };
        l.apply_fill(&o, &f, "USDT").unwrap();
        assert_eq!(l.cash("USDT"), Money::from_i64(799).raw());
        assert_eq!(
            l.position(&o.instrument).quantity.raw(),
            Quantity::from_i64(2).raw()
        );
    }

    #[test]
    fn derivative_fill_uses_contract_pnl_without_debiting_full_notional() {
        let instrument = InstrumentId::parse("BTC/USDT:USDT.BINANCE").unwrap();
        let spec = TradingInstrumentSpec {
            instrument: instrument.clone(),
            product: crate::TradingProduct::Perpetual,
            base_currency: "BTC".into(),
            quote_currency: "USDT".into(),
            settlement_currency: "USDT".into(),
            contract_size: SCALE,
            linear: true,
            inverse: false,
            price_tick: 1,
            qty_step: 1,
            min_qty: 1,
            max_leverage: 100,
            maintenance_margin_bps: 500,
            valid_from: 1,
            valid_to: None,
        };
        let mut ledger = Ledger::new();
        ledger
            .deposit("main", "USDT", Money::from_i64(1000), 1)
            .unwrap();
        let mut buy = order(Side::Buy);
        buy.client_id = 1;
        buy.instrument = instrument.clone();
        buy.qty = Quantity::from_i64(1);
        let mut sell = order(Side::Sell);
        sell.client_id = 2;
        sell.instrument = instrument.clone();
        sell.qty = Quantity::from_i64(1);
        ledger
            .apply_fill_with_spec(
                &buy,
                &Fill {
                    order_id: 1,
                    qty: Quantity::from_i64(1),
                    price: Price::from_i64(100),
                    ts: 2,
                    ..Fill::default()
                },
                "USDT",
                &spec,
            )
            .unwrap();
        assert_eq!(ledger.cash_for("main", "USDT"), Money::from_i64(1000).raw());
        let marks = BTreeMap::from([(instrument.clone(), Price::from_i64(110))]);
        assert_eq!(
            ledger
                .equity_for_with_spec("main", &marks, "USDT", &spec)
                .unwrap(),
            Money::from_i64(1010).raw()
        );
        ledger
            .apply_fill_with_spec(
                &sell,
                &Fill {
                    order_id: 2,
                    qty: Quantity::from_i64(1),
                    price: Price::from_i64(110),
                    ts: 3,
                    ..Fill::default()
                },
                "USDT",
                &spec,
            )
            .unwrap();
        assert_eq!(ledger.cash_for("main", "USDT"), Money::from_i64(1010).raw());
        assert_eq!(ledger.position_for("main", &instrument).quantity.raw(), 0);
    }

    #[test]
    fn hedge_mode_keeps_long_and_short_legs_separate_through_replay() {
        let instrument = InstrumentId::parse("BTC/USDT:USDT.BINANCE").unwrap();
        let spec = TradingInstrumentSpec {
            instrument: instrument.clone(),
            product: crate::TradingProduct::Perpetual,
            base_currency: "BTC".into(),
            quote_currency: "USDT".into(),
            settlement_currency: "USDT".into(),
            contract_size: SCALE,
            linear: true,
            inverse: false,
            price_tick: 1,
            qty_step: 1,
            min_qty: 1,
            max_leverage: 100,
            maintenance_margin_bps: 500,
            valid_from: 1,
            valid_to: None,
        };
        let mut ledger = Ledger::new();
        ledger
            .deposit("main", "USDT", Money::from_i64(1000), 1)
            .unwrap();
        let hedge_order = |client_id, side, position_side| Order {
            client_id,
            instrument: instrument.clone(),
            side,
            qty: Quantity::from_i64(1),
            limit: None,
            status: OrderStatus::Accepted,
            filled: Quantity::ZERO,
            account_id: "main".into(),
            trace: None,
            policy: Some(crate::OrderPolicy {
                reduce_only: false,
                position_side,
                margin_mode: crate::MarginMode::Cross,
                position_mode: crate::PositionMode::Hedge,
                leverage: 10,
                post_only: false,
            }),
        };
        let long = hedge_order(11, Side::Buy, crate::PositionSide::Long);
        let short = hedge_order(12, Side::Sell, crate::PositionSide::Short);
        ledger
            .apply_fill_with_spec(
                &long,
                &Fill {
                    order_id: 11,
                    qty: Quantity::from_i64(1),
                    price: Price::from_i64(100),
                    ts: 2,
                    ..Fill::default()
                },
                "USDT",
                &spec,
            )
            .unwrap();
        ledger
            .apply_fill_with_spec(
                &short,
                &Fill {
                    order_id: 12,
                    qty: Quantity::from_i64(1),
                    price: Price::from_i64(110),
                    ts: 3,
                    ..Fill::default()
                },
                "USDT",
                &spec,
            )
            .unwrap();
        assert_eq!(
            ledger
                .position_for_side("main", &instrument, crate::PositionSide::Long)
                .quantity
                .raw(),
            SCALE
        );
        assert_eq!(
            ledger
                .position_for_side("main", &instrument, crate::PositionSide::Short)
                .quantity
                .raw(),
            -SCALE
        );
        assert_eq!(ledger.position_for("main", &instrument).quantity.raw(), 0);
        let close_long = hedge_order(13, Side::Sell, crate::PositionSide::Long);
        ledger
            .apply_fill_with_spec(
                &close_long,
                &Fill {
                    order_id: 13,
                    qty: Quantity::from_i64(1),
                    price: Price::from_i64(120),
                    ts: 4,
                    ..Fill::default()
                },
                "USDT",
                &spec,
            )
            .unwrap();
        assert_eq!(
            ledger
                .position_for_side("main", &instrument, crate::PositionSide::Long)
                .quantity
                .raw(),
            0
        );
        assert_eq!(ledger.cash_for("main", "USDT"), Money::from_i64(1020).raw());
        let mut replay = Ledger::new();
        for entry in ledger.entries().iter().cloned() {
            replay.apply_entry(entry).unwrap();
        }
        assert_eq!(
            replay.position_for("main", &instrument),
            ledger.position_for("main", &instrument)
        );
        assert_eq!(
            replay
                .position_for_side("main", &instrument, crate::PositionSide::Short)
                .quantity
                .raw(),
            -SCALE
        );
    }

    #[test]
    fn replay_entries_are_append_only() {
        let mut l = Ledger::new();
        l.deposit("main", "USD", Money::from_i64(1), 1).unwrap();
        assert_eq!(l.entries()[0].id, 0);
        assert_eq!(l.entries().len(), 1);
    }

    #[test]
    fn accounts_and_realized_pnl_are_isolated() {
        let instrument = InstrumentId::parse("BTC-USDT.BINANCE").unwrap();
        let mut l = Ledger::new();
        l.deposit("a", "USD", Money::from_i64(1_000), 1).unwrap();
        l.deposit("b", "USD", Money::from_i64(1_000), 1).unwrap();
        let mut buy = order(Side::Buy);
        buy.client_id = 1;
        buy.account_id = "a".into();
        buy.instrument = instrument.clone();
        let mut sell = order(Side::Sell);
        sell.client_id = 2;
        sell.account_id = "a".into();
        sell.instrument = instrument.clone();
        let fill_buy = Fill {
            order_id: 1,
            qty: Quantity::from_i64(2),
            price: Price::from_i64(100),
            ts: 2,
            ..Fill::default()
        };
        let fill_sell = Fill {
            order_id: 2,
            qty: Quantity::from_i64(1),
            price: Price::from_i64(110),
            ts: 3,
            ..Fill::default()
        };
        l.apply_fill(&buy, &fill_buy, "USD").unwrap();
        l.apply_fill(&sell, &fill_sell, "USD").unwrap();
        assert_eq!(l.position_for("a", &instrument).quantity.raw(), SCALE);
        assert_eq!(
            l.position_for("a", &instrument).average_entry.raw(),
            Price::from_i64(100).raw()
        );
        assert_eq!(
            l.position_for("a", &instrument).realized_pnl.raw(),
            Money::from_i64(10).raw()
        );
        assert_eq!(l.position_for("b", &instrument), PositionState::default());
        assert_eq!(l.cash_for("b", "USD"), Money::from_i64(1_000).raw());
    }

    #[test]
    fn ledger_entries_replay_to_same_state() {
        let instrument = InstrumentId::parse("BTC-USDT.BINANCE").unwrap();
        let mut original = Ledger::new();
        original
            .deposit("main", "USD", Money::from_i64(1_000), 1)
            .unwrap();
        let mut o = order(Side::Buy);
        o.client_id = 3;
        o.instrument = instrument.clone();
        original
            .apply_fill(
                &o,
                &Fill {
                    order_id: 3,
                    qty: Quantity::from_i64(2),
                    price: Price::from_i64(100),
                    ts: 2,
                    ..Fill::default()
                },
                "USD",
            )
            .unwrap();
        let mut replay = Ledger::new();
        for entry in original.entries().iter().cloned() {
            replay.apply_entry(entry).unwrap();
        }
        let mut marks = BTreeMap::new();
        marks.insert(instrument.clone(), Price::from_i64(120));
        assert_eq!(
            replay.cash_for("main", "USD"),
            original.cash_for("main", "USD")
        );
        assert_eq!(
            replay.position_for("main", &instrument),
            original.position_for("main", &instrument)
        );
        assert_eq!(
            replay.equity_for("main", &marks, "USD"),
            original.equity_for("main", &marks, "USD")
        );
    }

    #[test]
    fn equity_uses_explicit_contract_multiplier() {
        let instrument = InstrumentId::parse("FUTURE.SIM").unwrap();
        let mut ledger = Ledger::new();
        ledger
            .deposit("main", "USD", Money::from_i64(1_000), 1)
            .unwrap();
        let mut buy = order(Side::Buy);
        buy.client_id = 8;
        buy.instrument = instrument.clone();
        ledger
            .apply_fill_with_multiplier(
                &buy,
                &Fill {
                    order_id: 8,
                    qty: Quantity::from_i64(2),
                    price: Price::from_i64(100),
                    ts: 2,
                    ..Fill::default()
                },
                "USD",
                10,
            )
            .unwrap();
        let marks = BTreeMap::from([(instrument, Price::from_i64(110))]);
        assert_eq!(
            ledger.equity_for_with_multiplier("main", &marks, "USD", 10),
            Some(Money::from_i64(1_200).raw())
        );
        assert_eq!(
            ledger.equity_for_with_multiplier("main", &marks, "USD", 0),
            None
        );
    }

    #[test]
    fn cross_currency_equity_requires_and_applies_explicit_fx_rates() {
        let instrument = InstrumentId::parse("BTC/USDT:USDT.SIM").unwrap();
        let spec = TradingInstrumentSpec {
            instrument: instrument.clone(),
            product: crate::TradingProduct::Perpetual,
            base_currency: "BTC".into(),
            quote_currency: "USDT".into(),
            settlement_currency: "USDT".into(),
            contract_size: SCALE,
            linear: true,
            inverse: false,
            price_tick: 1,
            qty_step: 1,
            min_qty: 1,
            max_leverage: 100,
            maintenance_margin_bps: 500,
            valid_from: 1,
            valid_to: None,
        };
        let mut ledger = Ledger::new();
        ledger
            .deposit("main", "BTC", Money::from_i64(1), 1)
            .unwrap();
        ledger
            .deposit("main", "USDT", Money::from_i64(100), 1)
            .unwrap();
        let marks = BTreeMap::new();
        assert!(ledger
            .equity_for_with_spec_and_fx("main", &marks, "USDT", &spec, &BTreeMap::new())
            .is_err());
        let fx = BTreeMap::from([(String::from("BTC"), Price::from_i64(60_000))]);
        assert_eq!(
            ledger
                .equity_for_with_spec_and_fx("main", &marks, "USDT", &spec, &fx)
                .unwrap(),
            Money::from_i64(60_100).raw()
        );
    }

    #[test]
    fn multiplier_is_preserved_in_realized_pnl_replay() {
        let instrument = InstrumentId::parse("FUTURE.SIM").unwrap();
        let mut ledger = Ledger::new();
        ledger
            .deposit("main", "USD", Money::from_i64(10_000), 1)
            .unwrap();
        let mut buy = order(Side::Buy);
        buy.client_id = 10;
        buy.instrument = instrument.clone();
        let mut sell = order(Side::Sell);
        sell.client_id = 11;
        sell.instrument = instrument.clone();
        ledger
            .apply_fill_with_multiplier(
                &buy,
                &Fill {
                    order_id: 10,
                    qty: Quantity::from_i64(2),
                    price: Price::from_i64(100),
                    ts: 2,
                    ..Fill::default()
                },
                "USD",
                10,
            )
            .unwrap();
        ledger
            .apply_fill_with_multiplier(
                &sell,
                &Fill {
                    order_id: 11,
                    qty: Quantity::from_i64(2),
                    price: Price::from_i64(110),
                    ts: 3,
                    ..Fill::default()
                },
                "USD",
                10,
            )
            .unwrap();
        assert_eq!(
            ledger.position_for("main", &instrument).realized_pnl,
            Money::from_i64(200)
        );
        let mut replay = Ledger::new();
        for entry in ledger.entries().iter().cloned() {
            replay.apply_entry(entry).unwrap();
        }
        assert_eq!(
            replay.position_for("main", &instrument).realized_pnl,
            ledger.position_for("main", &instrument).realized_pnl
        );
    }
}
