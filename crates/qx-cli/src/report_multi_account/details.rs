use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

const MAX_MULTI_ACCOUNT_DIAGNOSTICS_BYTES: u64 = 1024 * 1024;

pub(super) fn evidence_summary_stem(summary_path: &Path) -> Result<String, String> {
    summary_path
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_suffix(".summary.json"))
        .map(str::to_string)
        .ok_or_else(|| format!("账户 summary 文件名无效: {}", summary_path.display()))
}

pub(super) fn account_diagnostic_snapshot_path(
    summary_path: &Path,
    account_id: &str,
) -> Result<PathBuf, String> {
    let stem = evidence_summary_stem(summary_path)?;
    let account_digest = qx_strategy::sha256_hex(account_id.as_bytes());
    Ok(summary_path.with_file_name(format!(
        "{stem}.account-{}.strategy-diagnostics.json",
        &account_digest[..16]
    )))
}

/// Write a stable, account-specific diagnostic snapshot for a standard single-run EvidenceBundle.
/// Existing bytes are reused only when identical; a different file is never overwritten.
pub(super) fn write_account_diagnostic_snapshot(
    summary_path: &Path,
    account_id: &str,
    report: &Value,
) -> Result<(PathBuf, bool), String> {
    let path = account_diagnostic_snapshot_path(summary_path, account_id)?;
    let bytes = serde_json::to_vec_pretty(report)
        .map_err(|error| format!("编码账户 {account_id} 策略诊断快照失败: {error}"))?;
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(mut file) => {
            if let Err(error) = file.write_all(&bytes).and_then(|()| file.sync_all()) {
                let _ = std::fs::remove_file(&path);
                return Err(format!(
                    "写入账户 {account_id} 策略诊断快照失败 {}: {error}",
                    path.display()
                ));
            }
            Ok((path, true))
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let existing = std::fs::read(&path).map_err(|read_error| {
                format!(
                    "读取已有账户策略诊断快照失败 {}: {read_error}",
                    path.display()
                )
            })?;
            if existing != bytes {
                return Err(format!(
                    "账户 {account_id} 的策略诊断快照已存在且内容不同，拒绝覆盖: {}",
                    path.display()
                ));
            }
            Ok((path, false))
        }
        Err(error) => Err(format!(
            "创建账户 {account_id} 策略诊断快照失败 {}: {error}",
            path.display()
        )),
    }
}

