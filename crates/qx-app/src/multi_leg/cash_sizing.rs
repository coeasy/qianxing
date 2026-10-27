use crate::error::{AppError, AppErrorCategory};
use qx_core::{Money, SCALE};
use qx_guanxing::Bar;

/// Cash reserved for one independently accounted leg of a paired backtest.
///
/// Prices and Money are both 1e9 fixed point. Keep the full raw price precision here;
/// rounding each bar price down to whole currency units can underfund fractional-price
/// instruments. Preserve the established one-whole-unit fee headroom so existing integer-price
/// runs retain their account base and result hash.
pub fn multi_leg_leg_cash_reserve(
    quantity: i64,
    bars: &[Bar],
    taker_fee_bps: i64,
    label: &str,
) -> Result<Money, AppError> {
    if quantity <= 0 || taker_fee_bps < 0 || bars.is_empty() {
        return Err(invalid(format!(
            "{label}腿定资要求正数量、非负 taker 费率和非空行情"
        )));
    }
    let max_price_raw = bars.iter().map(|bar| bar.high).max().unwrap_or_default();
    if max_price_raw <= 0 {
        return Err(invalid(format!("{label}腿行情最高价必须为正")));
    }
    let peak_notional_raw = i128::from(quantity)
        .checked_mul(max_price_raw)
        .and_then(|value| value.checked_mul(2))
        .ok_or_else(|| invalid(format!("{label}腿计划名义额溢出，无法为回测账户定资")))?;
    let fee_numerator = peak_notional_raw
        .checked_mul(i128::from(taker_fee_bps))
        .ok_or_else(|| invalid(format!("{label}腿手续费余量溢出")))?;
    let fee_quotient = fee_numerator / 10_000;
    let fee_remainder = fee_numerator % 10_000;
    let rounded_fee_raw = if fee_remainder == 0 {
        fee_quotient
    } else {
        fee_quotient
            .checked_add(1)
            .ok_or_else(|| invalid(format!("{label}腿手续费余量溢出")))?
    };
    let fee_headroom_raw = rounded_fee_raw
        .checked_add(SCALE)
        .ok_or_else(|| invalid(format!("{label}腿手续费余量溢出")))?;
    let floor_raw = 100_000_i128
        .checked_mul(SCALE)
        .ok_or_else(|| invalid("多腿回测本金下限溢出"))?;
    let required_raw = peak_notional_raw
        .checked_add(fee_headroom_raw)
        .ok_or_else(|| invalid(format!("{label}腿所需现金溢出")))?
        .max(floor_raw);
    let max_supported_raw = i128::from(i64::MAX)
        .checked_mul(SCALE)
        .ok_or_else(|| invalid("账户可表示本金上限溢出"))?;
    if required_raw > max_supported_raw {
        return Err(invalid(format!(
            "{label}腿所需现金 {} 超出账户可表示上限；请降低 quantity 或修正夹具价格尺度",
            required_raw
        )));
    }
    Ok(Money::from_raw(required_raw))
}

fn invalid(message: impl Into<String>) -> AppError {
    AppError::new(AppErrorCategory::InvalidInput, message)
}

#[cfg(test)]
mod tests {
    use super::multi_leg_leg_cash_reserve;
    use qx_core::{Money, SCALE};
    use qx_guanxing::Bar;

    fn bar(high: i128) -> Bar {
        Bar {
            ts: 1,
            open: high,
            high,
            low: high,
            close: high,
            volume: 1,
            exact_ohlcv: None,
        }
    }

    #[test]
    fn fractional_prices_are_not_rounded_down_when_sizing_cash() {
        let reserve = multi_leg_leg_cash_reserve(1, &[bar(100 * SCALE + SCALE / 2)], 0, "主")
            .expect("reserve fractional price");
        assert_eq!(reserve.raw(), 100_000 * SCALE);
        let above_floor =
            multi_leg_leg_cash_reserve(1_000, &[bar(100 * SCALE + SCALE / 2)], 0, "主")
                .expect("reserve fractional price above floor");
        assert_eq!(above_floor.raw(), 201_001 * SCALE);
    }

    #[test]
    fn fee_reserve_keeps_the_established_whole_unit_headroom() {
        let reserve = multi_leg_leg_cash_reserve(1, &[bar(50 * SCALE + 1)], 1, "主")
            .expect("reserve taker fee");
        assert_eq!(reserve.raw(), Money::from_raw(100_000 * SCALE).raw());
        let reserve = multi_leg_leg_cash_reserve(1_000, &[bar(50 * SCALE + 1)], 1, "主")
            .expect("reserve taker fee above floor");
        assert_eq!(reserve.raw(), 100_011 * SCALE + 2_001);
    }

    #[test]
    fn fractional_fee_raw_units_are_rounded_up() {
        let reserve = multi_leg_leg_cash_reserve(1_001, &[bar(100 * SCALE + 1)], 1, "主")
            .expect("reserve fractional taker fee");
        // Peak notional is 200,200.000002002 currency units; 1 bp leaves a
        // fractional raw unit, so the fee must round up before adding the
        // established one-whole-unit headroom.
        assert_eq!(reserve.raw(), 200_221 * SCALE + 20_002_003);
    }
}
