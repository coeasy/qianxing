//! 费用模型契约。
//!
//! 放在内核而不是回测层：[`Fill`](crate::Fill) 的 `fee` 是内核事实字段，
//! 回测撮合、盘口逐档撮合与 Paper 模拟必须共用同一套费用语义，否则同一条
//! 成交会在不同执行平面上得出不同的成本。
//!
//! 约定：`qty`、`price` 一律是定点原始值（[`SCALE`](crate::SCALE) 标度），
//! 返回值也是定点原始值。合约乘数不在此处折算，而由调用方折进 `price`
//! （见 `qx_xingban::BarMatchingEngine` 的费用基准折算），使线性与
//! 反向合约都能用同一签名：线性折算为 `price × contract_size`，反向折算为
//! `contract_size / price`。

use crate::{DecimalRounding, ExactDecimal, QxError, QxResult, Side};
use serde::{Deserialize, Serialize};

/// 名义额 = qty * price / SCALE（两者均为定点原始值）。
pub fn notional(qty: i128, price: i128) -> QxResult<i128> {
    if qty < 0 || price < 0 {
        return Err(QxError::BusinessViolation(
            "名义额数量和价格不能为负数".into(),
        ));
    }
    // A raw projection may be zero even when the exact source value is positive.
    // Callers with exact amounts use the exact path; the legacy projection is 0 here.
    if qty == 0 || price == 0 {
        return Ok(0);
    }
    let quantity = ExactDecimal::from_parts(qty, 9)
        .map_err(|error| QxError::BusinessViolation(error.into()))?;
    let price = ExactDecimal::from_parts(price, 9)
        .map_err(|error| QxError::BusinessViolation(error.into()))?;
    quantity
        .checked_mul_at_scale(price, 9, DecimalRounding::TowardZero)
        .and_then(|value| value.to_raw(9).ok())
        .ok_or_else(|| QxError::Invariant("名义额计算溢出".into()))
}

/// 按基点取金额（1 bp = 万分之一）。
pub fn bp_amount(x: i128, bp: i64) -> QxResult<i128> {
    bp_amount_checked(x, i128::from(bp))
}

/// Compute a signed basis-point amount without overflowing the intermediate product.
/// The supported rate range is -10000..=10000 bps; quotient/remainder decomposition
/// preserves truncation-toward-zero semantics for both positive and negative rates.
pub fn bp_amount_checked(amount: i128, bps: i128) -> Result<i128, QxError> {
    if !(-10_000..=10_000).contains(&bps) {
        return Err(QxError::BusinessViolation(
            "基点费率必须在 -10000..=10000 bps".into(),
        ));
    }
    let quotient = amount / 10_000;
    let remainder = amount % 10_000;
    quotient
        .checked_mul(bps)
        .and_then(|whole| {
            remainder
                .checked_mul(bps)
                .and_then(|partial| whole.checked_add(partial / 10_000))
        })
        .ok_or_else(|| QxError::Invariant("基点金额计算溢出".into()))
}

pub trait FeeModel {
    fn name(&self) -> &'static str;
    /// Validate model parameters before an order or backtest can use it.
    /// Custom models remain source-compatible through the permissive default.
    fn validate(&self) -> crate::QxResult<()> {
        Ok(())
    }
    fn version(&self) -> &'static str {
        "v1"
    }
    /// 返回费用（定点原始值）。`is_maker` 区分挂单/吃单。
    fn commission(&self, qty: i128, price: i128, is_maker: bool) -> QxResult<i128>;

    fn commission_for_side(
        &self,
        qty: i128,
        price: i128,
        side: Side,
        is_maker: bool,
    ) -> QxResult<i128> {
        let _ = side;
        let fee = self.commission(qty, price, is_maker)?;
        if fee < 0 {
            return Err(QxError::BusinessViolation(
                "费用模型不能返回负手续费".into(),
            ));
        }
        Ok(fee)
    }

    /// Compute a fee without projecting quantity or price onto the legacy 1e-9 grid.
    /// Custom models inherit a conservative compatibility implementation: it succeeds only
    /// when both inputs are exactly representable at scale 9. Models that support finer
    /// arithmetic should override this method.
    fn commission_exact(
        &self,
        qty: ExactDecimal,
        price: ExactDecimal,
        side: Side,
        is_maker: bool,
    ) -> QxResult<ExactDecimal> {
        let qty = qty.to_raw(9).map_err(|error| {
            QxError::BusinessViolation(format!("费用数量无法精确兼容: {error}"))
        })?;
        let price = price.to_raw(9).map_err(|error| {
            QxError::BusinessViolation(format!("费用价格无法精确兼容: {error}"))
        })?;
        let fee = self.commission_for_side(qty, price, side, is_maker)?;
        if fee < 0 {
            return Err(QxError::BusinessViolation(
                "费用模型不能返回负手续费".into(),
            ));
        }
        ExactDecimal::from_parts(fee, 9).map_err(|error| QxError::Invariant(error.into()))
    }

    fn parameters(&self) -> String {
        String::new()
    }

    fn descriptor(&self) -> String {
        format!(
            "{}@{}[params={}]",
            self.name(),
            self.version(),
            self.parameters()
        )
    }
}

