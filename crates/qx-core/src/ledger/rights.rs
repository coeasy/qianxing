//! 配股权利事实归约：登记、认购、失效。配额不会自动变成普通持仓。

use super::{Ledger, LedgerEntry, LedgerEntryKind, RightsIssueEvent};
use crate::error::{QxError, QxResult};
use crate::identity::InstrumentId;
use crate::numeric::{Money, Quantity};

impl Ledger {
    /// 在登记日授予独立配股权利，不产生认购现金或普通持仓。
    ///
    /// 该事实与后续认购/失效分离，允许回测准确处理“登记日持有、除权日
    /// 已卖出、认购期内再认购”的生命周期，而不会重新读取除权日持仓伪造配额。
    pub fn grant_rights_entitlement(
        &mut self,
        account_id: &str,
        source_instrument: &InstrumentId,
        rights_instrument: &InstrumentId,
        currency: &str,
        entitled_qty_raw: i128,
        ts: u64,
    ) -> QxResult<u64> {
        if entitled_qty_raw <= 0 {
            return Err(QxError::BusinessViolation("配股登记配额必须为正".into()));
        }
        if self
            .position_for(account_id, source_instrument)
            .quantity
            .raw()
            < entitled_qty_raw
        {
            return Err(QxError::BusinessViolation(
                "配股登记日账户持仓不足以覆盖权利配额".into(),
            ));
        }
        self.append(LedgerEntry {
            id: 0,
            account_id: account_id.into(),
            currency: currency.into(),
            kind: LedgerEntryKind::RightsEntitlement,
            amount: Money::ZERO,
            instrument: Some(rights_instrument.clone()),
            quantity: Quantity::from_raw(entitled_qty_raw),
            price: None,
            order_id: None,
            ts,
            multiplier: 1,
            position_side: None,
        })
    }

    /// 使用登记日已经授予的权利完成认购，并原子扣减权利余额。
    pub fn apply_rights_issue_subscription_from_entitlement(
        &mut self,
        account_id: &str,
        rights_instrument: &InstrumentId,
        currency: &str,
        subscription_qty_raw: i128,
        subscription_price_raw: i128,
        ts: u64,
    ) -> QxResult<Vec<u64>> {
        if subscription_qty_raw <= 0 || subscription_price_raw <= 0 {
            return Err(QxError::BusinessViolation(
                "配股认购数量和价格必须为正".into(),
            ));
        }
        if self
            .rights_entitlement_for(account_id, rights_instrument)
            .raw()
            < subscription_qty_raw
        {
            return Err(QxError::BusinessViolation("配股权利余额不足".into()));
        }
        let mut staged = self.clone();
        let mut ids = staged.append_share_subscription_entries(
            account_id,
            rights_instrument,
            currency,
            subscription_qty_raw,
            subscription_price_raw,
            ts,
        )?;
        ids.push(staged.append(LedgerEntry {
            id: 0,
            account_id: account_id.into(),
            currency: currency.into(),
            kind: LedgerEntryKind::RightsEntitlement,
            amount: Money::ZERO,
            instrument: Some(rights_instrument.clone()),
            quantity: Quantity::from_raw(-subscription_qty_raw),
            price: None,
            order_id: None,
            ts,
            multiplier: 1,
            position_side: None,
        })?);
        *self = staged;
        Ok(ids)
    }

    /// 应用一条完整的配股权利事实：先登记账户配额，再按明确参与数量
    /// 扣款/入股；未认购部分保留在独立权利余额中，可在截止日失效。
    pub fn apply_rights_issue_event(
        &mut self,
        account_id: &str,
        event: RightsIssueEvent,
        ts: u64,
    ) -> QxResult<Vec<u64>> {
        if event.entitled_qty_raw <= 0
            || event.subscription_qty_raw < 0
            || event.subscription_qty_raw > event.entitled_qty_raw
            || (event.subscription_qty_raw > 0 && event.subscription_price_raw <= 0)
        {
            return Err(QxError::BusinessViolation("配股权利事实参数非法".into()));
        }
        if self
            .position_for(account_id, &event.source_instrument)
            .quantity
            .raw()
            < event.entitled_qty_raw
        {
            return Err(QxError::BusinessViolation(
                "配股登记日账户持仓不足以覆盖权利配额".into(),
            ));
        }
        let mut staged = self.clone();
        let grant_id = staged.append(LedgerEntry {
            id: 0,
            account_id: account_id.into(),
            currency: event.currency.clone(),
            kind: LedgerEntryKind::RightsEntitlement,
            amount: Money::ZERO,
            instrument: Some(event.rights_instrument.clone()),
            quantity: Quantity::from_raw(event.entitled_qty_raw),
            price: None,
            order_id: None,
            ts,
            multiplier: 1,
            position_side: None,
        })?;
        let mut ids = vec![grant_id];
        if event.subscription_qty_raw > 0 {
            ids.extend(staged.append_share_subscription_entries(
                account_id,
                &event.rights_instrument,
                &event.currency,
                event.subscription_qty_raw,
                event.subscription_price_raw,
                ts,
            )?);
            ids.push(staged.append(LedgerEntry {
                id: 0,
                account_id: account_id.into(),
                currency: event.currency.clone(),
                kind: LedgerEntryKind::RightsEntitlement,
                amount: Money::ZERO,
                instrument: Some(event.rights_instrument.clone()),
                quantity: Quantity::from_raw(-event.subscription_qty_raw),
                price: None,
                order_id: None,
                ts,
                multiplier: 1,
                position_side: None,
            })?);
        }
        *self = staged;
        Ok(ids)
    }

    /// 截止日使未认购权利失效；权利余额独立于普通持仓，不产生现金或股票。
    pub fn expire_rights_entitlement(
        &mut self,
        account_id: &str,
        rights_instrument: &InstrumentId,
        expired_qty_raw: i128,
        currency: &str,
        ts: u64,
    ) -> QxResult<u64> {
        if expired_qty_raw <= 0 {
            return Err(QxError::BusinessViolation("失效权利数量必须为正".into()));
        }
        if self
            .rights_entitlement_for(account_id, rights_instrument)
            .raw()
            < expired_qty_raw
        {
            return Err(QxError::BusinessViolation("失效权利余额不足".into()));
        }
        self.append(LedgerEntry {
            id: 0,
            account_id: account_id.into(),
            currency: currency.into(),
            kind: LedgerEntryKind::RightsEntitlement,
            amount: Money::ZERO,
            instrument: Some(rights_instrument.clone()),
            quantity: Quantity::from_raw(-expired_qty_raw),
            price: None,
            order_id: None,
            ts,
            multiplier: 1,
            position_side: None,
        })
    }

    pub fn rights_entitlement_for(
        &self,
        account_id: &str,
        rights_instrument: &InstrumentId,
    ) -> Quantity {
        Quantity::from_raw(
            *self
                .rights_entitlements
                .get(&(account_id.to_string(), rights_instrument.clone()))
                .unwrap_or(&0),
        )
    }
}
