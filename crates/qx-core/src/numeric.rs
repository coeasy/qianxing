//! 定点数值：128-bit，scale = 1e9。
//!
//! 为什么禁浮点：IEEE-754 的 NaN 位模式在不同硬件/编译优化下不确定，
//! 会直接破坏 bit-level 可重放。回测的确定性优先于浮点便利。

use serde::{Deserialize, Serialize};
use std::fmt;

/// 定点小数位数：1e9，即 9 位小数。
pub const SCALE: i128 = 1_000_000_000;

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
    fn extreme_absolute_value_is_checked_without_panicking() {
        let value = Fixed::from_raw(i128::MIN);
        assert!(value.checked_abs().is_none());
        assert_eq!(value.abs().raw(), i128::MAX);
        assert!(format!("{}", value).starts_with('-'));
    }
}