fn exact_notional(qty: ExactDecimal, price: ExactDecimal) -> QxResult<ExactDecimal> {
    if qty.is_negative() || !price.is_positive() {
        return Err(QxError::BusinessViolation("精确费用名义额输入非法".into()));
    }
    qty.checked_mul_at_scale(price, ExactDecimal::MAX_SCALE, DecimalRounding::TowardZero)
        .ok_or_else(|| QxError::Invariant("精确费用名义额计算溢出".into()))
}

fn validate_raw_fee_inputs(qty: i128, price: i128) -> QxResult<()> {
    if qty < 0 || price <= 0 {
        return Err(QxError::BusinessViolation(
            "手续费数量不能为负数且价格必须为正".into(),
        ));
    }
    Ok(())
}

fn exact_bp_amount(amount: ExactDecimal, bp: i64) -> QxResult<ExactDecimal> {
    let bps = ExactDecimal::from_parts(i128::from(bp), 0)
        .map_err(|error| QxError::Invariant(error.into()))?;
    let denominator =
        ExactDecimal::from_parts(10_000, 0).map_err(|error| QxError::Invariant(error.into()))?;
    amount
        .checked_mul_at_scale(bps, ExactDecimal::MAX_SCALE, DecimalRounding::TowardZero)
        .and_then(|value| {
            value.checked_div_at_scale(
                denominator,
                ExactDecimal::MAX_SCALE,
                DecimalRounding::TowardZero,
            )
        })
        .ok_or_else(|| QxError::Invariant("精确手续费基点计算溢出".into()))
}

pub struct ZeroFeeModel;

impl FeeModel for ZeroFeeModel {
    fn name(&self) -> &'static str {
        "ZeroFee"
    }
    fn commission(&self, _q: i128, _p: i128, _maker: bool) -> QxResult<i128> {
        Ok(0)
    }
    fn commission_exact(
        &self,
        _qty: ExactDecimal,
        _price: ExactDecimal,
        _side: Side,
        _is_maker: bool,
    ) -> QxResult<ExactDecimal> {
        Ok(ExactDecimal::ZERO)
    }
}

/// maker/taker 费率模型（加密现货与合约常见）。
pub struct MakerTakerFeeModel {
    pub maker_bp: i64,
    pub taker_bp: i64,
}

/// 加密现货的默认费率（基点）。
///
/// 定义点唯一在内核：Bar 回测装配、Paper 模拟与 `ExecutionCostRules` 的缺省值
/// 都从这里取，否则"同一条成交在两个执行平面上费用不同"只会靠人工审计发现。
pub const DEFAULT_MAKER_BP: i64 = 2;
pub const DEFAULT_TAKER_BP: i64 = 5;

/// Serializable maker/taker rates used to build the shared execution fee model.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MakerTakerFeeSchedule {
    pub maker_fee_bps: i64,
    pub taker_fee_bps: i64,
}

impl Default for MakerTakerFeeSchedule {
    fn default() -> Self {
        Self {
            maker_fee_bps: DEFAULT_MAKER_BP,
            taker_fee_bps: DEFAULT_TAKER_BP,
        }
    }
}

