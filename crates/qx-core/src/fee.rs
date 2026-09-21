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

use crate::{Side, SCALE};

/// 名义额 = qty * price / SCALE（两者均为定点原始值）。
pub fn notional(qty: i128, price: i128) -> i128 {
    qty.saturating_mul(price) / SCALE
}

/// 按基点取金额（1 bp = 万分之一）。
pub fn bp_amount(x: i128, bp: i64) -> i128 {
    x.saturating_mul(bp as i128) / 10_000
}

pub trait FeeModel {
    fn name(&self) -> &'static str;
    fn version(&self) -> &'static str {
        "v1"
    }
    /// 返回费用（定点原始值）。`is_maker` 区分挂单/吃单。
    fn commission(&self, qty: i128, price: i128, is_maker: bool) -> i128;

    fn commission_for_side(&self, qty: i128, price: i128, side: Side, is_maker: bool) -> i128 {
        let _ = side;
        self.commission(qty, price, is_maker)
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

pub struct ZeroFeeModel;

impl FeeModel for ZeroFeeModel {
    fn name(&self) -> &'static str {
        "ZeroFee"
    }
    fn commission(&self, _q: i128, _p: i128, _maker: bool) -> i128 {
        0
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

impl MakerTakerFeeModel {
    /// 未给成本配置时的默认费率模型。
    pub const fn default_maker_taker() -> Self {
        Self {
            maker_bp: DEFAULT_MAKER_BP,
            taker_bp: DEFAULT_TAKER_BP,
        }
    }
}

impl FeeModel for MakerTakerFeeModel {
    fn name(&self) -> &'static str {
        "MakerTaker"
    }
    fn commission(&self, qty: i128, price: i128, is_maker: bool) -> i128 {
        let n = notional(qty, price);
        if is_maker {
            bp_amount(n, self.maker_bp)
        } else {
            bp_amount(n, self.taker_bp)
        }
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
    fn commission(&self, qty: i128, price: i128, is_maker: bool) -> i128 {
        self.commission_for_side(qty, price, Side::Buy, is_maker)
    }
    fn commission_for_side(&self, qty: i128, price: i128, side: Side, _is_maker: bool) -> i128 {
        let n = notional(qty, price);
        let mut fee = bp_amount(n, self.commission_bp).max(self.min_commission);
        fee += bp_amount(n, self.transfer_fee_bp);
        if side == Side::Sell {
            fee += bp_amount(n, self.stamp_duty_bp);
        }
        fee
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

    #[test]
    fn maker_cheaper_than_taker() {
        let m = MakerTakerFeeModel {
            maker_bp: 2,
            taker_bp: 5,
        };
        let qty = 1_000_000_000i128; // 1.0
        let px = 100_000_000_000i128; // 100.0
        assert!(m.commission(qty, px, true) < m.commission(qty, px, false));
    }

    #[test]
    fn min_commission_floor() {
        let m = AShareFeeModel {
            commission_bp: 1,
            min_commission: 5_000_000_000, // 5 元
            stamp_duty_bp: 0,
            transfer_fee_bp: 0,
        };
        assert_eq!(m.commission(1, 1, true), 5_000_000_000);
    }

    #[test]
    fn ashare_stamp_duty_is_sell_side_only() {
        let m = AShareFeeModel {
            commission_bp: 0,
            min_commission: 0,
            stamp_duty_bp: 5,
            transfer_fee_bp: 0,
        };
        let buy = m.commission_for_side(1_000_000_000, 100_000_000_000, crate::Side::Buy, false);
        let sell = m.commission_for_side(1_000_000_000, 100_000_000_000, crate::Side::Sell, false);
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
}
