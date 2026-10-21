//! 定点数值：128-bit，scale = 1e9。
//!
//! 为什么禁浮点：IEEE-754 的 NaN 位模式在不同硬件/编译优化下不确定，
//! 会直接破坏 bit-level 可重放。回测的确定性优先于浮点便利。

use serde::{Deserialize, Serialize};
use std::fmt;

mod rounding;
pub use rounding::parse_decimal_half_even;

/// 定点小数位数：1e9，即 9 位小数。
pub const SCALE: i128 = 1_000_000_000;

/// Parse a decimal string into the fixed-point scale without rounding.
///
/// Scientific notation is accepted because exchange APIs commonly emit it. Any
/// non-zero precision beyond nine decimal places is rejected so callers cannot
/// silently change a price, quantity, or balance during normalization.
pub fn parse_decimal_exact(input: &str) -> Result<i128, &'static str> {
    let value = input.trim();
    if value.is_empty() {
        return Err("empty decimal value");
    }
    let (negative, unsigned) = match value.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, value.strip_prefix('+').unwrap_or(value)),
    };
    let (mantissa, exponent) = match unsigned.find(['e', 'E']) {
        Some(index) => {
            let exponent = unsigned[index + 1..]
                .parse::<i32>()
                .map_err(|_| "invalid decimal exponent")?;
            (&unsigned[..index], exponent)
        }
        None => (unsigned, 0),
    };
    if !(-100..=100).contains(&exponent) {
        return Err("decimal exponent is out of range");
    }
    let mut pieces = mantissa.split('.');
    let whole = pieces.next().unwrap_or_default();
    let fraction = pieces.next().unwrap_or_default();
    if pieces.next().is_some()
        || (whole.is_empty() && fraction.is_empty())
        || !whole.bytes().all(|byte| byte.is_ascii_digit())
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err("invalid decimal value");
    }
    let digits = format!("{whole}{fraction}");
    let magnitude = digits
        .parse::<u128>()
        .map_err(|_| "decimal value is out of range")?;
    let power = 9_i32 + exponent - fraction.len() as i32;
    let scaled = if power >= 0 {
        let factor = 10_u128
            .checked_pow(power as u32)
            .ok_or("decimal value is out of range")?;
        magnitude
            .checked_mul(factor)
            .ok_or("decimal value is out of range")?
    } else {
        let places = (-power) as u32;
        if places > 38 {
            if magnitude == 0 {
                0
            } else {
                return Err("decimal precision exceeds 1e-9");
            }
        } else {
            let divisor = 10_u128.pow(places);
            if magnitude % divisor != 0 {
                return Err("decimal precision exceeds 1e-9");
            }
            magnitude / divisor
        }
    };
    if negative {
        let min_magnitude = (i128::MAX as u128) + 1;
        if scaled == min_magnitude {
            Ok(i128::MIN)
        } else {
            i128::try_from(scaled)
                .map(|value| -value)
                .map_err(|_| "decimal value is out of range")
        }
    } else {
        i128::try_from(scaled).map_err(|_| "decimal value is out of range")
    }
}

/// Convert a decimal string to an integer unit without rounding.
///
/// `multiplier` maps the input unit to the requested integer unit (for example,
/// a ratio to basis points uses 10_000 and a percent to basis points uses 100).
pub fn parse_decimal_scaled_integer(input: &str, multiplier: i64) -> Result<i64, &'static str> {
    if multiplier <= 0 {
        return Err("decimal multiplier must be positive");
    }
    let raw = parse_decimal_exact(input)?;
    let scaled = raw
        .checked_mul(i128::from(multiplier))
        .ok_or("scaled decimal value is out of range")?;
    if scaled % SCALE != 0 {
        return Err("decimal value exceeds integer unit precision");
    }
    i64::try_from(scaled / SCALE).map_err(|_| "scaled decimal value is out of range")
}

