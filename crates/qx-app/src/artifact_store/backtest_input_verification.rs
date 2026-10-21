//! Recompute the input identity declared by a persisted backtest summary.

use super::{
    recompute_multi_barframe_input, verify_declared_run_manifest, verify_declared_run_record,
    BacktestInputProvenance,
};
use crate::backtest_input_identity::{
    barframe_dataset_identity, barframe_input_provenance, depth_frame_input_provenance,
};
use qx_datastruct::BarFrame;
use qx_xingban::DepthFrame;
use std::path::Path;

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
            let (_, manifest) = barframe_dataset_identity(path, &frame)?;
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
