//! 统一交易产品、杠杆和保证金语义。
//!
//! 这里不依赖交易所；现货、保证金、永续和交割合约都通过同一组确定性
//! 定点运算表达。CCXT 只负责把交易所 market/position/funding 映射到这些
//! 契约，回测和实盘不得各自重新实现一套保证金规则。

use crate::numeric::{DecimalRounding, ExactDecimal};
use crate::{InstrumentId, Money, QxError, QxResult, DEFAULT_SETTLEMENT_CURRENCY, SCALE};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TradingProduct {
    Spot,
    Margin,
    Perpetual,
    Future,
    /// 期权（规划 §15.1 的产品类型表把 `option` 单列一格）。
    ///
    /// 刻意**不**计入 [`TradingProduct::is_derivative`]：衍生品那条分支要求 `linear`/`inverse`
    /// 恰好指定一个，而期权两者都为 false 才是对的（权利金对标的线性程度不是合约的
    /// 线性/反向属性）。期权合约身份里真正缺的那几格（行权价、到期日、认购/认沽、行权方式）
    /// 尚未建模，见 `maturity/capabilities.yaml` 的 `option_contract_identity_is_not_modelled`。
    Option,
}

impl TradingProduct {
    pub fn is_derivative(self) -> bool {
        matches!(self, Self::Perpetual | Self::Future)
    }