impl MakerTakerFeeSchedule {
    pub fn validate(&self) -> crate::QxResult<()> {
        if !(0..=10_000).contains(&self.maker_fee_bps)
            || !(0..=10_000).contains(&self.taker_fee_bps)
        {
            return Err(crate::QxError::BusinessViolation(
                "MakerTakerFeeSchedule 费率必须在 0..=10000 bps".into(),
            ));
        }
        Ok(())
    }

    pub fn model(self) -> MakerTakerFeeModel {
        MakerTakerFeeModel {
            maker_bp: self.maker_fee_bps,
            taker_bp: self.taker_fee_bps,
        }
    }
}

impl MakerTakerFeeModel {
    /// 未给成本配置时的默认费率模型。
    pub const fn default_maker_taker() -> Self {
        Self {
            maker_bp: DEFAULT_MAKER_BP,
            taker_bp: DEFAULT_TAKER_BP,
        }
    }

    pub fn validate(&self) -> crate::QxResult<()> {
        MakerTakerFeeSchedule {
            maker_fee_bps: self.maker_bp,
            taker_fee_bps: self.taker_bp,
        }
        .validate()
    }
}

impl FeeModel for MakerTakerFeeModel {
    fn name(&self) -> &'static str {
        "MakerTaker"
    }
    fn validate(&self) -> crate::QxResult<()> {
        MakerTakerFeeModel::validate(self)
    }
    fn commission(&self, qty: i128, price: i128, is_maker: bool) -> QxResult<i128> {
        self.validate()?;
        validate_raw_fee_inputs(qty, price)?;
        let n = notional(qty, price)?;
        if is_maker {
            bp_amount(n, self.maker_bp)
        } else {
            bp_amount(n, self.taker_bp)
        }
    }
    fn commission_exact(
        &self,
        qty: ExactDecimal,
        price: ExactDecimal,
        _side: Side,
        is_maker: bool,
    ) -> QxResult<ExactDecimal> {
        self.validate()?;
        exact_bp_amount(
            exact_notional(qty, price)?,
            if is_maker {
                self.maker_bp
            } else {
                self.taker_bp
            },
        )
    }
    fn parameters(&self) -> String {
        format!("maker_bp={};taker_bp={}", self.maker_bp, self.taker_bp)
    }
}

/// A 股费用：佣金（含最低佣金）+ 卖出印花税 + 过户费。
///
/// 常见陷阱：把 T+1、涨跌停、除复权时点弄错；这里只负责"钱"，规则在规则包。
pub struct AShareFeeModel {
    pub commission_bp: i64,
    pub min_commission: i128,
    /// 仅卖出征收。
    pub stamp_duty_bp: i64,
    /// 双边征收。
    pub transfer_fee_bp: i64,
}

