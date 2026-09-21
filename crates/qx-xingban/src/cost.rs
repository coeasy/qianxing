//! 延迟与保证金。
//!
//! 费用契约在内核 [`qx_core::FeeModel`]：回测撮合、订单簿逐档撮合与 Paper 模拟
//! 都要为同一条 `Fill.fee` 负责，放在回测层会让其他执行平面各写一套。
//! 延迟与保证金只参与回测撮合，因此留在本层。

use qx_core::{bp_amount, QxError, QxResult, TradingInstrumentSpec};

pub trait LatencyModel {
    fn name(&self) -> &'static str;
    fn version(&self) -> &'static str {
        "v1"
    }
    /// 命令延迟（纳秒）。延迟必须影响命令的**可见时间**，
    /// 不能用成交价随机偏移偷偷替代——那会破坏时间因果。
    fn delay_ns(&self) -> u64;

    /// 同一条延迟在**撮合时间轴**上的口径：`Bar.ts` 与事件时间戳是毫秒，`delay_ns` 是纳秒。
    /// 把延迟加到时间轴上必须走这里——直接加 `delay_ns()` 会把 1ms 放大成 1e6 ms（约 16.7
    /// 分钟），于是"配了非零延迟"等于"订单在 bar 级回测里永不成交"，而且一声不响。
    fn delay_ms(&self) -> u64 {
        latency_delay_ms(self.delay_ns())
    }

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
        // 饱和加：`ExecutionCostRules::validate` 只在从配置加载时守住这次相加，
        // 而本结构体是公开字段。release 下的回绕会把延迟变短，等于把时间因果
        // 变成一个静默的乐观假设。
        self.base_ns.saturating_add(self.insert_ns)
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

/// 撮合时间轴（`Bar.ts`、`EventLog` 时间戳）统一是毫秒，而延迟按纳秒配置。
const NS_PER_MS: u64 = 1_000_000;

/// 把纳秒延迟换算成撮合时间轴的毫秒刻度。非零延迟一律**向上取整**：整除会把任何亚毫秒
/// 延迟静默归零。走 [`LatencyModel::delay_ms`]，不要在调用处加 `delay_ns()`。
fn latency_delay_ms(delay_ns: u64) -> u64 {
    delay_ns.saturating_add(NS_PER_MS - 1) / NS_PER_MS
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
    fn latency_is_additive() {
        let l = StaticLatency {
            base_ns: 100,
            insert_ns: 50,
        };
        assert_eq!(l.delay_ns(), 150);
    }

    /// 换算发生在毫秒刻度上：亚毫秒延迟不能整除归零，整毫秒延迟不能放大一千倍。
    #[test]
    fn latency_rounds_up_to_whole_milliseconds() {
        assert_eq!(latency_delay_ms(0), 0);
        assert_eq!(latency_delay_ms(1), 1);
        assert_eq!(latency_delay_ms(1_000_000), 1);
        assert_eq!(latency_delay_ms(1_000_001), 2);
        assert_eq!(latency_delay_ms(u64::MAX), u64::MAX / NS_PER_MS);
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
