//! 账簿只读投影：余额、持仓与权益（含合约乘数与跨币种 FX）。
//!
//! 本文件的 `equity*` 是**算术原语**：现货按乘数、衍生按合约规格、跨币种再叠一层 FX。
//! 「该用哪把尺子」的派发单源是 [`Ledger::valuate`]（P1-6 估值单点），它在下面按
//! 规格/汇率选一把原语并叠上保证金占用——调用方不要再自己判断该调哪个 `equity_*`。

use super::{Ledger, LedgerEntry, PositionState};
use crate::error::{QxError, QxResult};
use crate::identity::InstrumentId;
use crate::numeric::{Money, Price, Quantity, SCALE};
use crate::trading::{PositionSide, TradingInstrumentSpec};
use crate::valuation::{ValuationContext, ValuationResult};
use std::collections::BTreeMap;

impl Ledger {
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

    /// 账户级已实现盈亏：把该账户每条持仓（含单向与对冲腿）的 `realized_pnl` 相加（V13 R26）。
    ///
    /// `PositionState::realized_pnl` 由 `apply_position_state_delta` 在每次平仓时按
    /// "价差 × 平仓数量 × multiplier" 累加，**平仓到零也不清零**，所以这里逐条相加得到的
    /// 就是账户累计已实现盈亏。溢出返回 `None`：读模型宁可报缺席，也不发布回绕过的数。
    pub fn realized_pnl_for(&self, account_id: &str) -> Option<i128> {
        let mut total = 0_i128;
        for ((account, _), state) in &self.positions {
            if account == account_id {
                total = total.checked_add(state.realized_pnl.raw())?;
            }
        }
        for ((account, _, _), state) in &self.hedge_positions {
            if account == account_id {
                total = total.checked_add(state.realized_pnl.raw())?;
            }
        }
        Some(total)
    }

    /// 账户级未实现盈亏（现货乘数口径）：`Σ (mark − average_entry) × quantity × multiplier`。
    ///
    /// 与 `apply_position_state_delta` 的已实现公式同一套定点算子，也与
    /// [`Self::equity_for_with_multiplier`] 同一条契约：**任何一条非零持仓拿不到标记价，
    /// 这个数就算不出来**（返回 `None`），读侧不会把"算不出"印成 0。
    ///
    /// 这是账户读模型（现货、`multiplier = 1`）用的那把尺子，与 `equity_raw` 同源；
    /// 衍生品账户需要 `TradingInstrumentSpec` 才能算准，那不在本层。
    pub fn unrealized_pnl_for(
        &self,
        account_id: &str,
        marks: &BTreeMap<InstrumentId, Price>,
        multiplier: i128,
    ) -> Option<i128> {
        if multiplier <= 0 {
            return None;
        }
        let mut total = 0_i128;
        for ((account, instrument), position) in &self.positions {
            if account != account_id || position.quantity.is_zero() {
                continue;
            }
            let mark = marks.get(instrument)?;
            total = total.checked_add(position_pnl(position, mark, multiplier)?)?;
        }
        for ((account, instrument, _), position) in &self.hedge_positions {
            if account != account_id || position.quantity.is_zero() {
                continue;
            }
            let mark = marks.get(instrument)?;
            total = total.checked_add(position_pnl(position, mark, multiplier)?)?;
        }
        Some(total)
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

    /// 估值单点（P1-6）：按上下文派发到正确的权益原语，再叠上保证金占用。
    ///
    /// 派发规则（**全仓唯一一份**）：
    /// * 有杠杆合约规格 + 有汇率 → 跨币种衍生权益（[`Self::equity_for_with_spec_and_fx`]）；
    /// * 有杠杆合约规格 → 衍生权益（[`Self::equity_for_with_spec`]）；
    /// * 其余（现货/非杠杆）→ 按乘数折算（[`Self::equity_for_with_multiplier`]）。
    ///
    /// 回测里的 `equity_for` 与 paper 账户的可用保证金派发此前各抄了一份，现在都走这里。
    /// 实现在本文件而不是 `crate::valuation`：本仓不允许在 `ledger/` 目录外另起 `impl Ledger`。
    pub fn valuate(&self, ctx: &ValuationContext<'_>) -> QxResult<ValuationResult> {
        let equity_raw = match ctx.spec.filter(|spec| spec.product.supports_leverage()) {
            Some(spec) if !ctx.fx_rates.is_empty() => self.equity_for_with_spec_and_fx(
                ctx.account_id,
                ctx.marks,
                ctx.reporting_currency,
                spec,
                ctx.fx_rates,
            )?,
            Some(spec) => {
                self.equity_for_with_spec(ctx.account_id, ctx.marks, ctx.reporting_currency, spec)?
            }
            None => self
                .equity_for_with_multiplier(
                    ctx.account_id,
                    ctx.marks,
                    ctx.reporting_currency,
                    ctx.multiplier,
                )
                .ok_or_else(|| QxError::Invariant("无法计算账户权益".into()))?,
        };
        ValuationResult::new(
            Money::from_raw(equity_raw),
            ctx.margin_required,
            ctx.maintenance_required,
            ctx.reporting_currency,
        )
    }
}

/// 单条持仓的未实现盈亏（现货乘数口径）：`(mark − average_entry) × quantity × multiplier`。
///
/// 与 `apply_position_state_delta` 的已实现公式同一套定点算子；溢出返回 `None`。
fn position_pnl(position: &PositionState, mark: &Price, multiplier: i128) -> Option<i128> {
    mark.raw()
        .checked_sub(position.average_entry.raw())?
        .checked_mul(position.quantity.raw())?
        .checked_div(SCALE)?
        .checked_mul(multiplier)
}
