//! 可直接装配的内置 Bar 策略。
//!
//! 这些策略只负责从可见 Bar 生成统一 `StrategyDecision`，不接触交易所、
//! EventLog 或账户凭据。回测、Paper 和实盘都必须继续经过 Runtime 的
//! Portfolio、RiskGate、OMS 和 Execution 边界。

mod indicator;
mod sizing;

use crate::builtin_signal::{
    BuiltinSignalKnob, HISTORY_FLOOR_BARS, MACD_REQUIRED_BARS, MACD_WINDOWS,
};
use crate::{
    MarketEvent, Strategy, StrategyContext, StrategyDecision, StrategyOrderIntent,
    STRATEGY_API_VERSION,
};
use indicator::{atr, ema, macd_series, rsi, sma, stddev};
use qx_core::{Quantity, Side};
use std::collections::VecDeque;

const BPS_SCALE: i128 = 10_000;
const DEFAULT_KELLY_LOOKBACK_BARS: usize = 60;

mod config;
pub use config::{BuiltinStrategyConfig, BuiltinStrategyKind};

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
        // 窗口取样上限必须同时容得下信号门槛，否则门槛高的策略会被历史裁剪卡死：
        // 默认 5/20/14 只算出 32 根，而 MACD 需要 35 根，示例帧再长也永远不信号。
        // 上限只按这个 kind 真正读到的窗口开：EMA 类指标的结果取决于留了多少前缀，
        // 让不上场的窗口参与这里，等于给"这项参数改不动这一轮"留一条暗通道。
        let mut floor = HISTORY_FLOOR_BARS;
        for knob in self.config.kind.signal_knobs() {
            let window = match knob {
                BuiltinSignalKnob::SlowWindow => self.config.slow_window,
                BuiltinSignalKnob::Period => self.config.period,
                _ => continue,
            };
            floor = floor.max(window);
        }
        if self.config.target_kelly_fraction_bps.is_some() {
            let lookback = self
                .config
                .kelly_lookback_bars
                .unwrap_or(DEFAULT_KELLY_LOOKBACK_BARS);
            floor = floor.max(lookback + 1);
        }
        self.required_bars().max(floor + 2)
    }

    /// 该策略发出第一个信号所需的最少可见 Bar 数。历史上限与信号门槛共用这一份清单，
    /// 且只由上场的旋钮（或内核常数）决定：等一个不读周期的指标 = 白等。
    fn required_bars(&self) -> usize {
        use BuiltinStrategyKind::*;
        let signal_bars = match self.config.kind {
            SmaCross | EmaCross => self.config.slow_window + 1,
            // 26 根用于慢 EMA，至少还要 9 个 MACD 值计算 signal EMA，
            // 当前/上一根交叉判定再额外需要一根可见 Bar。
            Macd => MACD_REQUIRED_BARS,
            Rsi | Bollinger | DonchianBreakout | Momentum | MeanReversion | AtrTrend
            | KeltnerTrend | VwapReversion | VolatilityBreakout | PairsArbitrage
            | CrossVenueArbitrage => self.config.period + 1,
            Grid => 1,
            // 基差只看两条腿的当前价：腿没报到齐是预热未完（`basis_signal` 返回 `None`），
            // 齐了就能判，与 `period` 无关。
            BasisArbitrage | SpotFuturesArbitrage => 1,
        };
        signal_bars
            .max(if self.config.target_volatility_bps.is_some() {
                self.config.period + 1
            } else {
                1
            })
            .max(
                self.config
                    .target_kelly_fraction_bps
                    .map(|_| {
                        self.config
                            .kelly_lookback_bars
                            .unwrap_or(DEFAULT_KELLY_LOOKBACK_BARS)
                    })
                    .map_or(1, |lookback| lookback + 1),
            )
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
        // 腿还没报到齐是预热未完，不是错误：这一支只读两条腿的当前价，一根就够判，
        // 与 `period` 无关（清单见 `builtin_signal.rs`）。
        let (Some(reference), Some(primary)) = (self.reference_bars.back(), self.bars.back())
        else {
            return Ok(None);
        };
        let reference_now = reference.close;
        let primary_now = primary.close;
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
        let required = self.required_bars();
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
                let (fast, slow, signal_window) = MACD_WINDOWS;
                let macd_now = ema(&closes, fast).unwrap() - ema(&closes, slow).unwrap();
                let previous = &closes[..closes.len() - 1];
                let macd_prev = ema(previous, fast).unwrap() - ema(previous, slow).unwrap();
                let signal_now = macd_series(&closes, fast, slow, signal_window)
                    .and_then(|series| ema(&series, signal_window))
                    .unwrap();
                let signal_prev = macd_series(previous, fast, slow, signal_window)
                    .and_then(|series| ema(&series, signal_window))
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
        let is_pair = self.config.kind.needs_reference_leg();
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
            let leg_price = if leg_instrument == self.config.instrument {
                self.bars
                    .back()
                    .map(|bar| bar.close)
                    .ok_or("名义金额 sizing 缺少主腿价格")?
            } else {
                self.reference_bars
                    .back()
                    .map(|bar| bar.close)
                    .ok_or("双腿名义金额 sizing 缺少参考腿价格")?
            };
            let target = self
                .target_quantity_raw(leg_price, leg_signal, context)?
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
            let reduce_only = current.unsigned_abs() > target.unsigned_abs()
                && (target == 0 || current.signum() == target.signum());
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
                reduce_only,
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

#[cfg(test)]
mod tests;
