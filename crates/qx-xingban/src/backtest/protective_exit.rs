use qx_guanxing::Bar;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProtectiveExitReason {
    StopLoss,
    TrailingStop,
    TakeProfit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProtectiveExitPolicy {
    pub stop_loss_bps: Option<u32>,
    pub take_profit_bps: Option<u32>,
    pub trailing_stop_bps: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProtectiveExitSignal {
    pub reason: ProtectiveExitReason,
    pub trigger_price_raw: i128,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProtectiveExitFill {
    pub reason: ProtectiveExitReason,
    pub trigger_ts: u64,
    pub fill_ts: u64,
    pub order_id: u64,
    pub qty_raw: i128,
    pub price_raw: i128,
}

impl ProtectiveExitPolicy {
    pub fn validate(self) -> Result<(), qx_core::QxError> {
        let values = [
            self.stop_loss_bps,
            self.take_profit_bps,
            self.trailing_stop_bps,
        ];
        if values.iter().all(Option::is_none)
            || values
                .into_iter()
                .flatten()
                .any(|bps| bps == 0 || bps >= 10_000)
        {
            return Err(qx_core::QxError::BusinessViolation(
                "止损/止盈/跟踪止损必须至少配置一项，且基点在 1..=9999".into(),
            ));
        }
        Ok(())
    }

    /// Evaluate only completed OHLC bars. When a bar crosses both hard stop
    /// and target, the hard stop wins as the conservative intrabar ordering.
    pub fn evaluate(
        self,
        position_qty_raw: i128,
        entry_price_raw: i128,
        completed_bar: &Bar,
        favorable_extreme_raw: Option<i128>,
    ) -> Result<Option<ProtectiveExitSignal>, qx_core::QxError> {
        if position_qty_raw == 0 {
            return Ok(None);
        }
        self.validate()?;
        if entry_price_raw <= 0 || completed_bar.low <= 0 || completed_bar.high < completed_bar.low
        {
            return Err(qx_core::QxError::BusinessViolation(
                "保护性退出的 entry 或 Bar 高低价无效".into(),
            ));
        }
        let long = position_qty_raw > 0;
        let stop = match (long, self.stop_loss_bps) {
            (true, Some(bps)) => Some(scale_bps(entry_price_raw, 10_000 - i128::from(bps), false)?),
            (false, Some(bps)) => Some(scale_bps(entry_price_raw, 10_000 + i128::from(bps), true)?),
            (_, None) => None,
        };
        let take = match (long, self.take_profit_bps) {
            (true, Some(bps)) => Some(scale_bps(entry_price_raw, 10_000 + i128::from(bps), true)?),
            (false, Some(bps)) => {
                Some(scale_bps(entry_price_raw, 10_000 - i128::from(bps), false)?)
            }
            (_, None) => None,
        };
        let trailing = match (long, self.trailing_stop_bps, favorable_extreme_raw) {
            (true, Some(bps), Some(extreme)) => {
                Some(scale_bps(extreme, 10_000 - i128::from(bps), false)?)
            }
            (false, Some(bps), Some(extreme)) => {
                Some(scale_bps(extreme, 10_000 + i128::from(bps), true)?)
            }
            _ => None,
        };
        if let Some(level) = stop.filter(|level| {
            if long {
                completed_bar.low <= *level
            } else {
                completed_bar.high >= *level
            }
        }) {
            return Ok(Some(ProtectiveExitSignal {
                reason: ProtectiveExitReason::StopLoss,
                trigger_price_raw: level,
            }));
        }
        if let Some(level) = trailing.filter(|level| {
            if long {
                completed_bar.low <= *level
            } else {
                completed_bar.high >= *level
            }
        }) {
            return Ok(Some(ProtectiveExitSignal {
                reason: ProtectiveExitReason::TrailingStop,
                trigger_price_raw: level,
            }));
        }
        if let Some(level) = take.filter(|level| {
            if long {
                completed_bar.high >= *level
            } else {
                completed_bar.low <= *level
            }
        }) {
            return Ok(Some(ProtectiveExitSignal {
                reason: ProtectiveExitReason::TakeProfit,
                trigger_price_raw: level,
            }));
        }
        Ok(None)
    }
}

fn scale_bps(value: i128, multiplier_bps: i128, round_up: bool) -> Result<i128, qx_core::QxError> {
    let numerator = value
        .checked_mul(multiplier_bps)
        .ok_or_else(|| qx_core::QxError::BusinessViolation("保护性退出价格溢出".into()))?;
    let quotient = numerator / 10_000;
    let remainder = numerator % 10_000;
    let scaled = if round_up && remainder != 0 {
        quotient + 1
    } else {
        quotient
    };
    if scaled <= 0 {
        return Err(qx_core::QxError::BusinessViolation(
            "保护性退出价格必须为正".into(),
        ));
    }
    Ok(scaled)
}
