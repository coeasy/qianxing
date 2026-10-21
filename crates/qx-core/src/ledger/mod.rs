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

/// UTC funding settlement cadence used by the normalized perpetual market contract.
pub const FUNDING_INTERVAL_MS: u64 = 8 * 60 * 60 * 1_000;

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
    /// Leverage used to create a derivative position fill. `None` means that the
    /// entry predates leverage persistence or is not a leveraged TradePosition.
    #[serde(default)]
    pub leverage: Option<u32>,
}

/// A timestamped funding rate declared by a historical market-data source.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FundingRateEvent {
    pub instrument: InstrumentId,
    pub settlement_ts: u64,
    pub rate_bps: i64,
}

/// Funding cashflow together with the derivative exposure and mark that produced it.
#[derive(Clone, Copy, Debug)]
pub struct InstrumentFunding<'a> {
    pub instrument: &'a InstrumentId,
    pub quantity: Quantity,
    pub mark_price: Price,
    pub amount: Money,
    pub ts: u64,
}

fn default_multiplier() -> i128 {
    1
}

#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct PositionState {
    pub quantity: Quantity,
    pub average_entry: Price,
    /// 平仓累计已实现盈亏，按"价差 × 平仓数量 × `multiplier`"统计。衍生品走
    /// `TradingInstrumentSpec` 的现金腿时还会再乘 `contract_size`，所以
    /// `contract_size != SCALE` 的产品这里只是每手口径的统计，账户现金以现金腿为准。
    pub realized_pnl: Money,
    /// Quantity weighted reciprocal leverage at SCALE precision. This preserves
    /// the aggregate initial-margin ratio when fills add at different leverages.
    /// `None` is deliberately unknown and makes leveraged Paper valuation fail closed.
    #[serde(default)]
    pub margin_ratio_raw: Option<i128>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CorporateAction {
    pub cash_dividend_raw: i128,
    pub split_num: i128,
    pub split_den: i128,
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
    cash: BTreeMap<String, BTreeMap<String, i128>>,
    positions: BTreeMap<String, BTreeMap<InstrumentId, PositionState>>,
    hedge_positions:
        BTreeMap<String, BTreeMap<InstrumentId, BTreeMap<PositionSide, PositionState>>>,
    rights_entitlements: BTreeMap<(String, InstrumentId), i128>,
    cash_dividend_entitlements: BTreeMap<(String, String, InstrumentId), i128>,
    convertible_bond_interest_entitlements: BTreeMap<(String, String, InstrumentId), i128>,
    entries: Vec<LedgerEntry>,
    next_id: u64,
}

enum PositionUpdate {
    OneWay {
        account_id: String,
        instrument: InstrumentId,
        state: PositionState,
    },
    Hedge {
        account_id: String,
        instrument: InstrumentId,
        side: PositionSide,
        state: PositionState,
    },
}

fn required_instrument<'a>(
    entry: &'a LedgerEntry,
    reason: &'static str,
) -> QxResult<&'a InstrumentId> {
    entry
        .instrument
        .as_ref()
        .ok_or_else(|| QxError::Invariant(reason.into()))
}

mod cash;
mod corporate_action;
mod fill;
mod query;
mod rights;
mod state_query;
mod subscription;
mod valuation_fx;

impl Ledger {
    pub fn new() -> Self {
        Self::default()
    }

    /// 以完整 entry 重放账簿。entry id、现金和持仓变更都必须连续且可验证。
    pub fn apply_entry(&mut self, entry: LedgerEntry) -> QxResult<u64> {
        self.apply_entry_inner(entry)
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
        if entry.leverage == Some(0)
            || (entry.leverage.is_some() && !matches!(entry.kind, LedgerEntryKind::TradePosition))
        {
            return Err(QxError::Invariant(
                "杠杆只能是正数并绑定 TradePosition entry".into(),
            ));
        }
        let mut rights_update = None;
        let mut dividend_update = None;
        let mut convertible_interest_update = None;
        if matches!(entry.kind, LedgerEntryKind::RightsEntitlement) {
            if entry.amount.raw() != 0
                || entry.price.is_some()
                || entry.position_side.is_some()
                || entry.instrument.is_none()
                || entry.quantity.raw() == 0
            {
                return Err(QxError::Invariant("权利余额 entry 结构非法".into()));
            }
            let instrument = required_instrument(&entry, "权利余额 entry 缺少标的")?;
            let key = (entry.account_id.clone(), instrument.clone());
            let current = self.rights_entitlements.get(&key).copied().unwrap_or(0);
            let next = current
                .checked_add(entry.quantity.raw())
                .ok_or_else(|| QxError::Invariant("权利余额溢出".into()))?;
            if next < 0 {
                return Err(QxError::BusinessViolation("权利余额不足".into()));
            }
            rights_update = Some((key, next));
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
            let instrument = required_instrument(&entry, "现金分红权益 entry 缺少标的")?;
            let key = (
                entry.account_id.clone(),
                entry.currency.clone(),
                instrument.clone(),
            );
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
            dividend_update = Some((key, next));
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
            let instrument = required_instrument(&entry, "可转债利息权益 entry 缺少标的")?;
            let key = (
                entry.account_id.clone(),
                entry.currency.clone(),
                instrument.clone(),
            );
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
            convertible_interest_update = Some((key, next));
        }
        let cash_update = if entry.amount.raw() != 0 {
            let current = self
                .cash
                .get(&entry.account_id)
                .and_then(|currencies| currencies.get(&entry.currency))
                .copied()
                .unwrap_or(0);
            let next = current
                .checked_add(entry.amount.raw())
                .ok_or_else(|| QxError::Invariant("现金余额溢出".into()))?;
            Some((entry.account_id.clone(), entry.currency.clone(), next))
        } else {
            None
        };
        let mut position_update = None;
        if matches!(entry.kind, LedgerEntryKind::TradePosition)
            || (matches!(entry.kind, LedgerEntryKind::CorporateAction) && entry.quantity.raw() != 0)
        {
            let instrument = entry
                .instrument
                .as_ref()
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
                let mut state = self
                    .hedge_positions
                    .get(&entry.account_id)
                    .and_then(|positions| positions.get(instrument))
                    .and_then(|sides| sides.get(&position_side))
                    .cloned()
                    .unwrap_or_default();
                apply_position_state_delta(
                    &mut state,
                    entry.quantity.raw(),
                    price,
                    entry.multiplier,
                    entry.leverage,
                    matches!(entry.kind, LedgerEntryKind::CorporateAction),
                )?;
                position_update = Some(PositionUpdate::Hedge {
                    account_id: entry.account_id.clone(),
                    instrument: instrument.clone(),
                    side: position_side,
                    state,
                });
            } else {
                let mut state = self
                    .positions
                    .get(&entry.account_id)
                    .and_then(|positions| positions.get(instrument))
                    .cloned()
                    .unwrap_or_default();
                apply_position_state_delta(
                    &mut state,
                    entry.quantity.raw(),
                    price,
                    entry.multiplier,
                    entry.leverage,
                    matches!(entry.kind, LedgerEntryKind::CorporateAction),
                )?;
                position_update = Some(PositionUpdate::OneWay {
                    account_id: entry.account_id.clone(),
                    instrument: instrument.clone(),
                    state,
                });
            }
        }
        let id = entry.id;
        let next_id = self
            .next_id
            .checked_add(1)
            .ok_or_else(|| QxError::Invariant("账簿序号溢出".into()))?;

