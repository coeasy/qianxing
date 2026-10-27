//! 可直接装配的内置 Bar 策略。
//!
//! 这些策略只负责从可见 Bar 生成统一 `StrategyDecision`，不接触交易所、
//! EventLog 或账户凭据。回测、Paper 和实盘都必须继续经过 Runtime 的
//! Portfolio、RiskGate、OMS 和 Execution 边界。

mod indicator;
mod signal;
mod sizing;

use crate::builtin_signal::{
    BuiltinSignalKnob, HISTORY_FLOOR_BARS, MACD_REQUIRED_BARS, MACD_WINDOWS,
};
use crate::{
    MarketEvent, Strategy, StrategyContext, StrategyDecision, StrategyOrderIntent,
    STRATEGY_API_VERSION,
};
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
    close_scratch: Vec<i128>,
    fast_sma_sum: i128,
    slow_sma_sum: i128,
    previous_fast_sma_sum: i128,
    previous_slow_sma_sum: i128,
    sma_sums_ready: bool,
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
            close_scratch: Vec::new(),
            fast_sma_sum: 0,
            slow_sma_sum: 0,
            previous_fast_sma_sum: 0,
            previous_slow_sma_sum: 0,
            sma_sums_ready: false,
            next_signal_id: 1,
            next_intent_id: 1,
        })
    }

    fn max_history(&self) -> usize {
        // 窗口取样上限必须同时容得下信号门槛，否则门槛高的策略会被历史裁剪卡死：
        // 默认 5/20/14 只算出 32 根，而 MACD 需要 35 根，示例帧再长也永远不信号。
        // 上限只按本 kind 读取的窗口开：EMA 结果依赖保留前缀；未上场窗口不能改动策略结果。
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

    fn advance_sma_sums(&mut self, close: i128) -> Result<(), String> {
        if self.sma_sums_ready {
            self.previous_fast_sma_sum = self.fast_sma_sum;
            self.previous_slow_sma_sum = self.slow_sma_sum;
            self.fast_sma_sum = rolling_sum_after_append(
                self.fast_sma_sum,
                self.config.fast_window,
                close,
                &self.bars,
            )
            .ok_or_else(|| "SMA 快线滚动求和溢出".to_string())?;
            self.slow_sma_sum = rolling_sum_after_append(
                self.slow_sma_sum,
                self.config.slow_window,
                close,
                &self.bars,
            )
            .ok_or_else(|| "SMA 慢线滚动求和溢出".to_string())?;
        } else {
            let previous_fast = checked_tail_sum(&self.bars, self.config.fast_window)
                .ok_or_else(|| "SMA 快线前值计算溢出".to_string())?;
            let previous_slow = checked_tail_sum(&self.bars, self.config.slow_window)
                .ok_or_else(|| "SMA 慢线前值计算溢出".to_string())?;
            self.previous_fast_sma_sum = previous_fast;
            self.previous_slow_sma_sum = previous_slow;
            self.fast_sma_sum =
                rolling_sum_after_append(previous_fast, self.config.fast_window, close, &self.bars)
                    .ok_or_else(|| "SMA 快线滚动求和溢出".to_string())?;
            self.slow_sma_sum =
                rolling_sum_after_append(previous_slow, self.config.slow_window, close, &self.bars)
                    .ok_or_else(|| "SMA 慢线滚动求和溢出".to_string())?;
            self.sma_sums_ready = true;
        }
        Ok(())
    }
}

fn checked_tail_sum(history: &VecDeque<BarPoint>, window: usize) -> Option<i128> {
    if history.len() < window {
        return None;
    }
    history
        .iter()
        .rev()
        .take(window)
        .try_fold(0_i128, |sum, bar| sum.checked_add(bar.close))
}

fn rolling_sum_after_append(
    current_sum: i128,
    window: usize,
    close: i128,
    history: &VecDeque<BarPoint>,
) -> Option<i128> {
    let rolling = if history.len() >= window {
        let expired_index = history.len().checked_sub(window)?;
        current_sum
            .checked_sub(history.get(expired_index)?.close)
            .and_then(|sum| sum.checked_add(close))
    } else {
        current_sum.checked_add(close)
    };
    rolling.or_else(|| {
        // Subtract/add ordering can overflow even when the new window's exact sum is valid.
        // Recompute only on that rare path, preserving the checked left-to-right semantics.
        let start = history.len().saturating_sub(window.saturating_sub(1));
        history
            .iter()
            .skip(start)
            .map(|bar| bar.close)
            .chain(std::iter::once(close))
            .try_fold(0_i128, |sum, value| sum.checked_add(value))
    })
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
            if self.config.kind == BuiltinStrategyKind::SmaCross
                && self
                    .bars
                    .len()
                    .checked_add(1)
                    .is_some_and(|next_len| next_len >= self.required_bars())
            {
                self.advance_sma_sums(point.close)?;
            }
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
                .target_quantity_raw(&leg_instrument, leg_price, leg_signal, context)?
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

                qty_exact: None,
                limit_price_exact: None,
                quote_budget: None,
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
