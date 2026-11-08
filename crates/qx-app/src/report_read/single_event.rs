//! Single-market EventLog replay verification and bounded report-page projection.

use super::artifact_views::{malformed_event_log, read_json_bounded};
use super::*;

/// Read a bounded EventLog, replay it, and return a verified page plus its complete fill
/// multiset. Storage authorization is delegated to the caller's `ArtifactReader`; replay and
/// summary/manifest binding remain application responsibilities.
pub fn read_event_report_v1_with_reader(
    reference: &str,
    summary: &Value,
    manifest_reference: &str,
    requested_page: Option<(u64, usize)>,
    reader: &dyn ArtifactReader,
) -> Result<EventReportPageV1, AppError> {
    if let Some((_, limit)) = requested_page {
        if limit == 0 || limit > MAX_EVENT_REPORT_PAGE_ROWS {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                format!("事件报告分页 limit 必须在 1 到 {MAX_EVENT_REPORT_PAGE_ROWS} 之间"),
            ));
        }
    }
    let manifest = read_json_bounded(reader, manifest_reference, MAX_SUMMARY_BYTES, "RunManifest")?;
    let expected_digest = summary
        .pointer("/replay/log_digest")
        .and_then(Value::as_str)
        .ok_or_else(|| malformed_event_log("摘要缺少 replay.log_digest，无法验证事件轨迹"))?;
    let expected_digest = u64::from_str_radix(expected_digest, 16)
        .map_err(|_| malformed_event_log("摘要 replay.log_digest 不是十六进制摘要"))?;
    let expected_count = summary
        .pointer("/replay/events")
        .and_then(Value::as_u64)
        .ok_or_else(|| malformed_event_log("摘要缺少 replay.events，无法验证事件数量"))?;
    if expected_count > MAX_ARTIFACT_ROWS {
        return Err(AppError::new(
            AppErrorCategory::Timeout,
            "摘要声明的事件数超过 500 万行上限",
        ));
    }
    let expected_count_usize = usize::try_from(expected_count)
        .map_err(|_| malformed_event_log("摘要事件数超出本机索引范围"))?;
    let page = requested_page.unwrap_or((
        expected_count.saturating_sub(MAX_EVENT_REPORT_PAGE_ROWS as u64),
        MAX_EVENT_REPORT_PAGE_ROWS,
    ));
    let offset = page.0.min(expected_count);
    let end = offset.saturating_add(page.1 as u64).min(expected_count);
    let mut rows = Vec::with_capacity(usize::try_from(end - offset).unwrap_or(0));
    let mut fill_counts = std::collections::BTreeMap::<EventFillIdentityV1, usize>::new();
    let mut replay = qx_core::StreamingReplayVerifier::new(expected_count_usize);
    let mut artifact = reader.open(reference)?;
    if artifact.byte_len > MAX_ARTIFACT_BYTES {
        return Err(artifact_too_large(reference, MAX_ARTIFACT_BYTES));
    }
    let mut bytes_seen = 0_u64;
    let mut total = 0_u64;
    while let Some((line, line_bytes)) = read_bounded_line(artifact.reader.as_mut(), reference)? {
        bytes_seen = bytes_seen.saturating_add(line_bytes as u64);
        if bytes_seen > MAX_ARTIFACT_BYTES {
            return Err(artifact_too_large(reference, MAX_ARTIFACT_BYTES));
        }
        if line.trim().is_empty() {
            continue;
        }
        total = total.saturating_add(1);
        if total > MAX_ARTIFACT_ROWS {
            return Err(AppError::new(
                AppErrorCategory::Timeout,
                "事件轨迹超过 500 万行上限",
            ));
        }
        let event: qx_core::Event = serde_json::from_str(line.trim_end()).map_err(|error| {
            AppError::new(
                AppErrorCategory::StorageFailure,
                format!("事件轨迹第 {total} 行格式损坏: {error}"),
            )
        })?;
        if let qx_core::EventKind::Filled { fill } = &event.kind {
            let identity = EventFillIdentityV1 {
                ts: fill.ts,
                order_id: fill.order_id,
                qty_raw: fill.qty.raw(),
                price_raw: fill.price.raw(),
                fee_raw: fill.fee.raw(),
                strategy_id: fill.strategy_id.clone().unwrap_or_default(),
            };
            *fill_counts.entry(identity).or_default() += 1;
        }
        let event_index = total - 1;
        if event_index >= offset && event_index < end {
            rows.push(serde_json::to_value(&event).map_err(|error| {
                AppError::new(
                    AppErrorCategory::InternalInvariant,
                    format!("事件轨迹视图序列化失败: {error}"),
                )
            })?);
        }
        replay.append(event).map_err(|error| {
            AppError::new(
                AppErrorCategory::StorageFailure,
                format!("事件轨迹重放失败: {error}"),
            )
        })?;
    }
    let replayed = replay.finish().map_err(|error| {
        AppError::new(
            AppErrorCategory::StorageFailure,
            format!("事件轨迹重放失败: {error}"),
        )
    })?;
    if total != expected_count {
        return Err(malformed_event_log(
            "事件轨迹行数与 summary.replay.events 不一致",
        ));
    }
    if replayed.log_digest != expected_digest {
        return Err(malformed_event_log(
            "事件轨迹摘要与 summary.replay.log_digest 不一致",
        ));
    }
    let expected_manifest_digest = format!("{:016x}", replayed.log_digest);
    if manifest
        .pointer("/output_event_hash")
        .and_then(Value::as_str)
        != Some(expected_manifest_digest.as_str())
    {
        return Err(malformed_event_log(
            "RunManifest.output_event_hash 与 replay-verified 事件轨迹不一致",
        ));
    }
    let replayed_ledger_count = replayed.ledger.entries().len() as u64;
    for (path, expected) in [
        ("/ledger_entries", summary.pointer("/ledger_entries")),
        (
            "/replay_ledger_entries",
            summary.pointer("/replay_ledger_entries"),
        ),
        (
            "/replay/ledger_entries",
            summary.pointer("/replay/ledger_entries"),
        ),
        (
            "/replay/run_ledger_entries",
            summary.pointer("/replay/run_ledger_entries"),
        ),
    ] {
        if expected.and_then(Value::as_u64) != Some(replayed_ledger_count) {
            return Err(malformed_event_log(&format!(
                "summary{path} 与重放重建账簿条数不一致"
            )));
        }
    }

    Ok(EventReportPageV1 {
        schema_version: EVENT_REPORT_PAGE_SCHEMA_VERSION,
        total,
        offset,
        truncated: rows.len() < total as usize,
        rows,
        fill_counts: fill_counts
            .into_iter()
            .map(|(identity, count)| EventFillCountV1 { identity, count })
            .collect(),
    })
}
