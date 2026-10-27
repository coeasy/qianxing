//! 成交事实归约：现货与合约成交、合约乘数、逐仓与 Hedge 分仓的成本和已实现盈亏。

use super::{exact_abs, raw_decimal, Ledger, LedgerEntry, LedgerEntryKind};
use crate::error::{QxError, QxResult};
use crate::numeric::{DecimalRounding, ExactDecimal, Money, Quantity, SCALE};
use crate::order::{ExactFillFee, Fill, FillFee, Order, Side};
use crate::trading::TradingInstrumentSpec;
use std::collections::BTreeMap;

fn exact_cash_entry(
    order: &Order,
    fill: &Fill,
    currency: &str,
    kind: LedgerEntryKind,
    amount: ExactDecimal,
) -> LedgerEntry {
    LedgerEntry {
        id: 0,
        account_id: order.account_id.clone(),
        currency: currency.trim().to_ascii_uppercase(),
        kind,
        amount: Money::ZERO,
        instrument: Some(order.instrument.clone()),
        quantity: Quantity::ZERO,
        price: None,
        order_id: Some(fill.order_id),
        ts: fill.ts,
        multiplier: 1,
        position_side: None,
        leverage: None,
        exact_amounts: Some(super::ExactLedgerAmounts {
            amount,
            quantity: ExactDecimal::ZERO,
            price: None,
            contract_size: None,
            inverse: None,
        }),
    }
}

fn exact_fee_charges_for_ledger(
    fill: &Fill,
    settlement_currency: &str,
    base_asset: Option<&str>,
) -> QxResult<Vec<(String, ExactDecimal)>> {
    if fill.exact_fees.is_empty() {
        return fee_charges_for_ledger(fill, settlement_currency, base_asset).map(|rows| {
            rows.into_iter()
                .map(|(currency, amount)| (currency, raw_decimal(amount)))
                .collect()
        });
    }
    if fill.fee.raw() != 0 || !fill.fees.is_empty() {
        return Err(QxError::ReconcileRequired(
            "同一精确成交不能混用 raw fees 与 exact_fees".into(),
        ));
    }
    let mut by_currency = BTreeMap::<String, ExactDecimal>::new();
    for ExactFillFee {
        amount, currency, ..
    } in &fill.exact_fees
    {
        if amount.is_zero() {
            continue;
        }
        let currency = currency
            .as_deref()
            .map(str::trim)
            .filter(|currency| !currency.is_empty())
            .ok_or_else(|| QxError::ReconcileRequired("非零精确手续费必须声明币种".into()))?
            .to_ascii_uppercase();
        let total = by_currency.entry(currency).or_insert(ExactDecimal::ZERO);
        *total = total
            .checked_add(*amount)
            .ok_or_else(|| QxError::Invariant("精确手续费累计溢出".into()))?;
    }
    Ok(by_currency
        .into_iter()
        .filter(|(_, amount)| !amount.is_zero())
        .collect())
}

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