pub(super) fn read_depth_account_fills(
    report: &qx_app::ResearchRunReportV1,
    expected: u64,
    row_limit: usize,
) -> Result<(Vec<Value>, u64), String> {
    if report.fills.total != expected {
        return Err(format!(
            "Depth 成交报告行数与 outcome.fills 不一致: rows={} expected={expected}",
            report.fills.total
        ));
    }
    let rows = &report.fills.rows;
    let start = rows.len().saturating_sub(row_limit);
    let projected = rows[start..]
        .iter()
        .map(|row| {
            let timestamp_ms = row
                .get("ts")
                .and_then(|value| {
                    value
                        .as_u64()
                        .or_else(|| value.as_str().and_then(|text| text.parse::<u64>().ok()))
                })
                .ok_or("Depth 成交报告缺少有效 ts")?;
            let cell = |name: &str| {
                row.get(name)
                    .map(|value| {
                        value
                            .as_str()
                            .map(str::to_owned)
                            .unwrap_or_else(|| value.to_string())
                    })
                    .ok_or_else(|| format!("Depth 成交报告缺少 {name}"))
            };
            Ok::<_, String>(json!({
                "timestamp_ms": timestamp_ms,
                "order_id": cell("order_id")?,
                "qty_raw": cell("qty_raw")?,
                "price_raw": cell("price_raw")?,
                "fee_raw": cell("fee_raw")?,
                "strategy_id": row.get("strategy_id").filter(|value| !value.is_null()),
                "side": null,
                "side_status": "absent_from_fill_contract",
            }))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok((projected, report.fills.total))
}

pub(super) fn read_account_fills(
    path: &str,
    expected: u64,
    row_limit: usize,
) -> Result<(Vec<Value>, u64), String> {
    let (fills, total) = crate::report_html::read_fills_csv_tail(Path::new(path), row_limit)?;
    if total != expected {
        return Err(format!(
            "成交工件行数与 outcome.fills 不一致 {path}: rows={total} expected={expected}",
        ));
    }
    let rows = fills
        .into_iter()
        .map(|fill| {
            json!({
                "timestamp_ms": fill.ts,
                "order_id": fill.order_id.to_string(),
                "qty_raw": fill.qty_raw.to_string(),
                "qty_exact": fill.qty_exact,
                "price_raw": fill.price_raw.to_string(),
                "price_exact": fill.price_exact,
                "fee_raw": fill.fee_raw.map(|value| value.to_string()),
                "exact_fees_json": fill.exact_fees_json,
                "strategy_id": if fill.strategy_id.is_empty() { None } else { Some(fill.strategy_id) },
                "side": null,
                "side_status": "absent_from_fill_contract",
            })
        })
        .collect();
    Ok((rows, total))
}

pub(super) struct MultiAccountDiagnostics {
    pub(super) path: String,
    pub(super) sha256: String,
    pub(super) reports: BTreeMap<String, Value>,
}

pub(super) fn read_multi_account_diagnostics(
    path: &Path,
) -> Result<MultiAccountDiagnostics, String> {
    let size = std::fs::metadata(path)
        .map_err(|error| format!("读取多账户策略诊断文件属性失败 {}: {error}", path.display()))?
        .len();
    if size > MAX_MULTI_ACCOUNT_DIAGNOSTICS_BYTES {
        return Err(format!(
            "多账户策略诊断文件超过 {} MiB 上限: {}",
            MAX_MULTI_ACCOUNT_DIAGNOSTICS_BYTES / (1024 * 1024),
            path.display()
        ));
    }
    let bytes = std::fs::read(path)
        .map_err(|error| format!("读取多账户策略诊断文件失败 {}: {error}", path.display()))?;
    let payload: Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("多账户策略诊断 JSON 无效 {}: {error}", path.display()))?;
    if payload.get("schema_version").and_then(Value::as_u64) != Some(1)
        || payload.get("kind").and_then(Value::as_str)
            != Some("qianxing-multi-account-strategy-bias-diagnostics")
        || payload.as_object().is_none_or(|object| {
            object.len() != 3
                || !object.contains_key("schema_version")
                || !object.contains_key("kind")
                || !object.contains_key("accounts")
        })
    {
        return Err(
            "多账户策略诊断须符合 qianxing-multi-account-strategy-bias-diagnostics v1".into(),
        );
    }
    let accounts = payload
        .get("accounts")
        .and_then(Value::as_array)
        .filter(|accounts| !accounts.is_empty() && accounts.len() <= 32)
        .ok_or("多账户策略诊断 accounts 必须包含 1..=32 项")?;
    let mut reports = BTreeMap::new();
    for item in accounts {
        if item.as_object().is_none_or(|object| {
            object.len() != 2
                || !object.contains_key("account_id")
                || !object.contains_key("report")
        }) {
            return Err("多账户策略诊断项须且仅须包含 account_id 与 report".into());
        }
        let account_id = item
            .get("account_id")
            .and_then(Value::as_str)
            .filter(|account| !account.trim().is_empty())
            .ok_or("多账户策略诊断 account_id 必须为非空字符串")?;
        let report = item
            .get("report")
            .filter(|report| report.is_object())
            .ok_or_else(|| format!("账户 {account_id} 的策略诊断 report 必须为对象"))?;
        if reports
            .insert(account_id.to_string(), report.clone())
            .is_some()
        {
            return Err(format!("多账户策略诊断含重复 account_id: {account_id}"));
        }
    }
    Ok(MultiAccountDiagnostics {
        path: path.to_string_lossy().into_owned(),
        sha256: qx_data::sha256_hex_bytes(&bytes),
        reports,
    })
}
