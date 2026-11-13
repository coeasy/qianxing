//! 现金事实归约：入金、资金费、利息、交割、调整与强平。

use super::{
    ExactInstrumentFunding, ExactLedgerAmounts, InstrumentFunding, Ledger, LedgerEntry,
    LedgerEntryKind,
};
use crate::error::{QxError, QxResult};
use crate::identity::InstrumentId;
use crate::numeric::{ExactDecimal, Money, Quantity};
use crate::order::FillFee;
use std::collections::BTreeMap;

impl Ledger {
    /// Apply venue-reported fee changes that arrive after the fill quantity stopped moving.
    /// Amounts remain in their native currencies; positive fee amounts debit cash, negative
    /// rebates credit it. Runtime event identity provides replay idempotency.
    pub fn apply_fee_adjustment(
        &mut self,
        account_id: &str,
        order_id: u64,
        instrument: &InstrumentId,
        fees: &[FillFee],
        ts: u64,
    ) -> QxResult<Vec<u64>> {
        if account_id.trim().is_empty() || fees.is_empty() {
            return Err(QxError::ReconcileRequired(
                "手续费调整缺少账户或费用明细".into(),
            ));
        }
        let mut by_currency = BTreeMap::<String, i128>::new();
        for fee in fees {
            if fee.amount.raw() == 0 {
                continue;
            }
            let currency = fee
                .currency
                .as_deref()
                .map(str::trim)
                .filter(|currency| !currency.is_empty())
                .ok_or_else(|| QxError::ReconcileRequired("非零手续费调整缺少币种".into()))?
                .to_ascii_uppercase();
            let total = by_currency.entry(currency).or_default();
            *total = total
                .checked_add(fee.amount.raw())
                .ok_or_else(|| QxError::Invariant("手续费调整累计溢出".into()))?;
        }
        let mut staged = self.clone();
        let mut ids = Vec::new();
        for (currency, amount) in by_currency {
            if amount == 0 {
                continue;
            }
            ids.push(
                staged.append(LedgerEntry {
                    id: 0,
                    account_id: account_id.into(),
                    currency,
                    kind: LedgerEntryKind::Fee,
                    amount: Money::from_raw(
                        amount
                            .checked_neg()
                            .ok_or_else(|| QxError::Invariant("手续费调整取反溢出".into()))?,
                    ),
                    instrument: Some(instrument.clone()),
                    quantity: Quantity::ZERO,
                    price: None,
                    order_id: Some(order_id),
                    ts,
                    multiplier: 1,
                    position_side: None,
                    leverage: None,
                    exact_amounts: None,
                })?,
            );
        }
        if ids.is_empty() {
            return Err(QxError::ReconcileRequired(
                "手续费调整合并后没有非零费用".into(),
            ));
        }
        *self = staged;
        Ok(ids)
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

            leverage: None,

            exact_amounts: None,
        })
    }

    /// Seed an account with a scale-aware cash amount while retaining the legacy raw projection.
    /// The projection is toward zero; the exact amount is authoritative for exact-aware readers.
    pub fn deposit_exact(
        &mut self,
        account_id: &str,
        currency: &str,
        amount: ExactDecimal,
        ts: u64,
    ) -> QxResult<u64> {
        if account_id.trim().is_empty() || currency.trim().is_empty() || !amount.is_positive() {
            return Err(QxError::BusinessViolation(
                "精确入金需要账户、币种和正金额".into(),
            ));
        }
        self.append(LedgerEntry {
            id: 0,
            account_id: account_id.into(),
            currency: currency.trim().to_ascii_uppercase(),
            kind: LedgerEntryKind::Adjustment,
            amount: Money::ZERO,
            instrument: None,
            quantity: Quantity::ZERO,
            price: None,
            order_id: None,
            ts,
            multiplier: 1,
            position_side: None,
            leverage: None,
            exact_amounts: Some(ExactLedgerAmounts {
                amount,
                quantity: ExactDecimal::ZERO,
                price: None,
                contract_size: None,
                inverse: None,
            }),
        })
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

    /// Apply a funding cashflow with the derivative exposure and mark that
    /// produced it, so replay and audit can trace the cash movement to a market.
    pub fn apply_instrument_funding(
        &mut self,
        account_id: &str,
        currency: &str,
        funding: InstrumentFunding<'_>,
    ) -> QxResult<u64> {
        if funding.quantity.raw() == 0 || funding.mark_price.raw() <= 0 {
            return Err(QxError::BusinessViolation(
                "标的资金费必须包含非零持仓和正标记价".into(),
            ));
        }
        self.append(LedgerEntry {
            id: 0,
            account_id: account_id.into(),
            currency: currency.into(),
            kind: LedgerEntryKind::Funding,
            amount: funding.amount,
            instrument: Some(funding.instrument.clone()),
            quantity: funding.quantity,
            price: Some(funding.mark_price),
            order_id: None,
            ts: funding.ts,
            multiplier: 1,
            position_side: None,

            leverage: None,

            exact_amounts: None,
        })
    }

    /// Exact derivative funding entry retaining the position and mark used in the calculation.
    pub fn apply_instrument_funding_exact(
        &mut self,
        account_id: &str,
        currency: &str,
        funding: ExactInstrumentFunding<'_>,
    ) -> QxResult<u64> {
        if account_id.trim().is_empty()
            || currency.trim().is_empty()
            || funding.quantity.is_zero()
            || !funding.mark_price.is_positive()
            || !funding.contract_size.is_positive()
        {
            return Err(QxError::BusinessViolation(
                "精确标的资金费必须包含账户、币种、非零持仓、正标记价和正合约乘数".into(),
            ));
        }
        self.append(LedgerEntry {
            id: 0,
            account_id: account_id.into(),
            currency: currency.trim().to_ascii_uppercase(),
            kind: LedgerEntryKind::Funding,
            amount: Money::ZERO,
            instrument: Some(funding.instrument.clone()),
            quantity: Quantity::ZERO,
            price: None,
            order_id: None,
            ts: funding.ts,
            multiplier: 1,
            position_side: None,
            leverage: None,
            exact_amounts: Some(ExactLedgerAmounts {
                amount: funding.amount,
                quantity: funding.quantity,
                price: Some(funding.mark_price),
                contract_size: Some(funding.contract_size),
                inverse: Some(funding.inverse),
            }),
        })
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

    pub(super) fn apply_cash_entry(
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

            leverage: None,

            exact_amounts: None,
        })
    }
}
