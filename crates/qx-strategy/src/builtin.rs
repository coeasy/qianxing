//! 可直接装配的内置 Bar 策略。
//!
//! 这些策略只负责从可见 Bar 生成统一 `StrategyDecision`，不接触交易所、
//! EventLog 或账户凭据。回测、Paper 和实盘都必须继续经过 Runtime 的
//! Portfolio、RiskGate、OMS 和 Execution 边界。

use crate::{
    MarketEvent, Strategy, StrategyContext, StrategyDecision, StrategyOrderIntent,
    STRATEGY_API_VERSION,
};
use qx_core::{InstrumentId, OrderPolicy, Quantity, Side};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

const BPS_SCALE: i128 = 10_000;

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
        let config = Self {
            kind,
            strategy_id: strategy_id.into(),
            strategy_version: format!("builtin-{}-v1", kind.name()),
            instrument,
            quantity,
            fast_window: 5,
            slow_window: 20,
            period: 14,
            threshold_bps: 100,
            reference_instrument: None,
            primary_policy: None,
            reference_policy: None,
        };
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.strategy_id.trim().is_empty()
            || self.strategy_version.trim().is_empty()
            || self.quantity.raw() <= 0
            || self.fast_window == 0
            || self.slow_window == 0
            || self.fast_window >= self.slow_window
            || self.period < 2
            || self.threshold_bps < 0
        {
            return Err("内置策略参数非法：策略身份、数量、窗口或阈值不满足约束".into());
        }
        let needs_reference = matches!(
            self.kind,
            BuiltinStrategyKind::PairsArbitrage
                | BuiltinStrategyKind::BasisArbitrage
                | BuiltinStrategyKind::CrossVenueArbitrage
                | BuiltinStrategyKind::SpotFuturesArbitrage
        );
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

#[derive(Clone, Copy, Debug)]
struct BarPoint {
    high: i128,
    low: i128,
    close: i128,
    volume: i128,
}

pub struct BuiltinStrategy {
    pub config: BuiltinStrategyConfig,
    bars: VecDeque<BarPoint>,
    reference_bars: VecDeque<BarPoint>,
    next_signal_id: u64,
    next_intent_id: u64,
}

impl BuiltinStrategy {
    pub fn new(config: BuiltinStrategyConfig) -> Result<Self, String> {
        config.validate()?;
        Ok(Self {
            config,
            bars: VecDeque::new(),
            reference_bars: VecDeque::new(),
            next_signal_id: 1,
            next_intent_id: 1,
        })
    }

    fn max_history(&self) -> usize {
        self.config.slow_window.max(self.config.period).max(30) + 2
    }

    fn closes(&self) -> Vec<i128> {
        self.bars.iter().map(|bar| bar.close).collect()
    }

    fn spread_signal(&self) -> Result<Option<i8>, String> {
        if self.reference_bars.len() < self.config.period + 1 {
            return Ok(None);
        }
        let primary_base = self
            .bars
            .iter()
            .rev()
            .nth(self.config.period)
            .map(|bar| bar.close)
            .ok_or_else(|| "主腿价差历史不足".to_string())?;
        let primary_now = self
            .bars
            .back()
            .map(|bar| bar.close)
            .ok_or_else(|| "主腿价差当前值缺失".to_string())?;
        let reference_base = self
            .reference_bars
            .iter()
            .rev()
            .nth(self.config.period)
            .map(|bar| bar.close)
            .ok_or_else(|| "对冲腿价差历史不足".to_string())?;
        let reference_now = self
            .reference_bars
            .back()
            .map(|bar| bar.close)
            .ok_or_else(|| "对冲腿价差当前值缺失".to_string())?;
        if primary_base <= 0 || reference_base <= 0 {
            return Err("套利腿基准价格必须为正".into());
        }
        let primary_return = primary_now
            .checked_sub(primary_base)
            .and_then(|value| value.checked_mul(BPS_SCALE))
            .and_then(|value| value.checked_div(primary_base))
            .ok_or("主腿价差收益计算溢出")?;
        let reference_return = reference_now
            .checked_sub(reference_base)
            .and_then(|value| value.checked_mul(BPS_SCALE))
            .and_then(|value| value.checked_div(reference_base))
            .ok_or("对冲腿价差收益计算溢出")?;
        let spread = primary_return
            .checked_sub(reference_return)
            .ok_or("双腿价差计算溢出")?;
        if spread >= self.config.threshold_bps {
            Ok(Some(-1))
        } else if spread <= -self.config.threshold_bps {
            Ok(Some(1))
        } else {
            Ok(Some(0))
        }
    }

