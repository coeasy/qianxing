//! Application-owned report analysis derived from verified backtest facts.

mod comparison;
mod readout;

pub use comparison::{
    comparison_entry_from_summary, rank_comparison_by_active_return, rank_comparison_by_return,
    validate_comparison_input_identity, ComparisonBenchmark, ComparisonEntry,
    ComparisonInputIdentity,
};
pub use readout::{summary_block, summary_number, summary_text};

use qx_core::{DecimalRounding, ExactDecimal};
use qx_datastruct::BarFrame;
use serde_json::{json, Value};
use std::path::Path;

const SHANGHAI_OFFSET_MS: u64 = 8 * 60 * 60 * 1_000;
const DAY_MS: u64 = 24 * 60 * 60 * 1_000;

/// Recompute simple total return in basis points from verified initial and final equity.
pub fn equity_total_return_bps_v1(
    initial_equity_raw: i128,
    final_equity_raw: i128,
) -> Result<i32, String> {
    if initial_equity_raw <= 0 {
        return Err("权益收益的初始资金必须为正".into());
    }
    final_equity_raw
        .checked_sub(initial_equity_raw)
        .and_then(|pnl| pnl.checked_mul(10_000))
        .and_then(|pnl| pnl.checked_div(initial_equity_raw))
        .and_then(|value| i32::try_from(value).ok())
        .ok_or_else(|| "权益收益基点溢出或超出可表示范围".into())
}

/// Compute basis points from scale-aware equity without projecting onto the legacy 1e-9 grid.
pub fn exact_ratio_bps_v1(
    numerator: ExactDecimal,
    denominator: ExactDecimal,
) -> Result<i32, String> {
    if !denominator.is_positive() {
        return Err("精确权益基点分母必须为正数".into());
    }
    let ten_thousand = ExactDecimal::from_parts(10_000, 0)
        .map_err(|error| format!("精确权益基点常量无效: {error}"))?;
    let value = numerator
        .checked_mul_at_scale(
            ten_thousand,
            ExactDecimal::MAX_SCALE,
            DecimalRounding::TowardZero,
        )
        .and_then(|scaled| scaled.checked_div_at_scale(denominator, 0, DecimalRounding::TowardZero))
        .map(ExactDecimal::coefficient)
        .ok_or_else(|| "精确权益基点计算溢出".to_string())?;
    i32::try_from(value).map_err(|_| "精确权益基点超出 i32 范围".into())
}

/// Recompute scale-aware total return in basis points from verified equity values.
pub fn exact_equity_total_return_bps_v1(
    initial_equity: ExactDecimal,
    final_equity: ExactDecimal,
) -> Result<i32, String> {
    if !initial_equity.is_positive() {
        return Err("精确权益收益的初始资金必须为正".into());
    }
    let pnl = final_equity
        .checked_sub(initial_equity)
        .ok_or_else(|| "精确权益收益差值溢出".to_string())?;
    exact_ratio_bps_v1(pnl, initial_equity)
}

/// Recompute peak-to-trough drawdown from a scale-aware equity curve.
pub fn exact_equity_max_drawdown_bps_v1(equity: &[ExactDecimal]) -> Result<u32, String> {
    let mut peak = None::<ExactDecimal>;
    let mut maximum = 0_u32;
    for current in equity {
        let current_peak = match peak {
            None => *current,
            Some(previous) => match previous.checked_cmp(*current) {
                Some(std::cmp::Ordering::Less) => *current,
                Some(_) => previous,
                None => return Err("精确权益回撤高点比较溢出".into()),
            },
        };
        peak = Some(current_peak);
        if current_peak.is_positive() {
            let drawdown = current_peak
                .checked_sub(*current)
                .ok_or_else(|| "精确权益回撤差值溢出".to_string())?;
            let drawdown_bps = exact_ratio_bps_v1(drawdown, current_peak)?.clamp(0, 10_000) as u32;
            maximum = maximum.max(drawdown_bps);
        }
    }
    Ok(maximum)
}

