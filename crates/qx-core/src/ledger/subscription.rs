//! 认购与可转债事实归约：新股申购、可转债发行/付息/要约/转股、回购要约。

use super::{ConvertibleBondConversion, Ledger, LedgerEntry, LedgerEntryKind};
use crate::error::{QxError, QxResult};
use crate::identity::InstrumentId;
use crate::numeric::{Money, Price, Quantity, SCALE};

impl Ledger {
    /// 应用明确的新股/增发认购。与配股不同，它不要求账户先持有原标的，
    /// 但仍然必须显式给出认购数量和价格。
    pub fn apply_new_share_subscription(
        &mut self,
        account_id: &str,
        issue_instrument: &InstrumentId,
        currency: &str,
        subscription_qty_raw: i128,
        issue_price_raw: i128,
        ts: u64,
    ) -> QxResult<Vec<u64>> {
        if subscription_qty_raw <= 0 || issue_price_raw <= 0 {
            return Err(QxError::BusinessViolation(
                "增发认购数量和价格必须为正".into(),
            ));
        }
        self.apply_share_subscription(
            account_id,
            issue_instrument,
            currency,
            subscription_qty_raw,
            issue_price_raw,
            ts,
        )
    }

    /// 应用明确的可转债发行/配售认购。发行人发行本身不是账户事实；只有
    /// 账户已经确认的认购数量和价格才允许进入 Ledger，现金腿与债券持仓
    /// 在同一追加事实序列中产生。
    pub fn apply_convertible_bond_issue_subscription(
        &mut self,
        account_id: &str,
        bond_instrument: &InstrumentId,
        currency: &str,
        subscription_qty_raw: i128,
        issue_price_raw: i128,
        ts: u64,
    ) -> QxResult<Vec<u64>> {
        if subscription_qty_raw <= 0 || issue_price_raw <= 0 {
            return Err(QxError::BusinessViolation(
                "可转债发行认购数量和价格必须为正".into(),
            ));
        }
        self.apply_share_subscription(
            account_id,
            bond_instrument,
            currency,
            subscription_qty_raw,
            issue_price_raw,
            ts,
        )
    }

    /// 在可转债利息登记日按债券持仓锁定待支付利息；利息不会按支付日
    /// 当前持仓重新计算。
    pub fn grant_convertible_bond_interest_entitlement(
        &mut self,
        account_id: &str,
        bond_instrument: &InstrumentId,
        currency: &str,
        interest_per_bond_raw: i128,
        ts: u64,
    ) -> QxResult<Option<u64>> {
        if interest_per_bond_raw < 0 {
            return Err(QxError::BusinessViolation(
                "可转债每债券利息不能为负".into(),
            ));
        }
        let quantity = self
            .position_for(account_id, bond_instrument)
            .quantity
            .raw();
        if quantity <= 0 || interest_per_bond_raw == 0 {
            return Ok(None);
        }
        let amount = quantity
            .checked_mul(interest_per_bond_raw)
            .and_then(|value| value.checked_div(SCALE))
            .ok_or_else(|| QxError::Invariant("可转债利息权益计算溢出".into()))?;
        if amount <= 0 {
            return Ok(None);
        }
        self.append(LedgerEntry {
            id: 0,
            account_id: account_id.into(),
            currency: currency.into(),
            kind: LedgerEntryKind::ConvertibleBondInterestEntitlement,
            amount: Money::ZERO,
            instrument: Some(bond_instrument.clone()),
            quantity: Quantity::from_raw(amount),
            price: None,
            order_id: None,
            ts,
            multiplier: 1,
            position_side: None,
        })
        .map(Some)
    }

