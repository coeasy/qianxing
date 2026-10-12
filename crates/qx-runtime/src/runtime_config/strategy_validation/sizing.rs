use super::super::*;

/// Validate runtime sizing knobs and report whether this strategy declares one.
pub(super) fn validate_builtin_sizing(
    strategy: &StrategyRuntimeConfig,
    label: &str,
) -> Result<(), String> {
    if strategy
        .builtin_target_notional_raw
        .is_some_and(|value| value <= 0)
    {
        return Err(format!("{label} builtin_target_notional_raw 必须为正"));
    }
    for (name, value) in [
        (
            "builtin_target_available_margin_bps",
            strategy.builtin_target_available_margin_bps,
        ),
        (
            "builtin_target_volatility_bps",
            strategy.builtin_target_volatility_bps,
        ),
        (
            "builtin_target_kelly_fraction_bps",
            strategy.builtin_target_kelly_fraction_bps,
        ),
    ] {
        if value.is_some_and(|bps| !(1..=10_000).contains(&bps)) {
            return Err(format!("{label} {name} 必须在 1..=10000"));
        }
    }
    if strategy
        .builtin_kelly_lookback_bars
        .is_some_and(|bars| !(30..=1000).contains(&bars))
    {
        return Err(format!(
            "{label} builtin_kelly_lookback_bars 必须在 30..=1000"
        ));
    }
    let sizing_modes = [
        strategy.builtin_target_notional_raw.is_some(),
        strategy.builtin_target_available_margin_bps.is_some(),
        strategy.builtin_target_volatility_bps.is_some(),
        strategy.builtin_target_kelly_fraction_bps.is_some(),
    ];
    if sizing_modes.iter().filter(|enabled| **enabled).count() > 1 {
        return Err(format!("{label} builtin sizing 参数只能选择一种"));
    }
    if strategy.builtin_kelly_lookback_bars.is_some()
        != strategy.builtin_target_kelly_fraction_bps.is_some()
    {
        return Err(format!(
            "{label} builtin_kelly_lookback_bars 只能与 Kelly sizing 一起配置"
        ));
    }
    if (sizing_modes.iter().any(|enabled| *enabled)
        || strategy.builtin_kelly_lookback_bars.is_some())
        && strategy.builtin_strategy.is_none()
    {
        return Err(format!(
            "{label} builtin sizing 参数只能用于 builtin_strategy，不能成为孤儿配置"
        ));
    }
    Ok(())
}
