//! 权益 CSV 的读取和字段校验，供 HTML、基准与 XLSX 报告共用。

use std::path::Path;

/// 读已登记的 2 列、标准 3/4/5 列和多腿组合 4/6 列权益格式。非表头的行解析不出即报错，
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
    let (
        ts_index,
        equity_index,
        exact_index,
        combined_equity_index,
        declared_combined_equity_index,
        position_index,
    ) = match header {
        "ts,equity_raw,position_raw" => (0, 1, None, None, None, Some(2)),
        "index,ts,equity_raw,position_raw" => (1, 2, None, None, None, Some(3)),
        "ts,equity_raw,equity_exact,position_raw" => (0, 1, Some(2), None, None, Some(3)),
        "index,ts,equity_raw,equity_exact,position_raw" => (1, 2, Some(3), None, None, Some(4)),
        "ts,equity_raw" => (0, 1, None, None, None, None),
        // Multi-leg composition persists the verified legs separately. Sum their synchronized
        // equity facts for the combined portfolio curve shown by this report.
        "index,ts,primary_equity_raw,reference_equity_raw" => (1, 2, None, Some(3), None, None),
        "index,ts,primary_equity_raw,reference_equity_raw,equity_raw,equity_exact" => {
            (1, 2, Some(5), Some(3), Some(4), None)
        }
        _ => {
            return Err(format!(
                "权益曲线 {} 表头不属于已登记的 2 列、标准 3/4/5 列或多腿组合 4/6 列契约: {header}",
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
        if let Some(index) = declared_combined_equity_index {
            let declared = cols
                .get(index)
                .and_then(|value| value.parse::<i128>().ok())
                .ok_or_else(|| {
                    format!(
                        "权益曲线 {} 第 {} 行 equity_raw 非法",
                        path.display(),
                        line_no + 1
                    )
                })?;
            if declared != equity {
                return Err(format!(
                    "权益曲线 {} 第 {} 行腿权益合计与 equity_raw 不一致",
                    path.display(),
                    line_no + 1
                ));
            }
        }
        if let Some(index) = exact_index {
            let exact = cols
                .get(index)
                .and_then(|value| qx_core::ExactDecimal::parse(value).ok())
                .ok_or_else(|| {
                    format!(
                        "权益曲线 {} 第 {} 行 equity_exact 非法",
                        path.display(),
                        line_no + 1
                    )
                })?;
            let projected = exact
                .checked_mul_at_scale(
                    qx_core::ExactDecimal::ONE,
                    9,
                    qx_core::DecimalRounding::TowardZero,
                )
                .and_then(|value| value.to_raw(9).ok())
                .ok_or_else(|| {
                    format!(
                        "权益曲线 {} 第 {} 行 equity_exact 无法投影到 raw",
                        path.display(),
                        line_no + 1
                    )
                })?;
            if projected != equity {
                return Err(format!(
                    "权益曲线 {} 第 {} 行 equity_raw 与 equity_exact 投影不一致",
                    path.display(),
                    line_no + 1
                ));
            }
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
