//! Runtime-config projection into the native builtin strategy contract.

use super::{builtin_signal::apply_builtin_signal_overrides, StrategyRuntimeConfig};
use qx_core::{
    InstrumentId, MarginMode, OrderPolicy, PositionMode, PositionSide, Quantity, TradingProduct,
};
use qx_strategy::{BuiltinStrategyConfig, BuiltinStrategyKind};

/// Resolve the native builtin strategy inputs declared by one runtime strategy config.
///
/// Signal parameters are merged with the strategy's historical defaults before validation, so a
/// single-sided override is checked against the effective values that will actually run.
pub fn builtin_strategy_config_from_runtime(
    strategy: &StrategyRuntimeConfig,
    instrument: &InstrumentId,
) -> Result<BuiltinStrategyConfig, String> {
    let name = strategy
        .builtin_strategy
        .as_deref()
        .ok_or_else(|| "Strategy 未配置 builtin_strategy".to_string())?;
    let kind = BuiltinStrategyKind::parse(name)?;
    let strategy_id = strategy
        .id
        .clone()
        .unwrap_or_else(|| format!("builtin-{}", kind.name()));
    let quantity_raw = strategy
        .builtin_quantity
        .map(i128::from)
        .or_else(|| (strategy.target_qty != 0).then_some(strategy.target_qty.abs()))
        .unwrap_or(1);
    if quantity_raw <= 0 {
        return Err("builtin_quantity 必须为正整数".into());
    }

    let mut config = BuiltinStrategyConfig::new(
        kind,
        strategy_id,
        instrument.clone(),
        Quantity::from_raw(quantity_raw),
    )?;
    config.strategy_version = strategy.version.clone();
    config.target_notional_raw = strategy.builtin_target_notional_raw;
    config.target_available_margin_bps = strategy.builtin_target_available_margin_bps;
    config.target_volatility_bps = strategy.builtin_target_volatility_bps;
    config.target_kelly_fraction_bps = strategy.builtin_target_kelly_fraction_bps;
    config.kelly_lookback_bars = strategy.builtin_kelly_lookback_bars;
    apply_builtin_signal_overrides(&mut config, strategy);
    if let Some(reference) = strategy.builtin_reference_instrument.as_deref() {
        config.reference_instrument = Some(
            InstrumentId::parse(reference)
                .ok_or_else(|| format!("builtin_reference_instrument 非法: {reference}"))?,
        );
    }
    if config.reference_instrument.is_some() {
        let primary_product = strategy.product.unwrap_or(TradingProduct::Spot);
        let primary_margin =
            strategy
                .margin_mode
                .unwrap_or(if primary_product == TradingProduct::Spot {
                    MarginMode::Cash
                } else {
                    MarginMode::Cross
                });
        let primary_position = strategy.position_mode.unwrap_or(PositionMode::OneWay);
        config.primary_policy = Some(OrderPolicy {
            reduce_only: false,
            position_side: PositionSide::Net,
            margin_mode: primary_margin,
            position_mode: primary_position,
            leverage: strategy.leverage.unwrap_or(1),
            post_only: false,
        });
        config.reference_policy = Some(OrderPolicy {
            reduce_only: false,
            position_side: PositionSide::Net,
            margin_mode: strategy
                .builtin_reference_margin_mode
                .unwrap_or(MarginMode::Cash),
            position_mode: strategy
                .builtin_reference_position_mode
                .unwrap_or(PositionMode::OneWay),
            leverage: strategy.builtin_reference_leverage.unwrap_or(1),
            post_only: false,
        });
    }
    config.validate().map_err(|error| {
        format!(
            "{error}；合并后的 strategy.builtin_* 参数 fast_window={} slow_window={} period={} threshold_bps={}，请检查 builtin_fast_window / builtin_slow_window / builtin_period / builtin_threshold_bps",
            config.fast_window,
            config.slow_window,
            config.period,
            config.threshold_bps,
        )
    })?;
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn instrument() -> InstrumentId {
        InstrumentId::parse("BTCUSDT.BINANCE").unwrap()
    }

    #[test]
    fn runtime_builtin_config_merges_defaults_and_declared_sizing() {
        let strategy = StrategyRuntimeConfig {
            builtin_strategy: Some("ema_cross".into()),
            builtin_quantity: Some(3),
            builtin_fast_window: Some(2),
            builtin_slow_window: Some(7),
            builtin_period: Some(9),
            builtin_threshold_bps: Some(50),
            builtin_target_notional_raw: Some(123_000),
            ..StrategyRuntimeConfig::default()
        };
        let config = builtin_strategy_config_from_runtime(&strategy, &instrument()).unwrap();
        assert_eq!(config.kind, BuiltinStrategyKind::EmaCross);
        assert_eq!(config.strategy_id, "builtin-ema_cross");
        assert_eq!(config.quantity, Quantity::from_raw(3));
        assert_eq!((config.fast_window, config.slow_window), (2, 7));
        assert_eq!((config.period, config.threshold_bps), (9, 50));
        assert_eq!(config.target_notional_raw, Some(123_000));
    }

    #[test]
    fn runtime_builtin_config_rejects_invalid_effective_single_sided_window() {
        let strategy = StrategyRuntimeConfig {
            builtin_strategy: Some("ema_cross".into()),
            builtin_fast_window: Some(25),
            ..StrategyRuntimeConfig::default()
        };
        let error = builtin_strategy_config_from_runtime(&strategy, &instrument()).unwrap_err();
        assert!(error.contains("fast_window=25"), "{error}");
        assert!(error.contains("slow_window=20"), "{error}");
    }

    #[test]
    fn runtime_builtin_config_only_validates_signal_knobs_used_by_kind() {
        let strategy = StrategyRuntimeConfig {
            builtin_strategy: Some("macd".into()),
            builtin_fast_window: Some(25),
            builtin_slow_window: Some(3),
            ..StrategyRuntimeConfig::default()
        };
        let config = builtin_strategy_config_from_runtime(&strategy, &instrument()).unwrap();
        assert_eq!((config.fast_window, config.slow_window), (25, 3));
    }
}