impl FeeModel for AShareFeeModel {
    fn name(&self) -> &'static str {
        "AShareFee"
    }
    fn validate(&self) -> crate::QxResult<()> {
        let total_bps = self
            .commission_bp
            .checked_add(self.stamp_duty_bp)
            .and_then(|value| value.checked_add(self.transfer_fee_bp));
        if self.min_commission < 0
            || !(0..=10_000).contains(&self.commission_bp)
            || !(0..=10_000).contains(&self.stamp_duty_bp)
            || !(0..=10_000).contains(&self.transfer_fee_bp)
            || !total_bps.is_some_and(|value| value <= 10_000)
        {
            return Err(crate::QxError::BusinessViolation(
                "AShareFeeModel 要求各费率非负、合计不超过 10000 bps 且最低佣金非负".into(),
            ));
        }
        Ok(())
    }
    fn commission(&self, qty: i128, price: i128, is_maker: bool) -> QxResult<i128> {
        self.commission_for_side(qty, price, Side::Buy, is_maker)
    }
    fn commission_for_side(
        &self,
        qty: i128,
        price: i128,
        side: Side,
        _is_maker: bool,
    ) -> QxResult<i128> {
        self.validate()?;
        validate_raw_fee_inputs(qty, price)?;
        let n = notional(qty, price)?;
        let mut fee = bp_amount(n, self.commission_bp)?
            .max(self.min_commission)
            .checked_add(bp_amount(n, self.transfer_fee_bp)?)
            .ok_or_else(|| QxError::Invariant("A 股佣金与过户费累计溢出".into()))?;
        if side == Side::Sell {
            fee = fee
                .checked_add(bp_amount(n, self.stamp_duty_bp)?)
                .ok_or_else(|| QxError::Invariant("A 股印花税累计溢出".into()))?;
        }
        Ok(fee)
    }
    fn commission_exact(
        &self,
        qty: ExactDecimal,
        price: ExactDecimal,
        side: Side,
        _is_maker: bool,
    ) -> QxResult<ExactDecimal> {
        self.validate()?;
        let notional = exact_notional(qty, price)?;
        let minimum = ExactDecimal::from_parts(self.min_commission, 9)
            .map_err(|error| QxError::Invariant(error.into()))?;
        let commission = exact_bp_amount(notional, self.commission_bp)?;
        let transfer = exact_bp_amount(notional, self.transfer_fee_bp)?;
        let commission = match commission.checked_cmp(minimum) {
            Some(std::cmp::Ordering::Less) => minimum,
            Some(_) => commission,
            None => return Err(QxError::Invariant("精确 A 股最低佣金比较失败".into())),
        };
        let mut fee = commission
            .checked_add(transfer)
            .ok_or_else(|| QxError::Invariant("精确 A 股费用累计溢出".into()))?;
        if side == Side::Sell {
            fee = fee
                .checked_add(exact_bp_amount(notional, self.stamp_duty_bp)?)
                .ok_or_else(|| QxError::Invariant("精确 A 股印花税累计溢出".into()))?;
        }
        Ok(fee)
    }
    fn parameters(&self) -> String {
        format!(
            "commission_bp={};min_commission={};stamp_duty_bp={};transfer_fee_bp={}",
            self.commission_bp, self.min_commission, self.stamp_duty_bp, self.transfer_fee_bp
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SCALE;

    #[test]
    fn maker_cheaper_than_taker() {
        let m = MakerTakerFeeModel {
            maker_bp: 2,
            taker_bp: 5,
        };
        let qty = 1_000_000_000i128; // 1.0
        let px = 100_000_000_000i128; // 100.0
        assert!(m.commission(qty, px, true).unwrap() < m.commission(qty, px, false).unwrap());
    }

    #[test]
    fn maker_taker_model_rejects_negative_or_out_of_range_rates() {
        assert!(MakerTakerFeeModel {
            maker_bp: -1,
            taker_bp: 5,
        }
        .validate()
        .is_err());
        assert!(MakerTakerFeeModel {
            maker_bp: 2,
            taker_bp: 10_001,
        }
        .validate()
        .is_err());
        MakerTakerFeeModel {
            maker_bp: 0,
            taker_bp: 10_000,
        }
        .validate()
        .unwrap();
    }

    #[test]
    fn ashare_model_rejects_negative_and_unbounded_costs() {
        let base = AShareFeeModel {
            commission_bp: 3,
            min_commission: 0,
            stamp_duty_bp: 5,
            transfer_fee_bp: 1,
        };
        base.validate().unwrap();
        assert!(AShareFeeModel {
            commission_bp: -1,
            min_commission: 0,
            stamp_duty_bp: 5,
            transfer_fee_bp: 1,
        }
        .validate()
        .is_err());
        assert!(AShareFeeModel {
            commission_bp: 8_000,
            min_commission: 0,
            stamp_duty_bp: 2_001,
            transfer_fee_bp: 0,
        }
        .validate()
        .is_err());
        assert!(AShareFeeModel {
            min_commission: -1,
            ..base
        }
        .validate()
        .is_err());
    }

    #[test]
    fn checked_basis_point_amount_preserves_floor_without_product_overflow() {
        let amount = i128::MAX;
        assert_eq!(bp_amount_checked(amount, 10_000).unwrap(), amount);
        assert_eq!(bp_amount_checked(amount, 5_000).unwrap(), amount / 2);
        assert_eq!(bp_amount(amount, 5_000).unwrap(), amount / 2);
        assert_eq!(bp_amount_checked(19_999, 1).unwrap(), 1);
        assert_eq!(bp_amount_checked(-20_000, 5_000).unwrap(), -10_000);
        assert_eq!(bp_amount_checked(20_000, -5_000).unwrap(), -10_000);
        assert!(bp_amount_checked(1, 10_001).is_err());
        assert!(bp_amount_checked(1, -10_001).is_err());
    }

    #[test]
    fn raw_notional_is_checked_without_saturating_intermediate_products() {
        assert_eq!(notional(i128::MAX, SCALE).unwrap(), i128::MAX);
        assert!(notional(-1, SCALE).is_err());
        assert!(notional(SCALE, -1).is_err());
        assert_eq!(notional(SCALE, 0).unwrap(), 0);
        assert!(notional(i128::MAX, 2 * SCALE).is_err());
    }

    #[test]
    fn fee_models_fail_closed_for_invalid_rates_and_arithmetic() {
        let invalid = MakerTakerFeeModel {
            maker_bp: -1,
            taker_bp: 5,
        };
        assert!(invalid.commission(SCALE, 100 * SCALE, true).is_err());
        let valid = MakerTakerFeeModel {
            maker_bp: 2,
            taker_bp: 5,
        };
        assert!(valid.commission(i128::MAX, 2 * SCALE, true).is_err());
        assert!(valid
            .commission_for_side(SCALE, -1, Side::Buy, false)
            .is_err());
        assert!(valid
            .commission_for_side(SCALE, 0, Side::Buy, false)
            .is_err());

        struct NegativeCustomFee;
        impl FeeModel for NegativeCustomFee {
            fn name(&self) -> &'static str {
                "NegativeCustomFee"
            }
            fn commission(&self, _qty: i128, _price: i128, _is_maker: bool) -> QxResult<i128> {
                Ok(-1)
            }
        }
        assert!(NegativeCustomFee
            .commission_for_side(SCALE, SCALE, Side::Buy, false)
            .is_err());
    }

    #[test]
    fn min_commission_floor() {
        let m = AShareFeeModel {
            commission_bp: 1,
            min_commission: 5_000_000_000, // 5 元
            stamp_duty_bp: 0,
            transfer_fee_bp: 0,
        };
        assert_eq!(m.commission(1, 1, true).unwrap(), 5_000_000_000);
    }

    #[test]
    fn ashare_stamp_duty_is_sell_side_only() {
        let m = AShareFeeModel {
            commission_bp: 0,
            min_commission: 0,
            stamp_duty_bp: 5,
            transfer_fee_bp: 0,
        };
        let buy = m
            .commission_for_side(1_000_000_000, 100_000_000_000, crate::Side::Buy, false)
            .unwrap();
        let sell = m
            .commission_for_side(1_000_000_000, 100_000_000_000, crate::Side::Sell, false)
            .unwrap();
        assert_eq!(buy, 0);
        assert_eq!(sell, 50_000_000);
    }

    /// 费用模型的 descriptor 会进入 RunManifest 指纹，参数表达必须稳定。
    #[test]
    fn descriptors_are_stable() {
        let m = MakerTakerFeeModel {
            maker_bp: 2,
            taker_bp: 5,
        };
        assert_eq!(
            m.descriptor(),
            "MakerTaker@v1[params=maker_bp=2;taker_bp=5]"
        );
        let a = AShareFeeModel {
            commission_bp: 3,
            min_commission: 5 * SCALE,
            stamp_duty_bp: 5,
            transfer_fee_bp: 1,
        };
        assert_eq!(
            a.descriptor(),
            "AShareFee@v1[params=commission_bp=3;min_commission=5000000000;stamp_duty_bp=5;transfer_fee_bp=1]"
        );
    }

    #[test]
    fn serializable_fee_schedule_validates_and_builds_the_shared_model() {
        let schedule = MakerTakerFeeSchedule {
            maker_fee_bps: 13,
            taker_fee_bps: 41,
        };
        schedule.validate().unwrap();
        assert_eq!(
            schedule.model().descriptor(),
            "MakerTaker@v1[params=maker_bp=13;taker_bp=41]"
        );
        assert!(MakerTakerFeeSchedule {
            maker_fee_bps: 0,
            taker_fee_bps: 10_001,
        }
        .validate()
        .is_err());
    }
}
