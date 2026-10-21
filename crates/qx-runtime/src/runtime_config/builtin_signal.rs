//! Shared builtin signal overrides declared by runtime configuration.

use super::StrategyRuntimeConfig;
use qx_strategy::builtin_signal::{BuiltinSignalKnob, BuiltinSignalProvenance};
use qx_strategy::{BuiltinStrategyConfig, BuiltinStrategyKind};

/// Classify which declared signal knobs affect this strategy, using the shared strategy contract.
pub fn builtin_signal_provenance(
    kind: BuiltinStrategyKind,
    strategy: &StrategyRuntimeConfig,
) -> BuiltinSignalProvenance {
    let declared = BuiltinSignalKnob::ALL
        .into_iter()
        .filter(|knob| match knob {
            BuiltinSignalKnob::FastWindow => strategy.builtin_fast_window.is_some(),
            BuiltinSignalKnob::SlowWindow => strategy.builtin_slow_window.is_some(),
            BuiltinSignalKnob::Period => strategy.builtin_period.is_some(),
            BuiltinSignalKnob::ThresholdBps => strategy.builtin_threshold_bps.is_some(),
        });
    BuiltinSignalProvenance::from_declared(kind, declared)
}

/// Apply the signal knobs declared in runtime config to the native builtin contract.
/// Validation is intentionally left to the caller, after all strategy-specific fields are set.
pub fn apply_builtin_signal_overrides(
    config: &mut BuiltinStrategyConfig,
    strategy: &StrategyRuntimeConfig,
) {
    if let Some(window) = strategy.builtin_fast_window {
        config.fast_window = window;
    }
    if let Some(window) = strategy.builtin_slow_window {
        config.slow_window = window;
    }
    if let Some(period) = strategy.builtin_period {
        config.period = period;
    }
    if let Some(threshold) = strategy.builtin_threshold_bps {
        config.threshold_bps = threshold;
    }
}

/// Apply the declared signal overrides and validate the resulting strategy contract.
/// `config_label` is supplied by the facade so errors can name the configuration source without
/// making this shared runtime binding reopen a file or depend on facade path resolution.
pub fn apply_builtin_signal_overrides_from_runtime(
    config: &mut BuiltinStrategyConfig,
    strategy: Option<&StrategyRuntimeConfig>,
    config_label: &str,
) -> Result<BuiltinSignalProvenance, String> {
    let provenance = if let Some(strategy) = strategy {
        let provenance = builtin_signal_provenance(config.kind, strategy);
        apply_builtin_signal_overrides(config, strategy);
        provenance
    } else {
        BuiltinSignalProvenance::defaults(config.kind)
    };
    config.validate().map_err(|error| {
        format!(
            "{error}；{config_label} 的 strategy.builtin_* 覆盖后为 fast_window={} slow_window={} \
             period={} threshold_bps={}，请检查 builtin_fast_window / builtin_slow_window / \
             builtin_period / builtin_threshold_bps",
            config.fast_window, config.slow_window, config.period, config.threshold_bps
        )
    })?;
    Ok(provenance)
}

#[cfg(test)]
mod tests {
    use super::*;
    use qx_core::{InstrumentId, Quantity};

    fn config() -> BuiltinStrategyConfig {
        BuiltinStrategyConfig::new(
            BuiltinStrategyKind::EmaCross,
            "signal-test",
            InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
            Quantity::from_i64(1),
        )
        .unwrap()
    }

    #[test]
    fn runtime_signal_binding_applies_overrides_and_classifies_declared_knobs() {
        let strategy = StrategyRuntimeConfig {
            builtin_fast_window: Some(3),
            builtin_slow_window: Some(9),
            builtin_period: Some(7),
            ..StrategyRuntimeConfig::default()
        };
        let mut config = config();

        let provenance = apply_builtin_signal_overrides_from_runtime(
            &mut config,
            Some(&strategy),
            "settings/runtime.toml",
        )
        .unwrap();

        assert_eq!(
            (config.fast_window, config.slow_window, config.period),
            (3, 9, 7)
        );
        assert_eq!(provenance.source, "config");
        assert_eq!(provenance.declared_unused, "builtin_period");
    }

    #[test]
    fn runtime_signal_binding_reports_invalid_effective_values_with_source() {
        let strategy = StrategyRuntimeConfig {
            builtin_fast_window: Some(25),
            ..StrategyRuntimeConfig::default()
        };
        let mut config = config();

        let error = apply_builtin_signal_overrides_from_runtime(
            &mut config,
            Some(&strategy),
            "settings/runtime.toml",
        )
        .unwrap_err();

        assert!(error.contains("settings/runtime.toml"), "{error}");
        assert!(error.contains("fast_window=25 slow_window=20"), "{error}");
    }

    #[test]
    fn runtime_signal_binding_without_snapshot_uses_defaults() {
        let mut config = config();

        let provenance =
            apply_builtin_signal_overrides_from_runtime(&mut config, None, "runtime config")
                .unwrap();

        assert_eq!(provenance.source, "builtin-default");
        assert_eq!((config.fast_window, config.slow_window), (5, 20));
    }
}
