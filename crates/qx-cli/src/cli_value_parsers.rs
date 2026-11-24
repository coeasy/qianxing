/// Parse an integer order quantity.
pub(crate) fn parse_quantity(value: &str) -> Result<i64, String> {
    value
        .parse::<i64>()
        .map_err(|error| format!("quantity 非法: {value}（{error}）"))
}

/// Parse a positive quote-currency notional into the engine's exact 1e-9 raw scale.
pub(crate) fn parse_quote_notional(value: &str) -> Result<i128, String> {
    let decimal = qx_core::ExactDecimal::parse(value)
        .map_err(|error| format!("target-notional 非法: {value}（{error}）"))?;
    let raw = decimal
        .to_raw(9)
        .map_err(|_| format!("target-notional {value} 不能无损映射到 1e-9 精度"))?;
    if raw <= 0 {
        return Err("target-notional 必须为正数".into());
    }
    Ok(raw)
}

#[cfg(test)]
mod tests {
    use super::parse_quote_notional;

    #[test]
    fn quote_notional_parser_preserves_exact_nine_decimal_raw_units() {
        assert_eq!(parse_quote_notional("1000"), Ok(1_000_000_000_000));
        assert_eq!(parse_quote_notional("0.000000001"), Ok(1));
        assert!(parse_quote_notional("0").is_err());
        assert!(parse_quote_notional("-1").is_err());
        assert!(parse_quote_notional("0.0000000001").is_err());
    }
}

/// Parse basis-point flags in the shared `0..=10000` range.
pub(crate) fn parse_bps(value: &str) -> Result<i64, String> {
    match value.parse::<i64>() {
        Ok(parsed) if (0..=10_000).contains(&parsed) => Ok(parsed),
        _ => Err("必须在 0..=10000 内".to_string()),
    }
}

/// Parse a positive relay limit in the same domain as `relay_batch_size`.
pub(crate) fn parse_relay_limit(value: &str) -> Result<usize, String> {
    match value.parse::<usize>() {
        Ok(parsed) if (1..=10_000).contains(&parsed) => Ok(parsed),
        _ => Err("relay limit 必须在 1..=10000 内".to_string()),
    }
}