/// Parse a decimal spelling that must represent an exact whole integer.
pub fn parse_decimal_integer_exact(input: &str) -> Result<i128, &'static str> {
    let value = input.trim();
    if value.is_empty() {
        return Err("empty decimal value");
    }
    let (negative, unsigned) = match value.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, value.strip_prefix('+').unwrap_or(value)),
    };
    let (mantissa, exponent) = match unsigned.find(['e', 'E']) {
        Some(index) => {
            let exponent = unsigned[index + 1..]
                .parse::<i32>()
                .map_err(|_| "invalid decimal exponent")?;
            (&unsigned[..index], exponent)
        }
        None => (unsigned, 0),
    };
    if !(-100..=100).contains(&exponent) {
        return Err("decimal exponent is out of range");
    }
    let mut pieces = mantissa.split('.');
    let whole = pieces.next().unwrap_or_default();
    let fraction = pieces.next().unwrap_or_default();
    if pieces.next().is_some()
        || (whole.is_empty() && fraction.is_empty())
        || !whole.bytes().all(|byte| byte.is_ascii_digit())
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err("invalid decimal value");
    }
    let magnitude = format!("{whole}{fraction}")
        .parse::<u128>()
        .map_err(|_| "decimal value is out of range")?;
    let power = exponent - fraction.len() as i32;
    let integer = if power >= 0 {
        magnitude
            .checked_mul(
                10_u128
                    .checked_pow(power as u32)
                    .ok_or("decimal value is out of range")?,
            )
            .ok_or("decimal value is out of range")?
    } else {
        let places = (-power) as u32;
        if places > 38 {
            if magnitude == 0 {
                0
            } else {
                return Err("decimal value is not an integer");
            }
        } else {
            let divisor = 10_u128.pow(places);
            if magnitude % divisor != 0 {
                return Err("decimal value is not an integer");
            }
            magnitude / divisor
        }
    };
    if negative {
        let min_magnitude = (i128::MAX as u128) + 1;
        if integer == min_magnitude {
            Ok(i128::MIN)
        } else {
            i128::try_from(integer)
                .map(|value| -value)
                .map_err(|_| "decimal value is out of range")
        }
    } else {
        i128::try_from(integer).map_err(|_| "decimal value is out of range")
    }
}

/// 解析十进制字符串为定点原始值（含 9 位小数）。
pub(crate) fn parse_dec(s: &str) -> Option<i128> {
    let s = s.trim();
    let (neg, body) = match s.strip_prefix('-') {
        Some(b) => (true, b),
        None => (false, s),
    };
    let mut it = body.split('.');
    let int_part = it.next()?;
    let frac_part = it.next().unwrap_or("");
    if it.next().is_some() {
        return None;
    }
    if int_part.is_empty() && frac_part.is_empty() {
        return None;
    }
    if !int_part.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    if !frac_part.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    if frac_part.len() > 9 {
        return None;
    }
    let mut v: i128 = if int_part.is_empty() {
        0
    } else {
        int_part.parse().ok()?
    };
    v = v.checked_mul(SCALE)?;
    if !frac_part.is_empty() {
        let mut f: i128 = frac_part.parse().ok()?;
        for _ in frac_part.len()..9 {
            f *= 10;
        }
        v = v.checked_add(f)?;
    }
    Some(if neg { -v } else { v })
}

fn fmt_dec(raw: i128, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let neg = raw < 0;
    let raw = raw.unsigned_abs();
    let scale = SCALE as u128;
    let int = raw / scale;
    let frac = raw % scale;
    let mut s = format!("{:09}", frac);
    while s.ends_with('0') {
        s.pop();
    }
    let sign = if neg { "-" } else { "" };
    if s.is_empty() {
        write!(f, "{}{}", sign, int)
    } else {
        write!(f, "{}{}.{}", sign, int, s)
    }
}

macro_rules! impl_fixed {
    ($name:ident, $doc:expr) => {
        #[doc = $doc]
        #[derive(
            Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
        )]
        pub struct $name(pub i128);

        impl $name {
            pub const ZERO: Self = Self(0);

            pub const fn from_raw(raw: i128) -> Self {
                Self(raw)
            }

            pub const fn raw(self) -> i128 {
                self.0
            }

            pub fn from_i64(v: i64) -> Self {
                Self(v as i128 * SCALE)
            }

            /// 从十进制字符串构造，如 `"123.45"`。
            pub fn from_dec(s: &str) -> Option<Self> {
                parse_dec(s).map(Self)
            }

            pub fn checked_add(self, o: Self) -> Option<Self> {
                self.0.checked_add(o.0).map(Self)
            }

            pub fn checked_sub(self, o: Self) -> Option<Self> {
                self.0.checked_sub(o.0).map(Self)
            }

            /// 定点乘法：结果 = (a * b) / SCALE
            pub fn checked_mul(self, o: Self) -> Option<Self> {
                self.0.checked_mul(o.0).map(|v| Self(v / SCALE))
            }

            /// 定点除法：结果 = (a * SCALE) / b
            pub fn checked_div(self, o: Self) -> Option<Self> {
                if o.0 == 0 {
                    return None;
                }
                self.0.checked_mul(SCALE).map(|v| Self(v / o.0))
            }

            pub fn is_zero(self) -> bool {
                self.0 == 0
            }

            pub fn checked_abs(self) -> Option<Self> {
                self.0.checked_abs().map(Self)
            }

            /// 仅用于不会把极值当作合法订单量的展示/兼容路径；需要严格语义时使用
            /// `checked_abs`，避免 i128::MIN 在热路径触发 panic。
            pub fn abs(self) -> Self {
                Self(self.0.saturating_abs())
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt_dec(self.0, f)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt_dec(self.0, f)
            }
        }
    };
}

