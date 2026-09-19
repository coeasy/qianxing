//! 统一交易产品、杠杆和保证金语义。
//!
//! 这里不依赖交易所；现货、保证金、永续和交割合约都通过同一组确定性
//! 定点运算表达。CCXT 只负责把交易所 market/position/funding 映射到这些
//! 契约，回测和实盘不得各自重新实现一套保证金规则。

use crate::{InstrumentId, Money, QxError, QxResult, SCALE};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TradingProduct {
    Spot,
    Margin,
    Perpetual,
    Future,
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
    pub max_leverage: u32,
    pub maintenance_margin_bps: u32,
    pub valid_from: u64,
    pub valid_to: Option<u64>,
}

impl TradingInstrumentSpec {
    pub fn validate(&self) -> QxResult<()> {
        if self.base_currency.trim().is_empty()
            || self.quote_currency.trim().is_empty()
            || self.settlement_currency.trim().is_empty()
            || self.contract_size <= 0
            || self.price_tick <= 0
            || self.qty_step <= 0
            || self.min_qty <= 0
            || self.max_leverage == 0
            || self.maintenance_margin_bps > 10_000
            || self.valid_to.is_some_and(|to| to <= self.valid_from)
            || (self.linear == self.inverse && self.product.is_derivative())
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
        if qty < 0 || price <= 0 {
            return Err(QxError::BusinessViolation("名义额输入非法".into()));
        }
        let value = if self.inverse {
            // 反向合约的 contract_size 通常以报价币计价，名义保证金/结算
            // 价值换算到 settlement currency 需要除以价格。
            qty.checked_mul(self.contract_size)
                .and_then(|v| v.checked_div(price))
        } else {
            qty.checked_mul(self.contract_size)
                .and_then(|v| v.checked_div(SCALE))
                .and_then(|v| v.checked_mul(price))
                .and_then(|v| v.checked_div(SCALE))
        };
        value.ok_or_else(|| QxError::Invariant("名义额计算溢出".into()))
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
        self.notional(qty, price)?
            .checked_div(i128::from(leverage))
            .ok_or_else(|| QxError::Invariant("初始保证金计算溢出".into()))
    }

    /// 在进入 OMS/交易所适配器前校验数量和价格的交易所规格约束。
    ///
    /// 这里不做静默截断或四舍五入；策略目标若无法精确落在 lot/tick 上，
    /// 必须显式调整目标或由上层按业务规则拆单，避免回测与实盘产生不同语义。
    pub fn validate_order(&self, qty: i128, price: Option<i128>) -> QxResult<()> {
        if qty <= 0 || qty < self.min_qty || qty % self.qty_step != 0 {
            return Err(QxError::BusinessViolation(
                "订单数量不满足最小数量或数量步长".into(),
            ));
        }
        if let Some(price) = price {
            if price <= 0 || price % self.price_tick != 0 {
                return Err(QxError::BusinessViolation(
                    "订单价格不满足正数或价格 tick".into(),
                ));
            }
        }
        Ok(())
    }

    /// 校验交易所成交回报的精度约束（tick/step 对齐）。
    ///
    /// 与 `validate_order` 的差别是刻意的：`min_qty` 只约束下单数量，一笔合规订单可以被
    /// 拆成多笔小于最小下单量的成交，因此回报侧不套用该下限；落在 tick/step 之外的回报
    /// 意味着事实与冻结产品规格冲突，上层必须按未知结果处理而不是静默记账或截断。
    pub fn validate_fill(&self, qty: i128, price: i128) -> QxResult<()> {
        if qty <= 0 || qty % self.qty_step != 0 {
            return Err(QxError::BusinessViolation(
                "成交数量不是正数或不在数量步长上".into(),
            ));
        }
        if price <= 0 || price % self.price_tick != 0 {
            return Err(QxError::BusinessViolation(
                "成交价格不是正数或不在价格 tick 上".into(),
            ));
        }
        Ok(())
    }

    pub fn maintenance_margin(&self, qty: i128, price: i128) -> QxResult<i128> {
        self.notional(qty, price)?
            .checked_mul(i128::from(self.maintenance_margin_bps))
            .and_then(|v| v.checked_div(10_000))
            .ok_or_else(|| QxError::Invariant("维持保证金计算溢出".into()))
    }

    /// 线性合约按价格差计算，反向合约按 1/price 计算；qty 可为负表示空头。
    pub fn unrealized_pnl(&self, qty: i128, entry_price: i128, mark_price: i128) -> QxResult<i128> {
        if entry_price <= 0 || mark_price <= 0 {
            return Err(QxError::BusinessViolation("PnL 价格必须为正".into()));
        }
        let value = if self.inverse {
            qty.checked_mul(self.contract_size)
                .and_then(|v| v.checked_mul(mark_price.checked_sub(entry_price)?))
                .and_then(|v| v.checked_div(entry_price))
                .and_then(|v| v.checked_div(mark_price))
        } else {
            qty.checked_mul(self.contract_size)
                .and_then(|v| v.checked_div(SCALE))
                .and_then(|v| v.checked_mul(mark_price.checked_sub(entry_price)?))
                .and_then(|v| v.checked_div(SCALE))
        };
        value.ok_or_else(|| QxError::Invariant("未实现 PnL 计算溢出".into()))
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
        let notional = self.notional(
            qty.checked_abs()
                .ok_or_else(|| QxError::BusinessViolation("资金费数量绝对值溢出".into()))?,
            mark_price,
        )?;
        notional
            .checked_mul(i128::from(funding_rate_bps))
            .and_then(|v| v.checked_div(10_000))
            .map(|amount| if qty >= 0 { amount } else { -amount })
            .ok_or_else(|| QxError::Invariant("资金费计算溢出".into()))
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
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
    pub fn equity(self) -> Option<Money> {
        self.collateral
            .raw()
            .checked_add(self.unrealized_pnl.raw())
            .and_then(|v| v.checked_sub(self.funding.raw()))
            .and_then(|v| v.checked_sub(self.interest.raw()))
            .map(Money::from_raw)
    }

    pub fn available(self) -> Option<Money> {
        self.equity()?
            .raw()
            .checked_sub(self.initial_margin.raw())
            .map(Money::from_raw)
    }

    pub fn liquidatable(self) -> bool {
        self.equity()
            .map(|equity| equity.raw() <= self.maintenance_margin.raw())
            .unwrap_or(true)
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
            max_leverage: if product == TradingProduct::Spot {
                1
            } else {
                100
            },
            maintenance_margin_bps: 500,
            valid_from: 1,
            valid_to: None,
        }
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
        assert!(!healthy.liquidatable());
        let mut bad = healthy;
        bad.unrealized_pnl = Money::from_i64(-70);
        assert!(bad.liquidatable());
    }
}
