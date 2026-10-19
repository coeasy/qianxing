/// Parse an integer order quantity.
pub(crate) fn parse_quantity(value: &str) -> Result<i64, String> {
    value
        .parse::<i64>()
        .map_err(|error| format!("quantity 非法: {value}（{error}）"))
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
