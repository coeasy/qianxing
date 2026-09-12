//! 成本、延迟、保证金。
//!
//! 真实回测不能只扣比例手续费。Fee / Latency / Margin 属于不同语义层，
//! 却必须共享同一订单事件流，否则成交、账户与组合估值会彼此断裂。

use qx_core::{QxError, QxResult, Side, TradingInstrumentSpec};

/// 名义额 = qty * price / SCALE（两者均为定点原始值）。
pub fn notional(qty: i128, price: i128) -> i128 {
    qty.saturating_mul(price) / 1_000_000_000
}

/// 按基点取金额（1 bp = 万分之一）。
pub fn bp_amount(x: i128, bp: i64) -> i128 {
    x.saturating_mul(bp as i128) / 10_000
}

/// 成交流动性身份。
///
/// Bar 级回测通常无法证明一个限价单最终是 maker 还是 taker，因此必须允许
/// `Unknown`，并由费用模型采用保守策略，而不是把“限价单”错误等同于 maker。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LiquidityRole {
    Maker,
    Taker,
    Unknown,
}

/// 费用计算所需的完整成交语义。
///
/// `side` 与 `liquidity` 必须分离：A 股印花税由买卖方向决定，而加密交易所
/// maker/taker 费率由流动性身份决定，二者不能互相替代。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct FeeContext {
    pub side: Side,
    pub qty: i128,
    pub price: i128,
    pub liquidity: LiquidityRole,
}

pub trait FeeModel {
    fn name(&self) -> &'static str;
    fn version(&self) -> &'static str {
        "v1"
    }

    /// 兼容旧调用方的费用入口。
    ///
    /// 新撮合链路必须优先调用 `commission_for`，因为只有它同时携带交易方向
    /// 和流动性身份。这个方法保留给旧插件/测试，不能用于需要方向税费的模型。
    fn commission(&self, qty: i128, price: i128, is_maker: bool) -> i128;

    /// 正式费用入口。默认把明确 maker 映射到旧接口，其余情况按 taker
    /// 保守计费；具体模型可以覆盖它使用 side/liquidity 的完整语义。
    fn commission_for(&self, context: &FeeContext) -> i128 {
        self.commission(
            context.qty,
            context.price,
            matches!(context.liquidity, LiquidityRole::Maker),
        )
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

impl FeeModel for MakerTakerFeeModel {
    fn name(&self) -> &'static str {
        "MakerTaker"
    }
    fn version(&self) -> &'static str {
        "v2"
    }
    fn commission(&self, qty: i128, price: i128, is_maker: bool) -> i128 {
        let n = notional(qty, price);
        if is_maker {
            bp_amount(n, self.maker_bp)
        } else {
            bp_amount(n, self.taker_bp)
        }
    }
    fn commission_for(&self, context: &FeeContext) -> i128 {
        let n = notional(context.qty, context.price);
        match context.liquidity {
            LiquidityRole::Maker => bp_amount(n, self.maker_bp),
            LiquidityRole::Taker | LiquidityRole::Unknown => bp_amount(n, self.taker_bp),
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

impl AShareFeeModel {
    fn common_fee(&self, qty: i128, price: i128) -> i128 {
        let n = notional(qty, price);
        bp_amount(n, self.commission_bp).max(self.min_commission)
            + bp_amount(n, self.transfer_fee_bp)
    }
}

impl FeeModel for AShareFeeModel {
    fn name(&self) -> &'static str {
        "AShareFee"
    }
    fn version(&self) -> &'static str {
        "v2"
    }
    fn commission(&self, qty: i128, price: i128, _is_maker: bool) -> i128 {
        // 旧接口没有 Side，无法可靠判断印花税。禁止继续把 maker/taker 猜成
        // 买卖方向；正式撮合链路通过 commission_for 计算完整费用。
        self.common_fee(qty, price)
    }
    fn commission_for(&self, context: &FeeContext) -> i128 {
        let mut fee = self.common_fee(context.qty, context.price);
        if context.side == Side::Sell {
            fee += bp_amount(notional(context.qty, context.price), self.stamp_duty_bp);
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

pub trait LatencyModel {
    fn name(&self) -> &'static str;
    fn version(&self) -> &'static str {
        "v1"
    }
    /// 命令延迟（纳秒）。延迟必须影响命令的**可见时间**，
    /// 不能用成交价随机偏移偷偷替代——那会破坏时间因果。
    fn delay_ns(&self) -> u64;

    fn descriptor(&self) -> String {
        format!(
            "{}@{}[delay_ns={}]",
            self.name(),
            self.version(),
            self.delay_ns()
        )
    }
}

pub struct ZeroLatency;

impl LatencyModel for ZeroLatency {
    fn name(&self) -> &'static str {
        "ZeroLatency"
    }
    fn delay_ns(&self) -> u64 {
        0
    }
}

pub struct StaticLatency {
    pub base_ns: u64,
    pub insert_ns: u64,
}

impl LatencyModel for StaticLatency {
    fn name(&self) -> &'static str {
        "StaticLatency"
    }
    fn delay_ns(&self) -> u64 {
        self.base_ns + self.insert_ns
    }
    fn descriptor(&self) -> String {
        format!(
            "{}@{}[base_ns={};insert_ns={}]",
            self.name(),
            self.version(),
            self.base_ns,
            self.insert_ns
        )
    }
}