/// Checked accumulator for principal-weighted returns over independently funded accounts.
#[derive(Clone, Copy, Debug, Default)]
pub struct WeightedReturnAccumulatorV1 {
    weighted_numerator_raw: i128,
    initial_cash_raw: i128,
}

impl WeightedReturnAccumulatorV1 {
    /// Add one verified account return using its declared initial cash as the weight.
    pub fn push(&mut self, return_bps: i32, initial_cash_raw: i128) -> Result<(), String> {
        if initial_cash_raw <= 0 {
            return Err("账户加权收益的初始资金必须为正".into());
        }
        let weighted = i128::from(return_bps)
            .checked_mul(initial_cash_raw)
            .ok_or_else(|| "账户加权收益乘法溢出".to_string())?;
        self.weighted_numerator_raw = self
            .weighted_numerator_raw
            .checked_add(weighted)
            .ok_or_else(|| "账户加权收益求和溢出".to_string())?;
        self.initial_cash_raw = self
            .initial_cash_raw
            .checked_add(initial_cash_raw)
            .ok_or_else(|| "账户加权收益本金求和溢出".to_string())?;
        Ok(())
    }

    pub fn initial_cash_raw(self) -> i128 {
        self.initial_cash_raw
    }

    /// Finish with integer-truncated basis points; no successful accounts yields `None`.
    pub fn finish(self) -> Result<Option<i32>, String> {
        if self.initial_cash_raw == 0 {
            return Ok(None);
        }
        let value = self.weighted_numerator_raw / self.initial_cash_raw;
        i32::try_from(value)
            .map(Some)
            .map_err(|_| "账户加权收益超出 i32 范围".into())
    }
}

/// Recompute peak-to-trough drawdown in basis points from replay-verified equity values.
///
/// Overflow is treated conservatively as a full drawdown, matching the report contract used by
/// persisted multi-leg and multi-market runs.
pub fn equity_max_drawdown_bps_v1(equity_raw: &[i128]) -> u32 {
    let mut peak = 0_i128;
    let mut maximum = 0_u32;
    for value in equity_raw {
        peak = peak.max(*value);
        if peak <= 0 || *value >= peak {
            continue;
        }
        let drawdown = peak
            .checked_sub(*value)
            .and_then(|loss| loss.checked_mul(10_000))
            .and_then(|loss| loss.checked_div(peak))
            .unwrap_or(10_000)
            .clamp(0, 10_000) as u32;
        maximum = maximum.max(drawdown);
    }
    maximum
}

/// Recompute the legacy joint-account drawdown contract without a 100% cap.
///
/// This is the exact aggregation rule used by the joint-account backtest writer. Keeping the
/// checked arithmetic here lets report readers independently verify the persisted curve without
/// introducing a second implementation in an adapter.
pub fn joint_equity_max_drawdown_bps_v1(equity: &[(u64, i128)]) -> Result<u32, String> {
    let mut peak = 0_i128;
    let mut maximum = 0_u128;
    for (_, value) in equity {
        peak = peak.max(*value);
        if peak <= 0 {
            continue;
        }
        let drawdown = peak
            .checked_sub(*value)
            .ok_or_else(|| "联合账户权益回撤溢出".to_string())?;
        let bps = u128::try_from(drawdown)
            .ok()
            .and_then(|drawdown| drawdown.checked_mul(10_000))
            .and_then(|scaled| scaled.checked_div(peak as u128))
            .ok_or_else(|| "联合账户权益回撤无法表示".to_string())?;
        maximum = maximum.max(bps);
    }
    u32::try_from(maximum).map_err(|_| "联合账户最大回撤超出 u32 范围".into())
}

