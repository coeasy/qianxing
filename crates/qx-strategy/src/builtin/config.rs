use crate::builtin_signal::BuiltinSignalKnob;
use qx_core::{InstrumentId, OrderPolicy, Quantity, TradingInstrumentSpec, TradingProduct};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuiltinStrategyKind {
    SmaCross,
    EmaCross,
    Macd,
    Rsi,
    Bollinger,
    DonchianBreakout,
    Momentum,
    MeanReversion,
    Grid,
    AtrTrend,
    KeltnerTrend,
    VwapReversion,
    VolatilityBreakout,
    PairsArbitrage,
    BasisArbitrage,
    CrossVenueArbitrage,
    SpotFuturesArbitrage,
}

impl BuiltinStrategyKind {
    pub const ALL: [Self; 17] = [
        Self::SmaCross,
        Self::EmaCross,
        Self::Macd,
        Self::Rsi,
        Self::Bollinger,
        Self::DonchianBreakout,
        Self::Momentum,
        Self::MeanReversion,
        Self::Grid,
        Self::AtrTrend,
        Self::KeltnerTrend,
        Self::VwapReversion,
        Self::VolatilityBreakout,
        Self::PairsArbitrage,
        Self::BasisArbitrage,
        Self::CrossVenueArbitrage,
        Self::SpotFuturesArbitrage,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::SmaCross => "sma_cross",
            Self::EmaCross => "ema_cross",
            Self::Macd => "macd",
            Self::Rsi => "rsi",
            Self::Bollinger => "bollinger",
            Self::DonchianBreakout => "donchian_breakout",
            Self::Momentum => "momentum",
            Self::MeanReversion => "mean_reversion",
            Self::Grid => "grid",
            Self::AtrTrend => "atr_trend",
            Self::KeltnerTrend => "keltner_trend",
            Self::VwapReversion => "vwap_reversion",
            Self::VolatilityBreakout => "volatility_breakout",
            Self::PairsArbitrage => "pairs_arbitrage",
            Self::BasisArbitrage => "basis_arbitrage",
            Self::CrossVenueArbitrage => "cross_venue_arbitrage",
            Self::SpotFuturesArbitrage => "spot_futures_arbitrage",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::SmaCross => "简单移动平均线交叉趋势",
            Self::EmaCross => "指数移动平均线交叉趋势",
            Self::Macd => "MACD 趋势与动量",
            Self::Rsi => "RSI 超买超卖",
            Self::Bollinger => "布林带均值回归",
            Self::DonchianBreakout => "Donchian 通道突破",
            Self::Momentum => "区间动量",
            Self::MeanReversion => "均值偏离回归",
            Self::Grid => "固定基准网格信号",
            Self::AtrTrend => "ATR 波动突破趋势",
            Self::KeltnerTrend => "Keltner 通道趋势",
            Self::VwapReversion => "成交量加权均价回归",
            Self::VolatilityBreakout => "波动率突破趋势",
            Self::PairsArbitrage => "双腿配对价差套利",
            Self::BasisArbitrage => "现货/合约基差套利",
            Self::CrossVenueArbitrage => "跨交易所价差套利",
            Self::SpotFuturesArbitrage => "现货/期货基差套利",
        }
    }

    /// 双腿套利口径：这四个 kind 需要第二条 BarFrame 与 `reference_instrument`，
    /// 只有 `backtest multi-builtin` 喂得动，单标的入口（含 `init --strategy`）必须拒绝。
    pub const fn needs_reference_leg(self) -> bool {
        matches!(
            self,
            Self::PairsArbitrage
                | Self::BasisArbitrage
                | Self::CrossVenueArbitrage
                | Self::SpotFuturesArbitrage
        )
    }

    pub fn parse(value: &str) -> Result<Self, String> {
        let normalized = value.trim().to_ascii_lowercase().replace('-', "_");
        Self::ALL
            .into_iter()
            .find(|kind| kind.name() == normalized)
            .ok_or_else(|| {
                format!(
                    "未知内置策略 {value}，可选值: {}",
                    Self::ALL
                        .iter()
                        .map(|kind| kind.name())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct BuiltinStrategyConfig {
    pub kind: BuiltinStrategyKind,
    pub strategy_id: String,
    pub strategy_version: String,
    pub instrument: InstrumentId,
    pub quantity: Quantity,
    /// Optional fixed quote-currency notional target. When present, the kernel
    /// converts it to a quantity at the visible close; order/risk rules still
    /// validate the resulting quantity before execution.
    pub target_notional_raw: Option<i128>,
    /// Frozen leg specs required to convert fixed quote notional into contract
    /// quantities for two-leg derivative strategies.
    pub primary_market_spec: Option<TradingInstrumentSpec>,
    pub reference_market_spec: Option<TradingInstrumentSpec>,
    /// Optional fraction of the current available margin to target (basis points).
    /// The runtime must provide a fresh, as-of buying-power snapshot.
    pub target_available_margin_bps: Option<u32>,
    /// Optional per-trade volatility budget as a fraction of current available
    /// margin. Volatility is the rolling ATR/visible-close ratio in basis points.
    pub target_volatility_bps: Option<u32>,
    /// Fraction of the estimated full-Kelly fraction to deploy (basis points).
    pub target_kelly_fraction_bps: Option<u32>,
    /// Prior close-to-close return observations used by Kelly sizing (30..=1000).
    pub kelly_lookback_bars: Option<usize>,
    pub fast_window: usize,
    pub slow_window: usize,
    pub period: usize,
    /// 动量/均值回归/网格使用的阈值，单位为基点。
    pub threshold_bps: i128,
    pub reference_instrument: Option<InstrumentId>,
    pub primary_policy: Option<OrderPolicy>,
    pub reference_policy: Option<OrderPolicy>,
}

impl BuiltinStrategyConfig {
    pub fn new(
        kind: BuiltinStrategyKind,
        strategy_id: impl Into<String>,
        instrument: InstrumentId,
        quantity: Quantity,
    ) -> Result<Self, String> {
        Self::new_with_reference(kind, strategy_id, instrument, quantity, None)
    }

    /// Construct the canonical configuration for a strategy that requires a reference leg.
    pub fn new_multi_leg(
        kind: BuiltinStrategyKind,
        strategy_id: impl Into<String>,
        instrument: InstrumentId,
        reference_instrument: InstrumentId,
        quantity: Quantity,
    ) -> Result<Self, String> {
        if !kind.needs_reference_leg() {
            return Err(format!("策略 {} 不支持 reference_instrument", kind.name()));
        }
        Self::new_with_reference(
            kind,
            strategy_id,
            instrument,
            quantity,
            Some(reference_instrument),
        )
    }

    fn new_with_reference(
        kind: BuiltinStrategyKind,
        strategy_id: impl Into<String>,
        instrument: InstrumentId,
        quantity: Quantity,
        reference_instrument: Option<InstrumentId>,
    ) -> Result<Self, String> {
        let config = Self {
            kind,
            strategy_id: strategy_id.into(),
            strategy_version: format!("builtin-{}-v1", kind.name()),
            instrument,
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
            reference_instrument,
            primary_policy: None,
            reference_policy: None,
        };
        config.validate()?;
        Ok(config)
    }

    /// 参数体检只看这个 kind 真正读到的旋钮（清单在 `builtin_signal.rs`）。
    ///
    /// 一份运行时配置会同时喂给多种 kind（`backtest builtin <kind>` 的 kind 来自命令行），
    /// 于是"不上场的窗口合不合法"不该决定这一轮成不成：MACD 的 `fast_window>=slow_window`
    /// 既不改成交、也不该把整轮拒掉。清单外的一项因此两头都不影响结果，播报里那句
    /// "这一项没上场"才是真话。
    pub fn validate(&self) -> Result<(), String> {
        let uses = |knob| self.kind.uses_signal_knob(knob);
        if self.strategy_id.trim().is_empty()
            || self.strategy_version.trim().is_empty()
            || self.quantity.raw() <= 0
        {
            return Err("内置策略参数非法：策略身份或数量不满足约束".into());
        }
        if self
            .target_notional_raw
            .is_some_and(|notional| notional <= 0)
        {
            return Err("内置策略参数非法：target_notional_raw 必须为正".into());
        }
        if self
            .target_available_margin_bps
            .is_some_and(|bps| bps == 0 || bps > 10_000)
        {
            return Err("内置策略参数非法：target_available_margin_bps 必须在 1..=10000".into());
        }
        if self
            .target_volatility_bps
            .is_some_and(|bps| bps == 0 || bps > 10_000)
        {
            return Err("内置策略参数非法：target_volatility_bps 必须在 1..=10000".into());
        }
        if self
            .target_kelly_fraction_bps
            .is_some_and(|bps| bps == 0 || bps > 10_000)
        {
            return Err("内置策略参数非法：target_kelly_fraction_bps 必须在 1..=10000".into());
        }
        match (self.target_kelly_fraction_bps, self.kelly_lookback_bars) {
            (Some(_), Some(lookback)) if (30..=1_000).contains(&lookback) => {}
            (Some(_), None) => {}
            (Some(_), Some(_)) => {
                return Err("Kelly sizing 必须配置 30..=1000 根 kelly_lookback_bars".into());
            }
            (None, Some(_)) => {
                return Err("kelly_lookback_bars 只能与 Kelly sizing 一起配置".into());
            }
            (None, None) => {}
        }
        let sizing_modes = [
            self.target_notional_raw.is_some(),
            self.target_available_margin_bps.is_some(),
            self.target_volatility_bps.is_some(),
            self.target_kelly_fraction_bps.is_some(),
        ];
        if sizing_modes.into_iter().filter(|enabled| *enabled).count() > 1 {
            return Err(
                "内置策略参数非法：固定名义金额、可用资金比例、波动率预算与 Kelly sizing 互斥"
                    .into(),
            );
        }
        if self.kind.needs_reference_leg()
            && (self.target_available_margin_bps.is_some()
                || self.target_volatility_bps.is_some()
                || self.target_kelly_fraction_bps.is_some())
        {
            return Err("双腿动态 sizing 需要逐腿实时可用资金账本，当前不支持".into());
        }
        if self.kind.needs_reference_leg() && self.target_notional_raw.is_some() {
            let Some(reference_instrument) = self.reference_instrument.as_ref() else {
                return Err("双腿套利必须配置 reference_instrument".into());
            };
            let (Some(primary), Some(reference)) = (
                self.primary_market_spec.as_ref(),
                self.reference_market_spec.as_ref(),
            ) else {
                return Err("双腿固定名义金额 sizing 必须配置两腿 market spec".into());
            };
            for (label, market, instrument) in [
                ("primary", primary, &self.instrument),
                ("reference", reference, reference_instrument),
            ] {
                market
                    .validate()
                    .map_err(|error| format!("{label} market spec 非法: {error}"))?;
                if market.instrument != *instrument {
                    return Err(format!("{label} market spec instrument 不匹配"));
                }
                if !matches!(
                    market.product,
                    TradingProduct::Perpetual | TradingProduct::Future
                ) {
                    return Err("双腿固定名义金额 sizing 当前仅支持永续/交割合约".into());
                }
            }
            if primary.settlement_currency != reference.settlement_currency {
                return Err("双腿固定名义金额 sizing 要求两腿结算币种一致".into());
            }
            if primary.quote_currency != reference.quote_currency {
                return Err("双腿固定名义金额 sizing 要求两腿报价币种一致".into());
            }
            for market in [primary, reference] {
                let expected_settlement = if market.inverse {
                    &market.base_currency
                } else {
                    &market.quote_currency
                };
                if &market.settlement_currency != expected_settlement {
                    return Err(
                        "双腿固定名义金额 sizing 要求线性合约以报价币结算、反向合约以基础币结算"
                            .into(),
                    );
                }
            }
        }
        if !self.kind.needs_reference_leg()
            && (self.primary_market_spec.is_some() || self.reference_market_spec.is_some())
        {
            return Err("单腿策略不能配置双腿 market spec".into());
        }
        if (uses(BuiltinSignalKnob::FastWindow) || uses(BuiltinSignalKnob::SlowWindow))
            && (self.fast_window == 0
                || self.slow_window == 0
                || self.fast_window >= self.slow_window)
        {
            return Err("内置策略参数非法：快慢窗口必须为正且快线短于慢线".into());
        }
        if uses(BuiltinSignalKnob::Period) && self.period < 2 {
            return Err("内置策略参数非法：指标周期至少为 2".into());
        }
        if uses(BuiltinSignalKnob::ThresholdBps) && self.threshold_bps < 0 {
            return Err("内置策略参数非法：阈值不得为负".into());
        }
        let needs_reference = self.kind.needs_reference_leg();
        if needs_reference && self.reference_instrument.is_none() {
            return Err("双腿套利必须配置 reference_instrument".into());
        }
        if self
            .reference_instrument
            .as_ref()
            .is_some_and(|instrument| instrument == &self.instrument)
        {
            return Err("reference_instrument 不能与主腿 instrument 相同".into());
        }
        Ok(())
    }
}