    fn basis_signal(&self) -> Result<Option<i8>, String> {
        let reference_now = self
            .reference_bars
            .back()
            .map(|bar| bar.close)
            .ok_or_else(|| "基差对冲腿当前值缺失".to_string())?;
        let primary_now = self
            .bars
            .back()
            .map(|bar| bar.close)
            .ok_or_else(|| "基差主腿当前值缺失".to_string())?;
        if reference_now <= 0 || primary_now <= 0 {
            return Err("基差套利腿价格必须为正".into());
        }
        let basis_bps = primary_now
            .checked_sub(reference_now)
            .and_then(|value| value.checked_mul(BPS_SCALE))
            .and_then(|value| value.checked_div(reference_now))
            .ok_or("基差计算溢出")?;
        let exit_threshold = self.config.threshold_bps / 2;
        if basis_bps >= self.config.threshold_bps {
            Ok(Some(-1))
        } else if basis_bps <= -self.config.threshold_bps {
            Ok(Some(1))
        } else if basis_bps.abs() <= exit_threshold {
            Ok(Some(0))
        } else {
            Ok(None)
        }
    }

    fn signal(&self) -> Result<Option<i8>, String> {
        let closes = self.closes();
        let required = match self.config.kind {
            BuiltinStrategyKind::SmaCross | BuiltinStrategyKind::EmaCross => {
                self.config.slow_window + 1
            }
            // 26 根用于慢 EMA，至少还要 9 个 MACD 值计算 signal EMA，
            // 当前/上一根交叉判定再额外需要一根可见 Bar。
            BuiltinStrategyKind::Macd => 35,
            BuiltinStrategyKind::Rsi
            | BuiltinStrategyKind::Bollinger
            | BuiltinStrategyKind::MeanReversion
            | BuiltinStrategyKind::AtrTrend => self.config.period + 1,
            BuiltinStrategyKind::DonchianBreakout | BuiltinStrategyKind::Momentum => {
                self.config.period + 1
            }
            BuiltinStrategyKind::Grid => 1,
            BuiltinStrategyKind::KeltnerTrend
            | BuiltinStrategyKind::VwapReversion
            | BuiltinStrategyKind::VolatilityBreakout
            | BuiltinStrategyKind::PairsArbitrage
            | BuiltinStrategyKind::BasisArbitrage
            | BuiltinStrategyKind::CrossVenueArbitrage
            | BuiltinStrategyKind::SpotFuturesArbitrage => self.config.period + 1,
        };
        if closes.len() < required {
            return Ok(None);
        }
        let signal = match self.config.kind {
            BuiltinStrategyKind::SmaCross => {
                let fast_now = sma(&closes, self.config.fast_window).unwrap();
                let slow_now = sma(&closes, self.config.slow_window).unwrap();
                let previous = &closes[..closes.len() - 1];
                let fast_prev = sma(previous, self.config.fast_window).unwrap();
                let slow_prev = sma(previous, self.config.slow_window).unwrap();
                if fast_prev <= slow_prev && fast_now > slow_now {
                    Some(1)
                } else if fast_prev >= slow_prev && fast_now < slow_now {
                    Some(-1)
                } else {
                    None
                }
            }
            BuiltinStrategyKind::EmaCross => {
                let fast_now = ema(&closes, self.config.fast_window).unwrap();
                let slow_now = ema(&closes, self.config.slow_window).unwrap();
                let previous = &closes[..closes.len() - 1];
                let fast_prev = ema(previous, self.config.fast_window).unwrap();
                let slow_prev = ema(previous, self.config.slow_window).unwrap();
                if fast_prev <= slow_prev && fast_now > slow_now {
                    Some(1)
                } else if fast_prev >= slow_prev && fast_now < slow_now {
                    Some(-1)
                } else {
                    None
                }
            }
            BuiltinStrategyKind::Macd => {
                let macd_now = ema(&closes, 12).unwrap() - ema(&closes, 26).unwrap();
                let previous = &closes[..closes.len() - 1];
                let macd_prev = ema(previous, 12).unwrap() - ema(previous, 26).unwrap();
                let signal_now = macd_series(&closes, 12, 26, 9)
                    .and_then(|series| ema(&series, 9))
                    .unwrap();
                let signal_prev = macd_series(previous, 12, 26, 9)
                    .and_then(|series| ema(&series, 9))
                    .unwrap();
                if macd_prev <= signal_prev && macd_now > signal_now {
                    Some(1)
                } else if macd_prev >= signal_prev && macd_now < signal_now {
                    Some(-1)
                } else {
                    None
                }
            }
            BuiltinStrategyKind::Rsi => {
                let value = rsi(&closes, self.config.period).unwrap();
                if value <= 3_000 {
                    Some(1)
                } else if value >= 7_000 {
                    Some(-1)
                } else {
                    Some(0)
                }
            }
            BuiltinStrategyKind::Bollinger => {
                let window = &closes[closes.len() - self.config.period..];
                let mean = sma(window, self.config.period).unwrap();
                let deviation = stddev(window, mean)?;
                let distance = deviation.checked_mul(2).ok_or("Bollinger 计算溢出")?;
                let lower = mean.checked_sub(distance).ok_or("Bollinger 下轨溢出")?;
                let upper = mean.checked_add(distance).ok_or("Bollinger 上轨溢出")?;
                if *window.last().unwrap() < lower {
                    Some(1)
                } else if *window.last().unwrap() > upper {
                    Some(-1)
                } else {
                    Some(0)
                }
            }
            BuiltinStrategyKind::DonchianBreakout => {
                let current = *closes.last().unwrap();
                let previous = &closes[closes.len() - self.config.period - 1..closes.len() - 1];
                if current > *previous.iter().max().unwrap() {
                    Some(1)
                } else if current < *previous.iter().min().unwrap() {
                    Some(-1)
                } else {
                    None
                }
            }
            BuiltinStrategyKind::Momentum => {
                let current = *closes.last().unwrap();
                let base = closes[closes.len() - self.config.period - 1];
                if base <= 0 {
                    return Err("Momentum 基准价格必须为正".into());
                }
                let change = current
                    .checked_sub(base)
                    .and_then(|value| value.checked_mul(BPS_SCALE))
                    .and_then(|value| value.checked_div(base))
                    .ok_or("Momentum 计算溢出")?;
                if change >= self.config.threshold_bps {
                    Some(1)
                } else if change <= -self.config.threshold_bps {
                    Some(-1)
                } else {
                    Some(0)
                }
            }
            BuiltinStrategyKind::MeanReversion => {
                let window = &closes[closes.len() - self.config.period..];
                let mean = sma(window, self.config.period).unwrap();
                if mean <= 0 {
                    return Err("MeanReversion 均值必须为正".into());
                }
                let deviation = (*window.last().unwrap() - mean)
                    .checked_mul(BPS_SCALE)
                    .and_then(|value| value.checked_div(mean))
                    .ok_or("MeanReversion 计算溢出")?;
                if deviation <= -self.config.threshold_bps {
                    Some(1)
                } else if deviation >= self.config.threshold_bps {
                    Some(-1)
                } else {
                    Some(0)
                }
            }
            BuiltinStrategyKind::Grid => {
                let anchor = self.bars.front().unwrap().close;
                let current = *closes.last().unwrap();
                if anchor <= 0 {
                    return Err("Grid 基准价格必须为正".into());
                }
                let deviation = (current - anchor)
                    .checked_mul(BPS_SCALE)
                    .and_then(|value| value.checked_div(anchor))
                    .ok_or("Grid 计算溢出")?;
                if deviation <= -self.config.threshold_bps {
                    Some(1)
                } else if deviation >= self.config.threshold_bps {
                    Some(-1)
                } else {
                    Some(0)
                }
            }
            BuiltinStrategyKind::AtrTrend => {
                let current = self.bars.back().unwrap();
                let previous = self.bars.iter().rev().nth(1).unwrap();
                let atr = atr(&self.bars, self.config.period).unwrap();
                if current.close > previous.close.saturating_add(atr) {
                    Some(1)
                } else if current.close < previous.close.saturating_sub(atr) {
                    Some(-1)
                } else {
                    None
                }
            }
            BuiltinStrategyKind::KeltnerTrend => {
                let middle = ema(&closes, self.config.period).unwrap();
                let average_range = self
                    .bars
                    .iter()
                    .rev()
                    .take(self.config.period)
                    .try_fold(0_i128, |sum, bar| {
                        sum.checked_add(bar.high.checked_sub(bar.low)?)
                    })
                    .and_then(|value| value.checked_div(self.config.period as i128))
                    .ok_or("Keltner 波动计算溢出")?;
                let width = average_range.checked_mul(2).ok_or("Keltner 通道溢出")?;
                let current = *closes.last().unwrap();
                if current > middle.saturating_add(width) {
                    Some(1)
                } else if current < middle.saturating_sub(width) {
                    Some(-1)
                } else {
                    None
                }
            }
            BuiltinStrategyKind::VwapReversion => {
                let values = self
                    .bars
                    .iter()
                    .rev()
                    .take(self.config.period)
                    .collect::<Vec<_>>();
                let (weighted, volume) =
                    values
                        .into_iter()
                        .try_fold((0_i128, 0_i128), |(weighted, volume), bar| {
                            Ok::<_, String>((
                                weighted
                                    .checked_add(
                                        bar.close
                                            .checked_mul(bar.volume)
                                            .ok_or("VWAP 加权价格溢出")?,
                                    )
                                    .ok_or("VWAP 加权和溢出")?,
                                volume.checked_add(bar.volume).ok_or("VWAP 成交量溢出")?,
                            ))
                        })?;
                let vwap = if volume == 0 {
                    return Err("VWAP 成交量不能为零".into());
                } else {
                    weighted.checked_div(volume).ok_or("VWAP 计算溢出")?
                };
                let current = *closes.last().unwrap();
                let deviation = current
                    .checked_sub(vwap)
                    .and_then(|value| value.checked_mul(BPS_SCALE))
                    .and_then(|value| value.checked_div(vwap.max(1)))
                    .ok_or("VWAP 偏离计算溢出")?;
                if deviation <= -self.config.threshold_bps {
                    Some(1)
                } else if deviation >= self.config.threshold_bps {
                    Some(-1)
                } else {
                    Some(0)
                }
            }
            BuiltinStrategyKind::VolatilityBreakout => {
                let current = self.bars.back().unwrap();
                let range = current
                    .high
                    .checked_sub(current.low)
                    .ok_or("波动率范围非法")?;
                let average = self
                    .bars
                    .iter()
                    .rev()
                    .skip(1)
                    .take(self.config.period)
                    .try_fold(0_i128, |sum, bar| {
                        sum.checked_add(bar.high.checked_sub(bar.low)?)
                    })
                    .and_then(|value| value.checked_div(self.config.period as i128))
                    .ok_or("波动率均值计算溢出")?;
                if range > average.saturating_mul(2) && current.close >= current.high {
                    Some(1)
                } else if range > average.saturating_mul(2) && current.close <= current.low {
                    Some(-1)
                } else {
                    None
                }
            }
            BuiltinStrategyKind::PairsArbitrage | BuiltinStrategyKind::CrossVenueArbitrage => {
                return self.spread_signal();
            }
            BuiltinStrategyKind::BasisArbitrage | BuiltinStrategyKind::SpotFuturesArbitrage => {
                return self.basis_signal();
            }
        };
        Ok(signal)
    }