/// Build the report projection for a portfolio series and an explicit benchmark BarFrame.
///
/// The caller is responsible for loading and validating the summary/equity/BarFrame files. When
/// a calendar is supplied, its bytes and path are included in the projection and its coverage is
/// checked against the summary clock window before metrics are calculated.
pub fn benchmark_analysis(
    portfolio: &[(u64, i128)],
    frame: &BarFrame,
    benchmark_path: &Path,
    portfolio_return_bps: i64,
    calendar: Option<(&Path, &str, (u64, u64))>,
) -> Result<Value, String> {
    let calendar = calendar
        .map(|(path, payload, (window_start, window_end))| {
            validate_calendar_coverage(
                path,
                payload,
                portfolio,
                &frame.ts,
                window_start,
                window_end,
            )
        })
        .transpose()?;
    let metrics = qx_core::benchmark_relative_metrics_from_columns(
        portfolio,
        &frame.ts,
        &frame.close_raw,
        portfolio_return_bps,
    )?;
    let series = portfolio
        .windows(2)
        .zip(frame.ts.windows(2))
        .zip(frame.close_raw.windows(2))
        .zip(metrics.active_returns_bps.iter())
        .map(
            |(((portfolio_pair, benchmark_ts), benchmark_close), active_bps)| {
                let portfolio_return_bps =
                    (portfolio_pair[1].1 as f64 / portfolio_pair[0].1 as f64 - 1.0) * 10_000.0;
                let benchmark_return_bps =
                    (benchmark_close[1] as f64 / benchmark_close[0] as f64 - 1.0) * 10_000.0;
                json!({
                    "start_ts": benchmark_ts[0],
                    "end_ts": benchmark_ts[1],
                    "portfolio_equity_start_raw": portfolio_pair[0].1,
                    "portfolio_equity_end_raw": portfolio_pair[1].1,
                    "benchmark_close_start_raw": benchmark_close[0],
                    "benchmark_close_end_raw": benchmark_close[1],
                    "portfolio_return_bps": portfolio_return_bps,
                    "benchmark_return_bps": benchmark_return_bps,
                    "active_return_bps": active_bps,
                })
            },
        )
        .collect::<Vec<_>>();
    let fingerprint = qx_data::fingerprint_bar_frame(frame)?;
    let metrics_json = if calendar.is_some() {
        json!({
            "benchmark_return_bps": metrics.benchmark_return_bps,
            "active_return_bps": metrics.active_return_bps,
            "mean_active_return_bps_per_trading_day": metrics.mean_active_return_bps,
            "tracking_error_bps_per_trading_day": metrics.tracking_error_bps,
            "information_ratio_per_trading_day": metrics.information_ratio,
        })
    } else {
        json!({
            "benchmark_return_bps": metrics.benchmark_return_bps,
            "active_return_bps": metrics.active_return_bps,
            "mean_active_return_bps_per_bar": metrics.mean_active_return_bps,
            "tracking_error_bps_per_bar": metrics.tracking_error_bps,
            "information_ratio_per_bar": metrics.information_ratio,
        })
    };
    let mut analysis = json!({
        "schema_version": 1,
        "benchmark": {
            "instrument": frame.instrument.to_string(),
            "source": frame.source.0,
            "path": benchmark_path,
            "fingerprint": fingerprint,
            "price_basis": "raw_close_unadjusted",
        },
        "alignment": if calendar.is_some() {
            "exact_timestamp_and_trading_calendar_coverage"
        } else {
            "exact_timestamp"
        },
        "period_basis": if calendar.is_some() {
            "per_trading_day_no_annualization"
        } else {
            "per_bar_no_annualization"
        },
        "aligned_periods": metrics.aligned_periods,
        "series": series,
        "metrics": metrics_json
    });
    if let Some(calendar) = calendar {
        analysis["calendar"] = calendar;
    }
    Ok(analysis)
}

