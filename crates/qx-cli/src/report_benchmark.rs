//! Explicit benchmark-series binding for post-run A-share analysis.

use crate::report_html::read_equity_csv;
use crate::report_output_paths::report_artifact_paths;
use qx_datastruct::BarFrame;
use std::path::Path;

pub(crate) fn benchmark_html_section(summary: &serde_json::Value) -> Option<String> {
    let analysis = summary.get("benchmark_analysis")?;
    let metrics = analysis.get("metrics")?;
    let instrument = analysis
        .pointer("/benchmark/instrument")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown");
    let percent = |key: &str| {
        metrics
            .get(key)
            .and_then(serde_json::Value::as_i64)
            .map_or_else(
                || "—".to_string(),
                |value| format!("{:+.2}%", value as f64 / 100.0),
            )
    };
    let daily = analysis
        .get("period_basis")
        .and_then(serde_json::Value::as_str)
        == Some("per_trading_day_no_annualization");
    let ratio_key = if daily {
        "information_ratio_per_trading_day"
    } else {
        "information_ratio_per_bar"
    };
    let information_ratio = metrics
        .get(ratio_key)
        .and_then(serde_json::Value::as_f64)
        .map_or_else(|| "—".to_string(), |value| format!("{value:.2}"));
    let card = |label: &str, value: &str| {
        format!(
            "<div class=\"card\"><span class=\"k\">{label}</span><span class=\"v neutral\">{value}</span></div>"
        )
    };
    Some(format!(
        "<h2>基准对照：{}</h2><section class=\"cards\" aria-label=\"基准相对指标\">{}{}{}</section>\n",
        crate::report_html::escape_html(instrument),
        card("基准累计收益", &percent("benchmark_return_bps")),
        card("累计超额收益", &percent("active_return_bps")),
        card(
            if daily {
                "信息比率(每日)"
            } else {
                "信息比率(每 Bar)"
            },
            &information_ratio
        ),
    ))
}