    /// 在支付日结算登记日锁定的可转债利息权益。
    pub fn settle_convertible_bond_interest_entitlement(
        &mut self,
        account_id: &str,
        bond_instrument: &InstrumentId,
        currency: &str,
        ts: u64,
    ) -> QxResult<Option<u64>> {
        let pending = self
            .convertible_bond_interest_entitlements
            .get(&(account_id.into(), currency.into(), bond_instrument.clone()))
            .copied()
            .unwrap_or(0);
        if pending <= 0 {
            return Ok(None);
        }
        self.append(LedgerEntry {
            id: 0,
            account_id: account_id.into(),
            currency: currency.into(),
            kind: LedgerEntryKind::ConvertibleBondInterestEntitlement,
            amount: Money::from_raw(pending),
            instrument: Some(bond_instrument.clone()),
            quantity: Quantity::from_raw(-pending),
            price: None,
            order_id: None,
            ts,
            multiplier: 1,
            position_side: None,
        })
        .map(Some)
    }

    pub fn convertible_bond_interest_entitlement_for(
        &self,
        account_id: &str,
        bond_instrument: &InstrumentId,
        currency: &str,
    ) -> Money {
        Money::from_raw(
            self.convertible_bond_interest_entitlements
                .get(&(account_id.into(), currency.into(), bond_instrument.clone()))
                .copied()
                .unwrap_or(0),
        )
    }

    /// 账户明确接受的可转债回售/赎回结算，复用现金腿与债券交付的原子语义。
    pub fn apply_convertible_bond_tender(
        &mut self,
        account_id: &str,
        bond_instrument: &InstrumentId,
        currency: &str,
        quantity_raw: i128,
        price_raw: i128,
        ts: u64,
    ) -> QxResult<Vec<u64>> {
        if quantity_raw <= 0 || price_raw <= 0 {
            return Err(QxError::BusinessViolation(
                "可转债回售/赎回数量和价格必须为正".into(),
            ));
        }
        self.apply_repurchase_tender(
            account_id,
            bond_instrument,
            currency,
            quantity_raw,
            price_raw,
            ts,
        )
    }

    /// 应用已获配并成交的回购要约。回购不是所有持有人都自动参与，调用方
    /// 必须提供被接受的数量；数量和现金腿在同一份追加式账簿事实中产生。
    pub fn apply_repurchase_tender(
        &mut self,
        account_id: &str,
        instrument: &InstrumentId,
        currency: &str,
        tender_qty_raw: i128,
        tender_price_raw: i128,
        ts: u64,
    ) -> QxResult<Vec<u64>> {
        if tender_qty_raw <= 0 || tender_price_raw <= 0 {
            return Err(QxError::BusinessViolation(
                "回购要约数量和价格必须为正".into(),
            ));
        }
        if self.position_for(account_id, instrument).quantity.raw() < tender_qty_raw {
            return Err(QxError::BusinessViolation("回购要约可交付持仓不足".into()));
        }
        let mut staged = self.clone();
        let proceeds = tender_qty_raw
            .checked_mul(tender_price_raw)
            .and_then(|value| value.checked_div(SCALE))
            .ok_or_else(|| QxError::Invariant("回购要约现金腿溢出".into()))?;
        let cash_id = staged.append(LedgerEntry {
            id: 0,
            account_id: account_id.into(),
            currency: currency.into(),
            kind: LedgerEntryKind::CorporateAction,
            amount: Money::from_raw(proceeds),
            instrument: Some(instrument.clone()),
            quantity: Quantity::ZERO,
            price: None,
            order_id: None,
            ts,
            multiplier: 1,
            position_side: None,
        })?;
        let position_id = staged.append(LedgerEntry {
            id: 0,
            account_id: account_id.into(),
            currency: currency.into(),
            kind: LedgerEntryKind::CorporateAction,
            amount: Money::ZERO,
            instrument: Some(instrument.clone()),
            quantity: Quantity::from_raw(-tender_qty_raw),
            price: Some(Price::from_raw(tender_price_raw)),
            order_id: None,
            ts,
            multiplier: 1,
            position_side: None,
        })?;
        *self = staged;
        Ok(vec![cash_id, position_id])
    }