    fn empty_decision(&self, context: &StrategyContext, ts: u64) -> StrategyDecision {
        StrategyDecision {
            schema_version: STRATEGY_API_VERSION,
            request_id: format!("{}:{ts}", self.config.strategy_id),
            strategy_id: context.strategy_id.clone(),
            signal_id: self.next_signal_id,
            confidence: 0,
            priority: 0,
            expires_at: ts,
            intents: Vec::new(),
        }
    }
}

impl Strategy for BuiltinStrategy {
    fn on_event(
        &mut self,
        context: &StrategyContext,
        event: &MarketEvent,
    ) -> Result<StrategyDecision, String> {
        context.validate()?;
        event.validate()?;
        let MarketEvent::Bar {
            instrument,
            ts,
            high_raw,
            low_raw,
            close_raw,
            volume_raw,
            ..
        } = event
        else {
            return Ok(self.empty_decision(context, event.ts()));
        };
        let is_pair = matches!(
            self.config.kind,
            BuiltinStrategyKind::PairsArbitrage
                | BuiltinStrategyKind::BasisArbitrage
                | BuiltinStrategyKind::CrossVenueArbitrage
                | BuiltinStrategyKind::SpotFuturesArbitrage
        );
        let is_primary = instrument == &self.config.instrument;
        let is_reference = self
            .config
            .reference_instrument
            .as_ref()
            .is_some_and(|reference| reference == instrument);
        if !is_primary && !(is_pair && is_reference) {
            return Err(format!(
                "内置策略 instrument 不匹配: expected={} actual={}",
                self.config.instrument, instrument
            ));
        }
        self.next_signal_id = self.next_signal_id.saturating_add(1);
        let point = BarPoint {
            high: *high_raw,
            low: *low_raw,
            close: *close_raw,
            volume: *volume_raw,
        };
        if is_reference {
            self.reference_bars.push_back(point);
            while self.reference_bars.len() > self.max_history() {
                self.reference_bars.pop_front();
            }
            if !is_primary {
                return Ok(self.empty_decision(context, *ts));
            }
        } else {
            self.bars.push_back(point);
            while self.bars.len() > self.max_history() {
                self.bars.pop_front();
            }
        }
        let Some(signal) = self.signal()? else {
            return Ok(self.empty_decision(context, *ts));
        };
        let mut legs = vec![(self.config.instrument.clone(), i128::from(signal))];
        if is_pair {
            legs.push((
                self.config
                    .reference_instrument
                    .clone()
                    .ok_or("双腿套利缺少 reference_instrument")?,
                i128::from(-signal),
            ));
        }
        let mut intents = Vec::with_capacity(legs.len());
        let mut confidence = 0_i128;
        for (leg_instrument, leg_signal) in legs {
            let current = context
                .positions
                .get(&leg_instrument.to_string())
                .copied()
                .unwrap_or(0);
            let target = self
                .config
                .quantity
                .raw()
                .checked_mul(leg_signal)
                .ok_or("内置策略目标仓位溢出")?;
            let delta = target.checked_sub(current).ok_or("内置策略订单数量溢出")?;
            let Some(qty_raw) = delta.checked_abs() else {
                return Err("内置策略订单数量溢出".into());
            };
            if qty_raw == 0 {
                continue;
            }
            confidence = confidence.saturating_add(qty_raw);
            let side = if delta > 0 { Side::Buy } else { Side::Sell };
            let is_reference_leg = self
                .config
                .reference_instrument
                .as_ref()
                .is_some_and(|reference| reference == &leg_instrument);
            intents.push(StrategyOrderIntent {
                intent_id: self.next_intent_id,
                instrument: leg_instrument,
                side,
                qty: Quantity::from_raw(qty_raw),
                limit: None,
                policy: if is_reference_leg {
                    self.config.reference_policy
                } else {
                    self.config.primary_policy
                },
                reduce_only: signal == 0,
                post_only: false,
            });
            self.next_intent_id = self.next_intent_id.saturating_add(1);
        }
        if intents.is_empty() {
            return Ok(self.empty_decision(context, *ts));
        }
        let decision = StrategyDecision {
            schema_version: STRATEGY_API_VERSION,
            request_id: format!("{}:{ts}", self.config.strategy_id),
            strategy_id: context.strategy_id.clone(),
            signal_id: self.next_signal_id,
            confidence,
            priority: 0,
            expires_at: *ts,
            intents,
        };
        decision.validate_for(context, *ts)?;
        Ok(decision)
    }
}

