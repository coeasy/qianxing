pub fn realized_volatility_bps(samples: &[i128]) -> u32 {
    if samples.len() < 2 {
        return 0;
    }
    let mean = samples.iter().sum::<i128>() / samples.len() as i128;
    let variance = samples
        .iter()
        .map(|value| {
            let diff = *value - mean;
            diff * diff
        })
        .sum::<i128>() / samples.len() as i128;
    (variance.max(0) as u128).min(u32::MAX as u128) as u32
}
