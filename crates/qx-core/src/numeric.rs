//! 定点数值：128-bit，scale = 1e9。
//!
//! 为什么禁浮点：IEEE-754 的 NaN 位模式在不同硬件/编译优化下不确定，
//! 会直接破坏 bit-level 可重放。回测的确定性优先于浮点便利。

use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::fmt;

mod rounding;
pub use rounding::parse_decimal_half_even;

/// 定点小数位数：1e9，即 9 位小数。
pub const SCALE: i128 = 1_000_000_000;

/// 可用于版本化精度边界的精确十进制定点值。
///
/// 现有 `Price` / `Quantity` / `Money` 和所有历史 `*_raw` 字段仍保持 1e9 语义；
/// 这个类型明确携带小数位数，避免把更细的交易所数值误解释成旧 raw 单位。
/// 当前上限为 18 位。迁移到新的订单、账簿和事件协议前，旧接口必须通过
/// [`ExactDecimal::to_raw`] 精确转换；不能精确表示时会失败，不会舍入。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct ExactDecimal {
    coefficient: i128,
    scale: u8,
}

/// Deterministic rounding used only when an exact rational result is projected onto a
/// declared decimal scale. Trading-rule checks should use `AwayFromZero` for positive
/// amounts that must not be understated; order formatting should use `TowardZero` only
/// when the caller separately verifies that the venue value is unchanged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecimalRounding {
    TowardZero,
    AwayFromZero,
    Floor,
    Ceiling,
    HalfEven,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExactDecimalWire {
    coefficient: String,
    scale: u8,
}

impl Serialize for ExactDecimal {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;

        let mut value = serializer.serialize_struct("ExactDecimal", 2)?;
        // JSON number consumers such as JavaScript cannot represent every i128 exactly.
        // The new scale-aware wire representation therefore carries its coefficient as text.
        value.serialize_field("coefficient", &self.coefficient.to_string())?;
        value.serialize_field("scale", &self.scale)?;
        value.end()
    }
}

impl<'de> Deserialize<'de> for ExactDecimal {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error;

        let wire = ExactDecimalWire::deserialize(deserializer)?;
        let coefficient = wire
            .coefficient
            .parse::<i128>()
            .map_err(|_| D::Error::custom("decimal coefficient is outside i128 range"))?;
        let value = Self::from_parts(coefficient, wire.scale).map_err(D::Error::custom)?;
        if value.coefficient != coefficient || value.scale != wire.scale {
            return Err(D::Error::custom("decimal encoding must be normalized"));
        }
        Ok(value)
    }
}

impl ExactDecimal {
    pub const MAX_SCALE: u8 = 18;
    pub const ZERO: Self = Self {
        coefficient: 0,
        scale: 0,
    };
    pub const ONE: Self = Self {
        coefficient: 1,
        scale: 0,
    };