fn sma(values: &[i128], window: usize) -> Option<i128> {
    if window == 0 || values.len() < window {
        return None;
    }
    values[values.len() - window..]
        .iter()
        .try_fold(0_i128, |sum, value| sum.checked_add(*value))
        .and_then(|sum| sum.checked_div(window as i128))
}

fn ema(values: &[i128], window: usize) -> Option<i128> {
    if window == 0 || values.len() < window {
        return None;
    }
    let mut result = values[..window]
        .iter()
        .try_fold(0_i128, |sum, value| sum.checked_add(*value))?
        .checked_div(window as i128)?;
    let denominator = window as i128 + 1;
    for value in &values[window..] {
        result = value
            .checked_mul(2)
            .and_then(|next| {
                result
                    .checked_mul(window as i128)
                    .and_then(|old| next.checked_add(old))
            })
            .and_then(|next| next.checked_div(denominator))?;
    }
    Some(result)
}

fn macd_series(values: &[i128], fast: usize, slow: usize, _signal: usize) -> Option<Vec<i128>> {
    if values.len() < slow {
        return None;
    }
    let mut result = Vec::with_capacity(values.len() - slow + 1);
    for end in slow..=values.len() {
        let slice = &values[..end];
        result.push(ema(slice, fast)?.checked_sub(ema(slice, slow)?)?);
    }
    Some(result)
}

