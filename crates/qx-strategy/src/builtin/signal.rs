use super::indicator::{atr, ema, ema_previous_current, macd_series, rsi, sma, stddev};
use super::{BuiltinStrategy, BuiltinStrategyKind, BPS_SCALE, MACD_WINDOWS};

fn required_indicator<T>(value: Option<T>, name: &str) -> Result<T, String> {
    value.ok_or_else(|| format!("{name} 指标数据不足或计算溢出"))
}

impl BuiltinStrategy {
    fn sma_cross_signal(&self) -> Result<Option<i8>, String> {
        let fast_divisor =
            i128::try_from(self.config.fast_window).map_err(|_| "SMA 快线窗口超出整数范围")?;
        let slow_divisor =
            i128::try_from(self.config.slow_window).map_err(|_| "SMA 慢线窗口超出整数范围")?;
        let fast_previous = self
            .previous_fast_sma_sum
            .checked_div(fast_divisor)
            .ok_or("SMA 快线前值计算溢出")?;
        let fast_current = self
            .fast_sma_sum
            .checked_div(fast_divisor)
            .ok_or("SMA 快线当前值计算溢出")?;
        let slow_previous = self
            .previous_slow_sma_sum
            .checked_div(slow_divisor)
            .ok_or("SMA 慢线前值计算溢出")?;
        let slow_current = self
            .slow_sma_sum
            .checked_div(slow_divisor)
            .ok_or("SMA 慢线当前值计算溢出")?;
        Ok(
            if fast_previous <= slow_previous && fast_current > slow_current {
                Some(1)
            } else if fast_previous >= slow_previous && fast_current < slow_current {
                Some(-1)
            } else {
                None
            },
        )
    }

