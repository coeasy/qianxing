//! 成交事实归约：现货与合约成交、合约乘数、逐仓与 Hedge 分仓的成本和已实现盈亏。

use super::{Ledger, LedgerEntry, LedgerEntryKind};
use crate::error::{QxError, QxResult};
use crate::numeric::{Money, Quantity, SCALE};
use crate::order::{Fill, Order, Side};
use crate::trading::TradingInstrumentSpec;

/// 从"基准资产 + 结算币种"形态的符号里取出基准资产（`BTCUSDT` / `BTC/USDT` → `BTC`）。
///
/// 取不出就返回 `None`，让调用方走拒记分支：这里宁可挡下一笔本可以折算的成交，
/// 也不能靠猜把费用折成另一个数量。带前缀的杠杆/倒数符号（`1000SHIBUSDT`、`BTCUSD19Q`）
/// 的前缀对不上费用币种，因此同样落回 `None`。
fn base_asset_of(symbol: &str, settlement: &str) -> Option<String> {
    let symbol = symbol.trim().to_ascii_uppercase();
    let quote = settlement.trim().to_ascii_uppercase();
    if quote.is_empty() || symbol.len() <= quote.len() || !symbol.ends_with(&quote) {
        return None;
    }
    let base =
        symbol[..symbol.len() - quote.len()].trim_end_matches(|c: char| !c.is_alphanumeric());
    (!base.is_empty()).then(|| base.to_string())
}

/// 把成交费用折成账簿结算币种下的面值（raw 定点），折不出来就拒记。
///
/// 交易所回报的费用币种可以不是账簿的结算币种：Binance 现货默认从**收到的资产**里扣
/// （BTCUSDT 买入回报 `commissionAsset=BTC`），CCXT 的 `fee_currency` 同义。分三种情况：
///
/// 1. 费用本身就是结算币种，或回报根本没有币种事实（`None`）→ 按面值入账，沿用既有口径；
/// 2. 费用是这一对的基准资产 → 用**这笔成交自己的价格**折算：成交价就是基准资产对结算
///    币种的即期报价，不需要任何外部汇率。挡下这一类等于挡掉主力连接器上每天正常发生的
///    全部成交，所以必须折而不是拒；
/// 3. 其他币种（BNB 抵扣、第三币种付费）→ 账簿里没有它的价格，按面值记是凭空造钱
///    （0.001 BNB 记成 0.001 USDT 低估两个数量级），只能拒记并转待对账。
///
/// 派生品的基准资产折算需要 `contract_size` 与 inverse 口径，比现货更容易算错，
/// 因此只在无规格的现货/历史乘数路径（`multiplier == 1`）上开放。
fn fee_in_settlement_raw(fill: &Fill, currency: &str, base: Option<&str>) -> QxResult<i128> {
    let fee_raw = fill.fee.raw();
    if fee_raw == 0 {
        return Ok(0);
    }
    let Some(fee_currency) = fill.fee_currency.as_deref() else {
        return Ok(fee_raw);
    };
    if fee_currency.trim().eq_ignore_ascii_case(currency.trim()) {
        return Ok(fee_raw);
    }
    if let Some(base) = base {
        if fee_currency.trim().eq_ignore_ascii_case(base) {
            // fee_raw 与 price_raw 都是 SCALE 定点：fee*price 要除掉一份 SCALE。
            return fill
                .price
                .raw()
                .checked_mul(fee_raw)
                .and_then(|product| product.checked_div(SCALE))
                .ok_or_else(|| QxError::Invariant("成交费用折算溢出".into()));
        }
    }
    Err(QxError::ReconcileRequired(format!(
        "成交手续费币种 {fee_currency} 既不是账簿结算币种 {}，也不是这一对的基准资产，\
         账簿里没有它的价格，费用不能按面值记入结算币种；\
         请关闭交易所的异币种手续费抵扣，或把账户 settlement_currency 统一为费用币种后重新对账",
        currency.trim()
    )))
}

impl Ledger {
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
        // 异币种费用只有在现货口径下能按成交价折算，见 `fee_in_settlement_raw`。
        let base = (multiplier == 1)
            .then(|| base_asset_of(&order.instrument.symbol, currency))
            .flatten();
        let fee_raw = fee_in_settlement_raw(fill, currency, base.as_deref())?;
        if matches!(
            order.status,
            crate::order::OrderStatus::Rejected
                | crate::order::OrderStatus::Cancelled
                | crate::order::OrderStatus::Expired
        ) {
            return Err(QxError::BusinessViolation("终态订单不能记入成交".into()));
        }
        let mut staged = self.clone();
        let ids =
            staged.apply_fill_with_multiplier_inner(order, fill, currency, multiplier, fee_raw)?;
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
        // 衍生费用只认结算币种：折成结算币种在这里还要过 `contract_size` 与 inverse 口径，
        // 算错的代价比拒记更高，所以现货之外的折算一律不开。
        let fee_raw = fee_in_settlement_raw(fill, currency, None)?;
        let mut staged = self.clone();
        let ids = staged.apply_derivative_fill_inner(order, fill, currency, spec, fee_raw)?;
        *self = staged;
        Ok(ids)
    }

    pub(super) fn apply_derivative_fill_inner(
        &mut self,
        order: &Order,
        fill: &Fill,
        currency: &str,
        spec: &TradingInstrumentSpec,
        fee_raw: i128,
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
        if fee_raw != 0 {
            ids.push(self.append(LedgerEntry {
                id: 0,
                account_id: order.account_id.clone(),
                currency: currency.into(),
                kind: LedgerEntryKind::Fee,
                amount: Money::from_raw(-fee_raw),
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

    pub(super) fn apply_fill_with_multiplier_inner(
        &mut self,
        order: &Order,
        fill: &Fill,
        currency: &str,
        multiplier: i128,
        fee_raw: i128,
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
        // 折算后可能落到 0（极小费用 × 极低价），那时这一腿本就不该存在：
        // 判据用折完的数，不用回报里的原数。
        if fee_raw != 0 {
            let fee_id = self.append(LedgerEntry {
                id: 0,
                account_id: order.account_id.clone(),
                currency: currency.into(),
                kind: LedgerEntryKind::Fee,
                amount: Money::from_raw(-fee_raw),
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
}
