//! 内置策略信号参数的读取与播报：`--config` 里那四项如何覆盖到策略配置、生效口径如何印。
//!
//! 自 V11 Q72 起从 `single_strategy.rs` 搬来这里：那两条 Bar 链与深度链都要在同一处回答
//! "这套窗口参数从哪来"，而"措辞只有一处定义"正是 Q0b/Q54 一族缺陷的解法。

use super::*;

/// `--config` 里的 `strategy.builtin_*` 信号参数，逐项覆盖到内置策略配置上（V11 Q64）。
///
/// 覆盖本身住在 [`apply_builtin_signal_overrides`]，与 `strategy backtest` 同一处读法；这里只
/// 多两件事：把配置路径读开，以及在覆盖之后补一次体检 —— 只写了单边窗口时，运行时体检看不到
/// （它只比两个都给了的键），非法组合要等这四项并进各链的默认窗口之后才成立。复检必须在这里
/// 做，因为三条内置链紧接着就要印 `[X · Signal]` 那行生效口径：先宣告再报错等于往 stdout 写了一套
/// 并没有跑过的参数。下单数量仍由命令行位置参数点名，这里不替它做主。
pub(crate) fn apply_configured_builtin_signal(
    config: &mut BuiltinStrategyConfig,
    config_path: Option<&Path>,
) -> Result<&'static str, String> {
    let Some(path) = config_path else {
        return Ok("builtin-default");
    };
    let strategy = read_runtime_config(path)?.strategy;
    let source = builtin_signal_source(&strategy);
    apply_builtin_signal_overrides(config, &strategy);
    config.validate().map_err(|error| {
        format!(
            "{error}；{} 的 strategy.builtin_* 覆盖后为 fast_window={} slow_window={} \
             period={} threshold_bps={}，请检查 builtin_fast_window / builtin_slow_window / \
             builtin_period / builtin_threshold_bps",
            path.display(),
            config.fast_window,
            config.slow_window,
            config.period,
            config.threshold_bps
        )
    })?;
    Ok(source)
}

/// 把生效的那套信号口径写成一行 stdout 文案：三条链共用，措辞只有一处定义。
/// 产物里看不出口径的缺陷（Q0b/Q54 一族）都是从"各链各印一句"开始的。
pub(crate) fn builtin_signal_note(config: &BuiltinStrategyConfig, source: &str) -> String {
    format!(
        "source={} fast_window={} slow_window={} period={} threshold_bps={}",
        source, config.fast_window, config.slow_window, config.period, config.threshold_bps
    )
}