pub(crate) fn benchmark_analysis(
    summary_path: &Path,
    benchmark_path: &Path,
    portfolio_return_bps: i64,
    calendar_path: Option<&Path>,
) -> Result<serde_json::Value, String> {
    let payload = std::fs::read_to_string(benchmark_path).map_err(|error| {
        format!(
            "读取基准 BarFrame 失败 {}: {error}",
            benchmark_path.display()
        )
    })?;
    let frame = BarFrame::from_json(&payload)
        .map_err(|error| format!("基准 BarFrame 无效 {}: {error:?}", benchmark_path.display()))?;
    let artifact_paths = report_artifact_paths(summary_path)?;
    let equity_rows = read_equity_csv(&artifact_paths.equity_csv)?;
    let portfolio = equity_rows
        .iter()
        .map(|(ts, value)| {
            u64::try_from(*ts)
                .map(|ts| (ts, *value))
                .map_err(|_| format!("权益曲线含非法时间戳: {ts}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let calendar = calendar_path
        .map(|path| {
            let window = summary_clock_window(summary_path)?;
            let payload = std::fs::read_to_string(path)
                .map_err(|error| format!("读取基准交易日历失败 {}: {error}", path.display()))?;
            Ok::<_, String>((path, payload, window))
        })
        .transpose()?;
    qx_app::report_analysis::benchmark_analysis(
        &portfolio,
        &frame,
        benchmark_path,
        portfolio_return_bps,
        calendar
            .as_ref()
            .map(|(path, payload, window)| (*path, payload.as_str(), *window)),
    )
}

fn summary_clock_window(summary_path: &Path) -> Result<(u64, u64), String> {
    let payload = std::fs::read_to_string(summary_path).map_err(|error| {
        format!(
            "读取回测摘要时钟范围失败 {}: {error}",
            summary_path.display()
        )
    })?;
    let summary: serde_json::Value = serde_json::from_str(&payload)
        .map_err(|error| format!("回测摘要 JSON 无效 {}: {error}", summary_path.display()))?;
    let start = summary
        .get("clock_start")
        .and_then(serde_json::Value::as_u64)
        .ok_or("回测摘要缺少有效 clock_start，无法验证交易日历首尾覆盖")?;
    let end = summary
        .get("clock_end")
        .and_then(serde_json::Value::as_u64)
        .ok_or("回测摘要缺少有效 clock_end，无法验证交易日历首尾覆盖")?;
    if start > end {
        return Err(format!(
            "回测摘要时钟范围非法: clock_start={start} clock_end={end}"
        ));
    }
    Ok((start, end))
}

pub(crate) fn write_benchmark_csv(analysis: &serde_json::Value, path: &Path) -> Result<(), String> {
    let rows = analysis
        .get("series")
        .and_then(serde_json::Value::as_array)
        .ok_or("基准分析缺少逐 Bar 序列")?;
    let metadata = [
        analysis
            .pointer("/benchmark/instrument")
            .and_then(serde_json::Value::as_str)
            .ok_or("基准分析缺少 instrument")?,
        analysis
            .pointer("/benchmark/fingerprint")
            .and_then(serde_json::Value::as_str)
            .ok_or("基准分析缺少 fingerprint")?,
        analysis
            .pointer("/benchmark/price_basis")
            .and_then(serde_json::Value::as_str)
            .ok_or("基准分析缺少 price_basis")?,
        analysis
            .get("alignment")
            .and_then(serde_json::Value::as_str)
            .ok_or("基准分析缺少 alignment")?,
        analysis
            .get("period_basis")
            .and_then(serde_json::Value::as_str)
            .ok_or("基准分析缺少 period_basis")?,
    ];
    let mut csv = String::from(
        "benchmark_instrument,benchmark_fingerprint,price_basis,alignment,period_basis,start_ts,end_ts,portfolio_equity_start_raw,portfolio_equity_end_raw,benchmark_close_start_raw,benchmark_close_end_raw,portfolio_return_bps,benchmark_return_bps,active_return_bps\n",
    );
    for row in rows {
        let value = |key| {
            row.get(key)
                .filter(|value| !value.is_null())
                .map(serde_json::Value::to_string)
                .ok_or_else(|| format!("基准分析序列缺少 {key}"))
        };
        let row_values = metadata.iter().map(|value| csv_text(value)).chain(
            [
                "start_ts",
                "end_ts",
                "portfolio_equity_start_raw",
                "portfolio_equity_end_raw",
                "benchmark_close_start_raw",
                "benchmark_close_end_raw",
                "portfolio_return_bps",
                "benchmark_return_bps",
                "active_return_bps",
            ]
            .into_iter()
            .map(value)
            .collect::<Result<Vec<_>, _>>()?,
        );
        for (index, cell) in row_values.enumerate() {
            if index > 0 {
                csv.push(',');
            }
            csv.push_str(&cell);
        }
        csv.push('\n');
    }
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("创建基准 CSV 目录失败 {}: {error}", parent.display()))?;
    }
    std::fs::write(path, csv)
        .map_err(|error| format!("写入基准分析 CSV 失败 {}: {error}", path.display()))
}

fn csv_text(value: &str) -> String {
    if value.contains([',', '"', '\r', '\n']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn report_inputs(
        equity_timestamps: &[u64],
    ) -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("qx-cli manifest is nested in the workspace")
            .join("target")
            .join(format!(
                "qx-benchmark-calendar-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        std::fs::create_dir_all(&root).unwrap();
        let summary_path = root.join("daily.summary.json");
        let equity_path = root.join("daily.equity.csv");
        let benchmark_path = root.join("benchmark-bars.json");
        let calendar_path = root.join("calendar.json");
        let timestamps = [1_704_159_000_000_u64, 1_704_245_400_000, 1_704_331_800_000];
        std::fs::write(
            &summary_path,
            serde_json::to_vec(&json!({
                "clock_start": timestamps[0],
                "clock_end": timestamps[2]
            }))
            .unwrap(),
        )
        .unwrap();
        let mut equity = String::from("index,ts,equity_raw,position_raw\n");
        for (index, timestamp) in equity_timestamps.iter().enumerate() {
            equity.push_str(&format!(
                "{index},{timestamp},{},0\n",
                100_000_i64 + index as i64
            ));
        }
        std::fs::write(&equity_path, equity).unwrap();
        let frame = json!({
            "instrument": "000300.SSE",
            "source": "daily-benchmark-fixture-v1",
            "ts": timestamps,
            "open_raw": [4_000_000_000_i64, 4_010_000_000_i64, 4_020_000_000_i64],
            "high_raw": [4_010_000_000_i64, 4_020_000_000_i64, 4_030_000_000_i64],
            "low_raw": [3_990_000_000_i64, 4_000_000_000_i64, 4_010_000_000_i64],
            "close_raw": [4_000_000_000_i64, 4_010_000_000_i64, 4_020_000_000_i64],
            "volume_raw": [1_000_000_i64, 1_000_000, 1_000_000]
        });
        std::fs::write(&benchmark_path, serde_json::to_vec(&frame).unwrap()).unwrap();
        std::fs::write(
            &calendar_path,
            serde_json::to_vec(&json!({
                "schema_version": 1,
                "calendar_id": "daily-report-test-v1",
                "source": "unit-test-fixture",
                "trading_days": ["2024-01-02", "2024-01-03", "2024-01-04"],
                "sessions": [["09:30", "11:30"], ["13:00", "15:00"]]
            }))
            .unwrap(),
        )
        .unwrap();
        (summary_path, benchmark_path, calendar_path)
    }

    #[test]
    fn explicit_calendar_uses_daily_metric_names_and_labels() {
        let timestamps = [1_704_159_000_000_u64, 1_704_245_400_000, 1_704_331_800_000];
        let (summary, benchmark, calendar) = report_inputs(&timestamps);
        let analysis = benchmark_analysis(&summary, &benchmark, 100, Some(&calendar)).unwrap();
        let metrics = &analysis["metrics"];
        assert!(metrics.get("information_ratio_per_trading_day").is_some());
        assert!(metrics.get("information_ratio_per_bar").is_none());
        let rendered = benchmark_html_section(&json!({"benchmark_analysis": analysis})).unwrap();
        assert!(rendered.contains("信息比率(每日)"));
        assert!(!rendered.contains("信息比率(每 Bar)"));
        let _ = std::fs::remove_dir_all(summary.parent().unwrap());
    }

    #[test]
    fn explicit_calendar_rejects_missing_first_or_last_observation() {
        let missing_first = [1_704_245_400_000_u64, 1_704_331_800_000];
        let missing_last = [1_704_159_000_000_u64, 1_704_245_400_000];
        for timestamps in [&missing_first[..], &missing_last[..]] {
            let (summary, benchmark, calendar) = report_inputs(timestamps);
            let error = benchmark_analysis(&summary, &benchmark, 100, Some(&calendar)).unwrap_err();
            assert!(error.contains("missing_trading_day=Some"), "{error}");
            let _ = std::fs::remove_dir_all(summary.parent().unwrap());
        }
    }

    #[test]
    fn explicit_calendar_rejects_an_interior_gap_in_either_series() {
        let timestamps = [1_704_159_000_000_u64, 1_704_245_400_000, 1_704_331_800_000];

        let (summary, benchmark, calendar) = report_inputs(&[timestamps[0], timestamps[2]]);
        let error = benchmark_analysis(&summary, &benchmark, 100, Some(&calendar)).unwrap_err();
        assert!(error.contains("组合权益"), "{error}");
        assert!(error.contains("missing_trading_day=Some"), "{error}");
        let _ = std::fs::remove_dir_all(summary.parent().unwrap());

        let (summary, benchmark, calendar) = report_inputs(&timestamps);
        let mut frame: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&benchmark).unwrap()).unwrap();
        for field in [
            "ts",
            "open_raw",
            "high_raw",
            "low_raw",
            "close_raw",
            "volume_raw",
        ] {
            frame[field].as_array_mut().unwrap().remove(1);
        }
        std::fs::write(&benchmark, serde_json::to_vec(&frame).unwrap()).unwrap();
        let error = benchmark_analysis(&summary, &benchmark, 100, Some(&calendar)).unwrap_err();
        assert!(error.contains("基准行情"), "{error}");
        assert!(error.contains("missing_trading_day=Some"), "{error}");
        let _ = std::fs::remove_dir_all(summary.parent().unwrap());
    }
}
