//! Recompute the input identity declared by a persisted backtest summary.

use super::{
    recompute_multi_barframe_input, verify_declared_run_manifest, verify_declared_run_record,
    BacktestInputProvenance,
};
use crate::backtest_input_identity::{
    barframe_dataset_identity, barframe_input_provenance, depth_frame_input_provenance,
    legacy_barframe_dataset_identity,
};
use qx_datastruct::BarFrame;
use qx_xingban::DepthFrame;
use serde::{Deserialize, Serialize};
use std::path::Path;

const MAX_PREVIEW_INPUT_BYTES: u64 = 128 * 1024 * 1024;

#[derive(Clone, Debug, Serialize)]
pub struct VerifiedBarFramePreview {
    pub total: u64,
    pub truncated: bool,
    pub rows: Vec<VerifiedBarFramePoint>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct VerifiedBarFramePoint {
    pub ts: u64,
    pub open_raw: String,
    pub high_raw: String,
    pub low_raw: String,
    pub close_raw: String,
    pub volume_raw: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct VerifiedBarFrameEventContext {
    pub event_ts: u64,
    pub bar_index: u64,
    pub relation: &'static str,
    pub bar: VerifiedBarFramePoint,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct VerifiedDepthFramePreview {
    pub total: u64,
    pub truncated: bool,
    pub rows: Vec<VerifiedDepthFramePoint>,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct VerifiedDepthFramePoint {
    pub ts: u64,
    pub sequence: u64,
    pub bids: Vec<VerifiedDepthLevel>,
    pub asks: Vec<VerifiedDepthLevel>,
    pub levels_truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct VerifiedDepthLevel {
    pub price_raw: String,
    pub qty_raw: String,
}

/// Return a bounded, exact-integer BarFrame preview only when its source is inside the
/// caller's configured data root and its declared dataset identity re-verifies.
pub fn verified_barframe_preview(
    summary: &serde_json::Value,
    permitted_root: &Path,
    max_points: usize,
) -> Result<Option<VerifiedBarFramePreview>, String> {
    if max_points == 0 {
        return Err("BarFrame 预览点数上限必须为正数".into());
    }
    let Some(frame) = verified_barframe_frame(summary, permitted_root)? else {
        return Ok(None);
    };
    let total = frame.ts.len() as u64;
    let max_points = max_points as u64;
    let stride = total.div_ceil(max_points).max(1);
    let rows = (0..frame.ts.len())
        .filter(|index| (*index as u64).is_multiple_of(stride) || *index + 1 == frame.ts.len())
        .map(|index| VerifiedBarFramePoint {
            ts: frame.ts[index],
            open_raw: frame.open_raw[index].to_string(),
            high_raw: frame.high_raw[index].to_string(),
            low_raw: frame.low_raw[index].to_string(),
            close_raw: frame.close_raw[index].to_string(),
            volume_raw: frame.volume_raw[index].to_string(),
        })
        .collect::<Vec<_>>();
    Ok(Some(VerifiedBarFramePreview {
        total,
        truncated: rows.len() < total as usize,
        rows,
    }))
}

/// Find the latest Bar visible at each event timestamp. When an event precedes the
/// market-data priority at the same timestamp, that timestamp's Bar is excluded.
pub fn verified_barframe_event_contexts(
    summary: &serde_json::Value,
    permitted_root: &Path,
    event_times: &[(u64, u8)],
    max_events: usize,
) -> Result<Option<Vec<Option<VerifiedBarFrameEventContext>>>, String> {
    if max_events == 0 || event_times.len() > max_events {
        return Err("事件行情关联超过允许的事件数上限".into());
    }
    let Some(frame) = verified_barframe_frame(summary, permitted_root)? else {
        return Ok(None);
    };
    let contexts = event_times
        .iter()
        .map(|(event_ts, event_priority)| {
            let index = latest_visible_bar_index(&frame.ts, *event_ts, *event_priority)?;
            let bar_ts = frame.ts[index];
            Some(VerifiedBarFrameEventContext {
                event_ts: *event_ts,
                bar_index: index as u64,
                relation: if bar_ts == *event_ts {
                    "same_timestamp_visible"
                } else {
                    "previous_visible"
                },
                bar: VerifiedBarFramePoint {
                    ts: bar_ts,
                    open_raw: frame.open_raw[index].to_string(),
                    high_raw: frame.high_raw[index].to_string(),
                    low_raw: frame.low_raw[index].to_string(),
                    close_raw: frame.close_raw[index].to_string(),
                    volume_raw: frame.volume_raw[index].to_string(),
                },
            })
        })
        .collect();
    Ok(Some(contexts))
}

/// Return a bounded depth preview only when the source is within the configured data root and
/// its declared input, manifest, and completed-run record identities all re-verify.
pub fn verified_depthframe_preview(
    summary: &serde_json::Value,
    permitted_root: &Path,
    max_snapshots: usize,
    max_levels_per_side: usize,
) -> Result<Option<VerifiedDepthFramePreview>, String> {
    if max_snapshots == 0 || max_levels_per_side == 0 {
        return Err("深度帧预览快照数与盘口档位上限必须为正数".into());
    }
    let Some(frame) = verified_depthframe_frame(summary, permitted_root)? else {
        return Ok(None);
    };
    let total = frame.snapshots.len() as u64;
    let stride = total.div_ceil(max_snapshots as u64).max(1);
    let rows = frame
        .snapshots
        .iter()
        .enumerate()
        .filter(|(index, _)| {
            (*index as u64).is_multiple_of(stride) || *index + 1 == frame.snapshots.len()
        })
        .map(|(_, snapshot)| {
            let levels = |source: &[qx_xingban::BookLevel]| {
                source
                    .iter()
                    .take(max_levels_per_side)
                    .map(|level| VerifiedDepthLevel {
                        price_raw: level.price.raw().to_string(),
                        qty_raw: level.qty.raw().to_string(),
                    })
                    .collect::<Vec<_>>()
            };
            VerifiedDepthFramePoint {
                ts: snapshot.ts,
                sequence: snapshot.sequence,
                bids: levels(&snapshot.bids),
                asks: levels(&snapshot.asks),
                levels_truncated: snapshot.bids.len() > max_levels_per_side
                    || snapshot.asks.len() > max_levels_per_side,
            }
        })
        .collect::<Vec<_>>();
    Ok(Some(VerifiedDepthFramePreview {
        total,
        truncated: rows.len() < total as usize || rows.iter().any(|row| row.levels_truncated),
        rows,
    }))
}

fn verified_depthframe_frame(
    summary: &serde_json::Value,
    permitted_root: &Path,
) -> Result<Option<DepthFrame>, String> {
    let Some(input) = summary.get("input") else {
        return Ok(None);
    };
    if input.get("kind").and_then(serde_json::Value::as_str) != Some("depth-frame") {
        return Ok(None);
    }
    let source_path = input
        .get("path")
        .and_then(serde_json::Value::as_str)
        .ok_or("深度帧输入缺少 path")?;
    let root = std::fs::canonicalize(permitted_root)
        .map_err(|error| format!("解析深度帧允许目录失败: {error}"))?;
    let path = std::fs::canonicalize(source_path)
        .map_err(|error| format!("解析深度帧输入路径失败: {error}"))?;
    if !path.starts_with(&root) {
        return Ok(None);
    }
    let metadata =
        std::fs::metadata(&path).map_err(|error| format!("读取深度帧输入元数据失败: {error}"))?;
    if !metadata.is_file() || metadata.len() > MAX_PREVIEW_INPUT_BYTES {
        return Err(format!(
            "深度帧输入不是普通文件或超过 {} MiB",
            MAX_PREVIEW_INPUT_BYTES / 1024 / 1024
        ));
    }
    let declared = recompute_declared_backtest_input(summary)?
        .ok_or("深度帧输入身份未声明，不能展示原始盘口")?;
    if declared.kind != "depth-frame" {
        return Err("深度帧输入身份种类与摘要声明不一致".into());
    }
    let frame = read_depth_frame(&path)?;
    if frame.instrument.to_string()
        != summary
            .get("instrument")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
    {
        return Err("深度帧标的与已复核回测不一致".into());
    }
    Ok(Some(frame))
}

fn latest_visible_bar_index(
    timestamps: &[u64],
    event_ts: u64,
    event_priority: u8,
) -> Option<usize> {
    let end = if event_priority < qx_core::Priority::MARKET {
        timestamps.partition_point(|bar_ts| *bar_ts < event_ts)
    } else {
        timestamps.partition_point(|bar_ts| *bar_ts <= event_ts)
    };
    end.checked_sub(1)
}

fn verified_barframe_frame(
    summary: &serde_json::Value,
    permitted_root: &Path,
) -> Result<Option<BarFrame>, String> {
    let Some(input) = summary.get("input") else {
        return Ok(None);
    };
    if input.get("kind").and_then(serde_json::Value::as_str) != Some("barframe") {
        return Ok(None);
    }
    let path = input
        .get("path")
        .and_then(serde_json::Value::as_str)
        .ok_or("BarFrame 输入缺少 path")?;
    let root = std::fs::canonicalize(permitted_root)
        .map_err(|error| format!("解析 BarFrame 允许目录失败: {error}"))?;
    let path = std::fs::canonicalize(path)
        .map_err(|error| format!("解析 BarFrame 输入路径失败: {error}"))?;
    if !path.starts_with(&root) {
        return Ok(None);
    }
    let metadata = std::fs::metadata(&path)
        .map_err(|error| format!("读取 BarFrame 输入元数据失败: {error}"))?;
    if !metadata.is_file() || metadata.len() > MAX_PREVIEW_INPUT_BYTES {
        return Err(format!(
            "BarFrame 输入不是普通文件或超过 {} MiB",
            MAX_PREVIEW_INPUT_BYTES / 1024 / 1024
        ));
    }
    recompute_declared_backtest_input(summary)?
        .ok_or("BarFrame 输入身份未声明，不能展示原始行情")?;
    let frame = read_bar_frame(&path)?;
    if frame.instrument.to_string()
        != summary
            .get("instrument")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
    {
        return Err("BarFrame 标的与已复核回测不一致".into());
    }
    Ok(Some(frame))
}

/// Re-read the declared source frame, recompute its identity, then verify sibling artifacts.
///
/// `None` means the summary is a legacy artifact with no input block. It is deliberately not a
/// successful input verification; the run record is still checked when present.
pub fn recompute_declared_backtest_input(
    summary: &serde_json::Value,
) -> Result<Option<BacktestInputProvenance>, String> {
    let Some(input) = summary.get("input") else {
        verify_declared_run_record(summary)?;
        return Ok(None);
    };
    if input.get("kind").and_then(serde_json::Value::as_str) == Some("multi-barframe") {
        return recompute_multi_barframe_input(summary).map(Some);
    }

    let field = |name: &str| -> Result<String, String> {
        input
            .get(name)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| format!("回测摘要的 input 块缺 {name}"))
    };
    let kind = match field("kind")?.as_str() {
        "barframe" => "barframe",
        "depth-frame" => "depth-frame",
        other => return Err(format!("未知的回测输入种类: {other}")),
    };
    let declared = BacktestInputProvenance {
        kind,
        path: field("path")?,
        dataset_id: field("dataset_id")?,
        dataset_version: field("dataset_version")?,
        fingerprint: field("fingerprint")?,
    };
    let path = Path::new(&declared.path);
    let recomputed = match kind {
        "barframe" => {
            let frame = read_bar_frame(path)?;
            let (_, manifest) = if declared.dataset_version == qx_data::BARFRAME_DATASET_VERSION {
                legacy_barframe_dataset_identity(path, &frame)?
            } else {
                barframe_dataset_identity(path, &frame)?
            };
            barframe_input_provenance(path, &manifest)
        }
        _ => depth_frame_input_provenance(path, &read_depth_frame(path)?),
    };
    for (name, (declared_value, actual_value)) in [
        ("dataset_id", (&declared.dataset_id, &recomputed.dataset_id)),
        (
            "dataset_version",
            (&declared.dataset_version, &recomputed.dataset_version),
        ),
        (
            "fingerprint",
            (&declared.fingerprint, &recomputed.fingerprint),
        ),
    ] {
        if declared_value != actual_value {
            return Err(format!(
                "回测产物声明的输入与实况不符: {name} 声明={declared_value} 重算={actual_value}（输入文件 {}）",
                recomputed.path
            ));
        }
    }
    verify_declared_run_manifest(summary)?;
    verify_declared_run_record(summary)?;
    Ok(Some(recomputed))
}

fn read_bar_frame(path: &Path) -> Result<BarFrame, String> {
    let payload = std::fs::read_to_string(path)
        .map_err(|error| format!("读取策略回测 BarFrame 失败 {}: {error}", path.display()))?;
    BarFrame::from_json(&payload)
        .map_err(|error| format!("策略回测 BarFrame 校验失败 {}: {error:?}", path.display()))
}

fn read_depth_frame(path: &Path) -> Result<DepthFrame, String> {
    let payload = std::fs::read_to_string(path)
        .map_err(|error| format!("读取深度数据帧失败 {}: {error}", path.display()))?;
    DepthFrame::from_json(&payload).map_err(|error| format!("{}: {error}", path.display()))
}

#[cfg(test)]
mod event_context_tests {
    use super::latest_visible_bar_index;
    use qx_core::Priority;

    #[test]
    fn same_timestamp_bar_is_hidden_until_market_priority() {
        let timestamps = [10, 20, 30];
        assert_eq!(
            latest_visible_bar_index(&timestamps, 20, Priority::COMMAND),
            Some(1)
        );
        assert_eq!(
            latest_visible_bar_index(&timestamps, 20, Priority::MARKET),
            Some(1)
        );
        assert_eq!(
            latest_visible_bar_index(&timestamps, 20, Priority::MATCH),
            Some(1)
        );
        assert_eq!(latest_visible_bar_index(&timestamps, 20, 0), Some(0));
        assert_eq!(
            latest_visible_bar_index(&timestamps, 9, Priority::COMMAND),
            None
        );
    }
}
