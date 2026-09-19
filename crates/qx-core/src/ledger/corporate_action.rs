//! 公司行为事实归约：拆股与分红，以及现金分红权益的授予和结算。

use super::{CorporateAction, Ledger, LedgerEntry, LedgerEntryKind};
use crate::error::{QxError, QxResult};
use crate::identity::InstrumentId;
use crate::numeric::{Money, Quantity, SCALE};

impl Ledger {
    /// 应用 A 股现金分红或拆股事实。现金分红按持仓数量计入结算币，
    /// 拆股通过 TradePosition 增量保持平均成本可回放。
    pub fn apply_corporate_action(
        &mut self,
        account_id: &str,
        instrument: &InstrumentId,
        currency: &str,
        action: CorporateAction,
        ts: u64,
    ) -> QxResult<Vec<u64>> {
        if action.split_num <= 0 || action.split_den <= 0 || action.cash_dividend_raw < 0 {
            return Err(QxError::BusinessViolation("公司行为参数非法".into()));
        }
        let current = self.position_for(account_id, instrument).quantity.raw();
        if current == 0 {
            return Ok(Vec::new());
        }
        let mut ids = Vec::new();
        let dividend = current
            .checked_mul(action.cash_dividend_raw)
            .and_then(|value| value.checked_div(SCALE))
            .ok_or_else(|| QxError::Invariant("公司行为现金分红溢出".into()))?;
        if dividend != 0 {
            ids.push(self.append(LedgerEntry {
                id: 0,
                account_id: account_id.into(),
                currency: currency.into(),
                kind: LedgerEntryKind::CorporateAction,
                amount: Money::from_raw(dividend),
                instrument: Some(instrument.clone()),
                quantity: Quantity::ZERO,
                price: None,
                order_id: None,
                ts,
                multiplier: 1,
                position_side: None,
            })?);
        }
        if action.split_num != action.split_den {
            let next = current
                .checked_mul(action.split_num)
                .and_then(|value| value.checked_div(action.split_den))
                .ok_or_else(|| QxError::Invariant("公司行为拆股数量溢出".into()))?;
            let delta = next
                .checked_sub(current)
                .ok_or_else(|| QxError::Invariant("公司行为拆股增量溢出".into()))?;
            if delta != 0 {
                let price = self.position_for(account_id, instrument).average_entry;
                ids.push(self.append(LedgerEntry {
                    id: 0,
                    account_id: account_id.into(),
                    currency: currency.into(),
                    kind: LedgerEntryKind::TradePosition,
                    amount: Money::ZERO,
                    instrument: Some(instrument.clone()),
                    quantity: Quantity::from_raw(delta),
                    price: Some(price),
                    order_id: None,
                    ts,
                    multiplier: 1,
                    position_side: None,
                })?);
            }
        }
        Ok(ids)
    }

    /// 在登记日按当日持仓生成现金分红待结算权益。现金不会立即进入余额，
    /// 只有支付日的结算事实才能产生可用现金。
    pub fn grant_cash_dividend_entitlement(
        &mut self,
        account_id: &str,
        instrument: &InstrumentId,
        currency: &str,
        dividend_per_share_raw: i128,
        ts: u64,
    ) -> QxResult<Option<u64>> {
        if dividend_per_share_raw < 0 {
            return Err(QxError::BusinessViolation("每股现金分红不能为负".into()));
        }
        let quantity = self.position_for(account_id, instrument).quantity.raw();
        if quantity <= 0 || dividend_per_share_raw == 0 {
            return Ok(None);
        }
        let amount = quantity
            .checked_mul(dividend_per_share_raw)
            .and_then(|value| value.checked_div(SCALE))
            .ok_or_else(|| QxError::Invariant("现金分红权益计算溢出".into()))?;
        if amount <= 0 {
            return Ok(None);
        }
        self.append(LedgerEntry {
            id: 0,
            account_id: account_id.into(),
            currency: currency.into(),
            kind: LedgerEntryKind::CashDividendEntitlement,
            amount: Money::ZERO,
            instrument: Some(instrument.clone()),
            quantity: Quantity::from_raw(amount),
            price: None,
            order_id: None,
            ts,
            multiplier: 1,
            position_side: None,
        })
        .map(Some)
    }

    /// 在支付日结算该标的全部待支付现金分红权益。结算金额只来自登记日
    /// 已生成的权益，不会因为登记日后卖出/买入而改变。
    pub fn settle_cash_dividend_entitlement(
        &mut self,
        account_id: &str,
        instrument: &InstrumentId,
        currency: &str,
        ts: u64,
    ) -> QxResult<Option<u64>> {
        let key = (
            account_id.to_string(),
            currency.to_string(),
            instrument.clone(),
        );
        let amount = self
            .cash_dividend_entitlements
            .get(&key)
            .copied()
            .unwrap_or(0);
        if amount <= 0 {
            return Ok(None);
        }
        self.append(LedgerEntry {
            id: 0,
            account_id: account_id.into(),
            currency: currency.into(),
            kind: LedgerEntryKind::CashDividendEntitlement,
            amount: Money::from_raw(amount),
            instrument: Some(instrument.clone()),
            quantity: Quantity::from_raw(-amount),
            price: None,
            order_id: None,
            ts,
            multiplier: 1,
            position_side: None,
        })
        .map(Some)
    }

    pub fn cash_dividend_entitlement_for(
        &self,
        account_id: &str,
        instrument: &InstrumentId,
        currency: &str,
    ) -> Money {
        Money::from_raw(
            self.cash_dividend_entitlements
                .get(&(
                    account_id.to_string(),
                    currency.to_string(),
                    instrument.clone(),
                ))
                .copied()
                .unwrap_or(0),
        )
    }
}
