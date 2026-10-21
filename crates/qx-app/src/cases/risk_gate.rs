use crate::spec::{BacktestRiskRules, BacktestSpec};
use qx_risk::{
    MaxNotionalRule, MaxQtyRule, NoShortRule, RuleSet, CONSERVATIVE_DEFAULT_RULE_SET_VERSION,
    CONSERVATIVE_MAX_QTY_RAW,
};
use qx_zhenlu::RiskGate;

/// Match the legacy built-in backtest's no-config spot risk chain and audit version.
struct BacktestRiskPolicy<'a> {
    configured: Option<&'a BacktestRiskRules>,
    conservative_max_qty: bool,
    implicit_no_short: bool,
    version: String,
}

fn backtest_risk_policy<'a>(
    configured: Option<&'a BacktestRiskRules>,
    allow_short: bool,
) -> BacktestRiskPolicy<'a> {
    let short_banned_by_config = configured.is_some_and(|rules| rules.no_short);
    let implicit_no_short = !allow_short && !short_banned_by_config;
    let conservative_max_qty = configured.is_none_or(|rules| {
        rules.max_qty_raw.is_none() && rules.max_notional_raw.is_none() && !rules.no_short
    });
    let mut version = match configured {
        None => CONSERVATIVE_DEFAULT_RULE_SET_VERSION.to_string(),
        Some(rules) if conservative_max_qty => format!("{}+conservative-max-qty", rules.version),
        Some(rules) => rules.version.clone(),
    };
    if implicit_no_short {
        version.push_str("+implicit-no-short");
    }
    BacktestRiskPolicy {
        configured,
        conservative_max_qty,
        implicit_no_short,
        version,
    }
}

/// 回测链的风控门构造：**唯一**一处把"声明的规则 + 允许做空"折成 `RiskGate` 与版本字符串。
///
/// 自 WP-2 起 Bar 链与深度链共用它：两条链各自拼一份规则集，就会出现同一份
/// `strategy.risk_rules` 在两条链上得到两个 `rule_set_version`，而产物正是拿这一格判
/// "同一份配置"（V10 §4.2）。
pub(crate) fn risk_gate_for(configured: Option<&BacktestRiskRules>, allow_short: bool) -> RiskGate {
    let policy = backtest_risk_policy(configured, allow_short);
    let mut rules = RuleSet::with_version(policy.version);
    if policy.conservative_max_qty {
        rules.add(Box::new(MaxQtyRule {
            max_qty: CONSERVATIVE_MAX_QTY_RAW,
        }));
    }
    if let Some(configured) = policy.configured {
        if let Some(max_qty) = configured.max_qty_raw {
            rules.add(Box::new(MaxQtyRule { max_qty }));
        }
        if let Some(max_notional) = configured.max_notional_raw {
            rules.add(Box::new(MaxNotionalRule { max_notional }));
        }
        if configured.no_short {
            rules.add(Box::new(NoShortRule));
        }
    }
    if policy.implicit_no_short {
        rules.add(Box::new(NoShortRule));
    }
    RiskGate::from_rule_set(rules)
}

/// 产物里 `risk_rules.source` 那一格：只区分"吃没吃配置"，与门禁构造同源。
pub const BACKTEST_RISK_RULE_SOURCE_CONFIG: &str = "runtime-config";
pub const BACKTEST_RISK_RULE_SOURCE_DEFAULT: &str = "conservative-default";

pub(crate) fn risk_rule_source_label(configured: Option<&BacktestRiskRules>) -> &'static str {
    match configured {
        Some(_) => BACKTEST_RISK_RULE_SOURCE_CONFIG,
        None => BACKTEST_RISK_RULE_SOURCE_DEFAULT,
    }
}

/// Match the legacy built-in backtest's no-config spot risk chain and audit version.
pub(crate) fn backtest_risk_gate(spec: &BacktestSpec) -> RiskGate {
    let allow_short = spec.allow_short.unwrap_or_else(|| {
        spec.market_spec
            .as_ref()
            .is_some_and(|market| market.product.supports_leverage())
    });
    risk_gate_for(spec.risk_rules.as_ref(), allow_short)
}
