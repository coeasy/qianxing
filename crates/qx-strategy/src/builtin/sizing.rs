use super::indicator::atr;
use super::{BuiltinStrategy, BPS_SCALE, DEFAULT_KELLY_LOOKBACK_BARS};
use crate::StrategyContext;

impl BuiltinStrategy {
    pub(super) fn target_quantity_raw(
        &self,
        price_raw: i128,
        direction: i128,
        context: &StrategyContext,
    ) -> Result<i128, String> {
        let notional_raw = match (
            self.config.target_notional_raw,
            self.config.target_available_margin_bps,
            self.config.target_volatility_bps,
            self.config.target_kelly_fraction_bps,
        ) {
            (Some(notional), None, None, None) => notional,
            (None, Some(bps), None, None) => context
                .available_margin_raw
                .filter(|value| *value > 0)
                .ok_or("可用资金比例 sizing 缺少有效的实时 available_margin_raw")?
                .checked_mul(i128::from(bps))
                .and_then(|value| value.checked_div(BPS_SCALE))
                .ok_or("target_available_margin_bps 名义金额计算溢出")?,
            (None, None, Some(risk_bps), None) => {
                let available = context
                    .available_margin_raw
                    .filter(|value| *value > 0)
                    .ok_or("波动率预算 sizing 缺少有效的实时 available_margin_raw")?;
                if price_raw <= 0 {
                    return Err("波动率预算 sizing 的可见价格必须为正".into());
                }
                let average_range = atr(&self.bars, self.config.period)
                    .ok_or("波动率预算 sizing 的 ATR 样本不足或计算溢出")?;
                if average_range <= 0 {
                    return Err("波动率预算 sizing 的 ATR 必须为正".into());
                }
                let atr_ratio_bps = average_range
                    .checked_mul(BPS_SCALE)
                    .and_then(|value| value.checked_div(price_raw))
                    .filter(|value| *value > 0)
                    .ok_or("波动率预算 sizing 的 ATR/价格比例非法或溢出")?;
                available
                    .checked_mul(i128::from(risk_bps))
                    .and_then(|value| value.checked_div(atr_ratio_bps))
                    .ok_or("target_volatility_bps 名义金额计算溢出")?
                    // Volatility targeting can request more than 100% of buying power
                    // when the measured range is below the configured risk budget.
                    // Treat available margin as a hard ceiling; leverage must be an
                    // explicit account/risk configuration, never an accidental result.
                    .min(available)
            }
            (None, None, None, Some(fractional_bps)) => {
                let available = context
                    .available_margin_raw
                    .filter(|value| *value > 0)
                    .ok_or("Kelly sizing 缺少有效的实时 available_margin_raw")?;
                let lookback = self
                    .config
                    .kelly_lookback_bars
                    .unwrap_or(DEFAULT_KELLY_LOOKBACK_BARS);
                let Some(full_kelly_bps) =
                    self.estimated_kelly_fraction_bps(lookback, direction)?
                else {
                    // Insufficient or one-sided history is not evidence of positive edge.
                    return Ok(0);
                };
                let sized_fraction_bps = full_kelly_bps
                    .checked_mul(i128::from(fractional_bps))
                    .and_then(|value| value.checked_div(BPS_SCALE))
                    .ok_or("Kelly fraction scaling overflow")?;
                let notional = available
                    .checked_mul(sized_fraction_bps)
                    .and_then(|value| value.checked_div(BPS_SCALE))
                    .ok_or("Kelly notional calculation overflow")?;
                if notional <= 0 {
                    return Ok(0);
                }
                notional.min(available)
            }
            (None, None, None, None) => return Ok(self.config.quantity.raw()),
            _ => return Err("多种 sizing 参数不能同时配置".into()),
        };
        if notional_raw <= 0 {
            return Err("可用资金比例 sizing 得到的名义金额小于最小金额单位".into());
        }
        if price_raw <= 0 {
            return Err("按名义金额 sizing 时可见价格必须为正".into());
        }
        let quantity_raw = notional_raw
            .checked_mul(qx_core::SCALE)
            .and_then(|value| value.checked_div(price_raw))
            .ok_or("target_notional_raw 转数量溢出")?;
        if quantity_raw <= 0 {
            return Err("target_notional_raw 小于当前价格对应的最小数量单位".into());
        }
        Ok(quantity_raw)
    }

    /// Estimate the long-run Kelly edge from the prior `lookback` close returns.
    /// Flat observations count in the probability denominator. Both winning and
    /// losing observations are required; otherwise sizing fails closed to zero.
    fn estimated_kelly_fraction_bps(
        &self,
        lookback: usize,
        direction: i128,
    ) -> Result<Option<i128>, String> {
        if !matches!(direction, -1 | 1) {
            return Err("Kelly sizing direction must be -1 or 1".into());
        }
        if self.bars.len() < lookback.saturating_add(1) {
            return Ok(None);
        }
        let closes = self
            .bars
            .iter()
            .rev()
            .take(lookback + 1)
            .map(|bar| bar.close)
            .collect::<Vec<_>>();
        let mut wins = 0_i128;
        let mut losses = 0_i128;
        let mut win_sum = 0_i128;
        let mut loss_sum = 0_i128;
        for pair in closes.windows(2) {
            let previous = pair[1];
            let current = pair[0];
            if previous <= 0 || current <= 0 {
                return Err("Kelly sizing requires positive visible close prices".into());
            }
            let change = current
                .checked_sub(previous)
                .and_then(|delta| delta.checked_mul(BPS_SCALE))
                .and_then(|delta| delta.checked_div(previous))
                .and_then(|delta| delta.checked_mul(direction))
                .ok_or("Kelly return calculation overflow")?;
            if change > 0 {
                wins = wins.checked_add(1).ok_or("Kelly win count overflow")?;
                win_sum = win_sum
                    .checked_add(change)
                    .ok_or("Kelly win sum overflow")?;
            } else if change < 0 {
                losses = losses.checked_add(1).ok_or("Kelly loss count overflow")?;
                loss_sum = loss_sum
                    .checked_add(
                        change
                            .checked_abs()
                            .ok_or("Kelly loss magnitude overflow")?,
                    )
                    .ok_or("Kelly loss sum overflow")?;
            }
        }
        if wins == 0 || losses == 0 {
            return Ok(None);
        }
        let total = i128::try_from(lookback).map_err(|_| "Kelly lookback is too large")?;
        let average_win = win_sum / wins;
        let average_loss = loss_sum / losses;
        if average_win <= 0 || average_loss <= 0 {
            return Ok(None);
        }
        let win_probability_bps = wins
            .checked_mul(BPS_SCALE)
            .and_then(|value| value.checked_div(total))
            .ok_or("Kelly win probability overflow")?;
        let loss_probability_bps = losses
            .checked_mul(BPS_SCALE)
            .and_then(|value| value.checked_div(total))
            .ok_or("Kelly loss probability overflow")?;
        let loss_term_bps = loss_probability_bps
            .checked_mul(average_loss)
            .and_then(|value| value.checked_div(average_win))
            .ok_or("Kelly payoff ratio overflow")?;
        Ok(Some(
            win_probability_bps
                .saturating_sub(loss_term_bps)
                .clamp(0, BPS_SCALE),
        ))
    }
}