    /// 构造并规范化十进制值；尾随零会被去掉，零统一编码为 scale=0。
    pub fn from_parts(coefficient: i128, scale: u8) -> Result<Self, &'static str> {
        Self::normalized(coefficient, u32::from(scale))
    }

    fn normalized(mut coefficient: i128, mut scale: u32) -> Result<Self, &'static str> {
        while scale > 0 && coefficient % 10 == 0 {
            coefficient /= 10;
            scale -= 1;
        }
        if scale > u32::from(Self::MAX_SCALE) {
            return Err("decimal precision exceeds supported 18 places");
        }
        Ok(if coefficient == 0 {
            Self::ZERO
        } else {
            Self {
                coefficient,
                scale: scale as u8,
            }
        })
    }

    /// Parse plain or scientific decimal notation without using binary floats.
    pub fn parse(input: &str) -> Result<Self, &'static str> {
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
        if magnitude == 0 {
            return Ok(Self::ZERO);
        }

        let mut scale = fraction.len() as i32 - exponent;
        let magnitude = if scale < 0 {
            let factor = 10_u128
                .checked_pow((-scale) as u32)
                .ok_or("decimal value is out of range")?;
            scale = 0;
            magnitude
                .checked_mul(factor)
                .ok_or("decimal value is out of range")?
        } else {
            magnitude
        };
        let mut magnitude = magnitude;
        while scale > i32::from(Self::MAX_SCALE) && magnitude % 10 == 0 {
            magnitude /= 10;
            scale -= 1;
        }
        if scale > i32::from(Self::MAX_SCALE) {
            return Err("decimal precision exceeds supported 18 places");
        }

        let signed = if negative {
            let min_magnitude = (i128::MAX as u128) + 1;
            if magnitude == min_magnitude {
                i128::MIN
            } else {
                -i128::try_from(magnitude).map_err(|_| "decimal value is out of range")?
            }
        } else {
            i128::try_from(magnitude).map_err(|_| "decimal value is out of range")?
        };
        Self::normalized(signed, scale as u32)
    }

    pub const fn coefficient(self) -> i128 {
        self.coefficient
    }

    pub const fn scale(self) -> u8 {
        self.scale
    }

    pub const fn is_zero(self) -> bool {
        self.coefficient == 0
    }

    pub const fn is_positive(self) -> bool {
        self.coefficient > 0
    }

    pub const fn is_negative(self) -> bool {
        self.coefficient < 0
    }

    pub fn checked_neg(self) -> Option<Self> {
        Some(Self {
            coefficient: self.coefficient.checked_neg()?,
            scale: self.scale,
        })
    }

    /// Compare canonical decimals without rescaling into a potentially overflowing i128.
    pub fn checked_cmp(self, other: Self) -> Option<Ordering> {
        let left_sign = self.coefficient.signum();
        let right_sign = other.coefficient.signum();
        match left_sign.cmp(&right_sign) {
            Ordering::Equal if left_sign == 0 => Some(Ordering::Equal),
            Ordering::Equal => {
                let left = self.coefficient.unsigned_abs().to_string();
                let right = other.coefficient.unsigned_abs().to_string();
                let left_integer_digits = left.len() as i16 - i16::from(self.scale);
                let right_integer_digits = right.len() as i16 - i16::from(other.scale);
                let magnitude = left_integer_digits
                    .cmp(&right_integer_digits)
                    .then_with(|| {
                        let scale = self.scale.max(other.scale);
                        let left_aligned =
                            format!("{}{}", left, "0".repeat(usize::from(scale - self.scale)));
                        let right_aligned =
                            format!("{}{}", right, "0".repeat(usize::from(scale - other.scale)));
                        left_aligned.cmp(&right_aligned)
                    });
                Some(if left_sign < 0 {
                    magnitude.reverse()
                } else {
                    magnitude
                })
            }
            ordering => Some(ordering),
        }
    }

    /// Convert to the requested fixed scale only when the value is exact.
    pub fn to_raw(self, target_scale: u8) -> Result<i128, &'static str> {
        if target_scale > Self::MAX_SCALE {
            return Err("target decimal scale exceeds supported 18 places");
        }
        if target_scale >= self.scale {
            let factor = 10_i128
                .checked_pow(u32::from(target_scale - self.scale))
                .ok_or("decimal value is out of range")?;
            self.coefficient
                .checked_mul(factor)
                .ok_or("decimal value is out of range")
        } else {
            let divisor = 10_i128
                .checked_pow(u32::from(self.scale - target_scale))
                .ok_or("decimal value is out of range")?;
            if self.coefficient % divisor != 0 {
                return Err("decimal precision exceeds target scale");
            }
            Ok(self.coefficient / divisor)
        }
    }

    /// Project to a fixed raw scale by truncating excess fractional digits toward zero.
    ///
    /// Use this only for compatibility projections that retain the source `ExactDecimal`
    /// alongside the raw value. Financial decisions should continue using the exact value.
    pub fn to_raw_truncated(self, target_scale: u8) -> Result<i128, &'static str> {
        if target_scale > Self::MAX_SCALE {
            return Err("target decimal scale exceeds supported 18 places");
        }
        if target_scale >= self.scale {
            return self.to_raw(target_scale);
        }
        let divisor = 10_i128
            .checked_pow(u32::from(self.scale - target_scale))
            .ok_or("decimal value is out of range")?;
        Ok(self.coefficient / divisor)
    }

    pub fn checked_add(self, other: Self) -> Option<Self> {
        self.checked_add_with_sign(other, false)
    }

    pub fn checked_sub(self, other: Self) -> Option<Self> {
        self.checked_add_with_sign(other, true)
    }

    fn checked_add_with_sign(self, other: Self, subtract_other: bool) -> Option<Self> {
        let scale = self.scale.max(other.scale);
        let left = format!(
            "{}{}",
            self.coefficient.unsigned_abs(),
            "0".repeat(usize::from(scale - self.scale))
        );
        let right = format!(
            "{}{}",
            other.coefficient.unsigned_abs(),
            "0".repeat(usize::from(scale - other.scale))
        );
        let left_negative = self.coefficient < 0;
        let right_negative = (other.coefficient < 0) ^ subtract_other;
        let (digits, negative) = if left_negative == right_negative {
            (add_unsigned_decimal_strings(&left, &right), left_negative)
        } else {
            match compare_unsigned_decimal_strings(&left, &right) {
                Ordering::Greater => (
                    subtract_unsigned_decimal_strings(&left, &right),
                    left_negative,
                ),
                Ordering::Less => (
                    subtract_unsigned_decimal_strings(&right, &left),
                    right_negative,
                ),
                Ordering::Equal => return Some(Self::ZERO),
            }
        };
        Self::from_signed_decimal_digits(digits, u32::from(scale), negative)
    }

    fn from_signed_decimal_digits(
        mut digits: String,
        mut scale: u32,
        negative: bool,
    ) -> Option<Self> {
        let first_nonzero = digits.bytes().position(|byte| byte != b'0');
        let Some(first_nonzero) = first_nonzero else {
            return Some(Self::ZERO);
        };
        if first_nonzero > 0 {
            digits.drain(..first_nonzero);
        }
        while scale > 0 && digits.ends_with('0') {
            digits.pop();
            scale -= 1;
        }
        if scale > u32::from(Self::MAX_SCALE) {
            return None;
        }
        let magnitude = digits.parse::<u128>().ok()?;
        let coefficient = if negative {
            let min_magnitude = (i128::MAX as u128) + 1;
            if magnitude == min_magnitude {
                i128::MIN
            } else {
                -i128::try_from(magnitude).ok()?
            }
        } else {
            i128::try_from(magnitude).ok()?
        };
        Some(Self {
            coefficient,
            scale: scale as u8,
        })
    }

    pub fn checked_mul(self, other: Self) -> Option<Self> {
        let mut digits = multiply_unsigned_decimal_strings(
            &self.coefficient.unsigned_abs().to_string(),
            &other.coefficient.unsigned_abs().to_string(),
        );
        let mut scale = u32::from(self.scale) + u32::from(other.scale);
        while scale > 0 && digits.ends_with('0') {
            digits.pop();
            scale -= 1;
        }
        if scale > u32::from(Self::MAX_SCALE) {
            return None;
        }
        let magnitude = digits.parse::<u128>().ok()?;
        let negative = (self.coefficient < 0) != (other.coefficient < 0);
        let coefficient = if negative {
            let min_magnitude = (i128::MAX as u128) + 1;
            if magnitude == min_magnitude {
                i128::MIN
            } else {
                -i128::try_from(magnitude).ok()?
            }
        } else {
            i128::try_from(magnitude).ok()?
        };
        Some(Self {
            coefficient,
            scale: scale as u8,
        })
    }

    /// Multiply and project onto `target_scale` without floating point or overflowing an
    /// intermediate i128. Any discarded fraction follows the explicit rounding policy.
    pub fn checked_mul_at_scale(
        self,
        other: Self,
        target_scale: u8,
        rounding: DecimalRounding,
    ) -> Option<Self> {
        if target_scale > Self::MAX_SCALE {
            return None;
        }
        let negative = (self.coefficient < 0) != (other.coefficient < 0);
        let mut digits = multiply_unsigned_decimal_strings(
            &self.coefficient.unsigned_abs().to_string(),
            &other.coefficient.unsigned_abs().to_string(),
        );
        let product_scale = u16::from(self.scale) + u16::from(other.scale);
        let target = u16::from(target_scale);
        if product_scale < target {
            digits.push_str(&"0".repeat(usize::from(target - product_scale)));
        } else if product_scale > target {
            let divisor = format!("1{}", "0".repeat(usize::from(product_scale - target)));
            let (quotient, remainder) = divide_unsigned_decimal_strings(&digits, &divisor)?;
            let round_up = should_round_up(&quotient, &remainder, &divisor, negative, rounding)?;
            digits = if round_up {
                add_unsigned_decimal_strings(&quotient, "1")
            } else {
                quotient
            };
        }
        Self::from_signed_decimal_digits(digits, u32::from(target_scale), negative)
    }

    /// Divide and project onto `target_scale` using decimal-string long division. This is
    /// deterministic for values whose intermediate numerator would overflow i128 as well
    /// as for ordinary exchange precisions.
    pub fn checked_div_at_scale(
        self,
        other: Self,
        target_scale: u8,
        rounding: DecimalRounding,
    ) -> Option<Self> {
        if other.coefficient == 0 || target_scale > Self::MAX_SCALE {
            return None;
        }
        let negative = (self.coefficient < 0) != (other.coefficient < 0);
        let mut numerator = self.coefficient.unsigned_abs().to_string();
        let mut denominator = other.coefficient.unsigned_abs().to_string();
        let shift = i16::from(target_scale) + i16::from(other.scale) - i16::from(self.scale);
        if shift >= 0 {
            numerator.push_str(&"0".repeat(shift as usize));
        } else {
            denominator.push_str(&"0".repeat((-shift) as usize));
        }
        let (mut quotient, remainder) = divide_unsigned_decimal_strings(&numerator, &denominator)?;
        if should_round_up(&quotient, &remainder, &denominator, negative, rounding)? {
            quotient = add_unsigned_decimal_strings(&quotient, "1");
        }
        Self::from_signed_decimal_digits(quotient, u32::from(target_scale), negative)
    }

    /// Whether this value is an exact integer multiple of a positive decimal step.
    pub fn is_multiple_of(self, step: Self) -> Option<bool> {
        if !step.is_positive() {
            return None;
        }
        let mut numerator = self.coefficient.unsigned_abs().to_string();
        let mut denominator = step.coefficient.unsigned_abs().to_string();
        let shift = i16::from(step.scale) - i16::from(self.scale);
        if shift >= 0 {
            numerator.push_str(&"0".repeat(shift as usize));
        } else {
            denominator.push_str(&"0".repeat((-shift) as usize));
        }
        let (_, remainder) = divide_unsigned_decimal_strings(&numerator, &denominator)?;
        Some(remainder == "0")
    }
}

