//! Deterministic performance metrics derived from a completed equity series.
//!
//! This module owns analysis math shared by report producers. It consumes the
//! recorded equity projection and never changes execution or ledger facts.

/// Risk-adjusted ratios derived from periodic equity returns.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BacktestRiskMetrics {
    pub sharpe: Option<f64>,
    pub sortino: Option<f64>,
    pub calmar: Option<f64>,
    pub win_rate: Option<f64>,
    pub profit_factor: Option<f64>,
}

/// Compute ratios from fixed-point equity values and the authoritative return
/// and drawdown metrics recorded by the backtest engine.
///
/// Sharpe and Sortino use per-sample returns without annualization. Undefined
/// denominators and fewer than two returns produce `None`, so report layers
/// can preserve the distinction between an undefined metric and zero.
pub fn backtest_risk_metrics(
    equity: &[i128],
    return_bps: i32,
    max_drawdown_bps: u32,
) -> BacktestRiskMetrics {
    let returns: Vec<f64> = equity
        .windows(2)
        .filter_map(|pair| {
            let previous = pair[0] as f64;
            (previous != 0.0).then(|| (pair[1] as f64 - previous) / previous)
        })
        .collect();
    if returns.len() < 2 {
        return BacktestRiskMetrics::default();
    }

    let n = returns.len() as f64;
    let mean = returns.iter().sum::<f64>() / n;
    let mut squared_deviation_sum = 0.0;
    let mut downside_sum = 0.0;
    let mut wins = 0_usize;
    let mut gross_profit = 0.0;
    let mut gross_loss = 0.0;
    // One return vector serves every ratio; the old implementation allocated a
    // second vector just to hold downside squares and traversed returns repeatedly.
    for &value in &returns {
        squared_deviation_sum += (value - mean).powi(2);
        downside_sum += value.min(0.0).powi(2);
        if value > 0.0 {
            wins += 1;
            gross_profit += value;
        } else if value < 0.0 {
            gross_loss += value.abs();
        }
    }
    let std = (squared_deviation_sum / (n - 1.0)).sqrt();
    let sharpe = (std > 0.0).then_some(mean / std);
    let downside_std = (downside_sum / (n - 1.0)).sqrt();
    let sortino = (downside_std > 0.0).then_some(mean / downside_std);
    let calmar = (max_drawdown_bps > 0).then_some(f64::from(return_bps) / max_drawdown_bps as f64);
    let win_rate = Some(wins as f64 / n);
    let profit_factor = (gross_loss > 0.0).then_some(gross_profit / gross_loss);

    BacktestRiskMetrics {
        sharpe,
        sortino,
        calmar,
        win_rate,
        profit_factor,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allocated_reference(
        equity: &[i128],
        return_bps: i32,
        max_drawdown_bps: u32,
    ) -> BacktestRiskMetrics {
        let returns: Vec<f64> = equity
            .windows(2)
            .filter_map(|pair| {
                let previous = pair[0] as f64;
                (previous != 0.0).then(|| (pair[1] as f64 - previous) / previous)
            })
            .collect();
        if returns.len() < 2 {
            return BacktestRiskMetrics::default();
        }
        let n = returns.len() as f64;
        let mean = returns.iter().sum::<f64>() / n;
        let std = (returns
            .iter()
            .map(|value| (value - mean).powi(2))
            .sum::<f64>()
            / (n - 1.0))
            .sqrt();
        let downside: Vec<f64> = returns.iter().map(|value| value.min(0.0).powi(2)).collect();
        let downside_std = (downside.iter().sum::<f64>() / (n - 1.0)).sqrt();
        let wins = returns.iter().filter(|value| **value > 0.0).count() as f64;
        let gross_profit = returns.iter().filter(|value| **value > 0.0).sum::<f64>();
        let gross_loss = returns
            .iter()
            .filter(|value| **value < 0.0)
            .map(|value| value.abs())
            .sum::<f64>();
        BacktestRiskMetrics {
            sharpe: (std > 0.0).then_some(mean / std),
            sortino: (downside_std > 0.0).then_some(mean / downside_std),
            calmar: (max_drawdown_bps > 0)
                .then_some(f64::from(return_bps) / max_drawdown_bps as f64),
            win_rate: Some(wins / n),
            profit_factor: (gross_loss > 0.0).then_some(gross_profit / gross_loss),
        }
    }

    #[test]
    fn flat_equity_keeps_undefined_ratios_absent() {
        let metrics = backtest_risk_metrics(&[100_000, 100_000, 100_000, 100_000], 0, 0);
        assert_eq!(metrics.sharpe, None);
        assert_eq!(metrics.sortino, None);
        assert_eq!(metrics.calmar, None);
        assert_eq!(metrics.win_rate, Some(0.0));
        assert_eq!(metrics.profit_factor, None);
    }

    #[test]
    fn upward_and_mixed_series_preserve_ratio_semantics() {
        let upward = backtest_risk_metrics(&[100_000, 101_000, 102_000, 103_000], 0, 0);
        assert!(upward.sharpe.is_some_and(|value| value > 0.0));
        assert_eq!(upward.sortino, None);
        assert_eq!(upward.calmar, None);
        assert_eq!(upward.win_rate, Some(1.0));
        assert_eq!(upward.profit_factor, None);

        let mixed = backtest_risk_metrics(&[100_000, 110_000, 99_000, 105_000], 500, 1_000);
        assert!(mixed.calmar.is_some_and(|value| (value - 0.5).abs() < 0.05));
        assert!(mixed
            .win_rate
            .is_some_and(|value| value > 0.0 && value < 1.0));
        assert!(mixed.profit_factor.is_some_and(|value| value > 0.0));
    }

    #[test]
    fn too_few_returns_and_zero_denominators_are_absent() {
        assert_eq!(
            backtest_risk_metrics(&[100_000, 100_000], 100, 500),
            BacktestRiskMetrics::default()
        );
        assert_eq!(
            backtest_risk_metrics(&[0, 1_000, 2_000], 100, 500),
            BacktestRiskMetrics::default()
        );
    }

    #[test]
    fn single_vector_metrics_match_the_allocating_reference_exactly() {
        for equity in [
            vec![0, 0, 100_000, 101_000, 99_500, 102_000],
            vec![10_000, 10_100, 10_200, 9_900, 10_300, 10_100],
            vec![100_000, 99_999, 100_001, 99_998, 100_004],
        ] {
            for (return_bps, drawdown_bps) in [(0, 0), (137, 420)] {
                assert_eq!(
                    backtest_risk_metrics(&equity, return_bps, drawdown_bps),
                    allocated_reference(&equity, return_bps, drawdown_bps)
                );
            }
        }
    }
}
