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
    CorporateAction,
    RightsEntitlement,
    CashDividendEntitlement,
    ConvertibleBondInterestEntitlement,
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

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CorporateAction {
    pub cash_dividend_raw: i128,
    pub split_num: i128,
    pub split_den: i128,
}

/// 配股/增发的显式认购事实。配额不是自动成交，必须由上层策略或
/// 账户指令明确给出认购数量；这样回测与实盘对账不会凭空增加持仓。
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ShareSubscription {
    pub entitled_qty_raw: i128,
    pub subscription_qty_raw: i128,
    pub subscription_price_raw: i128,
}

/// 一次配股登记事实及其可选的账户级认购结果。未认购部分不会自动变成
/// 普通持仓，而是保留为独立权利余额，等待后续认购或失效事实。
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct RightsIssueEvent {
    pub source_instrument: InstrumentId,
    pub rights_instrument: InstrumentId,
    pub currency: String,
    pub entitled_qty_raw: i128,
    pub subscription_qty_raw: i128,
    pub subscription_price_raw: i128,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ConvertibleBondConversion {
    pub bond_instrument: InstrumentId,
    pub target_instrument: InstrumentId,
    pub bond_qty_raw: i128,
    pub target_qty_raw: i128,
    pub conversion_price_raw: i128,
}

#[derive(Clone, Debug, Default)]
pub struct Ledger {
    cash: BTreeMap<(String, String), i128>,
    positions: BTreeMap<(String, InstrumentId), PositionState>,
    hedge_positions: BTreeMap<(String, InstrumentId, PositionSide), PositionState>,
    rights_entitlements: BTreeMap<(String, InstrumentId), i128>,
    cash_dividend_entitlements: BTreeMap<(String, String, InstrumentId), i128>,
    convertible_bond_interest_entitlements: BTreeMap<(String, String, InstrumentId), i128>,
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

    /// 应用配股认购。rights_instrument 可以与 source_instrument 相同（普通股配股），
    /// 也可以是交易所独立挂牌的配股权/新股标的。此方法只处理已经明确认购的部分。
    pub fn apply_rights_issue_subscription(
        &mut self,
        account_id: &str,
        source_instrument: &InstrumentId,
        rights_instrument: &InstrumentId,
        currency: &str,
        subscription: ShareSubscription,
        ts: u64,
    ) -> QxResult<Vec<u64>> {
        if subscription.entitled_qty_raw <= 0
            || subscription.subscription_qty_raw <= 0
            || subscription.subscription_qty_raw > subscription.entitled_qty_raw
            || subscription.subscription_price_raw <= 0
        {
            return Err(QxError::BusinessViolation(
                "配股认购必须提供正的配额、认购数量和认购价格，且认购数量不能超过配额".into(),
            ));
        }
        let held = self
            .position_for(account_id, source_instrument)
            .quantity
            .raw();
        if held < subscription.entitled_qty_raw {
            return Err(QxError::BusinessViolation(
                "配股认购时账户持仓不足以覆盖登记配额".into(),
            ));
        }
        self.apply_share_subscription(
            account_id,
            rights_instrument,
            currency,
            subscription.subscription_qty_raw,
            subscription.subscription_price_raw,
            ts,
        )
    }

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

    fn apply_share_subscription(
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

    fn append_share_subscription_entries(
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
        let mut staged = self.clone();
        let id = staged.apply_entry_inner(entry)?;
        *self = staged;
        Ok(id)
    }

    fn apply_entry_inner(&mut self, entry: LedgerEntry) -> QxResult<u64> {
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
        if matches!(entry.kind, LedgerEntryKind::RightsEntitlement) {
            if entry.amount.raw() != 0
                || entry.price.is_some()
                || entry.position_side.is_some()
                || entry.instrument.is_none()
                || entry.quantity.raw() == 0
            {
                return Err(QxError::Invariant("权利余额 entry 结构非法".into()));
            }
            let instrument = entry.instrument.clone().unwrap();
            let key = (entry.account_id.clone(), instrument);
            let current = self.rights_entitlements.get(&key).copied().unwrap_or(0);
            let next = current
                .checked_add(entry.quantity.raw())
                .ok_or_else(|| QxError::Invariant("权利余额溢出".into()))?;
            if next < 0 {
                return Err(QxError::BusinessViolation("权利余额不足".into()));
            }
            self.rights_entitlements.insert(key, next);
        }
        if matches!(entry.kind, LedgerEntryKind::CashDividendEntitlement) {
            if entry.price.is_some()
                || entry.position_side.is_some()
                || entry.instrument.is_none()
                || entry.quantity.raw() == 0
                || entry.amount.raw() < 0
                || (entry.quantity.raw() > 0 && entry.amount.raw() != 0)
                || (entry.quantity.raw() < 0
                    && entry.quantity.raw().checked_neg() != Some(entry.amount.raw()))
            {
                return Err(QxError::Invariant("现金分红权益 entry 结构非法".into()));
            }
            let instrument = entry.instrument.clone().unwrap();
            let key = (entry.account_id.clone(), entry.currency.clone(), instrument);
            let current = self
                .cash_dividend_entitlements
                .get(&key)
                .copied()
                .unwrap_or(0);
            let next = current
                .checked_add(entry.quantity.raw())
                .ok_or_else(|| QxError::Invariant("现金分红权益溢出".into()))?;
            if next < 0 {
                return Err(QxError::BusinessViolation("现金分红权益余额不足".into()));
            }
            self.cash_dividend_entitlements.insert(key, next);
        }
        if matches!(
            entry.kind,
            LedgerEntryKind::ConvertibleBondInterestEntitlement
        ) {
            if entry.price.is_some()
                || entry.position_side.is_some()
                || entry.instrument.is_none()
                || entry.quantity.raw() == 0
                || entry.amount.raw() < 0
                || (entry.quantity.raw() > 0 && entry.amount.raw() != 0)
                || (entry.quantity.raw() < 0
                    && entry.quantity.raw().checked_neg() != Some(entry.amount.raw()))
            {
                return Err(QxError::Invariant("可转债利息权益 entry 结构非法".into()));
            }
            let instrument = entry.instrument.clone().unwrap();
            let key = (entry.account_id.clone(), entry.currency.clone(), instrument);
            let current = self
                .convertible_bond_interest_entitlements
                .get(&key)
                .copied()
                .unwrap_or(0);
            let next = current
                .checked_add(entry.quantity.raw())
                .ok_or_else(|| QxError::Invariant("可转债利息权益溢出".into()))?;
            if next < 0 {
                return Err(QxError::BusinessViolation("可转债利息权益余额不足".into()));
            }
            self.convertible_bond_interest_entitlements
                .insert(key, next);
        }
        if entry.amount.raw() != 0 {
            let key = (entry.account_id.clone(), entry.currency.clone());
            let current = self.cash.get(&key).copied().unwrap_or(0);
            let next = current
                .checked_add(entry.amount.raw())
                .ok_or_else(|| QxError::Invariant("现金余额溢出".into()))?;
            self.cash.insert(key, next);
        }
        if matches!(entry.kind, LedgerEntryKind::TradePosition)
            || (matches!(entry.kind, LedgerEntryKind::CorporateAction) && entry.quantity.raw() != 0)
        {
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
        self.apply_entry_inner(entry)
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

    #[test]
    fn corporate_action_dividend_and_split_replay() {
        let instrument = InstrumentId::parse("000001.SZSE").unwrap();
        let mut ledger = Ledger::new();
        ledger
            .deposit("main", "CNY", Money::from_i64(10_000), 1)
            .unwrap();
        let mut buy = order(Side::Buy);
        buy.client_id = 12;
        buy.instrument = instrument.clone();
        buy.qty = Quantity::from_i64(100);
        ledger
            .apply_fill(
                &buy,
                &Fill {
                    order_id: 12,
                    qty: Quantity::from_i64(100),
                    price: Price::from_i64(10),
                    ts: 2,
                    ..Fill::default()
                },
                "CNY",
            )
            .unwrap();
        ledger
            .apply_corporate_action(
                "main",
                &instrument,
                "CNY",
                CorporateAction {
                    cash_dividend_raw: Money::from_raw(SCALE / 10).raw(),
                    split_num: 2,
                    split_den: 1,
                },
                3,
            )
            .unwrap();
        assert_eq!(
            ledger.position_for("main", &instrument).quantity.raw(),
            200 * SCALE
        );
        assert_eq!(
            ledger.cash_for("main", "CNY"),
            Money::from_i64(9_000).raw() + 10 * SCALE
        );
        let mut replay = Ledger::new();
        for entry in ledger.entries().iter().cloned() {
            replay.apply_entry(entry).unwrap();
        }
        assert_eq!(
            replay.position_for("main", &instrument),
            ledger.position_for("main", &instrument)
        );
        assert_eq!(
            replay.cash_for("main", "CNY"),
            ledger.cash_for("main", "CNY")
        );
    }

    #[test]
    fn cash_dividend_entitlement_pays_by_record_date_position() {
        let instrument = InstrumentId::parse("000001.SZSE").unwrap();
        let mut ledger = Ledger::new();
        ledger
            .deposit("main", "CNY", Money::from_i64(10_000), 1)
            .unwrap();
        let mut buy = order(Side::Buy);
        buy.client_id = 13;
        buy.instrument = instrument.clone();
        buy.qty = Quantity::from_i64(100);
        ledger
            .apply_fill(
                &buy,
                &Fill {
                    order_id: 13,
                    qty: Quantity::from_i64(100),
                    price: Price::from_i64(10),
                    ts: 2,
                    ..Fill::default()
                },
                "CNY",
            )
            .unwrap();
        ledger
            .grant_cash_dividend_entitlement(
                "main",
                &instrument,
                "CNY",
                Money::from_raw(SCALE / 10).raw(),
                3,
            )
            .unwrap();

        let mut sell = order(Side::Sell);
        sell.client_id = 14;
        sell.instrument = instrument.clone();
        sell.qty = Quantity::from_i64(100);
        ledger
            .apply_fill(
                &sell,
                &Fill {
                    order_id: 14,
                    qty: Quantity::from_i64(100),
                    price: Price::from_i64(10),
                    ts: 4,
                    ..Fill::default()
                },
                "CNY",
            )
            .unwrap();
        assert_eq!(
            ledger.cash_dividend_entitlement_for("main", &instrument, "CNY"),
            Money::from_i64(10)
        );
        ledger
            .settle_cash_dividend_entitlement("main", &instrument, "CNY", 5)
            .unwrap();
        assert_eq!(
            ledger.cash_dividend_entitlement_for("main", &instrument, "CNY"),
            Money::ZERO
        );
        assert_eq!(
            ledger.cash_for("main", "CNY"),
            Money::from_i64(10_010).raw()
        );
        let mut replay = Ledger::new();
        for entry in ledger.entries().iter().cloned() {
            replay.apply_entry(entry).unwrap();
        }
        assert_eq!(
            replay.cash_dividend_entitlement_for("main", &instrument, "CNY"),
            Money::ZERO
        );
        assert_eq!(
            replay.cash_for("main", "CNY"),
            ledger.cash_for("main", "CNY")
        );
    }

    #[test]
    fn convertible_bond_issue_interest_and_tender_replay() {
        let bond = InstrumentId::parse("123001.SZSE").unwrap();
        let mut ledger = Ledger::new();
        ledger
            .deposit("main", "CNY", Money::from_i64(100_000), 1)
            .unwrap();

        ledger
            .apply_convertible_bond_issue_subscription(
                "main",
                &bond,
                "CNY",
                Quantity::from_i64(100).raw(),
                Price::from_i64(100).raw(),
                2,
            )
            .unwrap();
        assert_eq!(
            ledger.position_for("main", &bond).quantity,
            Quantity::from_i64(100)
        );
        assert_eq!(
            ledger.cash_for("main", "CNY"),
            Money::from_i64(90_000).raw()
        );

        ledger
            .grant_convertible_bond_interest_entitlement(
                "main",
                &bond,
                "CNY",
                Price::from_i64(5).raw(),
                3,
            )
            .unwrap();
        assert_eq!(
            ledger.convertible_bond_interest_entitlement_for("main", &bond, "CNY"),
            Money::from_i64(500)
        );

        ledger
            .apply_convertible_bond_tender(
                "main",
                &bond,
                "CNY",
                Quantity::from_i64(40).raw(),
                Price::from_i64(110).raw(),
                4,
            )
            .unwrap();
        assert_eq!(
            ledger.position_for("main", &bond).quantity,
            Quantity::from_i64(60)
        );
        assert_eq!(
            ledger.convertible_bond_interest_entitlement_for("main", &bond, "CNY"),
            Money::from_i64(500)
        );

        ledger
            .settle_convertible_bond_interest_entitlement("main", &bond, "CNY", 5)
            .unwrap();
        assert_eq!(
            ledger.convertible_bond_interest_entitlement_for("main", &bond, "CNY"),
            Money::ZERO
        );
        assert_eq!(
            ledger.cash_for("main", "CNY"),
            Money::from_i64(94_900).raw()
        );

        let mut replay = Ledger::new();
        for entry in ledger.entries().iter().cloned() {
            replay.apply_entry(entry).unwrap();
        }
        assert_eq!(replay.entries(), ledger.entries());
        assert_eq!(
            replay.position_for("main", &bond),
            ledger.position_for("main", &bond)
        );
        assert_eq!(
            replay.cash_for("main", "CNY"),
            ledger.cash_for("main", "CNY")
        );
        assert_eq!(
            replay.convertible_bond_interest_entitlement_for("main", &bond, "CNY"),
            Money::ZERO
        );
    }

    #[test]
    fn rights_subscription_is_explicit_and_replayable() {
        let instrument = InstrumentId::parse("000001.SZSE").unwrap();
        let mut ledger = Ledger::new();
        ledger
            .deposit("main", "CNY", Money::from_i64(10_000), 1)
            .unwrap();
        let mut buy = order(Side::Buy);
        buy.client_id = 20;
        buy.instrument = instrument.clone();
        buy.qty = Quantity::from_i64(100);
        ledger
            .apply_fill(
                &buy,
                &Fill {
                    order_id: 20,
                    qty: Quantity::from_i64(100),
                    price: Price::from_i64(10),
                    ts: 2,
                    ..Fill::default()
                },
                "CNY",
            )
            .unwrap();
        ledger
            .apply_rights_issue_subscription(
                "main",
                &instrument,
                &instrument,
                "CNY",
                ShareSubscription {
                    entitled_qty_raw: Quantity::from_i64(20).raw(),
                    subscription_qty_raw: Quantity::from_i64(20).raw(),
                    subscription_price_raw: Price::from_i64(5).raw(),
                },
                3,
            )
            .unwrap();
        assert_eq!(
            ledger.position_for("main", &instrument).quantity.raw(),
            Quantity::from_i64(120).raw()
        );
        assert_eq!(ledger.cash_for("main", "CNY"), Money::from_i64(8_900).raw());
        let mut replay = Ledger::new();
        for entry in ledger.entries().iter().cloned() {
            replay.apply_entry(entry).unwrap();
        }
        assert_eq!(
            replay.cash_for("main", "CNY"),
            ledger.cash_for("main", "CNY")
        );
        assert_eq!(
            replay.position_for("main", &instrument),
            ledger.position_for("main", &instrument)
        );
    }

    #[test]
    fn rights_entitlement_lifecycle_supports_partial_subscription_and_expiry() {
        let source = InstrumentId::parse("000001.SZSE").unwrap();
        let rights = InstrumentId::parse("700001.SZSE").unwrap();
        let mut ledger = Ledger::new();
        ledger
            .deposit("main", "CNY", Money::from_i64(10_000), 1)
            .unwrap();
        let mut buy = order(Side::Buy);
        buy.client_id = 22;
        buy.instrument = source.clone();
        buy.qty = Quantity::from_i64(100);
        ledger
            .apply_fill(
                &buy,
                &Fill {
                    order_id: 22,
                    qty: Quantity::from_i64(100),
                    price: Price::from_i64(10),
                    ts: 2,
                    ..Fill::default()
                },
                "CNY",
            )
            .unwrap();

        ledger
            .apply_rights_issue_event(
                "main",
                RightsIssueEvent {
                    source_instrument: source.clone(),
                    rights_instrument: rights.clone(),
                    currency: "CNY".into(),
                    entitled_qty_raw: Quantity::from_i64(20).raw(),
                    subscription_qty_raw: Quantity::from_i64(10).raw(),
                    subscription_price_raw: Price::from_i64(5).raw(),
                },
                3,
            )
            .unwrap();
        assert_eq!(
            ledger.rights_entitlement_for("main", &rights).raw(),
            Quantity::from_i64(10).raw()
        );
        assert_eq!(
            ledger.position_for("main", &source).quantity.raw(),
            Quantity::from_i64(100).raw()
        );
        assert_eq!(
            ledger.position_for("main", &rights).quantity.raw(),
            Quantity::from_i64(10).raw()
        );
        assert_eq!(ledger.cash_for("main", "CNY"), Money::from_i64(8_950).raw());

        ledger
            .expire_rights_entitlement("main", &rights, Quantity::from_i64(10).raw(), "CNY", 4)
            .unwrap();
        assert_eq!(
            ledger.rights_entitlement_for("main", &rights),
            Quantity::ZERO
        );

        let mut replay = Ledger::new();
        for entry in ledger.entries().iter().cloned() {
            replay.apply_entry(entry).unwrap();
        }
        assert_eq!(
            replay.rights_entitlement_for("main", &rights),
            ledger.rights_entitlement_for("main", &rights)
        );
        assert_eq!(
            replay.position_for("main", &rights),
            ledger.position_for("main", &rights)
        );
        assert_eq!(
            replay.cash_for("main", "CNY"),
            ledger.cash_for("main", "CNY")
        );
    }

    #[test]
    fn rights_entitlement_can_be_granted_then_subscribed_after_position_changes() {
        let source = InstrumentId::parse("000001.SZSE").unwrap();
        let rights = InstrumentId::parse("700001.SZSE").unwrap();
        let mut ledger = Ledger::new();
        ledger
            .deposit("main", "CNY", Money::from_i64(10_000), 1)
            .unwrap();
        let mut buy = order(Side::Buy);
        buy.client_id = 23;
        buy.instrument = source.clone();
        buy.qty = Quantity::from_i64(100);
        ledger
            .apply_fill(
                &buy,
                &Fill {
                    order_id: 23,
                    qty: Quantity::from_i64(100),
                    price: Price::from_i64(10),
                    ts: 2,
                    ..Fill::default()
                },
                "CNY",
            )
            .unwrap();
        ledger
            .grant_rights_entitlement(
                "main",
                &source,
                &rights,
                "CNY",
                Quantity::from_i64(20).raw(),
                3,
            )
            .unwrap();

        let mut sell = order(Side::Sell);
        sell.client_id = 24;
        sell.instrument = source.clone();
        sell.qty = Quantity::from_i64(100);
        ledger
            .apply_fill(
                &sell,
                &Fill {
                    order_id: 24,
                    qty: Quantity::from_i64(100),
                    price: Price::from_i64(10),
                    ts: 4,
                    ..Fill::default()
                },
                "CNY",
            )
            .unwrap();
        ledger
            .apply_rights_issue_subscription_from_entitlement(
                "main",
                &rights,
                "CNY",
                Quantity::from_i64(10).raw(),
                Price::from_i64(5).raw(),
                5,
            )
            .unwrap();
        assert_eq!(
            ledger.rights_entitlement_for("main", &rights).raw(),
            10 * SCALE
        );
        assert_eq!(
            ledger.position_for("main", &rights).quantity.raw(),
            10 * SCALE
        );
        assert_eq!(ledger.position_for("main", &source).quantity.raw(), 0);
        let mut replay = Ledger::new();
        for entry in ledger.entries().iter().cloned() {
            replay.apply_entry(entry).unwrap();
        }
        assert_eq!(
            replay.rights_entitlement_for("main", &rights),
            ledger.rights_entitlement_for("main", &rights)
        );
        assert_eq!(
            replay.position_for("main", &rights),
            ledger.position_for("main", &rights)
        );
    }

    #[test]
    fn rights_entitlement_rejects_insufficient_expiry_and_subscription_atomically() {
        let source = InstrumentId::parse("000001.SZSE").unwrap();
        let rights = InstrumentId::parse("700001.SZSE").unwrap();
        let mut ledger = Ledger::new();
        ledger
            .deposit("main", "CNY", Money::from_i64(10), 1)
            .unwrap();
        let before_entries = ledger.entries().len();
        assert!(ledger
            .apply_rights_issue_event(
                "main",
                RightsIssueEvent {
                    source_instrument: source.clone(),
                    rights_instrument: rights.clone(),
                    currency: "CNY".into(),
                    entitled_qty_raw: Quantity::from_i64(1).raw(),
                    subscription_qty_raw: Quantity::from_i64(1).raw(),
                    subscription_price_raw: Price::from_i64(100).raw(),
                },
                2,
            )
            .is_err());
        assert_eq!(ledger.entries().len(), before_entries);
        assert_eq!(
            ledger.rights_entitlement_for("main", &rights),
            Quantity::ZERO
        );
        assert!(ledger
            .expire_rights_entitlement("main", &rights, Quantity::from_i64(1).raw(), "CNY", 3)
            .is_err());
        assert_eq!(ledger.entries().len(), before_entries);
    }

    #[test]
    fn convertible_conversion_transfers_two_instruments() {
        let bond = InstrumentId::parse("123001.SZSE").unwrap();
        let stock = InstrumentId::parse("000001.SZSE").unwrap();
        let mut ledger = Ledger::new();
        ledger
            .deposit("main", "CNY", Money::from_i64(1_000), 1)
            .unwrap();
        let mut buy = order(Side::Buy);
        buy.client_id = 21;
        buy.instrument = bond.clone();
        buy.qty = Quantity::from_i64(10);
        ledger
            .apply_fill(
                &buy,
                &Fill {
                    order_id: 21,
                    qty: Quantity::from_i64(10),
                    price: Price::from_i64(100),
                    ts: 2,
                    ..Fill::default()
                },
                "CNY",
            )
            .unwrap();
        ledger
            .apply_convertible_bond_conversion(
                "main",
                "CNY",
                ConvertibleBondConversion {
                    bond_instrument: bond.clone(),
                    target_instrument: stock.clone(),
                    bond_qty_raw: Quantity::from_i64(2).raw(),
                    target_qty_raw: Quantity::from_i64(20).raw(),
                    conversion_price_raw: Price::from_i64(10).raw(),
                },
                3,
            )
            .unwrap();
        assert_eq!(
            ledger.position_for("main", &bond).quantity.raw(),
            Quantity::from_i64(8).raw()
        );
        assert_eq!(
            ledger.position_for("main", &stock).quantity.raw(),
            Quantity::from_i64(20).raw()
        );
        assert_eq!(ledger.position_for("main", &bond).realized_pnl, Money::ZERO);
        let mut replay = Ledger::new();
        for entry in ledger.entries().iter().cloned() {
            replay.apply_entry(entry).unwrap();
        }
        assert_eq!(replay.entries(), ledger.entries());
        assert_eq!(
            replay.position_for("main", &stock),
            ledger.position_for("main", &stock)
        );
    }

    #[test]
    fn subscription_rejects_insufficient_cash_without_partial_entries() {
        let instrument = InstrumentId::parse("000001.SZSE").unwrap();
        let mut ledger = Ledger::new();
        ledger
            .deposit("main", "CNY", Money::from_i64(1), 1)
            .unwrap();
        let before = ledger.entries().len();
        let result = ledger.apply_new_share_subscription(
            "main",
            &instrument,
            "CNY",
            Quantity::from_i64(1).raw(),
            Price::from_i64(5).raw(),
            2,
        );
        assert!(matches!(result, Err(QxError::BusinessViolation(_))));
        assert_eq!(ledger.entries().len(), before);
        assert_eq!(ledger.cash_for("main", "CNY"), Money::from_i64(1).raw());
    }
}
