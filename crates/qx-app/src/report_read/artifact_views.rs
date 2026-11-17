//! Shared bounded artifact readers, CSV projection and core research report assembly.

use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn read_report(
    run_id: &str,
    instrument: &str,
    summary_path: &str,
    equity_path: &str,
    fills_path: &str,
    verification: VerificationResult,
    declared_artifacts: Option<&RunRecordArtifactLocators>,
    reader: &dyn ArtifactReader,
) -> Result<ResearchRunReportV1, AppError> {
    let summary = if verification.verified {
        match declared_artifacts {
            Some(artifacts) => {
                read_json_text_by_id(reader, &artifacts.summary, MAX_SUMMARY_BYTES, "研究摘要")?
            }
            None => read_json_bounded(reader, summary_path, MAX_SUMMARY_BYTES, "研究摘要")?,
        }
    } else {
        read_json_bounded(reader, summary_path, MAX_SUMMARY_BYTES, "研究摘要")
            .unwrap_or(Value::Null)
    };
    let (equity, fills) = if verification.verified {
        match declared_artifacts {
            Some(artifacts) => (
                read_csv_view_by_id(reader, &artifacts.equity, MAX_EQUITY_ROWS, true)?,
                read_csv_view_by_id(reader, &artifacts.fills, MAX_FILL_ROWS, false)?,
            ),
            None => (
                read_csv_view(reader, equity_path, MAX_EQUITY_ROWS, true)?,
                read_csv_view(reader, fills_path, MAX_FILL_ROWS, false)?,
            ),
        }
    } else {
        (empty_rows(), empty_rows())
    };
    Ok(ResearchRunReportV1 {
        schema_version: RESEARCH_RUN_REPORT_SCHEMA_VERSION,
        run_id: run_id.to_owned(),
        instrument: instrument.to_owned(),
        verification,
        summary,
        equity,
        fills,
    })
}

pub(super) fn read_json_bounded(
    reader: &dyn ArtifactReader,
    path: &str,
    byte_limit: u64,
    label: &str,
) -> Result<Value, AppError> {
    let body = read_text_bounded(reader, path, byte_limit, label)?;
    serde_json::from_str(&body).map_err(|error| {
        AppError::new(
            AppErrorCategory::StorageFailure,
            format!("{label} {path} 格式损坏: {error}"),
        )
    })
}

pub(super) fn read_json_text_by_id(
    reader: &dyn ArtifactReader,
    locator: &ArtifactLocatorV1,
    byte_limit: u64,
    label: &str,
) -> Result<Value, AppError> {
    let body = read_text_bounded_by_id(
        reader,
        &locator.id,
        Some(&locator.legacy_reference),
        byte_limit,
        label,
    )?;
    serde_json::from_str(&body).map_err(|error| {
        AppError::new(
            AppErrorCategory::StorageFailure,
            format!("{label} {} 格式损坏: {error}", locator.id.as_str()),
        )
    })
}

pub(super) fn read_csv_view(
    artifact_reader: &dyn ArtifactReader,
    path: &str,
    sample_limit: usize,
    equity: bool,
) -> Result<ResearchReportRowsV1, AppError> {
    read_csv_view_with_opener(path, sample_limit, equity, None, || {
        open_bounded(artifact_reader, path, MAX_ARTIFACT_BYTES)
    })
}

pub(super) fn read_csv_view_by_id(
    artifact_reader: &dyn ArtifactReader,
    locator: &ArtifactLocatorV1,
    sample_limit: usize,
    equity: bool,
) -> Result<ResearchReportRowsV1, AppError> {
    read_csv_view_with_opener(
        locator.id.as_str(),
        sample_limit,
        equity,
        Some(&locator.id),
        || open_bounded_by_id(artifact_reader, locator, MAX_ARTIFACT_BYTES),
    )
}

