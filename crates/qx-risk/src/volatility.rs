pub fn realized_volatility_bps(samples: &[i128]) -> u32 {
    if samples.len() < 2 {
        return 0;
    }
    let sum = samples
        .iter()
        .fold(0_i128, |sum, value| sum.saturating_add(*value));
    let mean = sum / samples.len() as i128;
    let variance = samples
        .iter()
        .map(|value| {
            let diff = value.saturating_sub(mean);
            diff.saturating_mul(diff)
        })
        .fold(0_i128, |sum, value| sum.saturating_add(value))
        / samples.len() as i128;
    (variance.max(0) as u128).min(u32::MAX as u128) as u32
}
