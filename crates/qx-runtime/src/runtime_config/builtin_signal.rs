//! Shared builtin signal overrides declared by runtime configuration.

use super::StrategyRuntimeConfig;
use qx_strategy::BuiltinStrategyConfig;

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
