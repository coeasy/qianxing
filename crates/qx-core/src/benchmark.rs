//! Period-aligned performance analysis against an explicitly supplied benchmark series.
//!
//! This module only compares recorded equity and benchmark closes. It does not infer trading
//! calendars, corporate-action adjustments, or missing observations; callers must provide exact
//! timestamp alignment and disclose the benchmark price convention.

#[derive(Clone, Debug, Default, PartialEq)]
pub struct BenchmarkRelativeMetrics {
    pub benchmark_return_bps: i64,
    pub active_return_bps: i64,
    pub mean_active_return_bps: Option<f64>,
    pub tracking_error_bps: Option<f64>,
    pub information_ratio: Option<f64>,
    pub aligned_periods: usize,
    /// Unrounded active return for each adjacent, exactly aligned observation pair.
    pub active_returns_bps: Vec<f64>,
}

/// Compare portfolio equity with benchmark closes at identical timestamps.
///
/// Returns are per supplied observation; no annualization is applied because the core cannot
/// infer whether observations are daily, intraday, or irregularly spaced.
pub fn benchmark_relative_metrics(
    portfolio: &[(u64, i128)],
    benchmark: &[(u64, i128)],
    portfolio_return_bps: i64,
) -> Result<BenchmarkRelativeMetrics, String> {
    benchmark_relative_metrics_by(
        portfolio,
        benchmark.len(),
        |index| benchmark[index],
        portfolio_return_bps,
    )
}

/// Compare portfolio equity against columnar benchmark observations without materializing
/// `(timestamp, close)` tuples. The timestamp and close slices must have the same length.
pub fn benchmark_relative_metrics_from_columns(
    portfolio: &[(u64, i128)],
    benchmark_timestamps: &[u64],
    benchmark_closes: &[i128],
    portfolio_return_bps: i64,
) -> Result<BenchmarkRelativeMetrics, String> {
    if benchmark_timestamps.len() != benchmark_closes.len() {
        return Err(format!(
            "基准时间戳与收盘价数量不匹配: timestamps={} closes={}",
            benchmark_timestamps.len(),
            benchmark_closes.len()
        ));
    }
    benchmark_relative_metrics_by(
        portfolio,
        benchmark_timestamps.len(),
        |index| (benchmark_timestamps[index], benchmark_closes[index]),
        portfolio_return_bps,
    )
}

fn benchmark_relative_metrics_by(
    portfolio: &[(u64, i128)],
    benchmark_len: usize,
    benchmark_at: impl Fn(usize) -> (u64, i128),
    portfolio_return_bps: i64,
) -> Result<BenchmarkRelativeMetrics, String> {
    if portfolio.len() < 2 || benchmark_len < 2 {
        return Err("基准对照至少需要两个严格对齐的权益/收盘价观测".into());
    }
    if portfolio.len() != benchmark_len {
        return Err(format!(
            "基准观测数量不匹配: portfolio={} benchmark={}",
            portfolio.len(),
            benchmark_len
        ));
    }
    let mut active = Vec::with_capacity(portfolio.len() - 1);
    let mut mean = 0.0;
    let mut squared_deviation = 0.0;
    let mut benchmark_start = 0.0;
    let mut benchmark_end = 0.0;
    let mut previous_pair = None;
    for (index, portfolio_row) in portfolio.iter().enumerate() {
        let benchmark_row = benchmark_at(index);
        if portfolio_row.0 != benchmark_row.0 {
            return Err(format!(
                "基准时间戳必须逐项精确对齐: index={index} portfolio={} benchmark={}",
                portfolio_row.0, benchmark_row.0
            ));
        }
        if portfolio_row.1 <= 0 || benchmark_row.1 <= 0 {
            return Err(format!("基准对照权益与收盘价必须为正数: index={index}"));
        }
        if index == 0 {
            benchmark_start = benchmark_row.1 as f64;
        }
        benchmark_end = benchmark_row.1 as f64;
        if let Some((previous_portfolio, previous_benchmark)) = previous_pair {
            let portfolio_return = portfolio_row.1 as f64 / previous_portfolio as f64 - 1.0;
            let benchmark_return = benchmark_row.1 as f64 / previous_benchmark as f64 - 1.0;
            let value = (portfolio_return - benchmark_return) * 10_000.0;
            let count = (active.len() + 1) as f64;
            let delta = value - mean;
            mean += delta / count;
            squared_deviation += delta * (value - mean);
            active.push(value);
        }
        previous_pair = Some((portfolio_row.1, benchmark_row.1));
    }
    let benchmark_return_bps = ((benchmark_end / benchmark_start - 1.0) * 10_000.0).round() as i64;
    let active_return_bps = portfolio_return_bps
        .checked_sub(benchmark_return_bps)
        .ok_or_else(|| "组合超额收益 bps 溢出".to_string())?;

    // Report export needs each active return. Compute it in the validation pass, while Welford
    // produces the mean and sample variance without another scan over the aligned series.
    let tracking_error = if active.len() > 1 {
        Some((squared_deviation / (active.len() - 1) as f64).sqrt())
    } else {
        None
    };
    let information_ratio = tracking_error
        .filter(|value| *value > 0.0)
        .map(|value| mean / value);

    Ok(BenchmarkRelativeMetrics {
        benchmark_return_bps,
        active_return_bps,
        mean_active_return_bps: Some(mean),
        tracking_error_bps: tracking_error,
        information_ratio,
        aligned_periods: active.len(),
        active_returns_bps: active,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_alignment_produces_cumulative_and_period_relative_metrics() {
        let portfolio = [(1, 100_000), (2, 110_000), (3, 110_000)];
        let benchmark = [(1, 100_000), (2, 105_000), (3, 110_000)];
        let metrics = benchmark_relative_metrics(&portfolio, &benchmark, 1_000).unwrap();
        assert_eq!(metrics.benchmark_return_bps, 1_000);
        assert_eq!(metrics.active_return_bps, 0);
        assert_eq!(metrics.aligned_periods, 2);
        assert_eq!(metrics.active_returns_bps.len(), metrics.aligned_periods);
        assert!(metrics.active_returns_bps[0] > 0.0);
        assert!(metrics.active_returns_bps[1] < 0.0);
        assert!(metrics.mean_active_return_bps.unwrap() > 0.0);
        assert!(metrics.tracking_error_bps.unwrap() > 0.0);
        assert!(metrics.information_ratio.is_some());
    }

    #[test]
    fn rejects_missing_timestamp_alignment_and_non_positive_prices() {
        let portfolio = [(1, 100), (2, 110)];
        assert!(benchmark_relative_metrics(&portfolio, &[(1, 100), (3, 110)], 1).is_err());
        assert!(benchmark_relative_metrics(&portfolio, &[(1, 100)], 1).is_err());
        assert!(benchmark_relative_metrics(&[(1, 100), (2, 0)], &[(1, 100), (2, 110)], 1).is_err());
    }
}
