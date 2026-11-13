//! Versioned, actionable recipes for the built-in strategy catalog.
//!
//! Recipes are generated from the same strategy kinds and signal-knob defaults as the
//! execution kernel. They explain required inputs and risk/cost checks; they do not promise
//! a profitable configuration or silently authorize a live product.

use qx_strategy::{builtin_signal::BuiltinSignalKnob, BuiltinStrategyKind};
use serde::{Deserialize, Serialize};

pub const BUILTIN_STRATEGY_RECIPE_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StrategyRecipeParameter {
    pub name: String,
    pub config_key: Option<String>,
    pub default_value: i64,
    pub unit: String,
    pub configurable: bool,
    pub description: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuiltinStrategyRecipe {
    pub id: String,
    pub name: String,
    pub family: String,
    pub description: String,
    pub required_inputs: Vec<String>,
    pub signal_parameters: Vec<StrategyRecipeParameter>,
    pub cost_inputs: Vec<String>,
    pub risk_notes: Vec<String>,
    pub validation_notes: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuiltinStrategyRecipeCatalog {
    pub schema_version: u32,
    pub recipes: Vec<BuiltinStrategyRecipe>,
}

pub fn builtin_strategy_recipe_catalog() -> BuiltinStrategyRecipeCatalog {
    BuiltinStrategyRecipeCatalog {
        schema_version: BUILTIN_STRATEGY_RECIPE_SCHEMA_VERSION,
        recipes: BuiltinStrategyKind::ALL
            .into_iter()
            .map(recipe_for)
            .collect(),
    }
}

fn recipe_for(kind: BuiltinStrategyKind) -> BuiltinStrategyRecipe {
    let is_multi_leg = kind.needs_reference_leg();
    let mut cost_inputs = vec![
        "maker/taker fee schedule".to_string(),
        "slippage assumption".to_string(),
        "execution model and bar timing".to_string(),
    ];
    let mut risk_notes = vec![
        "内置默认 sizing 是固定目标数量，不代表账户风险预算；运行前应设置仓位上限和单笔风险规则。"
            .to_string(),
        "现货 Cash 按账户能力限制卖出；做空需要产品、权限、持仓模式和 RiskGate 全部通过。"
            .to_string(),
    ];
    if is_multi_leg {
        cost_inputs.push("funding, borrow, and settlement costs for each leg".to_string());
        risk_notes.push(
            "双腿信号不等于原子成交；须评估时间对齐、部分成交、腿间滑点和对冲失败。".to_string(),
        );
    }
    let mut validation_notes = vec![
        "用未参与参数选择的验证区间复核；没有 OOS 证据时只视为样例，不代表策略有效。".to_string(),
        "先核对数据连续性、未闭合 Bar、市场规格和费用版本，再解释结果。".to_string(),
    ];
    if is_multi_leg {
        validation_notes.push("两腿必须使用可对齐的时间戳和各自市场规格。".to_string());
    }

    BuiltinStrategyRecipe {
        id: format!("builtin.{}.v1", kind.name()),
        name: kind.name().to_string(),
        family: strategy_family(kind).to_string(),
        description: kind.description().to_string(),
        required_inputs: if is_multi_leg {
            vec!["primary_bar_series".into(), "reference_bar_series".into()]
        } else {
            vec!["bar_series".into()]
        },
        signal_parameters: signal_parameters(kind),
        cost_inputs,
        risk_notes,
        validation_notes,
    }
}

fn strategy_family(kind: BuiltinStrategyKind) -> &'static str {
    use BuiltinStrategyKind::*;
    match kind {
        SmaCross | EmaCross | Macd | AtrTrend | KeltnerTrend => "trend",
        Rsi | Bollinger | MeanReversion | VwapReversion => "mean_reversion",
        DonchianBreakout | VolatilityBreakout => "breakout",
        Momentum => "momentum",
        Grid => "grid",
        PairsArbitrage | BasisArbitrage | CrossVenueArbitrage | SpotFuturesArbitrage => "arbitrage",
    }
}

fn signal_parameters(kind: BuiltinStrategyKind) -> Vec<StrategyRecipeParameter> {
    if kind == BuiltinStrategyKind::Macd {
        return [
            ("fast_period", 12, "bars", "内核固定的 MACD 快速 EMA 周期。"),
            ("slow_period", 26, "bars", "内核固定的 MACD 慢速 EMA 周期。"),
            (
                "signal_period",
                9,
                "bars",
                "内核固定的 MACD 信号 EMA 周期。",
            ),
        ]
        .into_iter()
        .map(
            |(name, default_value, unit, description)| StrategyRecipeParameter {
                name: name.into(),
                config_key: None,
                default_value,
                unit: unit.into(),
                configurable: false,
                description: description.into(),
            },
        )
        .collect();
    }

    kind.signal_knobs()
        .iter()
        .map(|knob| {
            let (default_value, unit, description) = match knob {
                BuiltinSignalKnob::FastWindow => (5, "bars", "快速计算窗口；须小于慢速窗口。"),
                BuiltinSignalKnob::SlowWindow => {
                    (20, "bars", "慢速计算窗口；需要足够长的预热数据。")
                }
                BuiltinSignalKnob::Period => (14, "bars", "该策略使用的指标/观察周期。"),
                BuiltinSignalKnob::ThresholdBps => {
                    (100, "basis_points", "信号阈值；1 bp = 0.01%。")
                }
            };
            StrategyRecipeParameter {
                name: knob.field().to_string(),
                config_key: Some(format!("builtin_{}", knob.field())),
                default_value,
                unit: unit.into(),
                configurable: true,
                description: description.into(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_builtin_strategy_has_a_complete_recipe_with_kernel_defaults() {
        let catalog = builtin_strategy_recipe_catalog();
        assert_eq!(catalog.schema_version, 1);
        assert_eq!(catalog.recipes.len(), BuiltinStrategyKind::ALL.len());
        for (recipe, kind) in catalog.recipes.iter().zip(BuiltinStrategyKind::ALL) {
            assert_eq!(recipe.name, kind.name());
            assert_eq!(recipe.id, format!("builtin.{}.v1", kind.name()));
            assert!(!recipe.description.is_empty());
            assert!(!recipe.cost_inputs.is_empty());
            assert!(!recipe.risk_notes.is_empty());
            assert!(!recipe.validation_notes.is_empty());
            assert_eq!(
                recipe.required_inputs.len(),
                if kind.needs_reference_leg() { 2 } else { 1 }
            );
            if kind == BuiltinStrategyKind::Macd {
                assert_eq!(recipe.signal_parameters.len(), 3);
                assert!(recipe
                    .signal_parameters
                    .iter()
                    .all(|parameter| !parameter.configurable && parameter.config_key.is_none()));
            } else {
                assert_eq!(recipe.signal_parameters.len(), kind.signal_knobs().len());
            }
        }
        assert!(catalog
            .recipes
            .iter()
            .filter(|recipe| recipe.family == "arbitrage")
            .all(|recipe| recipe.required_inputs.len() == 2));
    }
}
