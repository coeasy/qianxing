//! 执行成本规则：把费率与延迟从代码常量变成可冻结、可比对的配置。
//!
//! 此前只有 A 股路径从 JSON 读费率，加密路径的 maker/taker 在 5 处代码里
//! 写死，延迟模型则完全没有配置入口。两者共用一个加载入口，避免"配置驱动"
//! 只在某一个市场成立。

use qx_core::{FeeModel, MakerTakerFeeModel, QxError, QxResult};
use serde::{Deserialize, Serialize};

use crate::cost::{LatencyModel, StaticLatency, ZeroLatency};

/// 无配置时的加密费率（基点），与历史硬编码保持一致。
///
/// 值定义在 `qx_core::fee`（费用模型的归属处），此处只做再导出：
/// 费率常数的第二处定义会让"配置驱动"与"默认口径"在不同 crate 里各自漂移。
pub use qx_core::fee::{DEFAULT_MAKER_BP, DEFAULT_TAKER_BP};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ExecutionCostRules {
    /// 仅作人读标注；真正的可追溯性来自 fee/latency descriptor。
    pub name: String,
    pub maker_bp: i64,
    pub taker_bp: i64,
    /// 下单到可成交的固定延迟，单位纳秒。撮合时间轴（`Bar.ts`、`EventLog` 时间戳）同为
    /// 毫秒，装配时按毫秒向上取整（见 [`LatencyModel::delay_ms`]），因此 1ns 与 1ms 在
    /// bar 级回测里同效，但非零延迟不会被整除成零。
    pub latency_base_ns: u64,
    /// 报单进队列的附加延迟，与 `latency_base_ns` 相加。
    pub latency_insert_ns: u64,
}

impl Default for ExecutionCostRules {
    fn default() -> Self {
        Self {
            name: "builtin-default".into(),
            maker_bp: DEFAULT_MAKER_BP,
            taker_bp: DEFAULT_TAKER_BP,
            latency_base_ns: 0,
            latency_insert_ns: 0,
        }
    }
}

impl ExecutionCostRules {
    /// 读取并校验成本规则文件。缺省会退化为内置默认费率，调用方必须把
    /// `descriptor()` 写进运行清单，否则无法区分"真的零延迟"和"忘了配置"。
    pub fn load(path: &std::path::Path) -> QxResult<Self> {
        let payload = std::fs::read_to_string(path)
            .map_err(|error| QxError::Permanent(format!("读取成本规则失败: {error}")))?;
        let rules: Self = serde_json::from_str(&payload)
            .map_err(|error| QxError::Permanent(format!("成本规则 JSON 非法: {error}")))?;
        rules.validate()?;
        Ok(rules)
    }

    pub fn validate(&self) -> QxResult<()> {
        if self.maker_bp < 0
            || self.taker_bp < 0
            || self.maker_bp > 10_000
            || self.taker_bp > 10_000
        {
            return Err(QxError::BusinessViolation(
                "成本规则 maker_bp/taker_bp 必须在 0..=10000".into(),
            ));
        }
        if self
            .latency_base_ns
            .checked_add(self.latency_insert_ns)
            .is_none()
        {
            return Err(QxError::BusinessViolation("成本规则延迟相加溢出".into()));
        }
        Ok(())
    }

    pub fn fee_model(&self) -> Box<dyn FeeModel + Send> {
        Box::new(MakerTakerFeeModel {
            maker_bp: self.maker_bp,
            taker_bp: self.taker_bp,
        })
    }

    pub fn latency_model(&self) -> Box<dyn LatencyModel + Send> {
        if self.latency_base_ns == 0 && self.latency_insert_ns == 0 {
            Box::new(ZeroLatency)
        } else {
            Box::new(StaticLatency {
                base_ns: self.latency_base_ns,
                insert_ns: self.latency_insert_ns,
            })
        }
    }

    pub fn descriptor(&self) -> String {
        format!(
            "{}|{}",
            self.fee_model().descriptor(),
            self.latency_model().descriptor()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qx_core::{Side, SCALE};

    #[test]
    fn defaults_match_the_previous_hardcoded_rates() {
        let rules = ExecutionCostRules::default();
        rules.validate().unwrap();
        assert_eq!(
            rules.fee_model().descriptor(),
            "MakerTaker@v1[params=maker_bp=2;taker_bp=5]"
        );
        assert_eq!(
            rules.latency_model().descriptor(),
            "ZeroLatency@v1[delay_ns=0]"
        );
    }

    #[test]
    fn nonzero_latency_switches_to_static_model() {
        let rules = ExecutionCostRules {
            latency_base_ns: 1_000,
            latency_insert_ns: 500,
            ..ExecutionCostRules::default()
        };
        assert_eq!(rules.latency_model().delay_ns(), 1_500);
        assert!(rules
            .latency_model()
            .descriptor()
            .starts_with("StaticLatency"));
    }

    #[test]
    fn absurd_rates_are_rejected() {
        let rules = ExecutionCostRules {
            taker_bp: 10_001,
            ..ExecutionCostRules::default()
        };
        assert!(rules.validate().is_err());
    }

    #[test]
    fn json_only_overrides_present_fields() {
        let rules: ExecutionCostRules =
            serde_json::from_str(r#"{"maker_bp": 0, "latency_base_ns": 250}"#).unwrap();
        rules.validate().unwrap();
        assert_eq!(rules.maker_bp, 0);
        assert_eq!(rules.taker_bp, DEFAULT_TAKER_BP);
        assert_eq!(
            rules
                .fee_model()
                .commission_for_side(SCALE, 100 * SCALE, Side::Buy, true),
            0
        );
    }
}
