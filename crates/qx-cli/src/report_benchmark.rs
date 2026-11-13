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
    let window = summary_clock_window(summary_path)?;
    benchmark_analysis_for_portfolio(
        &portfolio,
        benchmark_path,
        portfolio_return_bps,
        calendar_path,
        window,
    )
}

pub(crate) fn benchmark_analysis_for_portfolio(
    portfolio: &[(u64, i128)],
    benchmark_path: &Path,
    portfolio_return_bps: i64,
    calendar_path: Option<&Path>,
    window: (u64, u64),
) -> Result<serde_json::Value, String> {
    if portfolio.len() < 2 {
        return Err("基准对照至少需要两个严格对齐的权益观测".into());
    }
    let payload = std::fs::read_to_string(benchmark_path).map_err(|error| {
        format!(
            "读取基准 BarFrame 失败 {}: {error}",
            benchmark_path.display()
        )
    })?;
    let frame = BarFrame::from_json(&payload)
        .map_err(|error| format!("基准 BarFrame 无效 {}: {error:?}", benchmark_path.display()))?;
    let calendar = calendar_path
        .map(|path| {
            let payload = std::fs::read_to_string(path)
                .map_err(|error| format!("读取基准交易日历失败 {}: {error}", path.display()))?;
            Ok::<_, String>((path, payload, window))
        })
        .transpose()?;
    qx_app::report_analysis::benchmark_analysis(
        portfolio,
        &frame,
        benchmark_path,
        portfolio_return_bps,
        calendar
            .as_ref()
            .map(|(path, payload, window)| (*path, payload.as_str(), *window)),
    )
}