fn should_round_up(
    quotient: &str,
    remainder: &str,
    denominator: &str,
    negative: bool,
    rounding: DecimalRounding,
) -> Option<bool> {
    if remainder == "0" {
        return Some(false);
    }
    Some(match rounding {
        DecimalRounding::TowardZero => false,
        DecimalRounding::AwayFromZero => true,
        DecimalRounding::Floor => negative,
        DecimalRounding::Ceiling => !negative,
        DecimalRounding::HalfEven => {
            let doubled_remainder = add_unsigned_decimal_strings(remainder, remainder);
            match compare_unsigned_decimal_strings(&doubled_remainder, denominator) {
                Ordering::Greater => true,
                Ordering::Less => false,
                Ordering::Equal => quotient
                    .as_bytes()
                    .last()
                    .is_some_and(|digit| (digit - b'0') % 2 == 1),
            }
        }
    })
}

/// Unsigned decimal-string division. Returns normalized quotient and remainder strings;
/// no intermediate is converted to a machine integer.
fn divide_unsigned_decimal_strings(numerator: &str, denominator: &str) -> Option<(String, String)> {
    let numerator = numerator.trim_start_matches('0');
    let denominator = denominator.trim_start_matches('0');
    if denominator.is_empty() {
        return None;
    }
    if numerator.is_empty() {
        return Some(("0".into(), "0".into()));
    }

    let mut quotient = String::with_capacity(numerator.len());
    let mut remainder = String::from("0");
    for digit in numerator.bytes() {
        if remainder == "0" {
            remainder.clear();
        }
        remainder.push(char::from(digit));
        let trimmed = remainder.trim_start_matches('0');
        remainder = if trimmed.is_empty() {
            String::from("0")
        } else {
            trimmed.to_string()
        };
        let mut quotient_digit = 0_u8;
        while quotient_digit < 9 {
            let next = multiply_unsigned_decimal_string_by_digit(denominator, quotient_digit + 1);
            if compare_unsigned_decimal_strings(&next, &remainder) == Ordering::Greater {
                break;
            }
            quotient_digit += 1;
        }
        if quotient_digit > 0 {
            let product = multiply_unsigned_decimal_string_by_digit(denominator, quotient_digit);
            remainder = subtract_unsigned_decimal_strings(&remainder, &product);
        }
        quotient.push(char::from(b'0' + quotient_digit));
    }
    let quotient = quotient.trim_start_matches('0');
    Some((
        if quotient.is_empty() { "0" } else { quotient }.into(),
        remainder,
    ))
}

