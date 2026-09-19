//! 现金事实归约：入金、资金费、利息、交割、调整与强平。

use super::{Ledger, LedgerEntry, LedgerEntryKind};
use crate::error::{QxError, QxResult};
use crate::numeric::{Money, Quantity};

impl Ledger {
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
        })
    }
}