fn rsi(values: &[i128], period: usize) -> Option<i128> {
    if period == 0 || values.len() < period + 1 {
        return None;
    }
    let mut gain = 0_i128;
    let mut loss = 0_i128;
    for pair in values[values.len() - period - 1..].windows(2) {
        let difference = pair[1].checked_sub(pair[0])?;
        if difference >= 0 {
            gain = gain.checked_add(difference)?;
        } else {
            loss = loss.checked_add(difference.checked_abs()?)?;
        }
    }
    if loss == 0 {
        return Some(BPS_SCALE);
    }
    gain.checked_mul(BPS_SCALE)?
        .checked_div(gain.checked_add(loss)?)
}

fn stddev(values: &[i128], mean: i128) -> Result<i128, String> {
    if values.is_empty() {
        return Err("stddev 样本不能为空".into());
    }
    let variance = values
        .iter()
        .map(|value| {
            value
                .checked_sub(mean)
                .and_then(|difference| difference.checked_mul(difference))
        })
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| "stddev 计算溢出".to_string())?
        .into_iter()
        .try_fold(0_i128, |sum, value| sum.checked_add(value))
        .and_then(|sum| sum.checked_div(values.len() as i128))
        .ok_or_else(|| "stddev 方差计算溢出".to_string())?;
    Ok(integer_sqrt(variance))
}

