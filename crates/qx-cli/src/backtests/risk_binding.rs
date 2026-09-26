//! 命令行回测入口的风控绑定与 `--config` 旗标（V10 §4.2 的收口落点）。

use super::*;

/// 命令行回测入口（`backtest builtin` / `multi-builtin` / `book`）读取风控规则的单一落点。
///
/// 这些入口不像 `strategy backtest` 那样以运行时配置为主输入，因此历史上把规则写死成
/// `None`，导致同一份 `strategy.risk_rules` 在四条链上得到不同门禁（V10 §4.2）。给定
/// `--config` 时必须与策略回测 / Paper / Live 得到同一套规则；不给时才退回该入口的历史默认。
///
/// `default_allow_short` 只在缺配置时使用：多腿套利腿允许做空，单标的与深度链按现货保守禁空。
#[derive(Debug)]
pub(crate) struct BacktestRiskBinding {
    pub(crate) rules: Option<qx_runtime::RiskRulesConfig>,
    pub(crate) allow_short: bool,
    pub(crate) from_config: bool,
}

impl BacktestRiskBinding {
    pub(crate) fn gate(&self) -> RiskGate {
        strategy_risk_gate(self.rules.as_ref(), self.allow_short)
    }

    /// 产物里写明规则来源，让"没配置"与"配了但规则相同"能被区分。
    pub(crate) fn source(&self) -> &'static str {
        if self.from_config {
            "runtime-config"
        } else {
            "conservative-default"
        }
    }
}

pub(crate) fn backtest_risk_binding(
    config_path: Option<&Path>,
    default_allow_short: bool,
) -> Result<BacktestRiskBinding, String> {
    let Some(path) = config_path else {
        return Ok(BacktestRiskBinding {
            rules: None,
            allow_short: default_allow_short,
            from_config: false,
        });
    };
    let config = read_runtime_config(path)?;
    unapplied_strategy_declaration(&config, "risk_rules", |strategy| {
        strategy
            .risk_rules
            .as_ref()
            .map(|rules| format!("{rules:?}"))
    })?;
    // 同一处读点还顺带取走保证金模式与做空许可：它们与 `risk_rules` 一起构成风控门的三条
    // 声明，漏掉任何一条都是"实例里写了、门禁按顶层判"。
    unapplied_strategy_declaration(&config, "margin_mode", |strategy| {
        strategy.margin_mode.map(|mode| format!("{mode:?}"))
    })?;
    unapplied_strategy_declaration(&config, "allow_short", |strategy| {
        strategy.allow_short.map(|flag| flag.to_string())
    })?;
    Ok(BacktestRiskBinding {
        rules: config.strategy.risk_rules.clone(),
        allow_short: strategy_allows_short(&config, config_margin_mode(&config)),
        from_config: true,
    })
}
