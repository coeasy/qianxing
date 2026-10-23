//! 账簿只读投影：余额、持仓与权益（含合约乘数与跨币种 FX）。
//!
//! 本文件的 `equity*` 是**算术原语**：现货按乘数、衍生按合约规格、跨币种再叠一层 FX。
//! 「该用哪把尺子」的派发单源是 [`Ledger::valuate`]（P1-6 估值单点），它在下面按
//! 规格/汇率选一把原语并叠上保证金占用——调用方不要再自己判断该调哪个 `equity_*`。

use super::{Ledger, PositionState};
use crate::error::{QxError, QxResult};
use crate::identity::InstrumentId;
use crate::numeric::{Money, Price, Quantity, SCALE};
use crate::trading::TradingInstrumentSpec;
use crate::valuation::{ValuationContext, ValuationResult};
use std::collections::BTreeMap;

impl Ledger {
    /// Return the exact marked equity contribution of one instrument above the account's
    /// unallocated starting cash. Instrument-attributed cash ledger entries and that
    /// instrument's marked position are valued in the same reporting currency as the
    /// portfolio. The contributions are additive when every account cash movement is
    /// instrument-attributed; unallocated deposits/transfers remain outside this projection.
    pub fn instrument_equity_contribution(
        &self,
        account_id: &str,
        instrument: &InstrumentId,
        mark: Option<&Price>,
        reporting_currency: &str,
        spec: &TradingInstrumentSpec,
        fx_rates: &BTreeMap<String, Price>,
    ) -> QxResult<i128> {
        spec.validate()?;
        if spec.instrument != *instrument {
            return Err(QxError::BusinessViolation(
                "标的权益归因规格 instrument 不一致".into(),
            ));
        }

        let mut cash_by_currency = BTreeMap::<String, i128>::new();
        for entry in self.entries.iter().filter(|entry| {
            entry.account_id == account_id && entry.instrument.as_ref() == Some(instrument)
        }) {
            let amount = cash_by_currency.entry(entry.currency.clone()).or_default();
            *amount = amount
                .checked_add(entry.amount.raw())
                .ok_or_else(|| QxError::Invariant("标的现金归因累计溢出".into()))?;
        }
        let marks = mark
            .copied()
            .map(|mark| BTreeMap::from([(instrument.clone(), mark)]))
            .unwrap_or_default();
        let context = ValuationContext::spot(account_id, &marks, reporting_currency)
            .with_spec(Some(spec))
            .with_fx(fx_rates);
        self.instrument_equity_contribution_from_cash(&context, &cash_by_currency)
    }

    /// Value an instrument's P&L contribution using a caller-maintained cumulative ledger-cash
    /// projection. This lets event loops update the cash projection incrementally instead of
    /// rescanning the append-only ledger for every marked Bar. The market identity comes from
    /// the frozen single-instrument spec in the valuation context.
    pub fn instrument_equity_contribution_from_cash(
        &self,
        context: &ValuationContext<'_>,
        cash_by_currency: &BTreeMap<String, i128>,
    ) -> QxResult<i128> {
        let spec = context.spec.ok_or_else(|| {
            QxError::BusinessViolation("标的权益归因估值上下文缺少单标的规格".into())
        })?;
        let instrument = &spec.instrument;
        spec.validate()?;
        let account_id = context.account_id;
        let mark = context.marks.get(instrument);
        let mut contribution = 0_i128;
        for (currency, amount) in cash_by_currency {
            contribution = contribution
                .checked_add(Self::convert_equity_currency(
                    *amount,
                    currency,
                    context.reporting_currency,
                    context.fx_rates,
                )?)
                .ok_or_else(|| QxError::Invariant("标的现金归因估值溢出".into()))?;
        }

        let mut add_position = |position: &PositionState| -> QxResult<()> {
            if position.quantity.is_zero() {
                return Ok(());
            }
            let mark = mark.ok_or_else(|| {
                QxError::ReconcileRequired(format!("标的权益归因缺少标记价: {instrument}"))
            })?;
            let (value, currency) = Self::market_position_value(spec, position, mark)?;
            contribution = contribution
                .checked_add(Self::convert_equity_currency(
                    value,
                    currency,
                    context.reporting_currency,
                    context.fx_rates,
                )?)
                .ok_or_else(|| QxError::Invariant("标的持仓归因估值溢出".into()))?;
            Ok(())
        };
        for ((account, candidate), position) in self.iter_positions() {
            if account == account_id && candidate == instrument {
                add_position(position)?;
            }
        }
        for ((account, candidate, _), position) in self.iter_hedge_positions() {
            if account == account_id && candidate == instrument {
                add_position(position)?;
            }
        }
        Ok(contribution)
    }