        // Validate and calculate every fallible projection before touching the ledger. The only
        // staged copies above are the affected PositionState and keys, not the full append-only
        // ledger; successful commits therefore stay O(log accounts/instruments) instead of
        // cloning all prior entries on every replayed fact.
        if let Some((key, value)) = rights_update {
            self.rights_entitlements.insert(key, value);
        }
        if let Some((key, value)) = dividend_update {
            self.cash_dividend_entitlements.insert(key, value);
        }
        if let Some((key, value)) = convertible_interest_update {
            self.convertible_bond_interest_entitlements
                .insert(key, value);
        }
        if let Some((account_id, currency, value)) = cash_update {
            self.cash
                .entry(account_id)
                .or_default()
                .insert(currency, value);
        }
        if let Some(update) = position_update {
            match update {
                PositionUpdate::OneWay {
                    account_id,
                    instrument,
                    state,
                } => {
                    self.positions
                        .entry(account_id)
                        .or_default()
                        .insert(instrument, state);
                }
                PositionUpdate::Hedge {
                    account_id,
                    instrument,
                    side,
                    state,
                } => {
                    self.hedge_positions
                        .entry(account_id)
                        .or_default()
                        .entry(instrument)
                        .or_default()
                        .insert(side, state);
                }
            }
        }
        self.next_id = next_id;
        self.entries.push(entry);
        Ok(id)
    }

    pub(super) fn iter_positions(
        &self,
    ) -> impl Iterator<Item = ((&str, &InstrumentId), &PositionState)> {
        self.positions.iter().flat_map(|(account, positions)| {
            positions
                .iter()
                .map(move |(instrument, state)| ((account.as_str(), instrument), state))
        })
    }

    pub(super) fn iter_cash(&self) -> impl Iterator<Item = ((&str, &str), &i128)> {
        self.cash.iter().flat_map(|(account, currencies)| {
            currencies
                .iter()
                .map(move |(currency, amount)| ((account.as_str(), currency.as_str()), amount))
        })
    }

    pub(super) fn iter_hedge_positions(
        &self,
    ) -> impl Iterator<Item = ((&str, &InstrumentId, PositionSide), &PositionState)> {
        self.hedge_positions
            .iter()
            .flat_map(|(account, positions)| {
                positions.iter().flat_map(move |(instrument, sides)| {
                    sides
                        .iter()
                        .map(move |(side, state)| ((account.as_str(), instrument, *side), state))
                })
            })
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
    leverage: Option<u32>,
    preserve_margin_ratio: bool,
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
        if preserve_margin_ratio {
            // Corporate actions may rescale quantity while preserving economic exposure.
        } else if current == 0 {
            state.margin_ratio_raw = margin_ratio_for_leverage(leverage)?;
        } else {
            state.margin_ratio_raw = match (state.margin_ratio_raw, leverage) {
                (Some(existing), Some(leverage)) => {
                    let add_ratio = margin_ratio_for_leverage(Some(leverage))?
                        .ok_or_else(|| QxError::Invariant("新开仓杠杆缺失".into()))?;
                    let weighted_ratio = old_abs
                        .checked_mul(existing)
                        .and_then(|value| add_abs.checked_mul(add_ratio)?.checked_add(value))
                        .ok_or_else(|| QxError::Invariant("持仓保证金比例合计溢出".into()))?;
                    Some(weighted_ratio / next_abs)
                }
                _ => None,
            };
        }
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
            state.margin_ratio_raw = None;
        } else if current.signum() != next.signum() {
            state.average_entry = price;
            state.margin_ratio_raw = margin_ratio_for_leverage(leverage)?;
        }
    }
    state.quantity = Quantity::from_raw(next);
    Ok(())
}

fn margin_ratio_for_leverage(leverage: Option<u32>) -> QxResult<Option<i128>> {
    leverage
        .map(|leverage| {
            if leverage == 0 {
                return Err(QxError::BusinessViolation("持仓杠杆必须大于 0".into()));
            }
            Ok(SCALE / i128::from(leverage))
        })
        .transpose()
}