fn validate_calendar_coverage(
    path: &Path,
    payload: &str,
    portfolio: &[(u64, i128)],
    benchmark_timestamps: &[u64],
    window_start: u64,
    window_end: u64,
) -> Result<Value, String> {
    let mut rules = qx_xingban::AshareRuleConfig::default();
    let report = rules
        .apply_calendar_json_with_report(payload)
        .map_err(|error| format!("基准交易日历无效 {}: {error}", path.display()))?;
    if report.calendar_id.trim().is_empty() {
        return Err("基准交易日历必须声明非空 calendar_id".into());
    }
    if report.source.trim().is_empty() {
        return Err("基准交易日历必须声明非空 source".into());
    }
    let original: Value = serde_json::from_str(payload)
        .map_err(|error| format!("基准交易日历 JSON 无效: {error}"))?;
    let declared_days = original
        .get("trading_days")
        .and_then(Value::as_array)
        .ok_or("基准交易日历缺少 trading_days 数组")?;
    let mut previous = None;
    for (index, value) in declared_days.iter().enumerate() {
        let day = value
            .as_str()
            .ok_or_else(|| format!("基准交易日历 trading_days[{index}] 必须是日期字符串"))?;
        if day.len() != 10
            || !day.is_ascii()
            || day.as_bytes().get(4) != Some(&b'-')
            || day.as_bytes().get(7) != Some(&b'-')
        {
            return Err(format!(
                "基准交易日历 trading_days[{index}] 必须使用 YYYY-MM-DD"
            ));
        }
        if previous.is_some_and(|prior: &str| prior >= day) {
            return Err(format!(
                "基准交易日历日期必须严格递增且无重复: trading_days[{index}]"
            ));
        }
        previous = Some(day);
    }
    let days = rules
        .trading_days
        .iter()
        .map(|timestamp| (timestamp.saturating_add(SHANGHAI_OFFSET_MS)) / DAY_MS)
        .collect::<Vec<_>>();
    validate_daily_series_days("组合权益", portfolio, &days, window_start, window_end)?;
    validate_daily_timestamps(
        "基准行情",
        benchmark_timestamps.iter().copied(),
        &days,
        window_start,
        window_end,
    )?;
    let canonical_path = path
        .canonicalize()
        .map_err(|error| format!("解析基准交易日历路径失败 {}: {error}", path.display()))?;
    Ok(json!({
        "calendar_id": report.calendar_id,
        "source": report.source,
        "trading_days": report.trading_days,
        "sha256": qx_strategy::file_digest::sha256_file_hex(&canonical_path)?,
        "path": canonical_path,
        "coverage": "one_observation_per_trading_day_within_backtest_clock_range",
        "window_start": window_start,
        "window_end": window_end,
    }))
}

fn validate_daily_series_days(
    label: &str,
    rows: &[(u64, i128)],
    calendar_days: &[u64],
    window_start: u64,
    window_end: u64,
) -> Result<(), String> {
    validate_daily_timestamps(
        label,
        rows.iter().map(|(timestamp, _)| *timestamp),
        calendar_days,
        window_start,
        window_end,
    )
}