pub trait MarginRule {
    fn name(&self) -> &'static str;
    fn version(&self) -> &'static str {
        "v1"
    }
    fn initial_margin(&self, notional: i128) -> i128;
    fn maintain_margin(&self, notional: i128) -> i128;

    /// 产品规格路径的初始保证金。默认使用产品固定杠杆；阶梯规则覆盖此
    /// 方法后，回测的合约开仓和强平会真正使用同一套 tier。
    fn instrument_initial_margin(
        &self,
        spec: &TradingInstrumentSpec,
        qty: i128,
        price: i128,
        leverage: u32,
    ) -> QxResult<i128> {
        spec.initial_margin(qty, price, leverage)
    }

    fn instrument_maintenance_margin(
        &self,
        spec: &TradingInstrumentSpec,
        qty: i128,
        price: i128,
    ) -> QxResult<i128> {
        spec.maintenance_margin(qty, price)
    }

    fn descriptor(&self) -> String {
        format!("{}@{}", self.name(), self.version())
    }
}

pub struct NoMargin;

impl MarginRule for NoMargin {
    fn name(&self) -> &'static str {
        "NoMargin"
    }
    fn initial_margin(&self, _n: i128) -> i128 {
        0
    }
    fn maintain_margin(&self, _n: i128) -> i128 {
        0
    }
}

pub struct FixedRateMargin {
    pub initial_bp: i64,
    pub maintain_bp: i64,
}

/// 按固定杠杆计算初始保证金，并按维持保证金基点计算强平边界。
///
/// 该模型可直接用于永续/期货回测；交易所真实账户仍需在 CCXT 侧读取
/// leverage tier，并将得到的参数冻结进回测 RunManifest。
pub struct LeverageMargin {
    pub leverage: u32,
    pub maintenance_bp: i64,
}

impl MarginRule for LeverageMargin {
    fn name(&self) -> &'static str {
        "LeverageMargin"
    }
    fn initial_margin(&self, n: i128) -> i128 {
        if self.leverage == 0 {
            return i128::MAX;
        }
        n.max(0) / i128::from(self.leverage)
    }
    fn maintain_margin(&self, n: i128) -> i128 {
        bp_amount(n.max(0), self.maintenance_bp)
    }
    fn descriptor(&self) -> String {
        format!(
            "{}@{}[leverage={};maintenance_bp={}]",
            self.name(),
            self.version(),
            self.leverage,
            self.maintenance_bp
        )
    }
}

/// 交易所常见的阶梯保证金规则。按名义额升序选择第一个覆盖档位。
pub struct TieredMargin {
    pub tiers: Vec<MarginTier>,
}

pub struct MarginTier {
    pub max_notional: i128,
    pub initial_bp: i64,
    pub maintenance_bp: i64,
    pub max_leverage: Option<u32>,
}