pub(super) fn read_csv_view_with_opener<F>(
    reference: &str,
    sample_limit: usize,
    equity: bool,
    expected_id: Option<&ArtifactId>,
    mut open: F,
) -> Result<ResearchReportRowsV1, AppError>
where
    F: FnMut() -> Result<ArtifactStream, AppError>,
{
    let mut shape_artifact = open()?;
    let (headers, expected_total) = csv_shape(
        shape_artifact.reader.as_mut(),
        shape_artifact.byte_len,
        reference,
    )?;
    let timestamp_column = headers
        .iter()
        .position(|value| matches!(value.as_str(), "ts" | "timestamp" | "index"));
    let equity_column = headers.iter().position(|value| value == "equity_raw");
    let equity_columns = if equity {
        Some((
            timestamp_column.ok_or_else(|| malformed_csv("权益 CSV 缺少 ts/index 列"))?,
            equity_column.ok_or_else(|| malformed_csv("权益 CSV 缺少 equity_raw 列"))?,
        ))
    } else {
        None
    };
    let mut sample_artifact = open()?;
    let (header, header_bytes) = read_bounded_line(sample_artifact.reader.as_mut(), reference)?
        .ok_or_else(|| malformed_csv("CSV 产物为空"))?;
    let mut bytes_read = header_bytes as u64;
    let mut digest = expected_id.map(|_| qx_strategy::file_digest::Sha256Accumulator::new());
    if let Some(digest) = &mut digest {
        digest.update(header.as_bytes());
    }
    // Equity plots need an evenly distributed sample; fills preserve the historical API
    // contract of returning the first rows so the replay-validated API projection matches.
    let stride = if equity {
        expected_total
            .div_ceil(sample_limit.saturating_sub(1).max(1) as u64)
            .max(1)
    } else {
        1
    };
    let mut index = 0u64;
    let mut sampled = Vec::new();
    while let Some((line, bytes)) = read_bounded_line(sample_artifact.reader.as_mut(), reference)? {
        bytes_read = bytes_read.saturating_add(bytes as u64);
        if let Some(digest) = &mut digest {
            digest.update(line.as_bytes());
        }
        if bytes_read > MAX_ARTIFACT_BYTES {
            return Err(artifact_too_large(reference, MAX_ARTIFACT_BYTES));
        }
        if line.trim().is_empty() {
            continue;
        }
        index = index.saturating_add(1);
        if index > MAX_ARTIFACT_ROWS {
            return Err(AppError::new(
                AppErrorCategory::Timeout,
                "研究报告明细超过 500 万行上限",
            ));
        }
        if (index - 1).is_multiple_of(stride) || index == expected_total {
            let values = csv_record(line.trim_end())?;
            if values.len() != headers.len() {
                return Err(malformed_csv("CSV 行列数与表头不一致"));
            }
            if sampled.len() < sample_limit {
                if equity {
                    let (timestamp_column, equity_column) =
                        equity_columns.ok_or_else(|| malformed_csv("权益 CSV 列索引未初始化"))?;
                    sampled.push(equity_object(
                        &headers,
                        values,
                        timestamp_column,
                        equity_column,
                    )?);
                } else {
                    sampled.push(csv_object(&headers, values));
                }
            }
        }
    }
    if index != expected_total {
        return Err(malformed_csv(
            "CSV 在读取期间发生变化，数据行数与预检不一致",
        ));
    }
    if let (Some(expected_id), Some(digest)) = (expected_id, digest) {
        let actual = digest.finish_hex();
        if actual != expected_id.as_str() {
            return Err(AppError::new(
                AppErrorCategory::Conflict,
                format!(
                    "报告 CSV 与 ArtifactId 内容摘要不一致 {reference}: 期望={} 实际={actual}",
                    expected_id.as_str()
                ),
            ));
        }
    }
    Ok(ResearchReportRowsV1 {
        total: expected_total,
        truncated: sampled.len() < expected_total as usize,
        rows: sampled,
    })
}

pub(super) fn open_bounded(
    reader: &dyn ArtifactReader,
    reference: &str,
    byte_limit: u64,
) -> Result<ArtifactStream, AppError> {
    let artifact = reader.open(reference)?;
    if artifact.byte_len > byte_limit {
        return Err(artifact_too_large(reference, byte_limit));
    }
    Ok(artifact)
}