fn multiply_unsigned_decimal_string_by_digit(value: &str, digit: u8) -> String {
    if digit == 0 || value == "0" {
        return String::from("0");
    }
    let mut output = Vec::with_capacity(value.len() + 1);
    let mut carry = 0_u16;
    for byte in value.bytes().rev() {
        let product = u16::from(byte - b'0') * u16::from(digit) + carry;
        output.push((product % 10) as u8);
        carry = product / 10;
    }
    while carry > 0 {
        output.push((carry % 10) as u8);
        carry /= 10;
    }
    output
        .iter()
        .rev()
        .map(|value| char::from(b'0' + *value))
        .collect()
}

/// Multiply two non-negative decimal integer strings without overflowing an i128
/// intermediate. `ExactDecimal::checked_mul` normalizes trailing zeroes before it
/// checks the final coefficient range, so a product that is representable after
/// normalization is not incorrectly rejected just because the unnormalized product
/// briefly needs more than 128 bits.
fn multiply_unsigned_decimal_strings(left: &str, right: &str) -> String {
    let left = left
        .bytes()
        .rev()
        .map(|byte| u32::from(byte - b'0'))
        .collect::<Vec<_>>();
    let right = right
        .bytes()
        .rev()
        .map(|byte| u32::from(byte - b'0'))
        .collect::<Vec<_>>();
    let mut product = vec![0_u32; left.len() + right.len()];
    for (left_index, left_digit) in left.iter().copied().enumerate() {
        for (right_index, right_digit) in right.iter().copied().enumerate() {
            product[left_index + right_index] += left_digit * right_digit;
        }
    }
    for index in 0..product.len() - 1 {
        let carry = product[index] / 10;
        product[index] %= 10;
        product[index + 1] += carry;
    }
    while product.len() > 1 && product.last() == Some(&0) {
        product.pop();
    }
    product
        .iter()
        .rev()
        .map(|digit| char::from(b'0' + *digit as u8))
        .collect()
}