    /// 应用可转债转股。转股是债券减少与目标股票增加的双腿转换，必须由
    /// 上层传入已确认的转债数量、目标股票数量和转股价，禁止隐式推导。
    pub fn apply_convertible_bond_conversion(
        &mut self,
        account_id: &str,
        currency: &str,
        conversion: ConvertibleBondConversion,
        ts: u64,
    ) -> QxResult<Vec<u64>> {
        if conversion.bond_qty_raw <= 0
            || conversion.target_qty_raw <= 0
            || conversion.conversion_price_raw <= 0
        {
            return Err(QxError::BusinessViolation(
                "可转债转股数量、目标数量和转股价必须为正".into(),
            ));
        }
        if self
            .position_for(account_id, &conversion.bond_instrument)
            .quantity
            .raw()
            < conversion.bond_qty_raw
        {
            return Err(QxError::BusinessViolation(
                "可转债转股时债券持仓不足".into(),
            ));
        }
        let mut staged = self.clone();
        let bond_price = staged
            .position_for(account_id, &conversion.bond_instrument)
            .average_entry;
        let bond_id = staged.append(LedgerEntry {
            id: 0,
            account_id: account_id.into(),
            currency: currency.into(),
            kind: LedgerEntryKind::CorporateAction,
            amount: Money::ZERO,
            instrument: Some(conversion.bond_instrument.clone()),
            quantity: Quantity::from_raw(-conversion.bond_qty_raw),
            price: Some(bond_price),
            order_id: None,
            ts,
            multiplier: 1,
            position_side: None,
        })?;
        let target_id = staged.append(LedgerEntry {
            id: 0,
            account_id: account_id.into(),
            currency: currency.into(),
            kind: LedgerEntryKind::CorporateAction,
            amount: Money::ZERO,
            instrument: Some(conversion.target_instrument.clone()),
            quantity: Quantity::from_raw(conversion.target_qty_raw),
            price: Some(Price::from_raw(conversion.conversion_price_raw)),
            order_id: None,
            ts,
            multiplier: 1,
            position_side: None,
        })?;
        *self = staged;
        Ok(vec![bond_id, target_id])
    }

    pub(super) fn apply_share_subscription(
        &mut self,
        account_id: &str,
        instrument: &InstrumentId,
        currency: &str,
        quantity_raw: i128,
        price_raw: i128,
        ts: u64,
    ) -> QxResult<Vec<u64>> {
        let mut staged = self.clone();
        let ids = staged.append_share_subscription_entries(
            account_id,
            instrument,
            currency,
            quantity_raw,
            price_raw,
            ts,
        )?;
        *self = staged;
        Ok(ids)
    }

    pub(super) fn append_share_subscription_entries(
        &mut self,
        account_id: &str,
        instrument: &InstrumentId,
        currency: &str,
        quantity_raw: i128,
        price_raw: i128,
        ts: u64,
    ) -> QxResult<Vec<u64>> {
        let notional = quantity_raw
            .checked_mul(price_raw)
            .and_then(|value| value.checked_div(SCALE))
            .ok_or_else(|| QxError::Invariant("认购现金腿溢出".into()))?;
        if self.cash_for(account_id, currency) < notional {
            return Err(QxError::BusinessViolation("认购结算现金余额不足".into()));
        }
        let cash_id = self.append(LedgerEntry {
            id: 0,
            account_id: account_id.into(),
            currency: currency.into(),
            kind: LedgerEntryKind::CorporateAction,
            amount: Money::from_raw(-notional),
            instrument: Some(instrument.clone()),
            quantity: Quantity::ZERO,
            price: None,
            order_id: None,
            ts,
            multiplier: 1,
            position_side: None,
        })?;
        let position_id = self.append(LedgerEntry {
            id: 0,
            account_id: account_id.into(),
            currency: currency.into(),
            kind: LedgerEntryKind::CorporateAction,
            amount: Money::ZERO,
            instrument: Some(instrument.clone()),
            quantity: Quantity::from_raw(quantity_raw),
            price: Some(Price::from_raw(price_raw)),
            order_id: None,
            ts,
            multiplier: 1,
            position_side: None,
        })?;
        Ok(vec![cash_id, position_id])
    }
}