impl_fixed!(Fixed, "通用定点数（9 位小数）");
impl_fixed!(Price, "价格");
impl_fixed!(Quantity, "数量");
impl_fixed!(Money, "金额");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_display_roundtrip() {
        let p = Price::from_dec("123.45").unwrap();
        assert_eq!(p.raw(), 123_450_000_000);
        assert_eq!(format!("{}", p), "123.45");
    }

    #[test]
    fn negative_value() {
        let m = Money::from_dec("-0.000000001").unwrap();
        assert_eq!(m.raw(), -1);
        assert_eq!(format!("{}", m), "-0.000000001");
    }

    #[test]
    fn fixed_point_mul_div() {
        let a = Price::from_dec("2.5").unwrap();
        let b = Quantity::from_dec("4").unwrap();
        let notional = Money::from_raw(
            Fixed::from_raw(a.raw())
                .checked_mul(Fixed::from_raw(b.raw()))
                .unwrap()
                .raw(),
        );
        assert_eq!(format!("{}", notional), "10");
    }

    #[test]
    fn rejects_over_precision() {
        assert!(Price::from_dec("1.1234567891").is_none());
    }

    #[test]
    fn exact_decimal_parser_accepts_exponents_and_rejects_rounding() {
        assert_eq!(parse_decimal_exact("  +1.25e2 "), Ok(125 * SCALE));
        assert_eq!(parse_decimal_exact("1.23e-2"), Ok(12_300_000));
        assert_eq!(parse_decimal_exact("-0.000000001"), Ok(-1));
        assert_eq!(parse_decimal_exact("1.0000000000"), Ok(SCALE));
        assert_eq!(
            parse_decimal_exact("1.0000000001"),
            Err("decimal precision exceeds 1e-9")
        );
    }

    #[test]
    fn exact_decimal_parser_checks_sign_syntax_and_i128_bounds() {
        assert_eq!(parse_decimal_exact(""), Err("empty decimal value"));
        assert_eq!(parse_decimal_exact("NaN"), Err("invalid decimal value"));
        assert_eq!(
            parse_decimal_exact("1e101"),
            Err("decimal exponent is out of range")
        );
        assert_eq!(
            parse_decimal_exact("170141183460469231731687303715.884105727"),
            Ok(i128::MAX)
        );
        assert_eq!(
            parse_decimal_exact("-170141183460469231731687303715.884105728"),
            Ok(i128::MIN)
        );
        assert_eq!(
            parse_decimal_exact("170141183460469231731687303715.884105728"),
            Err("decimal value is out of range")
        );
    }

    #[test]
    fn decimal_to_integer_units_preserves_exact_basis_point_semantics() {
        assert_eq!(parse_decimal_scaled_integer("0.005", 10_000), Ok(50));
        assert_eq!(parse_decimal_scaled_integer("0.5", 100), Ok(50));
        assert_eq!(parse_decimal_scaled_integer("-0.0001", 10_000), Ok(-1));
        assert_eq!(
            parse_decimal_scaled_integer("0.00001", 10_000),
            Err("decimal value exceeds integer unit precision")
        );
        assert_eq!(
            parse_decimal_scaled_integer("1", 0),
            Err("decimal multiplier must be positive")
        );
    }

    #[test]
    fn exact_integer_parser_rejects_fraction_and_preserves_i128_range() {
        assert_eq!(parse_decimal_integer_exact("1.0"), Ok(1));
        assert_eq!(parse_decimal_integer_exact("1e3"), Ok(1_000));
        assert_eq!(
            parse_decimal_integer_exact("1.1"),
            Err("decimal value is not an integer")
        );
        assert_eq!(
            parse_decimal_integer_exact("170141183460469231731687303715884105727"),
            Ok(i128::MAX)
        );
    }

    #[test]
    fn extreme_absolute_value_is_checked_without_panicking() {
        let value = Fixed::from_raw(i128::MIN);
        assert!(value.checked_abs().is_none());
        assert_eq!(value.abs().raw(), i128::MAX);
        assert!(format!("{}", value).starts_with('-'));
    }
}
