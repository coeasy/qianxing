//! 内置策略信号参数的读取与播报：`--config` 里那四项如何覆盖到策略配置、生效口径如何印。
//!
//! 自 V11 Q72 起从 `single_strategy.rs` 搬来这里：那两条 Bar 链与深度链都要在同一处回答
//! "这套窗口参数从哪来"，而"措辞只有一处定义"正是 Q0b/Q54 一族缺陷的解法。

pub(crate) use qx_app::artifact_store::BacktestSignalParams;

use crate::{read_runtime_config, BuiltinSignalProvenance};
use qx_core::{InstrumentId, Quantity};
use qx_runtime::StrategyRuntimeConfig;
use qx_strategy::builtin_signal::BuiltinSignalKnob;
use qx_strategy::{BuiltinStrategyConfig, BuiltinStrategyKind};
use std::path::Path;

pub(crate) fn multi_leg_builtin_strategy_config(
    kind: BuiltinStrategyKind,
    primary_instrument: InstrumentId,
    reference_instrument: InstrumentId,
    quantity: Quantity,
) -> BuiltinStrategyConfig {
    BuiltinStrategyConfig {
        kind,
        strategy_id: format!("builtin-{}-multi", kind.name()),
        strategy_version: format!("builtin-{}-v1", kind.name()),
        instrument: primary_instrument,
        quantity,
        target_notional_raw: None,
        primary_market_spec: None,
        reference_market_spec: None,
        target_available_margin_bps: None,
        target_volatility_bps: None,
        target_kelly_fraction_bps: None,
        kelly_lookback_bars: None,
        fast_window: 5,
        slow_window: 20,
        period: 14,
        threshold_bps: 100,
        reference_instrument: Some(reference_instrument),
        primary_policy: None,
        reference_policy: None,
    }
}

/// Extract runtime-config declarations, then delegate effect classification to qx-strategy.
pub(crate) fn builtin_signal_provenance(
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

/// `--config` 里的 `strategy.builtin_*` 信号参数，逐项覆盖到内置策略配置上（V11 Q64）。
///
/// 覆盖本身住在 qx-runtime，与 worker 侧共用同一处读法；这里只多两件事：把配置路径读开，以及
/// 在覆盖之后补一次体检 —— 只写了单边窗口时，运行时体检看不到
/// （它只比两个都给了的键），非法组合要等这四项并进各链的默认窗口之后才成立。复检必须在这里
/// 做，因为三条内置链紧接着就要印 `[X · Signal]` 那行生效口径：先宣告再报错等于往 stdout 写了一套
/// 并没有跑过的参数。下单数量仍由命令行位置参数点名，这里不替它做主。
pub(crate) fn apply_configured_builtin_signal(
    config: &mut BuiltinStrategyConfig,
    config_path: Option<&Path>,
) -> Result<BuiltinSignalProvenance, String> {
    let provenance = if let Some(path) = config_path {
        let strategy = read_runtime_config(path)?.strategy;
        let provenance = builtin_signal_provenance(config.kind, &strategy);
        qx_runtime::apply_builtin_signal_overrides(config, &strategy);
        provenance
    } else {
        BuiltinSignalProvenance::defaults(config.kind)
    };
    let config_label = config_path
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "runtime config".into());
    config.validate().map_err(|error| {
        format!(
            "{error}；{} 的 strategy.builtin_* 覆盖后为 fast_window={} slow_window={} \
             period={} threshold_bps={}，请检查 builtin_fast_window / builtin_slow_window / \
             builtin_period / builtin_threshold_bps",
            config_label,
            config.fast_window,
            config.slow_window,
            config.period,
            config.threshold_bps
        )
    })?;
    Ok(provenance)
}

/// 把生效的那套信号口径写成一行 stdout 文案：三条链共用，措辞只有一处定义。
/// 产物里看不出口径的缺陷（Q0b/Q54 一族）都是从"各链各印一句"开始的。
///
/// 后半句是"这四项里到底哪几项进了本 kind 的信号"（V12 #102）：MACD 的 12/26/9 是内核
/// 常数，光印 `fast_window=5` 会让读者以为那一档可选、并且真的选了。
pub(crate) fn builtin_signal_note(
    config: &BuiltinStrategyConfig,
    provenance: &BuiltinSignalProvenance,
) -> String {
    format!(
        "source={} fast_window={} slow_window={} period={} threshold_bps={} \
         knobs={} declared_unused={}",
        provenance.source,
        config.fast_window,
        config.slow_window,
        config.period,
        config.threshold_bps,
        provenance.knobs,
        provenance.declared_unused,
    )
}

/// 这一轮并进策略配置的信号口径，结构与上面那行文案取自同一份配置（V12 R4-j）。
///
/// 摘要此前不写它——`[X · Signal] source=… fast_window=…` 只活在终端上，事后读产物分不出
/// fast=5 与 fast=20，等于 Q64（`--config` 的窗口被静默忽略）的另一半：参数确实并进了配置，
/// 只是没人能复查它并进的是什么。V12 #102 补上它没说完的那半句：并进了配置不等于上了赛场，
/// `knobs` / `declared_unused` 两格就是"这一轮真正生效的是哪几项"与"提了却没上场的键"。

pub(crate) fn builtin_signal_params(
    config: &BuiltinStrategyConfig,
    provenance: &BuiltinSignalProvenance,
) -> BacktestSignalParams {
    BacktestSignalParams {
        kind: config.kind.name(),
        source: provenance.source,
        knobs: provenance.knobs.clone(),
        declared_unused: provenance.declared_unused.clone(),
        fast_window: config.fast_window,
        slow_window: config.slow_window,
        period: config.period,
        threshold_bps: config.threshold_bps,
        quantity_raw: config.quantity.raw(),
    }
}
