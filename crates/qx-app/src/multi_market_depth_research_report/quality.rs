//! Depth report quality evidence collection, matching, and projection.

use super::{conflict, AppError, MultiMarketDepthBacktestSpec, Value};
use std::collections::{BTreeMap, BTreeSet};

pub(super) fn verify_quality_record_set(
    spec: &MultiMarketDepthBacktestSpec,
    quality_records: &Value,
) -> Result<(), AppError> {
    let records = quality_records
        .as_object()
        .ok_or_else(|| conflict("多市场深度数据质量记录必须是对象"))?;
    let expected = spec
        .markets
        .iter()
        .filter(|market| market.dataset_manifest_path.is_some())
        .map(|market| market.instrument.clone())
        .collect::<BTreeSet<_>>();
    let actual = records.keys().cloned().collect::<BTreeSet<_>>();
    if expected != actual {
        return Err(conflict("多市场深度质量记录集合与 spec 声明不一致"));
    }
    Ok(())
}

pub(super) fn quality_records_by_market(
    artifact: &Value,
    market_data: &[Value],
) -> Result<Value, AppError> {
    if let Some(records) = artifact.get("dataset_quality") {
        if records.is_object() {
            return Ok(records.clone());
        }
        return Err(conflict("多市场深度数据质量记录必须是对象"));
    }
    let mut records = serde_json::Map::new();
    for row in market_data {
        let instrument = row
            .get("instrument")
            .and_then(Value::as_str)
            .ok_or_else(|| conflict("多市场深度 market_data 缺少 instrument"))?;
        if let Some(quality) = row.get("dataset_quality").filter(|value| !value.is_null()) {
            if records
                .insert(instrument.to_owned(), quality.clone())
                .is_some()
            {
                return Err(conflict("多市场深度质量记录包含重复标的"));
            }
        }
    }
    Ok(Value::Object(records))
}

pub(super) fn depth_dataset_quality_panel(
    record: Option<&Value>,
    quality_fingerprint: Option<&str>,
) -> Result<Option<Value>, AppError> {
    let Some(record) = record else {
        return Ok(None);
    };
    let quality_report = record
        .get("quality_report")
        .cloned()
        .filter(Value::is_object)
        .ok_or_else(|| conflict("多市场深度质量记录缺少有效 quality_report"))?;
    let report_is_clean = ["duplicate_rows", "out_of_order_rows", "missing_intervals"]
        .iter()
        .all(|field| quality_report.get(field).and_then(Value::as_u64) == Some(0));
    Ok(Some(serde_json::json!({
        "manifest_fingerprint": record.get("manifest_fingerprint").cloned().unwrap_or(Value::Null),
        "dataset_quality_fingerprint": quality_fingerprint,
        "manifest_sha256": record.get("manifest_sha256").cloned().unwrap_or(Value::Null),
        "quality_report": quality_report,
        "status": if report_is_clean { "clean" } else { "issues" },
    })))
}

pub(super) fn index_market_data(rows: &[Value]) -> Result<BTreeMap<&str, &Value>, AppError> {
    let mut indexed = BTreeMap::new();
    for row in rows {
        let instrument = row
            .get("instrument")
            .and_then(Value::as_str)
            .ok_or_else(|| conflict("多市场深度 market_data 缺少 instrument"))?;
        if indexed.insert(instrument, row).is_some() {
            return Err(conflict("多市场深度 market_data 包含重复标的"));
        }
    }
    Ok(indexed)
}