/// Normalize all fee facts into deterministic, native-currency ledger rows.
/// Legacy scalar fees retain the historical quote/base conversion path; explicit `fees[]`
/// are already denominated in their venue currencies and must never be cross-summed.
fn fee_charges_for_ledger(
    fill: &Fill,
    currency: &str,
    base: Option<&str>,
) -> QxResult<Vec<(String, i128)>> {
    if fill.fees.is_empty() {
        let raw = fee_in_settlement_raw(fill, currency, base)?;
        return Ok((raw != 0)
            .then(|| (currency.trim().to_ascii_uppercase(), raw))
            .into_iter()
            .collect());
    }

    let mut by_currency = BTreeMap::<String, i128>::new();
    for FillFee {
        amount, currency, ..
    } in fill.fee_charges().map_err(QxError::ReconcileRequired)?
    {
        if amount.raw() == 0 {
            continue;
        }
        let currency = currency
            .as_deref()
            .map(str::trim)
            .filter(|currency| !currency.is_empty())
            .ok_or_else(|| {
                QxError::ReconcileRequired("非零成交手续费缺少币种，拒绝猜测费用单位".into())
            })?
            .to_ascii_uppercase();
        let total = by_currency.entry(currency).or_default();
        *total = total
            .checked_add(amount.raw())
            .ok_or_else(|| QxError::Invariant("成交手续费累计溢出".into()))?;
    }
    Ok(by_currency
        .into_iter()
        .filter(|(_, amount)| *amount != 0)
        .collect())
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
        if order.exact_amounts.is_some() || fill.exact_amounts.is_some() {
            return Err(QxError::BusinessViolation(
                "精确成交尚未接入多尺度现金/持仓账簿，拒绝按 1e9 raw 值记账".into(),
            ));
        }
        if fill.order_id != order.client_id {
            return Err(QxError::Invariant("成交 order_id 与订单不一致".into()));
        }
        if !fill.account_id.is_empty() && fill.account_id != order.account_id {
            return Err(QxError::Invariant("成交账户与订单账户不一致".into()));
        }
        if fill.price.raw() <= 0 {
            return Err(QxError::BusinessViolation("成交价格必须为正".into()));
        }
        // 异币种费用只有在现货口径下能按成交价折算，见 `fee_in_settlement_raw`。
        let base = (multiplier == 1)
            .then(|| base_asset_of(&order.instrument.symbol, currency))
            .flatten();
        let fee_charges = fee_charges_for_ledger(fill, currency, base.as_deref())?;
        if matches!(
            order.status,
            crate::order::OrderStatus::Rejected
                | crate::order::OrderStatus::Cancelled
                | crate::order::OrderStatus::Expired
        ) {
            return Err(QxError::BusinessViolation("终态订单不能记入成交".into()));
        }
        let mut staged = self.clone();
        let ids = staged.apply_fill_with_multiplier_inner(
            order,
            fill,
            currency,
            multiplier,
            &fee_charges,
        )?;
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
        if order.exact_amounts.is_some() || fill.exact_amounts.is_some() {
            if spec.product == crate::TradingProduct::Spot {
                return self.apply_exact_spot_fill(order, fill, spec);
            }
            if spec.product.is_derivative() {
                return self.apply_exact_derivative_fill(order, fill, currency, spec);
            }
            return Err(QxError::BusinessViolation(
                "精确保证金或期权成交尚无认证的账簿归约规则".into(),
            ));
        }
        // `TradingInstrumentSpec` 统一覆盖现货与衍生品，但现货仍必须按
        // 买入扣款/卖出入账的现金语义处理；只有保证金、永续和期货使用
        // “持仓 + 已实现 PnL”路径。
        if spec.product == crate::TradingProduct::Spot {
            // Spot cash belongs to the instrument's quote asset. The account's
            // reporting currency is an equity/risk concern and must not change
            // the currency in which a market fill is booked.
            let quote_currency = spec.quote_currency.trim().to_ascii_uppercase();
            return self.apply_fill_with_multiplier(order, fill, &quote_currency, 1);
        }
        if fill.order_id != order.client_id
            || (!fill.account_id.is_empty() && fill.account_id != order.account_id)
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
        // 衍生费用只认结算币种：折成结算币种在这里还要过 `contract_size` 与 inverse 口径，
        // 算错的代价比拒记更高，所以现货之外的折算一律不开。
        let settlement_currency = spec.settlement_currency.trim();
        if !settlement_currency.eq_ignore_ascii_case(currency.trim()) {
            return Err(QxError::BusinessViolation(format!(
                "衍生成交记账币种 {} 与规格结算币种 {settlement_currency} 不一致",
                currency.trim()
            )));
        }
        let fee_charges = fee_charges_for_ledger(fill, currency, None)?;
        let ledger_currency = settlement_currency.to_ascii_uppercase();
        let ids = staged.apply_derivative_fill_inner(
            order,
            fill,
            &ledger_currency,
            spec,
            &fee_charges,
        )?;
        *self = staged;
        Ok(ids)
    }

    fn apply_exact_spot_fill(
        &mut self,
        order: &Order,
        fill: &Fill,
        spec: &TradingInstrumentSpec,
    ) -> QxResult<Vec<u64>> {
        let order_amounts = order.exact_amounts.ok_or_else(|| {
            QxError::BusinessViolation("精确成交对应的订单没有 exact_amounts".into())
        })?;
        let fill_amounts = fill.exact_amounts.ok_or_else(|| {
            QxError::BusinessViolation("精确订单收到没有 exact_amounts 的成交".into())
        })?;
        order.validate().map_err(QxError::BusinessViolation)?;
        fill.validate_exact_amounts()
            .map_err(QxError::BusinessViolation)?;
        if fill_amounts.quote_budget_complete && order_amounts.quote_budget.is_none() {
            return Err(QxError::BusinessViolation(
                "非 quote_budget 订单不能标记预算完成".into(),
            ));
        }
        if fill.order_id != order.client_id
            || (!fill.account_id.is_empty() && fill.account_id != order.account_id)
        {
            return Err(QxError::Invariant("精确成交身份与订单不一致".into()));
        }
        if matches!(
            order.status,
            crate::order::OrderStatus::Rejected
                | crate::order::OrderStatus::Cancelled
                | crate::order::OrderStatus::Expired
        ) {
            return Err(QxError::BusinessViolation("终态订单不能记入成交".into()));
        }
        if let Some(budget) = order_amounts.quote_budget {
            let cost = fill_amounts.quote_cost.ok_or_else(|| {
                QxError::BusinessViolation("计价币预算成交缺少精确计价币成本".into())
            })?;
            let remaining_budget = budget
                .checked_sub(order_amounts.quote_filled)
                .ok_or_else(|| QxError::Invariant("计价币预算剩余量计算溢出".into()))?;
            if cost
                .checked_cmp(remaining_budget)
                .is_none_or(|ordering| ordering.is_gt())
            {
                return Err(QxError::BusinessViolation(
                    "精确成交花费超过剩余计价币预算".into(),
                ));
            }
        } else {
            let remaining = order_amounts
                .quantity
                .checked_sub(order_amounts.filled)
                .ok_or_else(|| QxError::Invariant("精确订单剩余量计算溢出".into()))?;
            if fill_amounts
                .quantity
                .checked_cmp(remaining)
                .is_none_or(|ordering| ordering.is_gt())
            {
                return Err(QxError::BusinessViolation("精确成交超过订单剩余量".into()));
            }
        }
        spec.validate_exact_fill(fill_amounts.quantity, fill_amounts.price)?;
        if let Some(limit) = order_amounts.limit_price {
            let comparison = fill_amounts
                .price
                .checked_cmp(limit)
                .ok_or_else(|| QxError::Invariant("精确限价比较失败".into()))?;
            if (order.side == Side::Buy && comparison.is_gt())
                || (order.side == Side::Sell && comparison.is_lt())
            {
                return Err(QxError::ReconcileRequired(
                    "精确成交价格越过订单限价".into(),
                ));
            }
        }
        let fees =
            exact_fee_charges_for_ledger(fill, &spec.quote_currency, Some(&spec.base_currency))?;
        let rounding = if order.side == Side::Buy {
            DecimalRounding::AwayFromZero
        } else {
            DecimalRounding::TowardZero
        };
        let cash_flow = match fill_amounts.quote_cost {
            Some(cost) => cost,
            None => spec.quote_cash_exact(fill_amounts.quantity, fill_amounts.price, rounding)?,
        };
        let signed_cash = match order.side {
            Side::Buy => cash_flow
                .checked_neg()
                .ok_or_else(|| QxError::Invariant("精确买入现金取反溢出".into()))?,
            Side::Sell => cash_flow,
        };
        let signed_quantity = match order.side {
            Side::Buy => fill_amounts.quantity,
            Side::Sell => fill_amounts
                .quantity
                .checked_neg()
                .ok_or_else(|| QxError::Invariant("精确卖出数量取反溢出".into()))?,
        };

        let mut staged = self.clone();
        let mut ids = Vec::with_capacity(2 + fees.len());
        ids.push(staged.append(exact_cash_entry(
            order,
            fill,
            spec.quote_currency.trim(),
            LedgerEntryKind::TradeCash,
            signed_cash,
        ))?);
        ids.push(staged.append(LedgerEntry {
            id: 0,
            account_id: order.account_id.clone(),
            currency: spec.quote_currency.trim().to_ascii_uppercase(),
            kind: LedgerEntryKind::TradePosition,
            amount: Money::ZERO,
            instrument: Some(order.instrument.clone()),
            quantity: Quantity::ZERO,
            price: None,
            order_id: Some(fill.order_id),
            ts: fill.ts,
            multiplier: 1,
            position_side: None,
            leverage: None,
            exact_amounts: Some(super::ExactLedgerAmounts {
                amount: ExactDecimal::ZERO,
                quantity: signed_quantity,
                price: Some(fill_amounts.price),
                contract_size: None,
                inverse: None,
            }),
        })?);
        for (currency, amount) in fees {
            let debit = amount
                .checked_neg()
                .ok_or_else(|| QxError::Invariant("精确手续费取反溢出".into()))?;
            ids.push(staged.append(exact_cash_entry(
                order,
                fill,
                &currency,
                LedgerEntryKind::Fee,
                debit,
            ))?);
        }
        *self = staged;
        Ok(ids)
    }

    fn apply_exact_derivative_fill(
        &mut self,
        order: &Order,
        fill: &Fill,
        currency: &str,
        spec: &TradingInstrumentSpec,
    ) -> QxResult<Vec<u64>> {
        let order_amounts = order.exact_amounts.ok_or_else(|| {
            QxError::BusinessViolation("精确衍生成交对应的订单缺少 exact_amounts".into())
        })?;
        let fill_amounts = fill.exact_amounts.ok_or_else(|| {
            QxError::BusinessViolation("精确衍生订单收到没有 exact_amounts 的成交".into())
        })?;
        order.validate().map_err(QxError::BusinessViolation)?;
        fill.validate_exact_amounts()
            .map_err(QxError::BusinessViolation)?;
        if fill_amounts.quote_budget_complete {
            return Err(QxError::BusinessViolation(
                "衍生品成交不能携带 quote_budget 完成标记".into(),
            ));
        }
        let policy = order.policy.unwrap_or_default();
        policy.validate_for(spec)?;
        if !spec.product.is_derivative() {
            return Err(QxError::BusinessViolation(
                "精确衍生账簿只支持永续和期货".into(),
            ));
        }
        if order.instrument != spec.instrument
            || fill.order_id != order.client_id
            || (!fill.account_id.is_empty() && fill.account_id != order.account_id)
        {
            return Err(QxError::Invariant("精确衍生成交身份与规格不一致".into()));
        }
        if !spec
            .settlement_currency
            .eq_ignore_ascii_case(currency.trim())
        {
            return Err(QxError::BusinessViolation(format!(
                "精确衍生成交记账币种 {} 与规格结算币种 {} 不一致",
                currency.trim(),
                spec.settlement_currency
            )));
        }
        if matches!(
            order.status,
            crate::order::OrderStatus::Rejected
                | crate::order::OrderStatus::Cancelled
                | crate::order::OrderStatus::Expired
        ) {
            return Err(QxError::BusinessViolation("终态订单不能记入成交".into()));
        }
        let remaining = order_amounts
            .quantity
            .checked_sub(order_amounts.filled)
            .ok_or_else(|| QxError::Invariant("精确衍生订单剩余量计算溢出".into()))?;
        if fill_amounts
            .quantity
            .checked_cmp(remaining)
            .is_none_or(|ordering| ordering.is_gt())
        {
            return Err(QxError::BusinessViolation(
                "精确衍生成交超过订单剩余量".into(),
            ));
        }
        spec.validate_exact_fill(fill_amounts.quantity, fill_amounts.price)?;
        if let Some(limit) = order_amounts.limit_price {
            let comparison = fill_amounts
                .price
                .checked_cmp(limit)
                .ok_or_else(|| QxError::Invariant("精确衍生限价比较失败".into()))?;
            if (order.side == Side::Buy && comparison.is_gt())
                || (order.side == Side::Sell && comparison.is_lt())
            {
                return Err(QxError::ReconcileRequired(
                    "精确衍生成交价格越过订单限价".into(),
                ));
            }
        }

        let position_side = if policy.position_mode == crate::trading::PositionMode::Hedge {
            policy.position_side
        } else {
            crate::trading::PositionSide::Net
        };
        let existing =
            self.position_for_side_exact(&order.account_id, &order.instrument, position_side);
        let signed_delta = match order.side {
            Side::Buy => fill_amounts.quantity,
            Side::Sell => fill_amounts
                .quantity
                .checked_neg()
                .ok_or_else(|| QxError::Invariant("精确衍生成交方向取反溢出".into()))?,
        };
        let closes_existing = !existing.quantity.is_zero()
            && existing.quantity.coefficient().signum() != signed_delta.coefficient().signum();
        let close_quantity = if closes_existing {
            let current_abs = exact_abs(existing.quantity)?;
            if current_abs
                .checked_cmp(fill_amounts.quantity)
                .is_some_and(|ordering| ordering.is_lt())
            {
                current_abs
            } else {
                fill_amounts.quantity
            }
        } else {
            ExactDecimal::ZERO
        };
        if policy.reduce_only
            && (!closes_existing
                || close_quantity
                    .checked_cmp(fill_amounts.quantity)
                    .is_none_or(|ordering| ordering.is_lt()))
        {
            return Err(QxError::BusinessViolation(
                "精确 reduce_only 成交必须只减少目标持仓腿且不得反向穿仓".into(),
            ));
        }

        let settlement = spec.settlement_currency.trim().to_ascii_uppercase();
        let fee_charges = exact_fee_charges_for_ledger(fill, &settlement, None)?;
        let signed_pnl_quantity = if existing.quantity.is_negative() {
            close_quantity
                .checked_neg()
                .ok_or_else(|| QxError::Invariant("精确空头平仓数量溢出".into()))?
        } else {
            close_quantity
        };
        let realized_pnl = if close_quantity.is_zero() {
            ExactDecimal::ZERO
        } else {
            spec.unrealized_pnl_exact(
                signed_pnl_quantity,
                existing.average_entry,
                fill_amounts.price,
            )?
        };
        let contract_size = spec
            .exact_rules
            .as_ref()
            .map(|rules| rules.contract_size)
            .unwrap_or_else(|| {
                ExactDecimal::from_parts(spec.contract_size, 9)
                    .expect("legacy contract size uses supported scale 9")
            });

        let mut staged = self.clone();
        let mut ids = Vec::with_capacity(2 + fee_charges.len());
        if !realized_pnl.is_zero() {
            ids.push(staged.append(exact_cash_entry(
                order,
                fill,
                &settlement,
                LedgerEntryKind::TradeCash,
                realized_pnl,
            ))?);
        }
        ids.push(
            staged.append(LedgerEntry {
                id: 0,
                account_id: order.account_id.clone(),
                currency: settlement,
                kind: LedgerEntryKind::TradePosition,
                amount: Money::ZERO,
                instrument: Some(order.instrument.clone()),
                quantity: Quantity::ZERO,
                price: None,
                order_id: Some(fill.order_id),
                ts: fill.ts,
                multiplier: 1,
                position_side: (policy.position_mode == crate::trading::PositionMode::Hedge)
                    .then_some(policy.position_side),
                leverage: Some(policy.leverage),
                exact_amounts: Some(super::ExactLedgerAmounts {
                    amount: ExactDecimal::ZERO,
                    quantity: signed_delta,
                    price: Some(fill_amounts.price),
                    contract_size: Some(contract_size),
                    inverse: Some(spec.inverse),
                }),
            })?,
        );
        for (fee_currency, amount) in fee_charges {
            let debit = amount
                .checked_neg()
                .ok_or_else(|| QxError::Invariant("精确衍生手续费取反溢出".into()))?;
            ids.push(staged.append(exact_cash_entry(
                order,
                fill,
                &fee_currency,
                LedgerEntryKind::Fee,
                debit,
            ))?);
        }
        *self = staged;
        Ok(ids)
    }

    pub(super) fn apply_derivative_fill_inner(
        &mut self,
        order: &Order,
        fill: &Fill,
        currency: &str,
        spec: &TradingInstrumentSpec,
        fee_charges: &[(String, i128)],
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

                    leverage: None,

                    exact_amounts: None,
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
            leverage: Some(policy.leverage),
            exact_amounts: None,
        })?);
        for (fee_currency, fee_raw) in fee_charges {
            ids.push(
                self.append(LedgerEntry {
                    id: 0,
                    account_id: order.account_id.clone(),
                    currency: fee_currency.clone(),
                    kind: LedgerEntryKind::Fee,
                    amount: Money::from_raw(
                        fee_raw
                            .checked_neg()
                            .ok_or_else(|| QxError::Invariant("成交费用取反溢出".into()))?,
                    ),
                    instrument: Some(order.instrument.clone()),
                    quantity: Quantity::ZERO,
                    price: None,
                    order_id: Some(fill.order_id),
                    ts: fill.ts,
                    multiplier: 1,
                    position_side: None,

                    leverage: None,

                    exact_amounts: None,
                })?,
            );
        }
        Ok(ids)
    }

    pub(super) fn apply_fill_with_multiplier_inner(
        &mut self,
        order: &Order,
        fill: &Fill,
        currency: &str,
        multiplier: i128,
        fee_charges: &[(String, i128)],
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

            leverage: None,

            exact_amounts: None,
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

            leverage: None,

            exact_amounts: None,
        })?;
        let mut ids = vec![cash_id, pos_id];
        // 费用以交易所回报币种逐项入账；只有旧版单币种费用才会在上游折算。
        for (fee_currency, fee_raw) in fee_charges {
            let fee_id = self.append(LedgerEntry {
                id: 0,
                account_id: order.account_id.clone(),
                currency: fee_currency.clone(),
                kind: LedgerEntryKind::Fee,
                amount: Money::from_raw(
                    fee_raw
                        .checked_neg()
                        .ok_or_else(|| QxError::Invariant("成交费用取反溢出".into()))?,
                ),
                instrument: Some(order.instrument.clone()),
                quantity: Quantity::ZERO,
                price: None,
                order_id: Some(fill.order_id),
                ts: fill.ts,
                multiplier,
                position_side: None,

                leverage: None,

                exact_amounts: None,
            })?;
            ids.push(fee_id);
        }
        Ok(ids)
    }
}
