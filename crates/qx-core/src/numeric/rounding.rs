//! Compatibility decimal parser for provider values that use half-even rounding.

/// Parse a decimal string into the fixed-point scale using round-half-to-even.
///
/// This is retained for provider inputs whose historical contract rounds excess
/// precision. Core trading values should prefer [`parse_decimal_exact`].
pub fn parse_decimal_half_even(input: &str) -> Result<i128, &'static str> {
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
    let rounded = if power >= 0 {
        let factor = 10_u128
            .checked_pow(power as u32)
            .ok_or("decimal value is out of range")?;
        magnitude
            .checked_mul(factor)
            .ok_or("decimal value is out of range")?
    } else {
        let places = (-power) as u32;
        if places > 38 {
            0
        } else {
            let divisor = 10_u128.pow(places);
            let quotient = magnitude / divisor;
            let remainder = magnitude % divisor;
            let doubled = remainder * 2;
            quotient + u128::from(doubled > divisor || (doubled == divisor && quotient % 2 == 1))
        }
    };
    let signed = i128::try_from(rounded).map_err(|_| "decimal value is out of range")?;
    if negative {
        signed.checked_neg().ok_or("decimal value is out of range")
    } else {
        Ok(signed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::numeric::SCALE;

    #[test]
    fn half_even_decimal_parser_preserves_provider_rounding_contract() {
        assert_eq!(parse_decimal_half_even("0.0000000005"), Ok(0));
        assert_eq!(parse_decimal_half_even("0.0000000015"), Ok(2));
        assert_eq!(parse_decimal_half_even("-0.0000000015"), Ok(-2));
        assert_eq!(parse_decimal_half_even("1.5e-9"), Ok(2));
        assert_eq!(parse_decimal_half_even("1.0000000001"), Ok(SCALE));
        assert_eq!(parse_decimal_half_even(""), Err("empty decimal value"));
        assert_eq!(parse_decimal_half_even("NaN"), Err("invalid decimal value"));
        assert!(parse_decimal_half_even("1e101").is_err());
    }
}
