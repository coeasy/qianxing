//! 风险调整收益比率（P2/P5 单一事实源）。
//!
//! 全部从权益曲线这一条 Ledger 投影推导，不引入第二套收益计算——报告卡与未来的 compare
//! 都只念这里落进 `summary.json` 的同一组数。仓库口径：没算过 ≠ 算出来是零，所以任何一格
//! 分母为 0 或样本不足都返回 `None`，落盘写成 `null`，读侧照 "absent" 处理。

/// 一组从权益曲线推导的风险调整收益比率。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct RiskRatios {
    pub(crate) sharpe: Option<f64>,
    pub(crate) sortino: Option<f64>,
    pub(crate) calmar: Option<f64>,
    pub(crate) win_rate: Option<f64>,
    pub(crate) profit_factor: Option<f64>,
}

/// 把 `Option<f64>` 收口成摘要里的一个数：`None` 或非有限值一律写 `null`，不假装成 0。
pub(crate) fn ratio_to_json(value: Option<f64>) -> serde_json::Value {
    match value {
        Some(finite) if finite.is_finite() => serde_json::Number::from_f64(finite)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        _ => serde_json::Value::Null,
    }
}

/// 从权益曲线（i128 定点，单位与 `account.initial_cash_raw` 同）推导风险调整比率。
///
/// - 夏普 / 索提诺：逐周期收益率（上一周期权益为分母）的样本均值除以波动 / 下行波动。
/// - 卡玛：总收益 bps / 最大回撤 bps。
/// - 盈利周期占比：收益为正的周期占比。
/// - 收益因子：正收益之和 / 负收益绝对值之和（无亏损周期时数学上无定义，写 `null`）。
///
/// 任何一格分母为 0 或样本不足（< 2 个有效周期收益率）都返回 `None`。
pub(crate) fn compute_risk_ratios(equity: &[i128], max_drawdown_bps: u32) -> RiskRatios {
    // 逐周期收益率（fraction），跳过分母为 0 的点：分母为 0 的曲线没有可对照的基准。
    let returns: Vec<f64> = equity
        .windows(2)
        .filter_map(|pair| {
            let previous = pair[0] as f64;
            if previous != 0.0 {
                Some((pair[1] as f64 - previous) / previous)
            } else {
                None
            }
        })
        .collect();
    if returns.len() < 2 {
        return RiskRatios::default();
    }
    let n = returns.len() as f64;
    let mean = returns.iter().sum::<f64>() / n;
    let std = sample_std(&returns, mean);
    let sharpe = if std > 0.0 { Some(mean / std) } else { None };
    let downside_std = {
        let downs: Vec<f64> = returns
            .iter()
            .map(|return_value| return_value.min(0.0).powi(2))
            .collect();
        (downs.iter().sum::<f64>() / (n - 1.0)).sqrt()
    };
    let sortino = if downside_std > 0.0 {
        Some(mean / downside_std)
    } else {
        None
    };
    let first = equity.first().copied().unwrap_or(0) as f64;
    let last = equity.last().copied().unwrap_or(0) as f64;
    let total_return_bps = if first != 0.0 {
        (last - first) / first * 10_000.0
    } else {
        0.0
    };
    let calmar = if max_drawdown_bps > 0 {
        Some(total_return_bps / max_drawdown_bps as f64)
    } else {
        None
    };
    let wins = returns
        .iter()
        .filter(|return_value| **return_value > 0.0)
        .count() as f64;
    let win_rate = Some(wins / n);
    let gross_profit: f64 = returns
        .iter()
        .filter(|return_value| **return_value > 0.0)
        .sum();
    let gross_loss: f64 = returns
        .iter()
        .filter(|return_value| **return_value < 0.0)
        .map(|return_value| return_value.abs())
        .sum();
    let profit_factor = if gross_loss > 0.0 {
        Some(gross_profit / gross_loss)
    } else {
        None
    };
    RiskRatios {
        sharpe,
        sortino,
        calmar,
        win_rate,
        profit_factor,
    }
}

/// 样本标准差（n-1 无偏估计）；样本不足时返回 0（调用方据此把夏普置为 `None`）。
fn sample_std(values: &[f64], mean: f64) -> f64 {
    if values.len() < 2 {
        return 0.0;
    }
    let degrees = (values.len() - 1) as f64;
    (values
        .iter()
        .map(|value| (value - mean).powi(2))
        .sum::<f64>()
        / degrees)
        .sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flat_equity_has_no_ratios() {
        // 一条水平线：周期收益全为 0，波动为 0，夏普/索提诺无定义；回撤为 0，卡玛无定义。
        let equity = [100_000i128, 100_000, 100_000, 100_000];
        let ratios = compute_risk_ratios(&equity, 0);
        assert_eq!(ratios.sharpe, None);
        assert_eq!(ratios.sortino, None);
        assert_eq!(ratios.calmar, None);
        // 盈利周期占比与收益因子仍有定义（0 个正收益 / 0 个负收益），但不假装成"算出来是零"之外的值。
        assert_eq!(ratios.win_rate, Some(0.0));
        assert_eq!(ratios.profit_factor, None);
    }

    #[test]
    fn monotonic_up_trend_scores_well() {
        // 单调上行：正收益、零回撤 → 夏普为正、卡玛无定义（回撤为 0）。
        // 索提诺分母为下行波动，纯上行序列没有下行波动，数学上无定义，写 `None`（不假装成无穷大）。
        let equity = [100_000i128, 101_000, 102_000, 103_000];
        let ratios = compute_risk_ratios(&equity, 0);
        assert!(ratios.sharpe.is_some_and(|value| value > 0.0));
        assert_eq!(ratios.sortino, None);
        assert_eq!(ratios.calmar, None);
        assert_eq!(ratios.win_rate, Some(1.0));
        assert_eq!(ratios.profit_factor, None);
    }

    #[test]
    fn drawdown_enables_calmar() {
        let equity = [100_000i128, 110_000, 99_000, 105_000];
        // 最大回撤约 10%（从 110k 到 99k），总收益 5% → 卡玛约 0.5。
        let ratios = compute_risk_ratios(&equity, 1_000);
        assert!(ratios
            .calmar
            .is_some_and(|value| (value - 0.5).abs() < 0.05));
        assert!(ratios
            .win_rate
            .is_some_and(|value| value > 0.0 && value < 1.0));
        // 有亏损周期 → 收益因子有定义且为正。
        assert!(ratios.profit_factor.is_some_and(|value| value > 0.0));
    }

    #[test]
    fn too_few_samples_is_absent() {
        let equity = [100_000i128, 100_000];
        assert_eq!(compute_risk_ratios(&equity, 500), RiskRatios::default());
    }
}
