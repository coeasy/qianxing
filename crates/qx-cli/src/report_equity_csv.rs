//! 权益 CSV 的读取和字段校验，供 HTML、基准与 XLSX 报告共用。

use std::path::Path;

/// 读已登记的 2 列、标准 3/4 列和多腿组合 4 列权益格式。非表头的行解析不出即报错，
/// 不静默跳过——静默跳过的曲线会与摘要数字对不上，那是更难查的错。
pub(crate) fn read_equity_csv(path: &Path) -> Result<Vec<(i64, i128)>, String> {
    Ok(read_equity_analysis_csv(path)?
        .into_iter()
        .map(|(ts, equity, _)| (ts, equity))
        .collect())
}

/// 读报告分析用权益序列，并保留工件已有的逐 Bar 持仓字段。
pub(crate) fn read_equity_analysis_csv(
    path: &Path,
) -> Result<Vec<(i64, i128, Option<i128>)>, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("读取权益曲线失败 {}: {error}", path.display()))?;
    let header = text.lines().next().unwrap_or_default().trim();
    let (ts_index, equity_index, combined_equity_index, position_index) = match header {
        "ts,equity_raw,position_raw" => (0, 1, None, Some(2)),
        "index,ts,equity_raw,position_raw" => (1, 2, None, Some(3)),
        "ts,equity_raw" => (0, 1, None, None),
        // Multi-leg composition persists the verified legs separately. Sum their synchronized
        // equity facts for the combined portfolio curve shown by this report.
        "index,ts,primary_equity_raw,reference_equity_raw" => (1, 2, Some(3), None),
        _ => {
            return Err(format!(
                "权益曲线 {} 表头不属于已登记的 2 列、标准 3/4 列或多腿组合 4 列契约: {header}",
                path.display()
            ));
        }
    };
    let mut rows = Vec::new();
    for (line_no, line) in text.lines().enumerate() {
        if line_no == 0 || line.trim().is_empty() {
            continue;
        }
        let cols: Vec<&str> = line.split(',').collect();
        let ts = cols
            .get(ts_index)
            .and_then(|value| value.parse::<i64>().ok())
            .ok_or_else(|| format!("权益曲线 {} 第 {} 行 ts 非法", path.display(), line_no + 1))?;
        let mut equity = cols
            .get(equity_index)
            .and_then(|value| value.parse::<i128>().ok())
            .ok_or_else(|| {
                format!(
                    "权益曲线 {} 第 {} 行 equity_raw 非法",
                    path.display(),
                    line_no + 1
                )
            })?;
        if let Some(index) = combined_equity_index {
            let reference_equity = cols
                .get(index)
                .and_then(|value| value.parse::<i128>().ok())
                .ok_or_else(|| {
                    format!(
                        "权益曲线 {} 第 {} 行 reference_equity_raw 非法",
                        path.display(),
                        line_no + 1
                    )
                })?;
            equity = equity.checked_add(reference_equity).ok_or_else(|| {
                format!(
                    "权益曲线 {} 第 {} 行多腿合并权益溢出",
                    path.display(),
                    line_no + 1
                )
            })?;
        }
        let position = position_index
            .and_then(|index| cols.get(index))
            .filter(|value| !value.is_empty())
            .map(|value| {
                value.parse::<i128>().map_err(|_| {
                    format!(
                        "权益曲线 {} 第 {} 行 position_raw 非法",
                        path.display(),
                        line_no + 1
                    )
                })
            })
            .transpose()?;
        rows.push((ts, equity, position));
    }
    Ok(rows)
}