impl MarginRule for TieredMargin {
    fn name(&self) -> &'static str {
        "TieredMargin"
    }
    fn initial_margin(&self, n: i128) -> i128 {
        self.tier(n)
            .map_or(i128::MAX, |tier| bp_amount(n.max(0), tier.initial_bp))
    }
    fn maintain_margin(&self, n: i128) -> i128 {
        self.tier(n)
            .map_or(i128::MAX, |tier| bp_amount(n.max(0), tier.maintenance_bp))
    }
    fn instrument_initial_margin(
        &self,
        spec: &TradingInstrumentSpec,
        qty: i128,
        price: i128,
        leverage: u32,
    ) -> QxResult<i128> {
        let notional = spec.notional(qty, price)?;
        let Some(tier) = self.tier(notional) else {
            return Err(QxError::BusinessViolation(
                "产品名义额超过阶梯保证金最大档位".into(),
            ));
        };
        if tier.max_leverage.is_some_and(|max| leverage > max) {
            return Err(QxError::BusinessViolation(
                "订单杠杆超过当前名义额阶梯限制".into(),
            ));
        }
        Ok(self.initial_margin(notional))
    }
    fn instrument_maintenance_margin(
        &self,
        spec: &TradingInstrumentSpec,
        qty: i128,
        price: i128,
    ) -> QxResult<i128> {
        let notional = spec.notional(qty, price)?;
        if self.tier(notional).is_none() {
            return Err(QxError::BusinessViolation(
                "产品名义额超过阶梯保证金最大档位".into(),
            ));
        }
        Ok(self.maintain_margin(notional))
    }
    fn descriptor(&self) -> String {
        let tiers = self
            .tiers
            .iter()
            .map(|tier| {
                format!(
                    "{}:{}:{}:{}",
                    tier.max_notional,
                    tier.initial_bp,
                    tier.maintenance_bp,
                    tier.max_leverage.unwrap_or(0)
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        format!("{}@{}[tiers={tiers}]", self.name(), self.version())
    }
}

impl TieredMargin {
    fn tier(&self, n: i128) -> Option<&MarginTier> {
        self.tiers
            .iter()
            .filter(|tier| tier.max_notional >= n.max(0))
            .min_by_key(|tier| tier.max_notional)
    }
}

impl MarginRule for FixedRateMargin {
    fn name(&self) -> &'static str {
        "FixedRateMargin"
    }
    fn initial_margin(&self, n: i128) -> i128 {
        bp_amount(n, self.initial_bp)
    }
    fn maintain_margin(&self, n: i128) -> i128 {
        bp_amount(n, self.maintain_bp)
    }
    fn descriptor(&self) -> String {
        format!(
            "{}@{}[initial_bp={};maintain_bp={}]",
            self.name(),
            self.version(),
            self.initial_bp,
            self.maintain_bp
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maker_cheaper_than_taker_and_unknown_is_conservative() {
        let m = MakerTakerFeeModel {
            maker_bp: 2,
            taker_bp: 5,
        };
        let qty = 1_000_000_000i128; // 1.0
        let px = 100_000_000_000i128; // 100.0
        let maker = m.commission_for(&FeeContext {
            side: Side::Buy,
            qty,
            price: px,
            liquidity: LiquidityRole::Maker,
        });
        let taker = m.commission_for(&FeeContext {
            side: Side::Buy,
            qty,
            price: px,
            liquidity: LiquidityRole::Taker,
        });
        let unknown = m.commission_for(&FeeContext {
            side: Side::Buy,
            qty,
            price: px,
            liquidity: LiquidityRole::Unknown,
        });
        assert!(maker < taker);
        assert_eq!(unknown, taker);
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
    fn a_share_stamp_duty_depends_on_sell_side_not_liquidity_role() {
        let m = AShareFeeModel {
            commission_bp: 0,
            min_commission: 0,
            stamp_duty_bp: 10,
            transfer_fee_bp: 0,
        };
        let qty = 1_000_000_000i128;
        let px = 100_000_000_000i128;
        let buy_taker = m.commission_for(&FeeContext {
            side: Side::Buy,
            qty,
            price: px,
            liquidity: LiquidityRole::Taker,
        });
        let sell_maker = m.commission_for(&FeeContext {
            side: Side::Sell,
            qty,
            price: px,
            liquidity: LiquidityRole::Maker,
        });
        assert_eq!(buy_taker, 0);
        assert_eq!(sell_maker, bp_amount(notional(qty, px), 10));
    }

    #[test]
    fn latency_is_additive() {
        let l = StaticLatency {
            base_ns: 100,
            insert_ns: 50,
        };
        assert_eq!(l.delay_ns(), 150);
    }

    #[test]
    fn leverage_and_tiered_margin_are_deterministic() {
        let leverage = LeverageMargin {
            leverage: 10,
            maintenance_bp: 500,
        };
        assert_eq!(leverage.initial_margin(1_000), 100);
        assert_eq!(leverage.maintain_margin(1_000), 50);
        let tiered = TieredMargin {
            tiers: vec![
                MarginTier {
                    max_notional: 1_000,
                    initial_bp: 1_000,
                    maintenance_bp: 500,
                    max_leverage: None,
                },
                MarginTier {
                    max_notional: 10_000,
                    initial_bp: 2_000,
                    maintenance_bp: 1_000,
                    max_leverage: None,
                },
            ],
        };
        assert_eq!(tiered.initial_margin(500), 50);
        assert_eq!(tiered.maintain_margin(2_000), 200);
    }
}