    pub fn supports_leverage(self) -> bool {
        !matches!(self, Self::Spot)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MarginMode {
    Cash,
    Cross,
    Isolated,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PositionMode {
    OneWay,
    Hedge,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PositionSide {
    Net,
    Long,
    Short,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ExactTradingRules {
    /// Base units per contract, or 1 for spot. Preserved as a decimal string on the wire.
    pub contract_size: ExactDecimal,
    pub price_tick: ExactDecimal,
    pub quantity_step: ExactDecimal,
    pub minimum_quantity: ExactDecimal,
    /// Quote-currency minimum order value; zero means the venue publishes no usable minimum.
    pub minimum_notional: ExactDecimal,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct TradingInstrumentSpec {
    pub instrument: InstrumentId,
    pub product: TradingProduct,
    pub base_currency: String,
    pub quote_currency: String,
    pub settlement_currency: String,
    /// 1.0 表示现货；合约可为 0.001 BTC/contract 或 1 USD/contract。
    pub contract_size: i128,
    pub linear: bool,
    pub inverse: bool,
    pub price_tick: i128,
    pub qty_step: i128,
    pub min_qty: i128,
    /// Minimum order notional in quote-currency raw units; zero means the venue did not
    /// publish a usable minimum. Kept optional at the JSON boundary for older frozen specs.
    #[serde(default, skip_serializing_if = "raw_zero")]
    pub min_notional_raw: i128,
    pub max_leverage: u32,
    pub maintenance_margin_bps: u32,
    pub valid_from: u64,
    pub valid_to: Option<u64>,
    /// Versioned decimal venue rules. Absent on historical 1e9 specifications.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exact_rules: Option<ExactTradingRules>,
}

/// Resolve the ledger currency for a backtest from its declared instrument spec,
/// using the core default only when the spec is absent.
pub fn resolve_settlement_currency(spec: Option<&TradingInstrumentSpec>) -> String {
    spec.map(|spec| spec.settlement_currency.clone())
        .unwrap_or_else(|| DEFAULT_SETTLEMENT_CURRENCY.into())
}

impl TradingInstrumentSpec {
    /// Canonical spot fallback used only when a caller explicitly permits an absent market
    /// specification. Keep the fallback definition in the domain contract so report verifiers
    /// can reproduce the exact specification used by execution.
    pub fn spot_default(instrument: &InstrumentId, settlement_currency: &str) -> Self {
        let currency = settlement_currency.trim().to_ascii_uppercase();
        Self {
            instrument: instrument.clone(),
            product: TradingProduct::Spot,
            base_currency: instrument.symbol.clone(),
            quote_currency: currency.clone(),
            settlement_currency: currency,
            contract_size: SCALE,
            linear: false,
            inverse: false,
            price_tick: 1,
            qty_step: 1,
            min_qty: 1,
            min_notional_raw: 0,
            max_leverage: 1,
            maintenance_margin_bps: 0,
            valid_from: 0,
            valid_to: None,
            exact_rules: None,
        }
    }

    pub fn validate(&self) -> QxResult<()> {
        if self.base_currency.trim().is_empty()
            || self.quote_currency.trim().is_empty()
            || self.settlement_currency.trim().is_empty()
            || self.contract_size <= 0
            || self.exact_rules.is_none()
                && (self.price_tick <= 0 || self.qty_step <= 0 || self.min_qty <= 0)
            || self.min_notional_raw < 0
            || self.price_tick < 0
            || self.qty_step < 0
            || self.min_qty < 0
            || self.max_leverage == 0
            || self.maintenance_margin_bps > 10_000
            || self.valid_to.is_some_and(|to| to <= self.valid_from)
            || (self.linear == self.inverse && self.product.is_derivative())
            || self.exact_rules.as_ref().is_some_and(|rules| {
                !rules.contract_size.is_positive()
                    || !rules.price_tick.is_positive()
                    || !rules.quantity_step.is_positive()
                    || !rules.minimum_quantity.is_positive()
                    || rules.minimum_notional.is_negative()
            })
        {
            return Err(QxError::BusinessViolation(
                "TradingInstrumentSpec 字段非法或 linear/inverse 未唯一指定".into(),
            ));
        }
        if self.product == TradingProduct::Spot
            && (self.max_leverage != 1 || self.margin_mode_default() != MarginMode::Cash)
        {
            return Err(QxError::BusinessViolation(
                "现货产品默认只能使用 Cash、1 倍杠杆".into(),
            ));
        }
        Ok(())
    }

    pub fn margin_mode_default(&self) -> MarginMode {
        if self.product == TradingProduct::Spot {
            MarginMode::Cash
        } else {
            MarginMode::Cross
        }
    }

    /// 返回结算币种定点原始值的名义额。
    pub fn notional(&self, qty: i128, price: i128) -> QxResult<i128> {
        if qty < 0 || price <= 0 || self.contract_size <= 0 {
            return Err(QxError::BusinessViolation("名义额输入非法".into()));
        }
        self.notional_exact(raw_decimal(qty), raw_decimal(price))?
            .checked_mul_at_scale(exact_one(), 9, DecimalRounding::AwayFromZero)
            .ok_or_else(|| QxError::Invariant("名义额计算溢出".into()))?
            .to_raw(9)
            .map_err(|error| QxError::Invariant(error.into()))
    }

    /// Venue order-cost amount in quote-currency raw units. Inverse contracts quote their
    /// fixed contract value directly; linear products multiply base quantity by price.
    pub fn quote_notional(&self, qty: i128, price: i128) -> QxResult<i128> {
        if qty < 0 || price <= 0 || self.contract_size <= 0 {
            return Err(QxError::BusinessViolation("订单名义金额输入非法".into()));
        }
        self.quote_notional_exact(raw_decimal(qty), raw_decimal(price))?
            .checked_mul_at_scale(exact_one(), 9, DecimalRounding::TowardZero)
            .ok_or_else(|| QxError::Invariant("订单名义金额计算溢出".into()))?
            .to_raw(9)
            .map_err(|error| QxError::Invariant(error.into()))
    }

    pub fn initial_margin(&self, qty: i128, price: i128, leverage: u32) -> QxResult<i128> {
        if leverage == 0 || leverage > self.max_leverage {
            return Err(QxError::BusinessViolation("杠杆超出产品限制".into()));
        }
        if !self.product.supports_leverage() && leverage != 1 {
            return Err(QxError::BusinessViolation(
                "现货不能使用大于 1 的杠杆".into(),
            ));
        }
        let notional = raw_decimal(self.notional(qty, price)?);
        let leverage = ExactDecimal::from_parts(i128::from(leverage), 0)
            .map_err(|error| QxError::Invariant(error.into()))?;
        notional
            .checked_div_at_scale(leverage, 9, DecimalRounding::AwayFromZero)
            .and_then(|value| value.to_raw(9).ok())
            .ok_or_else(|| QxError::Invariant("初始保证金计算溢出".into()))
    }

    /// Exact initial margin for high-precision order and account risk paths. Notional is
    /// rounded upward before division, and the resulting required margin is rounded upward
    /// again so a sub-nano amount cannot be understated at either step.
    pub fn initial_margin_exact(
        &self,
        quantity: ExactDecimal,
        price: ExactDecimal,
        leverage: u32,
    ) -> QxResult<ExactDecimal> {
        if leverage == 0 || leverage > self.max_leverage {
            return Err(QxError::BusinessViolation("杠杆超出产品限制".into()));
        }
        if !self.product.supports_leverage() && leverage != 1 {
            return Err(QxError::BusinessViolation(
                "现货不能使用大于 1 的杠杆".into(),
            ));
        }
        let notional = self.notional_exact(quantity, price)?;
        let leverage = ExactDecimal::from_parts(i128::from(leverage), 0)
            .map_err(|error| QxError::Invariant(error.into()))?;
        notional
            .checked_div_at_scale(
                leverage,
                ExactDecimal::MAX_SCALE,
                DecimalRounding::AwayFromZero,
            )
            .ok_or_else(|| QxError::Invariant("精确初始保证金计算溢出".into()))
    }

    /// 在进入 OMS/交易所适配器前校验数量和价格的交易所规格约束。
    ///
    /// 这里不做静默截断或四舍五入；策略目标若无法精确落在 lot/tick 上，
    /// 必须显式调整目标或由上层按业务规则拆单，避免回测与实盘产生不同语义。
    pub fn validate_order(&self, qty: i128, price: Option<i128>) -> QxResult<()> {
        self.validate_order_with_reference(qty, price, price)
    }

    /// Validate a market order against an observed price without treating that reference as a
    /// submitted limit price. This checks minimum quote cost while applying the tick rule only
    /// to actual limit prices.
    pub fn validate_order_with_reference(
        &self,
        qty: i128,
        limit_price: Option<i128>,
        reference_price: Option<i128>,
    ) -> QxResult<()> {
        if qty <= 0 {
            return Err(QxError::BusinessViolation("订单数量必须为正数".into()));
        }
        self.validate_exact_order_with_reference(
            raw_decimal(qty),
            limit_price.map(raw_decimal),
            reference_price.map(raw_decimal),
        )
    }

    /// Validate high-precision quantity/price values against the frozen venue rules.
    /// Amount filters use exact decimal multiples; minimum notional is rounded down so a
    /// value below the exchange floor cannot pass due to local rounding.
    pub fn validate_exact_order_with_reference(
        &self,
        quantity: ExactDecimal,
        limit_price: Option<ExactDecimal>,
        reference_price: Option<ExactDecimal>,
    ) -> QxResult<()> {
        self.validate_exact_order_precision(quantity, limit_price)?;
        let (_, _, _, minimum_notional) = self.exact_rule_values();
        if minimum_notional.is_positive() {
            let reference_price = reference_price.ok_or_else(|| {
                QxError::BusinessViolation("缺少可核验参考价，无法校验交易所最小名义金额".into())
            })?;
            let quote_notional = self.quote_notional_exact(quantity, reference_price)?;
            if !reference_price.is_positive()
                || quote_notional.checked_cmp(minimum_notional) == Some(std::cmp::Ordering::Less)
            {
                return Err(QxError::BusinessViolation(
                    "订单名义金额低于交易所最小限制".into(),
                ));
            }
        }
        Ok(())
    }

    /// Validate exact lot/tick/minimum quantity rules without a market-order reference price.
    /// The execution boundary uses this before risk evaluation; risk and the venue adapter then
    /// check notional using their fresh reference price.
    pub fn validate_exact_order_precision(
        &self,
        quantity: ExactDecimal,
        limit_price: Option<ExactDecimal>,
    ) -> QxResult<()> {
        let (minimum_quantity, quantity_step, price_tick, _) = self.exact_rule_values();
        if !quantity.is_positive()
            || quantity
                .checked_cmp(minimum_quantity)
                .is_none_or(|ordering| ordering.is_lt())
            || quantity.is_multiple_of(quantity_step) != Some(true)
        {
            return Err(QxError::BusinessViolation(
                "订单数量不满足最小数量或数量步长".into(),
            ));
        }
        if let Some(price) = limit_price {
            if !price.is_positive() || price.is_multiple_of(price_tick) != Some(true) {
                return Err(QxError::BusinessViolation(
                    "订单价格不满足正数或价格 tick".into(),
                ));
            }
        }
        Ok(())
    }

    /// Validate a quote-currency-sized Spot market buy. This path has no base quantity or
    /// limit price to pass through the ordinary lot/tick validator; its exact budget is the
    /// order notional and must satisfy the frozen quote-currency minimum.
    pub fn validate_exact_quote_budget(&self, budget: ExactDecimal) -> QxResult<()> {
        self.validate()?;
        if self.product != TradingProduct::Spot || !budget.is_positive() {
            return Err(QxError::BusinessViolation(
                "计价币预算只支持正数现货订单".into(),
            ));
        }
        let raw_minimum = ExactDecimal::from_parts(self.min_notional_raw, 9)
            .map_err(|error| QxError::Invariant(error.into()))?;
        let exact_minimum = self
            .exact_rules
            .as_ref()
            .map(|rules| rules.minimum_notional)
            .unwrap_or(ExactDecimal::ZERO);
        let minimum = if exact_minimum
            .checked_cmp(raw_minimum)
            .is_some_and(|ordering| ordering.is_gt())
        {
            exact_minimum
        } else {
            raw_minimum
        };
        if budget
            .checked_cmp(minimum)
            .is_some_and(|ordering| ordering.is_lt())
        {
            return Err(QxError::BusinessViolation(
                "订单名义金额低于交易所最小限制".into(),
            ));
        }
        Ok(())
    }

    /// Convert a remaining quote-currency budget into the largest base quantity that fits
    /// the frozen quantity step at `price`. This is a sizing helper, not a substitute for
    /// validating the original quote budget or the resulting fill.
    pub fn base_quantity_for_quote_budget(
        &self,
        budget: ExactDecimal,
        price: ExactDecimal,
    ) -> QxResult<ExactDecimal> {
        self.validate()?;
        if self.product != TradingProduct::Spot || !budget.is_positive() || !price.is_positive() {
            return Err(QxError::BusinessViolation(
                "计价币预算换算要求正数现货预算和价格".into(),
            ));
        }
        let contract_size = self.contract_size_decimal();
        let quote_per_quantity = price
            .checked_mul_at_scale(
                contract_size,
                ExactDecimal::MAX_SCALE,
                DecimalRounding::AwayFromZero,
            )
            .filter(|value| value.is_positive())
            .ok_or_else(|| QxError::Invariant("计价币预算单位成本计算失败".into()))?;
        let unrounded_quantity = budget
            .checked_div_at_scale(
                quote_per_quantity,
                ExactDecimal::MAX_SCALE,
                DecimalRounding::Floor,
            )
            .ok_or_else(|| QxError::Invariant("计价币预算基础币数量换算失败".into()))?;
        self.floor_exact_quantity_to_step(unrounded_quantity)
    }

    /// Round a non-negative exact quantity down to this instrument's frozen quantity step.
    pub fn floor_exact_quantity_to_step(&self, quantity: ExactDecimal) -> QxResult<ExactDecimal> {
        self.validate()?;
        if quantity.is_negative() {
            return Err(QxError::BusinessViolation(
                "数量步进取整不能接收负数".into(),
            ));
        }
        let quantity_step = self.exact_rule_values().1;
        let step_count = quantity
            .checked_div_at_scale(quantity_step, 0, DecimalRounding::Floor)
            .ok_or_else(|| QxError::Invariant("数量步进计数计算失败".into()))?;
        step_count
            .checked_mul_at_scale(
                quantity_step,
                ExactDecimal::MAX_SCALE,
                DecimalRounding::TowardZero,
            )
            .ok_or_else(|| QxError::Invariant("数量步进取整溢出".into()))
    }

    pub fn quantity_step_exact(&self) -> QxResult<ExactDecimal> {
        self.validate()?;
        Ok(self.exact_rule_values().1)
    }

    /// Quote-currency order value at a declared 18-place arithmetic scale, rounded down.
    pub fn quote_notional_exact(
        &self,
        quantity: ExactDecimal,
        price: ExactDecimal,
    ) -> QxResult<ExactDecimal> {
        self.quote_cash_exact(quantity, price, DecimalRounding::TowardZero)
    }

    /// Quote-currency cash flow at the declared exact scale. The caller chooses a conservative
    /// rounding direction: round a buy debit away from zero and a sell credit toward zero.
    pub fn quote_cash_exact(
        &self,
        quantity: ExactDecimal,
        price: ExactDecimal,
        rounding: DecimalRounding,
    ) -> QxResult<ExactDecimal> {
        if quantity.is_negative() || !price.is_positive() {
            return Err(QxError::BusinessViolation("订单名义金额输入非法".into()));
        }
        let contract_size = self.contract_size_decimal();
        let amount = quantity
            .checked_mul_at_scale(contract_size, ExactDecimal::MAX_SCALE, rounding)
            .ok_or_else(|| QxError::Invariant("订单合约乘数计算溢出".into()))?;
        if self.inverse {
            Ok(amount)
        } else {
            amount
                .checked_mul_at_scale(price, ExactDecimal::MAX_SCALE, rounding)
                .ok_or_else(|| QxError::Invariant("订单报价名义金额计算溢出".into()))
        }
    }

    /// Settlement-currency exposure using exact quantity, price, and contract size.
    /// Positive exposure is rounded up so a limit check cannot understate risk.
    pub fn notional_exact(
        &self,
        quantity: ExactDecimal,
        price: ExactDecimal,
    ) -> QxResult<ExactDecimal> {
        if quantity.is_negative() || !price.is_positive() {
            return Err(QxError::BusinessViolation("名义额输入非法".into()));
        }
        let amount = quantity
            .checked_mul_at_scale(
                self.contract_size_decimal(),
                ExactDecimal::MAX_SCALE,
                DecimalRounding::AwayFromZero,
            )
            .ok_or_else(|| QxError::Invariant("名义额乘数计算溢出".into()))?;
        let result = if self.inverse {
            amount.checked_div_at_scale(
                price,
                ExactDecimal::MAX_SCALE,
                DecimalRounding::AwayFromZero,
            )
        } else {
            amount.checked_mul_at_scale(
                price,
                ExactDecimal::MAX_SCALE,
                DecimalRounding::AwayFromZero,
            )
        };
        result.ok_or_else(|| QxError::Invariant("名义额计算溢出".into()))
    }

    fn contract_size_decimal(&self) -> ExactDecimal {
        self.exact_rules
            .as_ref()
            .map(|rules| rules.contract_size)
            .unwrap_or_else(|| raw_decimal(self.contract_size))
    }

    fn exact_rule_values(&self) -> (ExactDecimal, ExactDecimal, ExactDecimal, ExactDecimal) {
        self.exact_rules.as_ref().map_or_else(
            || {
                (
                    raw_decimal(self.min_qty),
                    raw_decimal(self.qty_step),
                    raw_decimal(self.price_tick),
                    raw_decimal(self.min_notional_raw),
                )
            },
            |rules| {
                (
                    rules.minimum_quantity,
                    rules.quantity_step,
                    rules.price_tick,
                    rules.minimum_notional,
                )
            },
        )
    }

    /// 校验交易所成交回报的精度约束（tick/step 对齐）。
    ///
    /// 与 `validate_order` 的差别是刻意的：`min_qty` 只约束下单数量，一笔合规订单可以被
    /// 拆成多笔小于最小下单量的成交，因此回报侧不套用该下限；落在 tick/step 之外的回报
    /// 意味着事实与冻结产品规格冲突，上层必须按未知结果处理而不是静默记账或截断。
    pub fn validate_fill(&self, qty: i128, price: i128) -> QxResult<()> {
        if qty <= 0 {
            return Err(QxError::BusinessViolation("成交数量必须为正数".into()));
        }
        self.validate_exact_fill(raw_decimal(qty), raw_decimal(price))
    }

    /// Validate exchange execution values at their original decimal precision. Fill quantity
    /// is checked against the lot step but not the order minimum, since one accepted order can
    /// produce multiple smaller partial fills.
    pub fn validate_exact_fill(&self, quantity: ExactDecimal, price: ExactDecimal) -> QxResult<()> {
        let (_, quantity_step, price_tick, _) = self.exact_rule_values();
        if !quantity.is_positive() || quantity.is_multiple_of(quantity_step) != Some(true) {
            return Err(QxError::BusinessViolation(
                "成交数量不是正数或不在数量步长上".into(),
            ));
        }
        if !price.is_positive() || price.is_multiple_of(price_tick) != Some(true) {
            return Err(QxError::BusinessViolation(
                "成交价格不是正数或不在价格 tick 上".into(),
            ));
        }
        Ok(())
    }

    pub fn maintenance_margin(&self, qty: i128, price: i128) -> QxResult<i128> {
        let notional = raw_decimal(self.notional(qty, price)?);
        let rate = ExactDecimal::from_parts(i128::from(self.maintenance_margin_bps), 0)
            .map_err(|error| QxError::Invariant(error.into()))?;
        let basis_points = ExactDecimal::from_parts(10_000, 0)
            .map_err(|error| QxError::Invariant(error.into()))?;
        notional
            .checked_mul_at_scale(rate, 9, DecimalRounding::AwayFromZero)
            .and_then(|value| {
                value.checked_div_at_scale(basis_points, 9, DecimalRounding::AwayFromZero)
            })
            .and_then(|value| value.to_raw(9).ok())
            .ok_or_else(|| QxError::Invariant("维持保证金计算溢出".into()))
    }

    /// 线性合约按价格差计算，反向合约按 1/price 计算；qty 可为负表示空头。
    pub fn unrealized_pnl(&self, qty: i128, entry_price: i128, mark_price: i128) -> QxResult<i128> {
        if entry_price <= 0 || mark_price <= 0 {
            return Err(QxError::BusinessViolation("PnL 价格必须为正".into()));
        }
        let quantity = raw_decimal(qty);
        let contract_size = raw_decimal(self.contract_size);
        let entry = raw_decimal(entry_price);
        let mark = raw_decimal(mark_price);
        let delta = mark
            .checked_sub(entry)
            .ok_or_else(|| QxError::Invariant("价格差计算溢出".into()))?;
        let value = if self.inverse {
            let denominator = entry.checked_mul_at_scale(
                mark,
                ExactDecimal::MAX_SCALE,
                DecimalRounding::TowardZero,
            );
            quantity
                .checked_mul_at_scale(
                    contract_size,
                    ExactDecimal::MAX_SCALE,
                    DecimalRounding::TowardZero,
                )
                .and_then(|contracts| {
                    contracts.checked_mul_at_scale(
                        delta,
                        ExactDecimal::MAX_SCALE,
                        DecimalRounding::Floor,
                    )
                })
                .and_then(|numerator| {
                    numerator.checked_div_at_scale(denominator?, 9, DecimalRounding::Floor)
                })
        } else {
            quantity
                .checked_mul_at_scale(
                    contract_size,
                    ExactDecimal::MAX_SCALE,
                    DecimalRounding::TowardZero,
                )
                .and_then(|contracts| {
                    contracts.checked_mul_at_scale(delta, 9, DecimalRounding::Floor)
                })
        };
        value
            .and_then(|value| value.to_raw(9).ok())
            .ok_or_else(|| QxError::Invariant("未实现 PnL 计算溢出".into()))
    }

    /// Compute derivative PnL in settlement currency without projecting quantity or prices
    /// onto the legacy 1e-9 grid. A signed quantity is positive for a long and negative for a
    /// short. Positive PnL rounds down and losses round toward negative infinity so accounting
    /// never overstates gains or understates losses.
    pub fn unrealized_pnl_exact(
        &self,
        signed_quantity: ExactDecimal,
        entry_price: ExactDecimal,
        mark_price: ExactDecimal,
    ) -> QxResult<ExactDecimal> {
        if signed_quantity.is_zero() || !entry_price.is_positive() || !mark_price.is_positive() {
            return Err(QxError::BusinessViolation("精确衍生品 PnL 输入非法".into()));
        }
        let contract_size = self.contract_size_decimal();
        let price_delta = mark_price
            .checked_sub(entry_price)
            .ok_or_else(|| QxError::Invariant("精确 PnL 价格差溢出".into()))?;
        let result = if self.inverse {
            let numerator = signed_quantity
                .checked_mul_at_scale(
                    contract_size,
                    ExactDecimal::MAX_SCALE,
                    DecimalRounding::HalfEven,
                )
                .and_then(|value| {
                    value.checked_mul_at_scale(
                        price_delta,
                        ExactDecimal::MAX_SCALE,
                        DecimalRounding::Floor,
                    )
                })
                .ok_or_else(|| QxError::Invariant("精确反向合约 PnL 分子溢出".into()))?;
            let denominator = entry_price
                .checked_mul_at_scale(
                    mark_price,
                    ExactDecimal::MAX_SCALE,
                    DecimalRounding::TowardZero,
                )
                .ok_or_else(|| QxError::Invariant("精确反向合约 PnL 分母溢出".into()))?;
            result_at_scale_floor(numerator, denominator)?
        } else {
            signed_quantity
                .checked_mul_at_scale(
                    contract_size,
                    ExactDecimal::MAX_SCALE,
                    DecimalRounding::HalfEven,
                )
                .and_then(|contracts| {
                    contracts.checked_mul_at_scale(
                        price_delta,
                        ExactDecimal::MAX_SCALE,
                        DecimalRounding::Floor,
                    )
                })
                .ok_or_else(|| QxError::Invariant("精确线性合约 PnL 溢出".into()))?
        };
        Ok(result)
    }

    pub fn funding_payment(
        &self,
        qty: i128,
        mark_price: i128,
        funding_rate_bps: i64,
    ) -> QxResult<i128> {
        if mark_price <= 0 {
            return Err(QxError::BusinessViolation("资金费标记价格必须为正".into()));
        }
        let notional = raw_decimal(
            self.notional(
                qty.checked_abs()
                    .ok_or_else(|| QxError::BusinessViolation("资金费数量绝对值溢出".into()))?,
                mark_price,
            )?,
        );
        let rate = ExactDecimal::from_parts(i128::from(funding_rate_bps), 0)
            .map_err(|error| QxError::Invariant(error.into()))?;
        let basis_points = ExactDecimal::from_parts(10_000, 0)
            .map_err(|error| QxError::Invariant(error.into()))?;
        let amount = notional
            .checked_mul_at_scale(rate, 9, DecimalRounding::TowardZero)
            .and_then(|value| {
                value.checked_div_at_scale(basis_points, 9, DecimalRounding::TowardZero)
            })
            .ok_or_else(|| QxError::Invariant("资金费计算溢出".into()))?;
        let amount = if qty < 0 {
            ExactDecimal::ZERO
                .checked_sub(amount)
                .ok_or_else(|| QxError::Invariant("资金费方向计算溢出".into()))?
        } else {
            amount
        };
        amount
            .to_raw(9)
            .map_err(|_| QxError::Invariant("资金费计算溢出".into()))
    }

    /// Scale-aware funding amount before applying the cashflow sign. Positive values mean
    /// the position pays the venue when the rate is positive; negative quantities reverse
    /// that direction. This keeps sub-1e-9 quantities and prices intact for small contracts.
    pub fn funding_payment_exact(
        &self,
        signed_quantity: ExactDecimal,
        mark_price: ExactDecimal,
        funding_rate_bps: i64,
    ) -> QxResult<ExactDecimal> {
        if !self.product.is_derivative() || signed_quantity.is_zero() || !mark_price.is_positive() {
            return Err(QxError::BusinessViolation(
                "精确资金费要求非零衍生品持仓和正标记价".into(),
            ));
        }
        let quantity = ExactDecimal::from_parts(
            signed_quantity
                .coefficient()
                .checked_abs()
                .ok_or_else(|| QxError::BusinessViolation("精确资金费数量绝对值溢出".into()))?,
            signed_quantity.scale(),
        )
        .map_err(|error| QxError::Invariant(error.into()))?;
        let notional = self.notional_exact(quantity, mark_price)?;
        let rate = ExactDecimal::from_parts(i128::from(funding_rate_bps), 0)
            .map_err(|error| QxError::Invariant(error.into()))?;
        let basis_points = ExactDecimal::from_parts(10_000, 0)
            .map_err(|error| QxError::Invariant(error.into()))?;
        let amount = notional
            .checked_mul_at_scale(rate, ExactDecimal::MAX_SCALE, DecimalRounding::TowardZero)
            .and_then(|value| {
                value.checked_div_at_scale(
                    basis_points,
                    ExactDecimal::MAX_SCALE,
                    DecimalRounding::TowardZero,
                )
            })
            .ok_or_else(|| QxError::Invariant("精确资金费计算溢出".into()))?;
        if signed_quantity.is_negative() {
            ExactDecimal::ZERO
                .checked_sub(amount)
                .ok_or_else(|| QxError::Invariant("精确资金费方向计算溢出".into()))
        } else {
            Ok(amount)
        }
    }
}

fn raw_decimal(value: i128) -> ExactDecimal {
    ExactDecimal::from_parts(value, 9).expect("legacy raw values always use supported scale 9")
}

fn result_at_scale_floor(
    numerator: ExactDecimal,
    denominator: ExactDecimal,
) -> QxResult<ExactDecimal> {
    numerator
        .checked_div_at_scale(denominator, ExactDecimal::MAX_SCALE, DecimalRounding::Floor)
        .ok_or_else(|| QxError::Invariant("精确反向合约 PnL 除法溢出".into()))
}

fn exact_one() -> ExactDecimal {
    ExactDecimal::from_parts(1, 0).expect("one is a supported exact decimal")
}

fn raw_zero(value: &i128) -> bool {
    *value == 0
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrderPolicy {
    pub reduce_only: bool,
    pub position_side: PositionSide,
    pub margin_mode: MarginMode,
    pub position_mode: PositionMode,
    pub leverage: u32,
    pub post_only: bool,
}

impl Default for OrderPolicy {
    fn default() -> Self {
        Self {
            reduce_only: false,
            position_side: PositionSide::Net,
            margin_mode: MarginMode::Cash,
            position_mode: PositionMode::OneWay,
            leverage: 1,
            post_only: false,
        }
    }
}

impl OrderPolicy {
    pub fn validate_for(&self, spec: &TradingInstrumentSpec) -> QxResult<()> {
        spec.validate()?;
        if self.leverage == 0 || self.leverage > spec.max_leverage {
            return Err(QxError::BusinessViolation(
                "OrderPolicy 杠杆超出产品限制".into(),
            ));
        }
        if !spec.product.supports_leverage()
            && (self.leverage != 1 || self.margin_mode != MarginMode::Cash)
        {
            return Err(QxError::BusinessViolation(
                "现货 OrderPolicy 只能为 Cash/1x".into(),
            ));
        }
        if self.position_mode == PositionMode::OneWay && self.position_side != PositionSide::Net {
            return Err(QxError::BusinessViolation(
                "OneWay 模式 position_side 必须为 Net".into(),
            ));
        }
        if self.position_mode == PositionMode::Hedge && !spec.product.is_derivative() {
            return Err(QxError::BusinessViolation(
                "Hedge 持仓模式仅支持永续或交割合约".into(),
            ));
        }
        if self.position_mode == PositionMode::Hedge && self.position_side == PositionSide::Net {
            return Err(QxError::BusinessViolation(
                "Hedge 模式 position_side 必须为 Long 或 Short".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct MarginState {
    pub collateral: Money,
    pub unrealized_pnl: Money,
    pub funding: Money,
    pub interest: Money,
    pub initial_margin: Money,
    pub maintenance_margin: Money,
}

impl MarginState {
    /// 权益 = 抵押 + 未实现 − 资金费 − 利息。
    ///
    /// 算式单源在 [`MarginState::valuate`]（P1-6 估值单点），这里只取结果的 `equity` 字段。
    /// `MarginState` 本身不带币种，故传空串。
    pub fn equity(self) -> Option<Money> {
        self.valuate(String::new()).map(|result| result.equity)
    }

    /// 可用保证金 = 权益 − 初始保证金（同样走 [`MarginState::valuate`] 的 `available` 字段）。
    pub fn available(self) -> Option<Money> {
        self.valuate(String::new()).map(|result| result.available)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(product: TradingProduct) -> TradingInstrumentSpec {
        TradingInstrumentSpec {
            instrument: InstrumentId::parse("BTC/USDT:USDT.BYBIT").unwrap(),
            product,
            base_currency: "BTC".into(),
            quote_currency: "USDT".into(),
            settlement_currency: "USDT".into(),
            contract_size: SCALE,
            linear: product != TradingProduct::Spot,
            inverse: false,
            price_tick: 1,
            qty_step: 1,
            min_qty: 1,
            min_notional_raw: 0,
            max_leverage: if product == TradingProduct::Spot {
                1
            } else {
                100
            },
            maintenance_margin_bps: 500,
            valid_from: 1,
            valid_to: None,
            exact_rules: None,
        }
    }

    #[test]
    fn settlement_currency_comes_from_the_spec_or_the_core_default() {
        assert_eq!(
            resolve_settlement_currency(None),
            DEFAULT_SETTLEMENT_CURRENCY
        );
        let mut market = spec(TradingProduct::Spot);
        market.settlement_currency = "CNY".into();
        assert_eq!(resolve_settlement_currency(Some(&market)), "CNY");
    }

    #[test]
    fn linear_contract_margin_pnl_and_funding_are_deterministic() {
        let spec = spec(TradingProduct::Perpetual);
        assert!(spec.validate().is_ok());
        assert_eq!(spec.notional(SCALE, 100 * SCALE).unwrap(), 100 * SCALE);
        assert_eq!(
            spec.initial_margin(SCALE, 100 * SCALE, 10).unwrap(),
            10 * SCALE
        );
        assert_eq!(
            spec.maintenance_margin(SCALE, 100 * SCALE).unwrap(),
            5 * SCALE
        );
        assert_eq!(
            spec.unrealized_pnl(SCALE, 100 * SCALE, 110 * SCALE)
                .unwrap(),
            10 * SCALE
        );
        assert_eq!(
            spec.funding_payment(SCALE, 100 * SCALE, 10).unwrap(),
            100_000_000
        );
    }

    #[test]
    fn exact_funding_payment_preserves_sub_nano_exposure_and_short_direction() {
        let spec = spec(TradingProduct::Perpetual);
        let quantity = ExactDecimal::parse("0.000000000001").unwrap();
        let mark = ExactDecimal::parse("100").unwrap();
        let expected = ExactDecimal::parse("0.000000000001").unwrap();
        assert_eq!(
            spec.funding_payment_exact(quantity, mark, 100).unwrap(),
            expected
        );
        assert_eq!(
            spec.funding_payment_exact(quantity.checked_neg().unwrap(), mark, 100)
                .unwrap(),
            expected.checked_neg().unwrap()
        );
        assert_eq!(
            spec.funding_payment_exact(quantity, mark, -100).unwrap(),
            expected.checked_neg().unwrap()
        );
        assert_eq!(
            spec.funding_payment_exact(quantity.checked_neg().unwrap(), mark, -100)
                .unwrap(),
            expected
        );
        let mut inverse = spec;
        inverse.linear = false;
        inverse.inverse = true;
        let inverse_payment = inverse
            .funding_payment_exact(
                ExactDecimal::parse("10").unwrap(),
                ExactDecimal::parse("10000").unwrap(),
                100,
            )
            .unwrap();
        assert_eq!(inverse_payment, ExactDecimal::parse("0.00001").unwrap());
    }

    #[test]
    fn inverse_contract_uses_settlement_currency_notional_and_pnl() {
        let mut spec = spec(TradingProduct::Perpetual);
        spec.instrument = InstrumentId::parse("BTC/USD:BTC.BYBIT").unwrap();
        spec.base_currency = "BTC".into();
        spec.quote_currency = "USD".into();
        spec.settlement_currency = "BTC".into();
        spec.contract_size = SCALE;
        spec.linear = false;
        spec.inverse = true;
        spec.validate().unwrap();
        let ten_contracts = 10 * SCALE;
        let entry = 10_000 * SCALE;
        let mark = 11_000 * SCALE;
        assert_eq!(spec.notional(ten_contracts, entry).unwrap(), SCALE / 1_000);
        assert_eq!(
            spec.unrealized_pnl(ten_contracts, entry, mark).unwrap(),
            90_909
        );
        assert_eq!(
            spec.initial_margin(ten_contracts, entry, 10).unwrap(),
            100_000
        );
    }

    #[test]
    fn order_policy_rejects_invalid_spot_and_hedge_combinations() {
        let spot = spec(TradingProduct::Spot);
        let mut policy = OrderPolicy::default();
        assert!(policy.validate_for(&spot).is_ok());
        policy.leverage = 2;
        assert!(policy.validate_for(&spot).is_err());

        let derivative = spec(TradingProduct::Perpetual);
        policy = OrderPolicy {
            position_mode: PositionMode::OneWay,
            position_side: PositionSide::Long,
            ..OrderPolicy::default()
        };
        assert!(policy.validate_for(&derivative).is_err());
    }

    #[test]
    fn order_spec_rejects_unaligned_quantity_and_price_without_rounding() {
        let mut instrument = spec(TradingProduct::Perpetual);
        instrument.qty_step = 10;
        instrument.min_qty = 20;
        instrument.price_tick = 5;
        assert!(instrument.validate_order(20, Some(100)).is_ok());
        assert!(instrument.validate_order(19, Some(100)).is_err());
        assert!(instrument.validate_order(25, Some(100)).is_err());
        assert!(instrument.validate_order(20, Some(101)).is_err());
    }

    #[test]
    fn exact_market_rules_validate_sub_nano_orders_and_fills() {
        let mut instrument = spec(TradingProduct::Spot);
        instrument.instrument = InstrumentId::parse("TINY/USDT.BINANCE").unwrap();
        instrument.base_currency = "TINY".into();
        instrument.exact_rules = Some(ExactTradingRules {
            contract_size: ExactDecimal::parse("1").unwrap(),
            price_tick: ExactDecimal::parse("0.0000000001").unwrap(),
            quantity_step: ExactDecimal::parse("0.0000000001").unwrap(),
            minimum_quantity: ExactDecimal::parse("0.0000000001").unwrap(),
            minimum_notional: ExactDecimal::ZERO,
        });
        let encoded = serde_json::to_string(&instrument).unwrap();
        let decoded: TradingInstrumentSpec = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, instrument);

        let quantity = ExactDecimal::parse("0.0000000003").unwrap();
        let price = ExactDecimal::parse("0.0000000004").unwrap();
        assert!(instrument
            .validate_exact_order_with_reference(quantity, Some(price), Some(price))
            .is_ok());
        assert!(instrument.validate_exact_fill(quantity, price).is_ok());
        assert!(instrument
            .validate_exact_order_with_reference(
                ExactDecimal::parse("0.00000000031").unwrap(),
                Some(price),
                Some(price),
            )
            .is_err());
        assert!(instrument
            .validate_exact_fill(quantity, ExactDecimal::parse("0.00000000041").unwrap())
            .is_err());
        assert_eq!(
            instrument
                .notional_exact(quantity, price)
                .unwrap()
                .to_string(),
            "0.000000000000000001"
        );
    }

    #[test]
    fn quote_budget_sizing_floors_to_lot_step_and_never_exceeds_budget() {
        let mut instrument = spec(TradingProduct::Spot);
        instrument.instrument = InstrumentId::parse("MEME/USDT.BINANCE").unwrap();
        instrument.base_currency = "MEME".into();
        instrument.exact_rules = Some(ExactTradingRules {
            contract_size: ExactDecimal::ONE,
            price_tick: ExactDecimal::parse("0.01").unwrap(),
            quantity_step: ExactDecimal::parse("0.003").unwrap(),
            minimum_quantity: ExactDecimal::parse("0.003").unwrap(),
            minimum_notional: ExactDecimal::ZERO,
        });

        let budget = ExactDecimal::parse("10").unwrap();
        let price = ExactDecimal::parse("3").unwrap();
        let quantity = instrument
            .base_quantity_for_quote_budget(budget, price)
            .unwrap();
        let cost = instrument
            .quote_cash_exact(quantity, price, DecimalRounding::AwayFromZero)
            .unwrap();

        assert_eq!(quantity, ExactDecimal::parse("3.333").unwrap());
        assert_eq!(cost, ExactDecimal::parse("9.999").unwrap());
        assert!(cost.checked_cmp(budget).unwrap().is_le());
        assert_eq!(
            instrument
                .floor_exact_quantity_to_step(ExactDecimal::parse("0.5").unwrap())
                .unwrap(),
            ExactDecimal::parse("0.498").unwrap()
        );
    }

    #[test]
    fn low_price_spot_instrument_keeps_small_ticks_and_exact_notional() {
        let mut instrument = spec(TradingProduct::Spot);
        instrument.instrument = InstrumentId::parse("PEPE/USDT.BINANCE").unwrap();
        instrument.base_currency = "PEPE".into();
        instrument.price_tick = 10;
        instrument.qty_step = 1_000;
        instrument.min_qty = 1_000;
        instrument.min_notional_raw = 1_001;
        instrument.validate().unwrap();

        let one_token = SCALE;
        let one_micro_usdt = 1_000;
        assert!(instrument
            .validate_order(one_token, Some(one_micro_usdt))
            .is_err());
        assert!(instrument
            .validate_order(one_token + 1_000_000, Some(one_micro_usdt))
            .is_ok());
        assert!(instrument.validate_order(one_token, None).is_err());
        assert!(instrument
            .validate_order_with_reference(one_token + 1_000_000, None, Some(1_001))
            .is_ok());
        assert_eq!(
            instrument.notional(one_token, one_micro_usdt).unwrap(),
            1_000
        );
        assert!(instrument.validate_order(one_token, Some(1_001)).is_err());
    }

    #[test]
    fn risk_notional_rounds_up_while_minimum_quote_check_rounds_down() {
        let instrument = spec(TradingProduct::Perpetual);
        // qty and price are each one legacy raw unit, so their exact product is below
        // the 1e-9 reporting grid. Exposure must not be understated; minimum-cost
        // validation must not accept an order by rounding its cost upward.
        assert_eq!(instrument.notional(1, 1).unwrap(), 1);
        assert_eq!(instrument.quote_notional(1, 1).unwrap(), 0);
        assert_eq!(instrument.initial_margin(1, 1, 10).unwrap(), 1);
        assert_eq!(instrument.maintenance_margin(1, 1).unwrap(), 1);
        assert_eq!(
            instrument
                .initial_margin_exact(
                    ExactDecimal::parse("0.000000001").unwrap(),
                    ExactDecimal::parse("0.000000001").unwrap(),
                    10,
                )
                .unwrap(),
            ExactDecimal::parse("0.000000000000000001").unwrap()
        );
    }

    #[test]
    fn tiny_unrealized_losses_round_away_from_equity() {
        let instrument = spec(TradingProduct::Perpetual);
        // The exact loss is 1e-18, below the 1e-9 ledger reporting unit. Floor rounding
        // keeps the reported equity from overstating the account by dropping that loss.
        assert_eq!(instrument.unrealized_pnl(-1, 1, 2).unwrap(), -1);
        assert_eq!(instrument.unrealized_pnl(1, 1, 2).unwrap(), 0);
    }

    #[test]
    fn exact_derivative_pnl_uses_decimal_contract_size_for_linear_and_inverse_products() {
        let mut linear = spec(TradingProduct::Perpetual);
        linear.exact_rules = Some(ExactTradingRules {
            contract_size: ExactDecimal::parse("0.001").unwrap(),
            price_tick: ExactDecimal::parse("0.01").unwrap(),
            quantity_step: ExactDecimal::parse("0.1").unwrap(),
            minimum_quantity: ExactDecimal::parse("0.1").unwrap(),
            minimum_notional: ExactDecimal::ZERO,
        });
        assert_eq!(
            linear
                .unrealized_pnl_exact(
                    ExactDecimal::parse("0.5").unwrap(),
                    ExactDecimal::parse("10000").unwrap(),
                    ExactDecimal::parse("11000").unwrap(),
                )
                .unwrap(),
            ExactDecimal::parse("0.5").unwrap()
        );
        assert_eq!(
            linear
                .unrealized_pnl_exact(
                    ExactDecimal::parse("-0.5").unwrap(),
                    ExactDecimal::parse("10000").unwrap(),
                    ExactDecimal::parse("11000").unwrap(),
                )
                .unwrap(),
            ExactDecimal::parse("-0.5").unwrap()
        );

        let mut inverse = spec(TradingProduct::Perpetual);
        inverse.settlement_currency = "BTC".into();
        inverse.inverse = true;
        inverse.linear = false;
        inverse.exact_rules = Some(ExactTradingRules {
            contract_size: ExactDecimal::parse("100").unwrap(),
            price_tick: ExactDecimal::parse("0.01").unwrap(),
            quantity_step: ExactDecimal::parse("1").unwrap(),
            minimum_quantity: ExactDecimal::parse("1").unwrap(),
            minimum_notional: ExactDecimal::ZERO,
        });
        assert_eq!(
            inverse
                .unrealized_pnl_exact(
                    ExactDecimal::parse("2").unwrap(),
                    ExactDecimal::parse("10000").unwrap(),
                    ExactDecimal::parse("11000").unwrap(),
                )
                .unwrap(),
            ExactDecimal::parse("0.001818181818181818").unwrap()
        );
    }

    #[test]
    fn exact_intermediates_avoid_legacy_product_overflow_when_result_fits() {
        let instrument = spec(TradingProduct::Perpetual);
        assert_eq!(instrument.notional(i128::MAX, SCALE).unwrap(), i128::MAX);
        assert_eq!(
            instrument.quote_notional(i128::MAX, SCALE).unwrap(),
            i128::MAX
        );
    }

    #[test]
    fn margin_state_exposes_available_and_liquidation_boundary() {
        let healthy = MarginState {
            collateral: Money::from_i64(100),
            initial_margin: Money::from_i64(20),
            maintenance_margin: Money::from_i64(30),
            ..MarginState::default()
        };
        assert_eq!(
            healthy.available().unwrap().raw(),
            Money::from_i64(80).raw()
        );
        assert!(healthy.equity().unwrap().raw() > healthy.maintenance_margin.raw());
        let mut bad = healthy;
        bad.unrealized_pnl = Money::from_i64(-70);
        assert!(bad.equity().unwrap().raw() <= bad.maintenance_margin.raw());
    }
}
