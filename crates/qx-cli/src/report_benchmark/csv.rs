//! Benchmark CSV serialization and atomic artifact publication.

use std::path::Path;

fn benchmark_csv_text(analysis: &serde_json::Value) -> Result<String, String> {
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
    Ok(csv)
}

pub(crate) fn write_benchmark_csv(analysis: &serde_json::Value, path: &Path) -> Result<(), String> {
    publish_benchmark_csv(&benchmark_csv_text(analysis)?, path)
}

pub(crate) fn write_benchmark_runs_csv(
    runs: &[(String, serde_json::Value)],
    path: &Path,
) -> Result<(), String> {
    let mut output = String::from("run_id,");
    let mut has_header = false;
    for (run_id, analysis) in runs {
        let content = benchmark_csv_text(analysis)?;
        for (index, line) in content.lines().enumerate() {
            if index == 0 {
                if !has_header {
                    output.push_str(line);
                    output.push('\n');
                    has_header = true;
                }
                continue;
            }
            output.push_str(&csv_text(run_id));
            output.push(',');
            output.push_str(line);
            output.push('\n');
        }
    }
    if !has_header {
        return Err("没有可导出的基准分析序列".into());
    }
    publish_benchmark_csv(&output, path)
}

fn publish_benchmark_csv(csv: &str, path: &Path) -> Result<(), String> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("创建基准 CSV 目录失败 {}: {error}", parent.display()))?;
    }
    qx_app::artifact_store::sink::publish(
        path,
        csv,
        qx_app::artifact_store::sink::PublishMode::Overwrite,
    )
    .map_err(|error| format!("发布基准分析 CSV 失败 {}: {error}", path.display()))
}

fn csv_text(value: &str) -> String {
    if value.contains([',', '"', '\r', '\n']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}