pub(crate) fn portfolio_from_report_equity(
    equity: &serde_json::Value,
) -> Result<Vec<(u64, i128)>, String> {
    let rows = equity
        .get("rows")
        .and_then(serde_json::Value::as_array)
        .or_else(|| equity.as_array())
        .ok_or("报告缺少可用于基准对照的权益序列")?;
    let mut points: Vec<(u64, i128)> = Vec::with_capacity(rows.len());
    for (index, row) in rows.iter().enumerate() {
        let (timestamp, value) = if let Some(columns) = row.as_array() {
            (columns.first(), columns.get(1))
        } else {
            (row.get("ts"), row.get("equity_raw"))
        };
        let parse_integer = |value: Option<&serde_json::Value>, name: &str| {
            let value = value.ok_or_else(|| format!("权益序列第 {index} 行缺少 {name}"))?;
            value
                .as_i64()
                .map(i128::from)
                .or_else(|| value.as_u64().map(i128::from))
                .or_else(|| {
                    value
                        .is_number()
                        .then(|| value.to_string().parse::<i128>().ok())
                        .flatten()
                })
                .or_else(|| value.as_str().and_then(|text| text.parse::<i128>().ok()))
                .ok_or_else(|| format!("权益序列第 {index} 行 {name} 非法"))
        };
        let timestamp = u64::try_from(parse_integer(timestamp, "ts")?)
            .map_err(|_| format!("权益序列第 {index} 行 ts 非法"))?;
        let equity = parse_integer(value, "equity_raw")?;
        if let Some((previous_timestamp, previous_equity)) = points.last_mut() {
            if timestamp < *previous_timestamp {
                return Err(format!("权益序列时间倒退: index={index} ts={timestamp}"));
            }
            if timestamp == *previous_timestamp {
                *previous_equity = equity;
                continue;
            }
        }
        points.push((timestamp, equity));
    }
    Ok(points)
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

mod csv;
pub(crate) use csv::{write_benchmark_csv, write_benchmark_runs_csv};

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn multi_run_benchmark_csv_keeps_run_identity_and_period_rows() {
        let root = std::env::temp_dir().join(format!(
            "qx-benchmark-multi-csv-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let analysis = |offset| {
            json!({
                "benchmark": {
                    "instrument": "000300.SSE",
                    "fingerprint": "benchmark-fingerprint",
                    "price_basis": "raw_close"
                },
                "alignment": "exact_timestamp",
                "period_basis": "per_bar_no_annualization",
                "series": [{
                    "start_ts": 10,
                    "end_ts": 20,
                    "portfolio_equity_start_raw": 100_000 + offset,
                    "portfolio_equity_end_raw": 101_000 + offset,
                    "benchmark_close_start_raw": 4_000_000,
                    "benchmark_close_end_raw": 4_010_000,
                    "portfolio_return_bps": 100,
                    "benchmark_return_bps": 25,
                    "active_return_bps": 75
                }]
            })
        };
        let path = root.join("benchmark.csv");
        write_benchmark_runs_csv(
            &[
                ("run-a".into(), analysis(0)),
                ("run-b".into(), analysis(1_000)),
            ],
            &path,
        )
        .unwrap();
        let lines = std::fs::read_to_string(&path)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        assert_eq!(lines.len(), 3);
        assert!(lines[0].starts_with("run_id,benchmark_instrument,"));
        assert!(lines[1].starts_with("run-a,000300.SSE,"));
        assert!(lines[2].starts_with("run-b,000300.SSE,"));
        assert!(lines[2].contains("101000"));
        std::fs::remove_dir_all(root).unwrap();
    }

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
        std::fs::write(
            root.join("daily.fills.csv"),
            "order_id,ts,qty_raw,price_raw\n",
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
        let rendered =
            benchmark_html_section(&json!({"benchmark_analysis": analysis.clone()})).unwrap();
        assert!(rendered.contains("信息比率(每日)"));
        assert!(!rendered.contains("信息比率(每 Bar)"));

        let csv_path = summary.parent().unwrap().join("benchmark.csv");
        write_benchmark_csv(&analysis, &csv_path).unwrap();
        let csv = std::fs::read_to_string(&csv_path).unwrap();
        let csv_rows = csv.lines().collect::<Vec<_>>();
        let series = analysis["series"].as_array().unwrap();
        assert_eq!(csv_rows.len(), series.len() + 1);
        assert!(csv_rows[0].contains("period_basis,start_ts,end_ts"));
        for (line, row) in csv_rows.iter().skip(1).zip(series) {
            let fields = line.split(',').collect::<Vec<_>>();
            assert_eq!(fields[4], "per_trading_day_no_annualization");
            for (column, key) in [
                (5, "start_ts"),
                (6, "end_ts"),
                (7, "portfolio_equity_start_raw"),
                (8, "portfolio_equity_end_raw"),
                (9, "benchmark_close_start_raw"),
                (10, "benchmark_close_end_raw"),
                (11, "portfolio_return_bps"),
                (12, "benchmark_return_bps"),
                (13, "active_return_bps"),
            ] {
                assert_eq!(fields[column], row[key].to_string(), "field {key}");
            }
        }

        let mut display_summary = json!({
            "schema_version": 7,
            "run_id": "daily-report-test",
            "instrument": "000300.SSE",
            "strategy_id": "fixture",
            "result_hash": "test-result",
            "metrics": {"return_bps": 125, "max_drawdown_bps": 30},
            "metrics_definition": {
                "schema_version": 1,
                "risk_ratio_period_basis": "per_sample_no_annualization",
                "benchmark_period_basis": "exact_timestamp_aligned_observations_no_annualization",
                "fees_exact_basis": "ledger_fee_entries_grouped_by_currency",
                "fees_raw_basis": "compatibility_projection_only"
            }
        });
        display_summary["benchmark_analysis"] = analysis.clone();
        let rendered = crate::report_html::render_report_html(&display_summary, &[], &[], &[]);
        assert!(rendered.contains("基准对照"));
        assert!(rendered.contains("per_trading_day_no_annualization"));
        assert!(rendered.contains("125"));

        let xlsx_path = summary.parent().unwrap().join("daily.xlsx");
        crate::report_xlsx::write_report_xlsx(
            &summary,
            &display_summary,
            Some(&analysis),
            None,
            &xlsx_path,
        )
        .unwrap();
        let mut workbook = zip::ZipArchive::new(std::fs::File::open(&xlsx_path).unwrap()).unwrap();
        let mut workbook_xml = String::new();
        std::io::Read::read_to_string(
            &mut workbook.by_name("xl/workbook.xml").unwrap(),
            &mut workbook_xml,
        )
        .unwrap();
        assert!(workbook_xml.contains("name=\"Overview\""));
        assert!(workbook_xml.contains("name=\"Benchmark\""));
        let mut benchmark_xml = String::new();
        std::io::Read::read_to_string(
            &mut workbook.by_name("xl/worksheets/sheet5.xml").unwrap(),
            &mut benchmark_xml,
        )
        .unwrap();
        assert!(benchmark_xml.contains("per_trading_day_no_annualization"));
        for row in series {
            for key in [
                "start_ts",
                "end_ts",
                "portfolio_equity_start_raw",
                "portfolio_equity_end_raw",
                "benchmark_close_start_raw",
                "benchmark_close_end_raw",
                "portfolio_return_bps",
                "benchmark_return_bps",
                "active_return_bps",
            ] {
                assert!(
                    benchmark_xml.contains(&row[key].to_string()),
                    "XLSX field {key}"
                );
            }
        }
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