pub(super) fn open_bounded_by_id(
    reader: &dyn ArtifactReader,
    locator: &ArtifactLocatorV1,
    byte_limit: u64,
) -> Result<ArtifactStream, AppError> {
    let artifact = reader.open_by_id(&locator.id, Some(&locator.legacy_reference))?;
    if artifact.byte_len > byte_limit {
        return Err(artifact_too_large(locator.id.as_str(), byte_limit));
    }
    Ok(artifact)
}

pub(super) fn csv_record(line: &str) -> Result<Vec<String>, AppError> {
    let mut fields = Vec::new();
    let mut field = String::new();
    let mut chars = line.chars().peekable();
    let mut quoted = false;
    while let Some(character) = chars.next() {
        match character {
            '"' if quoted && chars.peek() == Some(&'"') => {
                chars.next();
                field.push('"');
            }
            '"' => quoted = !quoted,
            ',' if !quoted => fields.push(std::mem::take(&mut field)),
            value => field.push(value),
        }
    }
    if quoted {
        return Err(malformed_csv("CSV 引号未闭合"));
    }
    fields.push(field);
    Ok(fields)
}

pub(super) fn csv_shape(
    reader: &mut dyn BufRead,
    byte_limit: u64,
    reference: &str,
) -> Result<(Vec<String>, u64), AppError> {
    let (line, bytes) =
        read_bounded_line(reader, reference)?.ok_or_else(|| malformed_csv("CSV 产物为空"))?;
    if bytes as u64 > byte_limit {
        return Err(artifact_too_large(reference, byte_limit));
    }
    let headers = csv_record(line.trim_end())?;
    if headers.is_empty() || headers.iter().any(|header| header.is_empty()) {
        return Err(malformed_csv("CSV 表头为空或存在空字段"));
    }
    let mut total = 0u64;
    let mut bytes_seen = bytes as u64;
    while let Some((line, bytes)) = read_bounded_line(reader, reference)? {
        bytes_seen = bytes_seen.saturating_add(bytes as u64);
        if bytes_seen > byte_limit {
            return Err(artifact_too_large(reference, byte_limit));
        }
        if !line.trim().is_empty() {
            total += 1;
            if total > MAX_ARTIFACT_ROWS {
                return Err(AppError::new(
                    AppErrorCategory::Timeout,
                    "研究报告明细超过 500 万行上限",
                ));
            }
        }
    }
    Ok((headers, total))
}

pub(super) fn csv_object(headers: &[String], values: Vec<String>) -> Value {
    Value::Object(
        headers
            .iter()
            .cloned()
            .zip(values.into_iter().map(Value::String))
            .collect(),
    )
}

pub(super) fn equity_object(
    headers: &[String],
    values: Vec<String>,
    timestamp_column: usize,
    equity_column: usize,
) -> Result<Value, AppError> {
    let timestamp = values
        .get(timestamp_column)
        .ok_or_else(|| malformed_csv("权益 CSV 时间列缺失"))?;
    timestamp
        .parse::<i128>()
        .map_err(|_| malformed_csv("权益 CSV 时间戳不是整数"))?;
    let equity_raw = values
        .get(equity_column)
        .ok_or_else(|| malformed_csv("权益 CSV equity_raw 缺失"))?;
    equity_raw
        .parse::<i128>()
        .map_err(|_| malformed_csv("权益 CSV equity_raw 不是整数"))?;
    let mut row = serde_json::json!({"ts": timestamp, "equity_raw": equity_raw});
    if let Some(position_column) = headers.iter().position(|value| value == "position_raw") {
        let position_raw = values
            .get(position_column)
            .ok_or_else(|| malformed_csv("权益 CSV position_raw 缺失"))?;
        position_raw
            .parse::<i128>()
            .map_err(|_| malformed_csv("权益 CSV position_raw 不是整数"))?;
        row["position_raw"] = Value::String(position_raw.clone());
    }
    Ok(row)
}

pub(super) fn empty_rows() -> ResearchReportRowsV1 {
    ResearchReportRowsV1 {
        total: 0,
        truncated: false,
        rows: Vec::new(),
    }
}

pub(super) fn malformed_csv(message: &str) -> AppError {
    AppError::new(AppErrorCategory::StorageFailure, message)
}

pub(super) fn malformed_event_log(message: &str) -> AppError {
    AppError::new(AppErrorCategory::StorageFailure, message)
}