fn compare_unsigned_decimal_strings(left: &str, right: &str) -> Ordering {
    let left = left.trim_start_matches('0');
    let right = right.trim_start_matches('0');
    left.len().cmp(&right.len()).then_with(|| left.cmp(right))
}

fn add_unsigned_decimal_strings(left: &str, right: &str) -> String {
    let left = left
        .bytes()
        .rev()
        .map(|byte| byte - b'0')
        .collect::<Vec<_>>();
    let right = right
        .bytes()
        .rev()
        .map(|byte| byte - b'0')
        .collect::<Vec<_>>();
    let mut result = Vec::with_capacity(left.len().max(right.len()) + 1);
    let mut carry = 0_u8;
    for index in 0..left.len().max(right.len()) {
        let sum =
            left.get(index).copied().unwrap_or(0) + right.get(index).copied().unwrap_or(0) + carry;
        result.push(sum % 10);
        carry = sum / 10;
    }
    if carry > 0 {
        result.push(carry);
    }
    result
        .iter()
        .rev()
        .map(|digit| char::from(b'0' + *digit))
        .collect()
}

/// Subtract two non-negative decimal integer strings where `left >= right`.
fn subtract_unsigned_decimal_strings(left: &str, right: &str) -> String {
    let left = left
        .bytes()
        .rev()
        .map(|byte| byte - b'0')
        .collect::<Vec<_>>();
    let right = right
        .bytes()
        .rev()
        .map(|byte| byte - b'0')
        .collect::<Vec<_>>();
    let mut result = Vec::with_capacity(left.len());
    let mut borrow = 0_i16;
    for (index, left_digit) in left.iter().copied().enumerate() {
        let mut difference =
            i16::from(left_digit) - borrow - i16::from(right.get(index).copied().unwrap_or(0));
        if difference < 0 {
            difference += 10;
            borrow = 1;
        } else {
            borrow = 0;
        }
        result.push(difference as u8);
    }
    while result.len() > 1 && result.last() == Some(&0) {
        result.pop();
    }
    result
        .iter()
        .rev()
        .map(|digit| char::from(b'0' + *digit))
        .collect()
}