    pub fn cash(&self, currency: &str) -> i128 {
        self.iter_cash()
            .filter(|((_, candidate), _)| *candidate == currency)
            .map(|(_, amount)| *amount)
            .sum()
    }

    pub fn position(&self, instrument: &InstrumentId) -> PositionState {
        let mut out = PositionState::default();
        for ((_, id), state) in self
            .iter_positions()
            .filter(|((_, id), _)| *id == instrument)
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
            .iter_hedge_positions()
            .filter(|((_, id, _), _)| *id == instrument)
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
            .get(account_id)
            .and_then(|currencies| currencies.get(currency))
            .unwrap_or(&0)
    }

    /// 账户级已实现盈亏：把该账户每条持仓（含单向与对冲腿）的 `realized_pnl` 相加（V13 R26）。
    ///
    /// `PositionState::realized_pnl` 由 `apply_position_state_delta` 在每次平仓时按
    /// "价差 × 平仓数量 × multiplier" 累加，**平仓到零也不清零**，所以这里逐条相加得到的
    /// 就是账户累计已实现盈亏。溢出返回 `None`：读模型宁可报缺席，也不发布回绕过的数。
    pub fn realized_pnl_for(&self, account_id: &str) -> Option<i128> {
        let mut total = 0_i128;
        for ((account, _), state) in self.iter_positions() {
            if account == account_id {
                total = total.checked_add(state.realized_pnl.raw())?;
            }
        }
        for ((account, _, _), state) in self.iter_hedge_positions() {
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
        for ((account, instrument), position) in self.iter_positions() {
            if account != account_id || position.quantity.is_zero() {
                continue;
            }
            let mark = marks.get(instrument)?;
            total = total.checked_add(Ledger::position_pnl(position, mark, multiplier)?)?;
        }
        for ((account, instrument, _), position) in self.iter_hedge_positions() {
            if account != account_id || position.quantity.is_zero() {
                continue;
            }
            let mark = marks.get(instrument)?;
            total = total.checked_add(Ledger::position_pnl(position, mark, multiplier)?)?;
        }
        Some(total)
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
        for ((_, instrument), position) in self.iter_positions() {
            let mark = marks.get(instrument)?;
            let n = position
                .quantity
                .raw()
                .checked_mul(mark.raw())?
                .checked_div(SCALE)?
                .checked_mul(multiplier)?;
            value = value.checked_add(n)?;
        }
        for ((_, instrument, _), position) in self.iter_hedge_positions() {
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
        for ((account, instrument), position) in self.iter_positions() {
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
        for ((account, instrument, _), position) in self.iter_hedge_positions() {
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
        Ledger::ensure_only_instrument_is_valued(self, account_id, &spec.instrument)?;
        let mut value = self.cash_for(account_id, currency);
        for ((account, instrument), position) in self.iter_positions() {
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
        for ((account, instrument, _), position) in self.iter_hedge_positions() {
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

    /// 在指定报价币种下计算单品种跨币种保证金权益。
    ///
    /// `fx_rates` 的含义是“1 单位资产币种可兑换多少报价币种”。
    pub fn equity_for_with_spec_and_fx(
        &self,
        account_id: &str,
        marks: &BTreeMap<crate::InstrumentId, Price>,
        reporting_currency: &str,
        spec: &TradingInstrumentSpec,
        fx_rates: &BTreeMap<String, Price>,
    ) -> QxResult<i128> {
        spec.validate()?;
        Ledger::ensure_only_instrument_is_valued(self, account_id, &spec.instrument)?;
        let convert = |amount: i128, currency: &str| {
            Ledger::convert_equity_currency(amount, currency, reporting_currency, fx_rates)
        };
        let mut value = 0_i128;
        for ((account, currency), amount) in self.iter_cash() {
            if account == account_id {
                value = value
                    .checked_add(convert(*amount, currency)?)
                    .ok_or_else(|| QxError::Invariant("跨币种现金权益溢出".into()))?;
            }
        }
        let settlement_currency = &spec.settlement_currency;
        for ((account, instrument), position) in self.iter_positions() {
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
        for ((account, instrument, _), position) in self.iter_hedge_positions() {
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

    /// 以每个 instrument 独立的冻结规格估算共享账户权益；任何非零持仓缺价格、规格或 FX 都失败关闭。
    pub fn equity_for_with_market_specs_and_fx(
        &self,
        account_id: &str,
        marks: &BTreeMap<InstrumentId, Price>,
        reporting_currency: &str,
        specs: &BTreeMap<InstrumentId, TradingInstrumentSpec>,
        fx_rates: &BTreeMap<String, Price>,
    ) -> QxResult<i128> {
        for (instrument, spec) in specs {
            spec.validate()?;
            if instrument != &spec.instrument {
                return Err(QxError::BusinessViolation(
                    "多市场估值规格的 map key 必须等于 TradingInstrumentSpec.instrument".into(),
                ));
            }
        }
        let mut equity = 0_i128;
        for ((account, currency), amount) in self.iter_cash() {
            if account == account_id && *amount != 0 {
                equity = equity
                    .checked_add(Ledger::convert_equity_currency(
                        *amount,
                        currency,
                        reporting_currency,
                        fx_rates,
                    )?)
                    .ok_or_else(|| QxError::Invariant("多市场现金权益溢出".into()))?;
            }
        }
        for ((account, instrument), position) in self.iter_positions() {
            if account != account_id || position.quantity.is_zero() {
                continue;
            }
            let spec = specs.get(instrument).ok_or_else(|| {
                QxError::ReconcileRequired(format!("多市场估值缺少 instrument 规格: {instrument}"))
            })?;
            let mark = marks.get(instrument).ok_or_else(|| {
                QxError::ReconcileRequired(format!(
                    "多市场估值缺少 instrument 标记价: {instrument}"
                ))
            })?;
            let (value, currency) = Ledger::market_position_value(spec, position, mark)?;
            equity = equity
                .checked_add(Ledger::convert_equity_currency(
                    value,
                    currency,
                    reporting_currency,
                    fx_rates,
                )?)
                .ok_or_else(|| QxError::Invariant("多市场持仓权益溢出".into()))?;
        }
        for ((account, instrument, _), position) in self.iter_hedge_positions() {
            if account != account_id || position.quantity.is_zero() {
                continue;
            }
            let spec = specs.get(instrument).ok_or_else(|| {
                QxError::ReconcileRequired(format!("多市场估值缺少 instrument 规格: {instrument}"))
            })?;
            let mark = marks.get(instrument).ok_or_else(|| {
                QxError::ReconcileRequired(format!(
                    "多市场估值缺少 instrument 标记价: {instrument}"
                ))
            })?;
            let (value, currency) = Ledger::market_position_value(spec, position, mark)?;
            equity = equity
                .checked_add(Ledger::convert_equity_currency(
                    value,
                    currency,
                    reporting_currency,
                    fx_rates,
                )?)
                .ok_or_else(|| QxError::Invariant("多市场对冲持仓权益溢出".into()))?;
        }
        Ok(equity)
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
        let equity_raw = if let Some(specs) = ctx.market_specs {
            self.equity_for_with_market_specs_and_fx(
                ctx.account_id,
                ctx.marks,
                ctx.reporting_currency,
                specs,
                ctx.fx_rates,
            )?
        } else {
            match ctx.spec.filter(|spec| spec.product.supports_leverage()) {
                Some(spec) if !ctx.fx_rates.is_empty() => self.equity_for_with_spec_and_fx(
                    ctx.account_id,
                    ctx.marks,
                    ctx.reporting_currency,
                    spec,
                    ctx.fx_rates,
                )?,
                Some(spec) => self.equity_for_with_spec(
                    ctx.account_id,
                    ctx.marks,
                    ctx.reporting_currency,
                    spec,
                )?,
                None => self
                    .equity_for_with_multiplier(
                        ctx.account_id,
                        ctx.marks,
                        ctx.reporting_currency,
                        ctx.multiplier,
                    )
                    .ok_or_else(|| QxError::Invariant("无法计算账户权益".into()))?,
            }
        };
        ValuationResult::new(
            Money::from_raw(equity_raw),
            ctx.margin_required,
            ctx.maintenance_required,
            ctx.reporting_currency,
        )
    }

    /// Paper accounts use the same multi-market and FX valuation rules as every other ledger
    /// consumer, with an explicit entry point so callers cannot accidentally select a legacy
    /// equity ruler. The result's `available` field is the value to inject into Paper risk.
    pub fn valuate_paper(
        &self,
        paper: &crate::valuation::PaperValuationContext<'_>,
    ) -> QxResult<ValuationResult> {
        let equity = self.equity_for_with_market_specs_and_fx(
            paper.account_id,
            paper.marks,
            paper.reporting_currency,
            paper.market_specs,
            paper.fx_rates,
        )?;
        let initial_margin = self.initial_margin_for_market_specs_and_fx(
            paper.account_id,
            paper.marks,
            paper.reporting_currency,
            paper.market_specs,
            paper.fx_rates,
        )?;
        ValuationResult::new(
            Money::from_raw(equity),
            Money::from_raw(initial_margin),
            Money::ZERO,
            paper.reporting_currency,
        )
    }

    fn initial_margin_for_market_specs_and_fx(
        &self,
        account_id: &str,
        marks: &BTreeMap<InstrumentId, Price>,
        reporting_currency: &str,
        specs: &BTreeMap<InstrumentId, TradingInstrumentSpec>,
        fx_rates: &BTreeMap<String, Price>,
    ) -> QxResult<i128> {
        let mut total = 0_i128;
        for ((account, instrument), position) in self.iter_positions() {
            if account != account_id || position.quantity.is_zero() {
                continue;
            }
            let spec = specs.get(instrument).ok_or_else(|| {
                QxError::ReconcileRequired(format!("保证金估值缺少 instrument 规格: {instrument}"))
            })?;
            if !spec.product.supports_leverage() {
                continue;
            }
            total = total
                .checked_add(position_initial_margin_in_reporting_currency(
                    position,
                    instrument,
                    spec,
                    marks,
                    reporting_currency,
                    fx_rates,
                )?)
                .ok_or_else(|| QxError::Invariant("Paper 初始保证金合计溢出".into()))?;
        }
        for ((account, instrument, _), position) in self.iter_hedge_positions() {
            if account != account_id || position.quantity.is_zero() {
                continue;
            }
            let spec = specs.get(instrument).ok_or_else(|| {
                QxError::ReconcileRequired(format!("保证金估值缺少 instrument 规格: {instrument}"))
            })?;
            if !spec.product.supports_leverage() {
                continue;
            }
            total = total
                .checked_add(position_initial_margin_in_reporting_currency(
                    position,
                    instrument,
                    spec,
                    marks,
                    reporting_currency,
                    fx_rates,
                )?)
                .ok_or_else(|| QxError::Invariant("Paper 初始保证金合计溢出".into()))?;
        }
        Ok(total)
    }

    /// Return the existing one-way and hedge-position initial margin for one instrument.
    ///
    /// Each position uses the leverage facts that created it. A non-flat position whose
    /// historical leverage is unavailable cannot be projected safely and is rejected.
    pub fn initial_margin_for_instrument(
        &self,
        account_id: &str,
        spec: &TradingInstrumentSpec,
        mark: Price,
    ) -> QxResult<i128> {
        spec.validate()?;
        if !spec.product.supports_leverage() {
            return Ok(0);
        }
        let mut total = 0_i128;
        for ((account, instrument), position) in self.iter_positions() {
            if account == account_id && instrument == &spec.instrument {
                total = total
                    .checked_add(position_initial_margin_raw(
                        position, instrument, spec, mark,
                    )?)
                    .ok_or_else(|| QxError::Invariant("标的持仓初始保证金合计溢出".into()))?;
            }
        }
        for ((account, instrument, _), position) in self.iter_hedge_positions() {
            if account == account_id && instrument == &spec.instrument {
                total = total
                    .checked_add(position_initial_margin_raw(
                        position, instrument, spec, mark,
                    )?)
                    .ok_or_else(|| QxError::Invariant("标的持仓初始保证金合计溢出".into()))?;
            }
        }
        Ok(total)
    }
}

fn position_initial_margin_raw(
    position: &PositionState,
    instrument: &InstrumentId,
    spec: &TradingInstrumentSpec,
    mark: Price,
) -> QxResult<i128> {
    if position.quantity.is_zero() {
        return Ok(0);
    }
    let margin_ratio = position.margin_ratio_raw.ok_or_else(|| {
        QxError::ReconcileRequired(format!(
            "持仓 {instrument} 缺少可回放的开仓杠杆，需从成交事实重建或对账"
        ))
    })?;
    if !(1..=SCALE).contains(&margin_ratio) {
        return Err(QxError::Invariant(format!(
            "持仓 {instrument} 的保证金比例非法: {margin_ratio}"
        )));
    }
    let qty = position
        .quantity
        .raw()
        .checked_abs()
        .ok_or_else(|| QxError::Invariant("保证金持仓绝对值溢出".into()))?;
    spec.notional(qty, mark.raw())?
        .checked_mul(margin_ratio)
        .and_then(|value| value.checked_div(SCALE))
        .ok_or_else(|| QxError::Invariant("杠杆持仓初始保证金溢出".into()))
}

fn position_initial_margin_in_reporting_currency(
    position: &PositionState,
    instrument: &InstrumentId,
    spec: &TradingInstrumentSpec,
    marks: &BTreeMap<InstrumentId, Price>,
    reporting_currency: &str,
    fx_rates: &BTreeMap<String, Price>,
) -> QxResult<i128> {
    let mark = marks.get(instrument).ok_or_else(|| {
        QxError::ReconcileRequired(format!("Paper 保证金估值缺少标记价 {instrument}"))
    })?;
    let margin = position_initial_margin_raw(position, instrument, spec, *mark)?;
    Ledger::convert_equity_currency(
        margin,
        &spec.settlement_currency,
        reporting_currency,
        fx_rates,
    )
}
