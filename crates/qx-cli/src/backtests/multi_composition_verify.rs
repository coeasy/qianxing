//! 组合级回测摘要的双输入与腿级产物复核。

use super::{
    barframe_dataset_identity, multi_leg_combined_return_bps, read_bar_frame_for_backtest,
    recompute_declared_backtest_input, verify_declared_run_manifest, verify_declared_run_record,
    BacktestInputProvenance, BARFRAME_DATASET_VERSION,
};
use crate::summary_schema_version;
use std::collections::BTreeMap;
use std::path::Path;

pub(crate) fn recompute_multi_barframe_input(
    summary: &serde_json::Value,
) -> Result<BacktestInputProvenance, String> {
    recompute_multi_barframe_input_inner(summary, true)
}

pub(crate) fn recompute_multi_barframe_input_from_evidence(
    summary: &serde_json::Value,
) -> Result<BacktestInputProvenance, String> {
    recompute_multi_barframe_input_inner(summary, false)
}

fn recompute_multi_barframe_input_inner(
    summary: &serde_json::Value,
    verify_run_record: bool,
) -> Result<BacktestInputProvenance, String> {
    let input = summary.get("input").ok_or("组合级摘要缺少 input")?;
    let mut actual = BTreeMap::new();
    let mut declared_paths = Vec::new();
    for leg in ["primary", "reference"] {
        let part = input
            .get("legs")
            .and_then(|legs| legs.get(leg))
            .ok_or_else(|| format!("组合级摘要缺少 {leg} 输入身份"))?;
        let path = part
            .get("path")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| format!("组合级摘要的 {leg} 输入缺 path"))?;
        let frame = read_bar_frame_for_backtest(Path::new(path))?;
        let (_, dataset) = barframe_dataset_identity(Path::new(path), &frame)?;
        for field in ["dataset_id", "dataset_version", "fingerprint"] {
            let declared_value = part
                .get(field)
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| format!("组合级摘要的 {leg} 输入缺 {field}"))?;
            let actual_value = match field {
                "dataset_id" => dataset.dataset_id.as_str(),
                "dataset_version" => dataset.version.as_str(),
                _ => dataset.fingerprint.as_str(),
            };
            if declared_value != actual_value {
                return Err(format!(
                    "组合级回测输入不匹配: leg={leg} {field} 声明={declared_value} 重算={actual_value}"
                ));
            }
        }
        actual.insert(leg.to_string(), dataset.fingerprint);
        declared_paths.push(path.to_string());
    }
    let quality_legs = summary.pointer("/dataset_quality/legs");
    let primary_quality = quality_legs.and_then(|legs| legs.get("primary"));
    let reference_quality = quality_legs.and_then(|legs| legs.get("reference"));
    if primary_quality.is_some() != reference_quality.is_some() {
        return Err("组合级数据质量清单必须同时声明主腿和对冲腿".into());
    }
    let mut verified_qualities = Vec::new();
    for (leg, quality) in [
        ("primary", primary_quality),
        ("reference", reference_quality),
    ] {
        let Some(quality) = quality else { continue };
        let manifest_path = quality
            .get("manifest_path")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| format!("组合级 {leg} 数据质量清单缺 manifest_path"))?;
        let frame_path = summary
            .pointer(&format!("/input/legs/{leg}/path"))
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| format!("组合级 {leg} 输入缺 path"))?;
        let frame = read_bar_frame_for_backtest(Path::new(frame_path))?;
        let (_, dataset) = barframe_dataset_identity(Path::new(frame_path), &frame)?;
        let recomputed = super::verified_dataset_quality(
            Path::new(manifest_path),
            Path::new(frame_path),
            &frame,
            &dataset,
        )?;
        for field in ["manifest_fingerprint", "content_hash", "quality_report"] {
            if quality.get(field) != recomputed.get(field) {
                return Err(format!(
                    "组合级 {leg} 数据质量清单的 {field} 与输入复核结果不一致"
                ));
            }
        }
        verified_qualities.push(recomputed);
    }
    if !verified_qualities.is_empty() {
        let expected_manifest_fingerprint = qx_strategy::sha256_hex(
            format!(
                "primary:{}|reference:{}",
                verified_qualities[0]["manifest_fingerprint"]
                    .as_str()
                    .unwrap_or(""),
                verified_qualities[1]["manifest_fingerprint"]
                    .as_str()
                    .unwrap_or("")
            )
            .as_bytes(),
        );
        let expected_quality_report = super::aggregate_multi_leg_quality_report(
            Some(&verified_qualities[0]),
            Some(&verified_qualities[1]),
        );
        if summary.pointer("/dataset_quality/manifest_fingerprint")
            != Some(&serde_json::Value::String(expected_manifest_fingerprint))
            || summary.pointer("/dataset_quality/quality_report") != Some(&expected_quality_report)
        {
            return Err("组合级数据质量汇总与两腿清单复核结果不一致".into());
        }
    }
    let fingerprint = qx_strategy::sha256_hex(
        format!(
            "primary:{}|reference:{}",
            actual["primary"], actual["reference"]
        )
        .as_bytes(),
    );
    let declared = |field: &str| {
        input
            .get(field)
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| format!("组合级摘要 input 缺 {field}"))
    };
    if declared("fingerprint")? != fingerprint
        || summary
            .get("input_data_hash")
            .and_then(serde_json::Value::as_str)
            != Some(fingerprint.as_str())
    {
        return Err("组合级摘要数据指纹与两腿输入重算结果不一致".into());
    }
    let expected_dataset_id = format!(
        "{}+{}",
        input["legs"]["primary"]["dataset_id"]
            .as_str()
            .ok_or("组合级主腿输入缺 dataset_id")?,
        input["legs"]["reference"]["dataset_id"]
            .as_str()
            .ok_or("组合级对冲腿输入缺 dataset_id")?
    );
    if declared("dataset_id")? != expected_dataset_id
        || declared("dataset_version")? != BARFRAME_DATASET_VERSION
    {
        return Err("组合级摘要的数据集身份或版本与两腿输入不一致".into());
    }
    if declared("path")? != declared_paths.join("|") {
        return Err("组合级摘要的输入路径与腿级输入声明不一致".into());
    }
    let leg_summaries = summary
        .get("leg_summaries")
        .and_then(serde_json::Value::as_array)
        .filter(|legs| legs.len() == 2)
        .ok_or("组合级摘要必须引用两份腿级摘要")?;
    let mut leg_result_hashes = Vec::with_capacity(2);
    for (index, pointer) in leg_summaries.iter().enumerate() {
        let pointer = pointer.as_str().ok_or("组合级腿摘要路径无效")?;
        let payload = std::fs::read_to_string(pointer)
            .map_err(|error| format!("读取组合级腿摘要失败 {pointer}: {error}"))?;
        let leg_summary: serde_json::Value = serde_json::from_str(&payload)
            .map_err(|error| format!("组合级腿摘要 JSON 无效 {pointer}: {error}"))?;
        if !summary_schema_version(&leg_summary).is_some_and(|version| (5..=7).contains(&version))
            || leg_summary["input"]["kind"] != "barframe"
        {
            return Err(format!(
                "组合级第 {} 腿引用的不是可复核的 v5/v6/v7 BarFrame 摘要",
                index + 1
            ));
        }
        recompute_declared_backtest_input(&leg_summary)?;
        let result_hash = leg_summary
            .get("result_hash")
            .and_then(serde_json::Value::as_str)
            .ok_or("组合级腿摘要缺 result_hash")?;
        leg_result_hashes.push(result_hash.to_string());
    }
    let expected_result_hash = qx_strategy::sha256_hex(
        format!(
            "primary:{}|reference:{}",
            leg_result_hashes[0], leg_result_hashes[1]
        )
        .as_bytes(),
    );
    if summary
        .get("result_hash")
        .and_then(serde_json::Value::as_str)
        != Some(expected_result_hash.as_str())
    {
        return Err("组合级 result_hash 与两份腿级摘要不一致".into());
    }
    let account_raw = |field: &str| -> Result<i128, String> {
        summary
            .get("accounts")
            .and_then(|accounts| accounts.get(field))
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| format!("组合级摘要缺少 {field}"))?
            .parse::<i128>()
            .map_err(|error| format!("组合级摘要 {field} 无法解析: {error}"))
    };
    let combined_return = multi_leg_combined_return_bps([
        (
            "primary",
            account_raw("primary_initial_cash")?,
            account_raw("primary_final_equity_raw")?,
        ),
        (
            "reference",
            account_raw("reference_initial_cash")?,
            account_raw("reference_final_equity_raw")?,
        ),
    ])?;
    if summary["totals"]["combined_return_bps"].as_i64() != Some(combined_return)
        || summary["metrics"]["return_bps"].as_i64() != Some(combined_return)
    {
        return Err("组合级摘要的 combined_return_bps 未能由两腿账户权益重算".into());
    }
    let manifest_pointer = summary
        .get("run_manifest")
        .and_then(serde_json::Value::as_str)
        .ok_or("组合级摘要缺少 run_manifest")?;
    let manifest_payload = std::fs::read_to_string(manifest_pointer)
        .map_err(|error| format!("读取组合级 RunManifest 失败 {manifest_pointer}: {error}"))?;
    let manifest = qx_core::RunManifest::from_json(&manifest_payload)?;
    for leg in ["primary", "reference"] {
        if manifest.input_components.get(&format!("{leg}_barframe")) != actual.get(leg) {
            return Err(format!("组合级 RunManifest 的 {leg} 输入组件指纹不匹配"));
        }
    }
    for (leg, result_hash) in ["primary", "reference"].into_iter().zip(&leg_result_hashes) {
        if manifest.input_components.get(&format!("{leg}_result")) != Some(result_hash) {
            return Err(format!(
                "组合级 RunManifest 的 {leg} 结果组件与腿级摘要不匹配"
            ));
        }
    }
    verify_declared_run_manifest(summary)?;
    if verify_run_record {
        verify_declared_run_record(summary)?;
    }
    Ok(BacktestInputProvenance {
        kind: "multi-barframe",
        path: declared_paths.join("|"),
        dataset_id: declared("dataset_id")?.to_string(),
        dataset_version: declared("dataset_version")?.to_string(),
        fingerprint,
    })
}