    pub(super) fn signal(&mut self) -> Result<Option<i8>, String> {
        let required = self.required_bars();
        if self.bars.len() < required {
            return Ok(None);
        }
        if self.config.kind.needs_reference_leg() {
            return match self.config.kind {
                BuiltinStrategyKind::PairsArbitrage | BuiltinStrategyKind::CrossVenueArbitrage => {
                    self.spread_signal()
                }
                BuiltinStrategyKind::BasisArbitrage | BuiltinStrategyKind::SpotFuturesArbitrage => {
                    self.basis_signal()
                }
                _ => Err("双腿策略类型缺少信号实现".into()),
            };
        }
        if self.config.kind == BuiltinStrategyKind::SmaCross {
            return self.sma_cross_signal();
        }
        self.close_scratch.clear();
        self.close_scratch
            .extend(self.bars.iter().map(|bar| bar.close));
        let closes = self.close_scratch.as_slice();
        let signal = match self.config.kind {
            BuiltinStrategyKind::SmaCross => unreachable!("SMA cross uses rolling sums"),
            BuiltinStrategyKind::EmaCross => {
                let (fast_prev, fast_now) = required_indicator(
                    ema_previous_current(closes, self.config.fast_window),
                    "EMA 快线",
                )?;
                let (slow_prev, slow_now) = required_indicator(
                    ema_previous_current(closes, self.config.slow_window),
                    "EMA 慢线",
                )?;
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
                let fast_now = required_indicator(ema(closes, fast), "MACD 快线")?;
                let slow_now = required_indicator(ema(closes, slow), "MACD 慢线")?;
                let macd_now = fast_now.checked_sub(slow_now).ok_or("MACD 当前值溢出")?;
                let previous = closes
                    .get(..closes.len().saturating_sub(1))
                    .ok_or("MACD 前一时点数据缺失")?;
                let fast_prev = required_indicator(ema(previous, fast), "MACD 前一快线")?;
                let slow_prev = required_indicator(ema(previous, slow), "MACD 前一慢线")?;
                let macd_prev = fast_prev.checked_sub(slow_prev).ok_or("MACD 前一值溢出")?;
                let signal_now = macd_series(closes, fast, slow, signal_window)
                    .and_then(|series| ema(&series, signal_window))
                    .ok_or("MACD 信号线数据不足或计算溢出")?;
                let signal_prev = macd_series(previous, fast, slow, signal_window)
                    .and_then(|series| ema(&series, signal_window))
                    .ok_or("MACD 前一信号线数据不足或计算溢出")?;
                if macd_prev <= signal_prev && macd_now > signal_now {
                    Some(1)
                } else if macd_prev >= signal_prev && macd_now < signal_now {
                    Some(-1)
                } else {
                    None
                }
            }
            BuiltinStrategyKind::Rsi => {
                let value = required_indicator(rsi(closes, self.config.period), "RSI")?;
                if value <= 3_000 {
                    Some(1)
                } else if value >= 7_000 {
                    Some(-1)
                } else {
                    Some(0)
                }
            }
            BuiltinStrategyKind::Bollinger => {
                let start = closes
                    .len()
                    .checked_sub(self.config.period)
                    .ok_or("Bollinger 周期窗口越界")?;
                let window = closes.get(start..).ok_or("Bollinger 周期窗口缺失")?;
                let mean = required_indicator(sma(window, self.config.period), "Bollinger SMA")?;
                let deviation = stddev(window, mean)?;
                let distance = deviation.checked_mul(2).ok_or("Bollinger 计算溢出")?;
                let lower = mean.checked_sub(distance).ok_or("Bollinger 下轨溢出")?;
                let upper = mean.checked_add(distance).ok_or("Bollinger 上轨溢出")?;
                let current = window.last().copied().ok_or("Bollinger 当前收盘价缺失")?;
                if current < lower {
                    Some(1)
                } else if current > upper {
                    Some(-1)
                } else {
                    Some(0)
                }
            }
            BuiltinStrategyKind::DonchianBreakout => {
                let current = closes.last().copied().ok_or("Donchian 当前收盘价缺失")?;
                let previous_start = closes
                    .len()
                    .checked_sub(self.config.period.saturating_add(1))
                    .ok_or("Donchian 前序窗口越界")?;
                let previous_end = closes.len().saturating_sub(1);
                let previous = closes
                    .get(previous_start..previous_end)
                    .ok_or("Donchian 前序窗口缺失")?;
                let previous_high = previous
                    .iter()
                    .max()
                    .copied()
                    .ok_or("Donchian 前序高点缺失")?;
                let previous_low = previous
                    .iter()
                    .min()
                    .copied()
                    .ok_or("Donchian 前序低点缺失")?;
                if current > previous_high {
                    Some(1)
                } else if current < previous_low {
                    Some(-1)
                } else {
                    None
                }
            }
            BuiltinStrategyKind::Momentum => {
                let current = closes.last().copied().ok_or("Momentum 当前收盘价缺失")?;
                let base_index = closes
                    .len()
                    .checked_sub(self.config.period.saturating_add(1))
                    .ok_or("Momentum 基准窗口越界")?;
                let base = *closes.get(base_index).ok_or("Momentum 基准收盘价缺失")?;
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
                let start = closes
                    .len()
                    .checked_sub(self.config.period)
                    .ok_or("MeanReversion 周期窗口越界")?;
                let window = closes.get(start..).ok_or("MeanReversion 周期窗口缺失")?;
                let mean =
                    required_indicator(sma(window, self.config.period), "MeanReversion SMA")?;
                if mean <= 0 {
                    return Err("MeanReversion 均值必须为正".into());
                }
                let current = window
                    .last()
                    .copied()
                    .ok_or("MeanReversion 当前收盘价缺失")?;
                let deviation = current
                    .checked_sub(mean)
                    .ok_or("MeanReversion 偏离计算溢出")?
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
                let anchor = self.bars.front().ok_or("Grid 基准 Bar 缺失")?.close;
                let current = closes.last().copied().ok_or("Grid 当前收盘价缺失")?;
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
                let current = self.bars.back().ok_or("ATR 当前 Bar 缺失")?;
                let previous = self.bars.iter().rev().nth(1).ok_or("ATR 前一 Bar 缺失")?;
                let atr = required_indicator(atr(&self.bars, self.config.period), "ATR")?;
                if current.close > previous.close.saturating_add(atr) {
                    Some(1)
                } else if current.close < previous.close.saturating_sub(atr) {
                    Some(-1)
                } else {
                    None
                }
            }
            BuiltinStrategyKind::KeltnerTrend => {
                let middle = required_indicator(ema(closes, self.config.period), "Keltner EMA")?;
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
                let current = closes.last().copied().ok_or("Keltner 当前收盘价缺失")?;
                if current > middle.saturating_add(width) {
                    Some(1)
                } else if current < middle.saturating_sub(width) {
                    Some(-1)
                } else {
                    None
                }
            }
            BuiltinStrategyKind::VwapReversion => {
                let (weighted, volume) = self.bars.iter().rev().take(self.config.period).try_fold(
                    (0_i128, 0_i128),
                    |(weighted, volume), bar| {
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
                    },
                )?;
                let vwap = if volume == 0 {
                    return Err("VWAP 成交量不能为零".into());
                } else {
                    weighted.checked_div(volume).ok_or("VWAP 计算溢出")?
                };
                let current = closes.last().copied().ok_or("VWAP 当前收盘价缺失")?;
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
                let current = self.bars.back().ok_or("波动率当前 Bar 缺失")?;
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
            _ => return Err("双腿策略信号应在收盘价缓冲准备前完成分派".into()),
        };
        Ok(signal)
    }
}