impl fmt::Display for ExactDecimal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.scale == 0 {
            return write!(f, "{}", self.coefficient);
        }
        let negative = self.coefficient < 0;
        let magnitude = self.coefficient.unsigned_abs();
        let divisor = 10_u128.pow(u32::from(self.scale));
        let whole = magnitude / divisor;
        let fraction = magnitude % divisor;
        let mut digits = format!("{fraction:0width$}", width = usize::from(self.scale));
        while digits.ends_with('0') {
            digits.pop();
        }
        write!(f, "{}{}.{}", if negative { "-" } else { "" }, whole, digits)
    }
}

/// Parse a decimal string into the fixed-point scale without rounding.
///
/// Scientific notation is accepted because exchange APIs commonly emit it. Any
/// non-zero precision beyond nine decimal places is rejected so callers cannot
/// silently change a price, quantity, or balance during normalization.
pub fn parse_decimal_exact(input: &str) -> Result<i128, &'static str> {
    ExactDecimal::parse(input)
        .and_then(|decimal| decimal.to_raw(9))
        .map_err(|error| match error {
            "decimal precision exceeds supported 18 places"
            | "decimal precision exceeds target scale" => "decimal precision exceeds 1e-9",
            other => other,
        })
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
    fn exact_decimal_preserves_precision_beyond_legacy_scale() {
        let amount = ExactDecimal::parse("0.000000000123456789").unwrap();
        assert_eq!(amount.coefficient(), 123_456_789);
        assert_eq!(amount.scale(), 18);
        assert_eq!(amount.to_string(), "0.000000000123456789");
        assert_eq!(amount.to_raw(18), Ok(123_456_789));
        assert_eq!(
            amount.to_raw(9),
            Err("decimal precision exceeds target scale")
        );
        assert_eq!(
            parse_decimal_exact("0.000000000123456789"),
            Err("decimal precision exceeds 1e-9")
        );
        assert_eq!(amount.to_raw_truncated(9), Ok(0));
        assert_eq!(
            ExactDecimal::parse("-0.0000000019")
                .unwrap()
                .to_raw_truncated(9),
            Ok(-1)
        );
    }

    #[test]
    fn exact_decimal_normalizes_exponents_and_trailing_zeroes() {
        let one = ExactDecimal::parse("1000e-3").unwrap();
        assert_eq!(one, ExactDecimal::parse("1.0000").unwrap());
        assert_eq!(one.scale(), 0);
        assert_eq!(one.to_string(), "1");

        let subnano = ExactDecimal::parse("1.234567890000000000e-9").unwrap();
        assert_eq!(subnano.to_string(), "0.00000000123456789");
        assert_eq!(
            ExactDecimal::parse("1.0000000000000000000")
                .unwrap()
                .to_string(),
            "1"
        );
    }

    #[test]
    fn exact_decimal_arithmetic_aligns_scales_without_rounding() {
        let left = ExactDecimal::parse("0.0000000001").unwrap();
        let right = ExactDecimal::parse("0.00000000002").unwrap();
        assert_eq!(
            left.checked_add(right).unwrap().to_string(),
            "0.00000000012"
        );
        assert_eq!(
            left.checked_sub(right).unwrap().to_string(),
            "0.00000000008"
        );
        assert_eq!(
            ExactDecimal::parse("0.00000001")
                .unwrap()
                .checked_mul(ExactDecimal::parse("0.00000002").unwrap())
                .unwrap()
                .to_string(),
            "0.0000000000000002"
        );
    }

    #[test]
    fn exact_decimal_scaled_division_applies_explicit_signed_rounding() {
        let one = ExactDecimal::parse("1").unwrap();
        let three = ExactDecimal::parse("3").unwrap();
        let third = one
            .checked_div_at_scale(three, 4, DecimalRounding::TowardZero)
            .unwrap();
        assert_eq!(third.to_string(), "0.3333");
        assert_eq!(
            one.checked_div_at_scale(three, 4, DecimalRounding::AwayFromZero)
                .unwrap()
                .to_string(),
            "0.3334"
        );
        assert_eq!(
            one.checked_div_at_scale(three, 4, DecimalRounding::Ceiling)
                .unwrap()
                .to_string(),
            "0.3334"
        );
        assert_eq!(
            one.checked_div_at_scale(three, 4, DecimalRounding::Floor)
                .unwrap()
                .to_string(),
            "0.3333"
        );

        let negative_one = ExactDecimal::parse("-1").unwrap();
        assert_eq!(
            negative_one
                .checked_div_at_scale(three, 4, DecimalRounding::Floor)
                .unwrap()
                .to_string(),
            "-0.3334"
        );
        assert_eq!(
            negative_one
                .checked_div_at_scale(three, 4, DecimalRounding::Ceiling)
                .unwrap()
                .to_string(),
            "-0.3333"
        );
    }

    #[test]
    fn exact_decimal_scaled_multiplication_and_half_even_are_deterministic() {
        let first = ExactDecimal::parse("1.245").unwrap();
        let second = ExactDecimal::parse("1.255").unwrap();
        assert_eq!(
            first
                .checked_mul_at_scale(
                    ExactDecimal::parse("1").unwrap(),
                    2,
                    DecimalRounding::HalfEven,
                )
                .unwrap()
                .to_string(),
            "1.24"
        );
        assert_eq!(
            second
                .checked_mul_at_scale(
                    ExactDecimal::parse("1").unwrap(),
                    2,
                    DecimalRounding::HalfEven,
                )
                .unwrap()
                .to_string(),
            "1.26"
        );
        assert_eq!(
            ExactDecimal::parse("0.0000000001")
                .unwrap()
                .checked_mul_at_scale(
                    ExactDecimal::parse("0.1").unwrap(),
                    10,
                    DecimalRounding::AwayFromZero,
                )
                .unwrap()
                .to_string(),
            "0.0000000001"
        );
    }

    #[test]
    fn exact_decimal_checks_exchange_steps_without_rescaling_to_nine_places() {
        let step = ExactDecimal::parse("0.0000000001").unwrap();
        assert_eq!(
            ExactDecimal::parse("0.0000000003")
                .unwrap()
                .is_multiple_of(step),
            Some(true)
        );
        assert_eq!(
            ExactDecimal::parse("0.00000000031")
                .unwrap()
                .is_multiple_of(step),
            Some(false)
        );
        assert_eq!(
            ExactDecimal::parse("1")
                .unwrap()
                .is_multiple_of(ExactDecimal::ZERO),
            None
        );
    }

    #[test]
    fn exact_decimal_multiplication_normalizes_before_checking_i128_overflow() {
        let value = ExactDecimal::from_parts(i128::MAX, 18).unwrap();
        let ten = ExactDecimal::from_parts(10, 0).unwrap();
        assert_eq!(
            value.checked_mul(ten),
            Some(ExactDecimal::from_parts(i128::MAX, 17).unwrap())
        );

        let negative_min = ExactDecimal::from_parts(i128::MIN, 18).unwrap();
        assert_eq!(
            negative_min.checked_mul(ten),
            Some(ExactDecimal::from_parts(i128::MIN, 17).unwrap())
        );

        assert_eq!(
            ExactDecimal::parse("0.00000001")
                .unwrap()
                .checked_mul(ExactDecimal::parse("0.00000002").unwrap())
                .unwrap()
                .to_string(),
            "0.0000000000000002"
        );
    }

    #[test]
    fn exact_decimal_addition_normalizes_wide_intermediates_and_handles_i128_min() {
        let left = ExactDecimal::from_parts(i128::MAX, 18).unwrap();
        let right = ExactDecimal::from_parts(i128::MAX - 4, 18).unwrap();
        assert_eq!(
            left.checked_add(right),
            Some(ExactDecimal::parse("340282366920938463463.37460743176821145").unwrap())
        );

        let minimum = ExactDecimal::from_parts(i128::MIN, 0).unwrap();
        let minus_one = ExactDecimal::from_parts(-1, 0).unwrap();
        assert_eq!(
            minimum.checked_sub(minus_one),
            Some(ExactDecimal::from_parts(i128::MIN + 1, 0).unwrap())
        );
        assert_eq!(
            minimum.checked_add(ExactDecimal::from_parts(i128::MAX, 0).unwrap()),
            Some(ExactDecimal::from_parts(-1, 0).unwrap())
        );
    }

    #[test]
    fn exact_decimal_json_roundtrip_validates_scale() {
        let value = ExactDecimal::parse("-0.000000000123456789").unwrap();
        let json = serde_json::to_string(&value).unwrap();
        assert!(json.contains(r#""coefficient":"-123456789""#));
        assert_eq!(serde_json::from_str::<ExactDecimal>(&json).unwrap(), value);
        assert!(serde_json::from_str::<ExactDecimal>(r#"{"coefficient":"1","scale":19}"#).is_err());
        assert!(serde_json::from_str::<ExactDecimal>(r#"{"coefficient":"10","scale":1}"#).is_err());
    }

    #[test]
    fn json_i128_raw_values_roundtrip_without_float_conversion() {
        #[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
        struct RawAmount {
            qty_raw: i128,
        }

        let samples = [
            u64::MAX as i128,
            u64::MAX as i128 + 1,
            100_000_000_000_000_000_001_i128,
            i128::MAX,
            i128::MIN,
        ];
        for qty_raw in samples {
            let expected = RawAmount { qty_raw };
            let json = serde_json::to_string(&expected).unwrap();
            assert_eq!(json, format!(r#"{{"qty_raw":{qty_raw}}}"#));
            assert_eq!(serde_json::from_str::<RawAmount>(&json).unwrap(), expected);

            let value: serde_json::Value = serde_json::from_str(&json).unwrap();
            assert_eq!(value["qty_raw"].to_string(), qty_raw.to_string());
            assert_eq!(
                serde_json::from_value::<RawAmount>(value).unwrap(),
                expected
            );
        }
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
