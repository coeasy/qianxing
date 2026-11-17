//! Strategy correctness diagnostic summary and lossless evidence sheets.

use super::{t, Sheet};
use serde_json::Value;

/// Add a compact, filterable diagnostic summary and lossless JSON evidence to a workbook.
/// The evidence is split into UTF-16-safe chunks below Excel's 32,767-character cell limit.
pub(super) fn strategy_diagnostics_sheets(diagnostics: &Value) -> Result<Vec<Sheet>, String> {
    const MAX_CELL_UTF16_UNITS: usize = 30_000;
    let records: Vec<&Value> = match diagnostics {
        Value::Array(items) => items.iter().collect(),
        Value::Object(_) => vec![diagnostics],
        _ => return Err("策略诊断 XLSX 输入必须是对象或对象数组".into()),
    };
    if records.is_empty() {
        return Err("策略诊断 XLSX 输入不能为空".into());
    }

    let has_run_id = records
        .iter()
        .any(|record| record.get("run_id").and_then(Value::as_str).is_some());
    let mut summary_header = vec![
        t("diagnostic_id"),
        t("instrument"),
        t("strategy_id"),
        t("data_fingerprint"),
        t("input_kind"),
        t("input_points"),
        t("as_of"),
        t("lookahead_status"),
        t("startup_status"),
        t("pit_visibility_status"),
    ];
    if has_run_id {
        summary_header.insert(0, t("run_id"));
    }
    let mut summary_rows = vec![summary_header];
    let mut evidence_rows = vec![vec![
        t("diagnostic_id"),
        t("chunk_index"),
        t("chunk_count"),
        t("json_utf8_fragment"),
    ]];

    for (index, record) in records.iter().enumerate() {
        if !record.is_object() {
            return Err(format!("策略诊断 XLSX 第 {} 项必须是对象", index + 1));
        }
        let detail = record
            .get("report")
            .filter(|value| value.is_object())
            .unwrap_or(record);
        let text = |key: &str| -> String {
            record
                .get(key)
                .and_then(Value::as_str)
                .or_else(|| detail.get(key).and_then(Value::as_str))
                .unwrap_or("—")
                .to_owned()
        };
        let points = record
            .get("input_points")
            .or_else(|| record.get("input_bars"))
            .or_else(|| record.get("input_events"))
            .or_else(|| detail.get("input_bars"))
            .or_else(|| detail.get("input_events"))
            .map(Value::to_string)
            .unwrap_or_else(|| "—".into());
        let as_of = record
            .get("as_of")
            .or_else(|| detail.get("as_of"))
            .map(Value::to_string)
            .unwrap_or_else(|| "—".into());
        let status = |pointer: &str| {
            detail
                .pointer(pointer)
                .and_then(Value::as_str)
                .unwrap_or("missing")
                .to_owned()
        };
        let diagnostic_id = (index + 1).to_string();
        let mut row = vec![
            t(&diagnostic_id),
            t(text("instrument")),
            t(text("strategy_id")),
            t(text("data_fingerprint")),
            t(text("input_kind")),
            t(&points),
            t(&as_of),
            t(status("/lookahead/status")),
            t(status("/startup/status")),
            t(status("/data_visibility/status")),
        ];
        if has_run_id {
            row.insert(0, t(text("run_id")));
        }
        summary_rows.push(row);

        let json = serde_json::to_string(record)
            .map_err(|error| format!("策略诊断 XLSX JSON 序列化失败: {error}"))?;
        let fragments = split_utf16_chunks(&json, MAX_CELL_UTF16_UNITS);
        let chunk_count = fragments.len().to_string();
        for (chunk_index, fragment) in fragments.into_iter().enumerate() {
            evidence_rows.push(vec![
                t(&diagnostic_id),
                t((chunk_index + 1).to_string()),
                t(&chunk_count),
                t(&fragment),
            ]);
        }
    }
    Ok(vec![
        ("StrategyDiagnostics", summary_rows),
        ("DiagnosticEvidence", evidence_rows),
    ])
}

pub(super) fn split_utf16_chunks(value: &str, max_units: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut current_units = 0;
    for character in value.chars() {
        let units = character.len_utf16();
        if current_units > 0 && current_units + units > max_units {
            chunks.push(std::mem::take(&mut current));
            current_units = 0;
        }
        current.push(character);
        current_units += units;
    }
    if !current.is_empty() || chunks.is_empty() {
        chunks.push(current);
    }
    chunks
}
