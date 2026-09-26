//! 内置策略"哪个信号旋钮上哪一场"的单点表（V12 #102）。
//!
//! `strategy.builtin_fast_window / builtin_slow_window / builtin_period / builtin_threshold_bps`
//! 这四项不是对 17 个 kind 都成立：`builtin.rs` 的 `signal()` 分支各读各的几项，MACD 用的是
//! 内核常数 12/26/9，网格只看阈值。此前这份对应关系只活在 `match` 分支里，于是
//! CLI 的 `[X · Signal]` 播报与回测摘要会把"配置里写了"当成"这一轮生效了"，把从没上场的
//! 旋钮连同数值一起印给读者（`deploy` 的 macd 示例就带着 `builtin_fast_window=5`）。
//!
//! 清单放在这里之后，同一份表要管三件事：哪些旋钮进得了这一轮的结果（`max_history` 与
//! `required_bars` 都只按它开窗口，见 `builtin.rs`），哪些旋钮参与参数体检，以及播报/摘要
//! 里那句"这一轮真正生效的是这几项"。清单外的旋钮因此既改不动成交序列，也改不动成败。

use crate::builtin::BuiltinStrategyKind;

/// 一个 `strategy.builtin_*` 信号旋钮。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BuiltinSignalKnob {
    FastWindow,
    SlowWindow,
    Period,
    ThresholdBps,
}

impl BuiltinSignalKnob {
    /// 播报与用例遍历这份清单的顺序，与配置里的键顺序一致。
    pub const ALL: [Self; 4] = [
        Self::FastWindow,
        Self::SlowWindow,
        Self::Period,
        Self::ThresholdBps,
    ];

    /// 运行时配置里的键名：报错与 `declared_unused` 都写它，读者能直接回去改那一行。
    pub const fn config_key(self) -> &'static str {
        match self {
            Self::FastWindow => "builtin_fast_window",
            Self::SlowWindow => "builtin_slow_window",
            Self::Period => "builtin_period",
            Self::ThresholdBps => "builtin_threshold_bps",
        }
    }

    /// 摘要 `signal` 块与 stdout 文案里的那一格的名字（去掉 `builtin_` 前缀）。
    pub const fn field(self) -> &'static str {
        match self {
            Self::FastWindow => "fast_window",
            Self::SlowWindow => "slow_window",
            Self::Period => "period",
            Self::ThresholdBps => "threshold_bps",
        }
    }
}

/// MACD 的三档窗口：内核常数，不随 `builtin_*` 四项变动（清单里它是唯一没有任何旋钮的 kind）。
pub const MACD_WINDOWS: (usize, usize, usize) = (12, 26, 9);
/// MACD 发出第一个信号所需的最少可见 Bar 数：26 根慢线 + 9 个 MACD 值 + 1 根判定交叉。
pub const MACD_REQUIRED_BARS: usize = 35;
/// 与任何旋钮无关的最低取样：EMA 类指标留着更长的前缀才不会因历史裁剪而漂移。
pub const HISTORY_FLOOR_BARS: usize = 30;

impl BuiltinStrategyKind {
    /// 这个 kind 的信号读哪几项旋钮，逐项对应 `builtin.rs` 的 `signal()` / `spread_signal()` /
    /// `basis_signal()` 分支。改那些分支时必须同时改这里：清单外的一项若仍能改动结果，
    /// `knobs_outside_the_list_cannot_change_a_single_run` 会红。
    pub const fn signal_knobs(self) -> &'static [BuiltinSignalKnob] {
        use BuiltinSignalKnob::{FastWindow, Period, SlowWindow, ThresholdBps};
        match self {
            Self::SmaCross | Self::EmaCross => &[FastWindow, SlowWindow],
            // 12/26/9 是内核常数（`MACD_WINDOWS`），四项没有一项进它的信号。
            Self::Macd => &[],
            Self::Rsi
            | Self::Bollinger
            | Self::DonchianBreakout
            | Self::AtrTrend
            | Self::KeltnerTrend
            | Self::VolatilityBreakout => &[Period],
            Self::Momentum
            | Self::MeanReversion
            | Self::VwapReversion
            | Self::PairsArbitrage
            | Self::CrossVenueArbitrage => &[Period, ThresholdBps],
            // 网格的基准是历史窗口里最早那一根 Bar，只有阈值可选。
            Self::Grid => &[ThresholdBps],
            // 基差只看两条腿的当前价：等腿报到齐（`required_bars` 为 1）而不看周期。
            Self::BasisArbitrage | Self::SpotFuturesArbitrage => &[ThresholdBps],
        }
    }

    /// 该 kind 用不用这一项旋钮。
    pub fn uses_signal_knob(self, knob: BuiltinSignalKnob) -> bool {
        self.signal_knobs().contains(&knob)
    }

    /// 生效清单的逗号分隔写法，供播报、摘要与 `builtin-strategies` 列表共用。
    /// 一个旋钮也没有时写 `none`：MACD 那类内核常数策略必须显式说"这四项都不上场"。
    pub fn signal_knob_list(self) -> String {
        let names = self
            .signal_knobs()
            .iter()
            .map(|knob| knob.field())
            .collect::<Vec<_>>();
        if names.is_empty() {
            return "none".to_string();
        }
        names.join(",")
    }
}