fn integer_sqrt(value: i128) -> i128 {
    if value <= 0 {
        return 0;
    }
    let mut low = 1_i128;
    let mut high = value.min(i128::from(u64::MAX));
    while low <= high {
        let middle = low + (high - low) / 2;
        if middle <= value / middle {
            low = middle + 1;
        } else {
            high = middle - 1;
        }
    }
    high
}

fn atr(values: &VecDeque<BarPoint>, period: usize) -> Option<i128> {
    if period == 0 || values.len() < period + 1 {
        return None;
    }
    let start = values.len() - period;
    let mut total = 0_i128;
    for index in start..values.len() {
        let current = values[index];
        let previous = values[index - 1];
        let high_low = current.high.checked_sub(current.low)?;
        let high_close = (current.high - previous.close).checked_abs()?;
        let low_close = (current.low - previous.close).checked_abs()?;
        total = total.checked_add(high_low.max(high_close).max(low_close))?;
    }
    total.checked_div(period as i128)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn context(instrument: &InstrumentId) -> StrategyContext {
        StrategyContext {
            strategy_id: "builtin".into(),
            strategy_version: "builtin-v1".into(),
            account_id: "main".into(),
            venue_id: instrument.venue.to_string(),
            data_fingerprint: "bars-v1".into(),
            as_of: 1,
            positions: BTreeMap::new(),
            cash: BTreeMap::new(),
            available_margin_raw: Some(1_000_000),
            risk_state: "ready".into(),
        }
    }

    #[test]
    fn registry_contains_fifteen_builtin_strategies() {
        assert_eq!(BuiltinStrategyKind::ALL.len(), 17);
        assert_eq!(
            BuiltinStrategyKind::parse("EMA-CROSS").unwrap(),
            BuiltinStrategyKind::EmaCross
        );
    }

    #[test]
    fn momentum_emits_a_target_order_after_warmup() {
        let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
        let config = BuiltinStrategyConfig {
            kind: BuiltinStrategyKind::Momentum,
            strategy_id: "momentum".into(),
            strategy_version: "momentum-v1".into(),
            instrument: instrument.clone(),
            quantity: Quantity::from_i64(1),
            fast_window: 2,
            slow_window: 3,
            period: 2,
            threshold_bps: 10,
            reference_instrument: None,
            primary_policy: None,
            reference_policy: None,
        };
        let mut strategy = BuiltinStrategy::new(config).unwrap();
        let mut context = context(&instrument);
        for (ts, close) in [(1, 100), (2, 100), (3, 101)] {
            context.as_of = ts;
            let decision = strategy
                .on_event(
                    &context,
                    &MarketEvent::Bar {
                        instrument: instrument.clone(),
                        ts,
                        open_raw: close,
                        high_raw: close,
                        low_raw: close,
                        close_raw: close,
                        volume_raw: 1,
                    },
                )
                .unwrap();
            if ts == 3 {
                assert_eq!(decision.intents.len(), 1);
                assert_eq!(decision.intents[0].side, Side::Buy);
            }
        }
    }

    #[test]
    fn every_builtin_strategy_survives_a_warmup_and_signal_cycle() {
        let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
        let reference = InstrumentId::parse("ETHUSDT.BINANCE").unwrap();
        for kind in BuiltinStrategyKind::ALL {
            let config = if matches!(
                kind,
                BuiltinStrategyKind::PairsArbitrage
                    | BuiltinStrategyKind::BasisArbitrage
                    | BuiltinStrategyKind::CrossVenueArbitrage
                    | BuiltinStrategyKind::SpotFuturesArbitrage
            ) {
                BuiltinStrategyConfig {
                    kind,
                    strategy_id: format!("builtin-{}", kind.name()),
                    strategy_version: format!("builtin-{}-v1", kind.name()),
                    instrument: instrument.clone(),
                    quantity: Quantity::from_i64(1),
                    fast_window: 5,
                    slow_window: 20,
                    period: 14,
                    threshold_bps: 100,
                    reference_instrument: Some(reference.clone()),
                    primary_policy: None,
                    reference_policy: None,
                }
            } else {
                BuiltinStrategyConfig::new(
                    kind,
                    format!("builtin-{}", kind.name()),
                    instrument.clone(),
                    Quantity::from_i64(1),
                )
                .unwrap()
            };
            let has_reference = config.reference_instrument.is_some();
            let mut strategy = BuiltinStrategy::new(config).unwrap();
            let mut context = context(&instrument);
            for ts in 1..=40 {
                if has_reference {
                    let reference_close = 100 + i128::from(ts);
                    context.as_of = ts;
                    strategy
                        .on_event(
                            &context,
                            &MarketEvent::Bar {
                                instrument: reference.clone(),
                                ts,
                                open_raw: reference_close,
                                high_raw: reference_close + 1,
                                low_raw: reference_close - 1,
                                close_raw: reference_close,
                                volume_raw: 1,
                            },
                        )
                        .unwrap();
                }
                let close = 100 + i128::from((ts % 7) as i64) * 3 + i128::from(ts);
                context.as_of = ts;
                let decision = strategy
                    .on_event(
                        &context,
                        &MarketEvent::Bar {
                            instrument: instrument.clone(),
                            ts,
                            open_raw: close,
                            high_raw: close + 1,
                            low_raw: close - 1,
                            close_raw: close,
                            volume_raw: 1,
                        },
                    )
                    .unwrap();
                assert_eq!(decision.schema_version, STRATEGY_API_VERSION);
            }
        }
    }
}
