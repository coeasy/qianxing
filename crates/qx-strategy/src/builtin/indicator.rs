//! 内置策略的指标数学：只吃定点序列，不读配置、不碰事件与账户。
//!
//! 搬出来这一份的理由与行数无关：`builtin.rs` 里那台决策机每加一个 kind 就要重排一次，
//! 而这里的算法自 Q1a-0（ema 的直流增益）之后就没再动过。两者混在一处时，改动一侧的
//! diff 会盖住另一侧的回归。

use super::{BarPoint, BPS_SCALE};
use std::collections::VecDeque;

pub(super) fn sma(values: &[i128], window: usize) -> Option<i128> {
    if window == 0 || values.len() < window {
        return None;
    }
    values[values.len() - window..]
        .iter()
        .try_fold(0_i128, |sum, value| sum.checked_add(*value))
        .and_then(|sum| sum.checked_div(window as i128))
}

pub(super) fn ema(values: &[i128], window: usize) -> Option<i128> {
    if window == 0 || values.len() < window {
        return None;
    }
    let mut result = values[..window]
        .iter()
        .try_fold(0_i128, |sum, value| sum.checked_add(*value))?
        .checked_div(window as i128)?;
    let denominator = window as i128 + 1;
    for value in &values[window..] {
        // α = 2/(window+1)，因此旧值权重是 1-α = (window-1)/(window+1)。写成
        // `window` 会让两个权重之和变成 (window+2)/(window+1)，常数序列收敛到
        // 2×该常数，快慢线的相对位置随窗口大小漂移，交叉判定就废了。
        result = value
            .checked_mul(2)
            .and_then(|next| {
                result
                    .checked_mul(window as i128 - 1)
                    .and_then(|old| next.checked_add(old))
            })
            .and_then(|next| next.checked_div(denominator))?;
    }
    Some(result)
}

pub(super) fn macd_series(
    values: &[i128],
    fast: usize,
    slow: usize,
    _signal: usize,
) -> Option<Vec<i128>> {
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

pub(super) fn rsi(values: &[i128], period: usize) -> Option<i128> {
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

pub(super) fn stddev(values: &[i128], mean: i128) -> Result<i128, String> {
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

pub(super) fn atr(values: &VecDeque<BarPoint>, period: usize) -> Option<i128> {
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
