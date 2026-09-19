//! 事件驱动账簿。
//!
//! 余额和持仓不允许被策略直接写入，只能由 Fill、费用、资金费或结算事实
//! 经过本模块归约。账簿保留追加式 LedgerEntry，便于重放与对账。

use crate::error::{QxError, QxResult};
use crate::identity::InstrumentId;
use crate::numeric::{Money, Price, Quantity, SCALE};
use crate::trading::PositionSide;
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

mod cash;
mod corporate_action;
mod fill;
mod query;
mod rights;
mod subscription;

impl Ledger {
    pub fn new() -> Self {
        Self::default()
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

    fn append(&mut self, mut entry: LedgerEntry) -> QxResult<u64> {
        entry.id = self.next_id;
        self.apply_entry_inner(entry)
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