fn validate_daily_timestamps(
    label: &str,
    timestamps: impl IntoIterator<Item = u64>,
    calendar_days: &[u64],
    window_start: u64,
    window_end: u64,
) -> Result<(), String> {
    let mut observed = Vec::new();
    for timestamp in timestamps {
        let day = timestamp.saturating_add(SHANGHAI_OFFSET_MS) / DAY_MS;
        if observed.last() == Some(&day) {
            return Err(format!(
                "{label} 包含同一交易日的多条记录；显式交易日历覆盖校验仅接受日线序列"
            ));
        }
        if observed.last().is_some_and(|previous| *previous > day) {
            return Err(format!("{label} 日期必须严格递增"));
        }
        observed.push(day);
    }
    let first = window_start.saturating_add(SHANGHAI_OFFSET_MS) / DAY_MS;
    let last = window_end.saturating_add(SHANGHAI_OFFSET_MS) / DAY_MS;
    let expected = calendar_days
        .iter()
        .copied()
        .filter(|day| (first..=last).contains(day))
        .collect::<Vec<_>>();
    if expected.is_empty() {
        return Err(format!(
            "{label} 的摘要时钟范围内没有显式交易日历记录: start={window_start} end={window_end}"
        ));
    }
    if expected != observed {
        let missing = expected
            .iter()
            .find(|day| observed.binary_search(day).is_err())
            .copied();
        let outside = observed
            .iter()
            .find(|day| calendar_days.binary_search(day).is_err())
            .copied();
        let outside_window = observed
            .iter()
            .find(|day| !(first..=last).contains(day))
            .copied();
        return Err(format!(
            "{label} 与回测时钟范围/显式交易日历不一致：missing_trading_day={missing:?} non_trading_day={outside:?} outside_backtest_window={outside_window:?}"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        benchmark_analysis, equity_max_drawdown_bps_v1, equity_total_return_bps_v1,
        exact_equity_max_drawdown_bps_v1, exact_equity_total_return_bps_v1,
        validate_daily_timestamps, WeightedReturnAccumulatorV1,
    };
    use qx_core::{ExactDecimal, InstrumentId};
    use qx_datastruct::BarFrame;
    use qx_guanxing::DataSourceId;
    use serde_json::json;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const DAY_MS: u64 = 24 * 60 * 60 * 1_000;

    #[test]
    fn equity_drawdown_uses_running_peak_and_fails_conservatively_on_overflow() {
        assert_eq!(
            equity_max_drawdown_bps_v1(&[10_000, 9_000, 11_000, 5_500]),
            5_000
        );
        assert_eq!(equity_max_drawdown_bps_v1(&[100, -1]), 10_000);
        assert_eq!(equity_max_drawdown_bps_v1(&[i128::MAX, i128::MIN]), 10_000);
        assert_eq!(equity_max_drawdown_bps_v1(&[-10, -20, -5]), 0);
    }

    #[test]
    fn equity_total_return_uses_checked_integer_arithmetic() {
        assert_eq!(equity_total_return_bps_v1(10_000, 11_000), Ok(1_000));
        assert_eq!(equity_total_return_bps_v1(10_000, 9_000), Ok(-1_000));
        assert!(equity_total_return_bps_v1(0, 1).is_err());
        assert!(equity_total_return_bps_v1(i128::MAX, i128::MIN).is_err());
    }

    #[test]
    fn exact_equity_metrics_preserve_subnano_return_and_drawdown() {
        let initial = ExactDecimal::parse("0.0000000019").unwrap();
        let peak = ExactDecimal::parse("0.0000000020").unwrap();
        assert_eq!(exact_equity_total_return_bps_v1(initial, peak), Ok(526));
        assert_eq!(
            exact_equity_max_drawdown_bps_v1(&[initial, peak, initial]),
            Ok(500)
        );
        assert!(exact_equity_total_return_bps_v1(ExactDecimal::ZERO, peak).is_err());
    }

    #[test]
    fn weighted_account_returns_use_principal_weights_and_checked_sums() {
        let mut weighted = WeightedReturnAccumulatorV1::default();
        weighted.push(1_000, 3).unwrap();
        weighted.push(-500, 1).unwrap();
        assert_eq!(weighted.initial_cash_raw(), 4);
        assert_eq!(weighted.finish(), Ok(Some(625)));

        assert!(weighted.push(100, 0).is_err());
        let mut overflowing = WeightedReturnAccumulatorV1::default();
        overflowing.push(1, i128::MAX).unwrap();
        assert!(overflowing.push(1, 1).is_err());
    }

    fn isolated_file(label: &str) -> PathBuf {
        static SERIAL: AtomicUsize = AtomicUsize::new(0);
        let serial = SERIAL.fetch_add(1, Ordering::Relaxed);
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let workspace = manifest
            .parent()
            .and_then(|path| path.parent())
            .expect("qx-app manifest is two levels below the workspace root")
            .to_path_buf();
        let directory = workspace.join("target").join(format!(
            "qx-app-benchmark-analysis-{label}-{}-{serial}",
            std::process::id()
        ));
        std::fs::create_dir_all(&directory).expect("create benchmark analysis fixture dir");
        directory.join("calendar.json")
    }

    #[test]
    fn benchmark_analysis_binds_daily_calendar_and_emits_per_trading_day_metrics() {
        let calendar_path = isolated_file("daily-projection");
        let calendar_payload = json!({
            "schema_version": 1,
            "source": "qx-app-test-fixture",
            "calendar_id": "cn-test-2024-01",
            "trading_days": ["2024-01-02", "2024-01-03", "2024-01-04"],
            "sessions": [["09:30", "11:30"], ["13:00", "15:00"]]
        })
        .to_string();
        std::fs::write(&calendar_path, &calendar_payload).expect("write calendar fixture");

        let first = 1_704_159_000_000_u64; // 2024-01-02 09:30 Asia/Shanghai.
        let timestamps = vec![first, first + DAY_MS, first + 2 * DAY_MS];
        let frame = BarFrame {
            instrument: InstrumentId::parse("000300.SSE").unwrap(),
            source: DataSourceId("benchmark-daily-test-v1".into()),
            ts: timestamps.clone(),
            open_raw: vec![4_000_000_000; 3],
            high_raw: vec![4_100_000_000; 3],
            low_raw: vec![3_900_000_000; 3],
            close_raw: vec![4_000_000_000, 4_020_000_000, 4_040_000_000],
            volume_raw: vec![1_000; 3],
            exact_ohlcv: None,
        };
        let portfolio = vec![
            (timestamps[0], 10_000_000_000_i128),
            (timestamps[1], 10_100_000_000_i128),
            (timestamps[2], 10_200_000_000_i128),
        ];

        let analysis = benchmark_analysis(
            &portfolio,
            &frame,
            &frame_path(),
            200,
            Some((
                &calendar_path,
                &calendar_payload,
                (timestamps[0], timestamps[2]),
            )),
        )
        .expect("daily benchmark analysis with complete calendar");

        assert_eq!(
            analysis["alignment"],
            "exact_timestamp_and_trading_calendar_coverage"
        );
        assert_eq!(analysis["period_basis"], "per_trading_day_no_annualization");
        assert_eq!(analysis["calendar"]["calendar_id"], "cn-test-2024-01");
        assert_eq!(
            analysis["calendar"]["coverage"],
            "one_observation_per_trading_day_within_backtest_clock_range"
        );
        assert_eq!(analysis["aligned_periods"], 2);
        assert_eq!(analysis["series"].as_array().unwrap().len(), 2);
        assert!(analysis["metrics"]["mean_active_return_bps_per_trading_day"].is_number());
        assert!(analysis["metrics"]["tracking_error_bps_per_trading_day"].is_number());
        assert!(analysis["metrics"]["information_ratio_per_trading_day"].is_number());
        assert!(analysis["metrics"]["mean_active_return_bps_per_bar"].is_null());

        let _ = std::fs::remove_dir_all(calendar_path.parent().unwrap());
    }

    #[test]
    fn explicit_calendar_rejects_missing_first_middle_and_last_observations() {
        let expected_days = [0, 1, 2];
        let full_series = [0, DAY_MS, 2 * DAY_MS];
        assert!(
            validate_daily_timestamps("基准行情", full_series, &expected_days, 0, 2 * DAY_MS)
                .is_ok()
        );

        for (missing_day, observed) in [
            (0, vec![DAY_MS, 2 * DAY_MS]),
            (1, vec![0, 2 * DAY_MS]),
            (2, vec![0, DAY_MS]),
        ] {
            let error =
                validate_daily_timestamps("基准行情", observed, &expected_days, 0, 2 * DAY_MS)
                    .unwrap_err();
            assert!(
                error.contains(&format!("missing_trading_day=Some({missing_day})")),
                "unexpected missing-day diagnostic: {error}"
            );
        }
    }

    #[test]
    fn explicit_calendar_rejects_intraday_and_out_of_order_daily_series() {
        let days = [0, 1];
        let intraday = validate_daily_timestamps("组合权益", [0, 1_000, DAY_MS], &days, 0, DAY_MS)
            .unwrap_err();
        assert!(intraday.contains("同一交易日的多条记录"));

        let out_of_order =
            validate_daily_timestamps("组合权益", [DAY_MS, 0], &days, 0, DAY_MS).unwrap_err();
        assert!(out_of_order.contains("日期必须严格递增"));
    }

    fn frame_path() -> PathBuf {
        PathBuf::from("benchmark-bars.json")
    }
}
