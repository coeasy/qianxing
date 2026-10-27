//! 组合级回测摘要的双输入与腿级产物复核。

use super::{verify_declared_run_manifest, verify_declared_run_record, BacktestInputProvenance};
use crate::multi_leg_combined_return_bps;
use qx_core::ExactDecimal;
use qx_data::JsonBarFrameProvider;
use qx_datastruct::BarFrame;
use std::collections::BTreeMap;
use std::path::Path;

fn parse_exact_string(
    value: Option<&serde_json::Value>,
    label: &str,
) -> Result<Option<ExactDecimal>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    let text = value
        .as_str()
        .ok_or_else(|| format!("{label} 必须是十进制字符串"))?;
    let parsed = ExactDecimal::parse(text).map_err(|error| format!("{label} 无效: {error}"))?;
    if parsed.to_string() != text {
        return Err(format!("{label} 不是规范十进制字符串"));
    }
    Ok(Some(parsed))
}

fn exact_equity_metrics(
    summary: &serde_json::Value,
) -> Result<Option<(ExactDecimal, ExactDecimal)>, String> {
    let Some(equity) = summary.pointer("/metrics_exact/equity") else {
        return Ok(None);
    };
    let initial = parse_exact_string(equity.get("initial"), "metrics_exact.equity.initial")?;
    let final_equity = parse_exact_string(equity.get("final"), "metrics_exact.equity.final")?;
    match (initial, final_equity) {
        (Some(initial), Some(final_equity)) => Ok(Some((initial, final_equity))),
        (None, None) => Ok(None),
        _ => Err("腿级 metrics_exact.equity 的初始/期末权益字段不完整".into()),
    }
}

pub fn recompute_multi_barframe_input(
    summary: &serde_json::Value,
) -> Result<BacktestInputProvenance, String> {
    recompute_multi_barframe_input_inner(summary, true, None)
}

pub fn recompute_multi_barframe_input_from_evidence(
    summary: &serde_json::Value,
    manifest: &qx_core::RunManifest,
) -> Result<BacktestInputProvenance, String> {
    recompute_multi_barframe_input_inner(summary, false, Some(manifest))
}

fn read_bar_frame(path: &Path) -> Result<BarFrame, String> {
    let payload = std::fs::read_to_string(path)
        .map_err(|error| format!("读取组合级 BarFrame 失败 {}: {error}", path.display()))?;
    BarFrame::from_json(&payload)
        .map_err(|error| format!("组合级 BarFrame 校验失败 {}: {error:?}", path.display()))
}

pub(super) fn barframe_dataset_identity(
    frame_path: &Path,
    frame: &BarFrame,
) -> Result<qx_data::DatasetManifest, String> {
    barframe_dataset_identity_with_version(
        frame_path,
        frame,
        qx_data::barframe_registry_version(frame),
    )
}

pub(super) fn legacy_barframe_dataset_identity(
    frame_path: &Path,
    frame: &BarFrame,
) -> Result<qx_data::DatasetManifest, String> {
    barframe_dataset_identity_with_version(
        frame_path,
        frame,
        qx_data::BARFRAME_DATASET_VERSION.to_owned(),
    )
}

fn barframe_dataset_identity_with_version(
    frame_path: &Path,
    frame: &BarFrame,
    version: String,
) -> Result<qx_data::DatasetManifest, String> {
    let bars: Vec<qx_guanxing::Bar> = frame.into();
    let provider = JsonBarFrameProvider::new(frame.source.0.clone(), version, frame_path);
    let (provider_bars, manifest) = provider.load_bars_with_manifest(
        &format!("strategy-bars:{}", frame.instrument),
        &frame.instrument.to_string(),
        bars.first().map(|bar| bar.ts).unwrap_or(1),
        bars.last().map(|bar| bar.ts).unwrap_or(1),
    )?;
    let bars_match = provider_bars.len() == bars.len()
        && provider_bars.iter().zip(&bars).all(|(ingest, market)| {
            ingest.timestamp == market.ts
                && ingest.open_raw == market.open
                && ingest.high_raw == market.high
                && ingest.low_raw == market.low
                && ingest.close_raw == market.close
                && ingest.volume_raw == market.volume
        });
    if !bars_match {
        return Err("qx-data Provider 与 BarFrame 列式输入不一致，拒绝复核回测产物".into());
    }
    Ok(manifest)
}

fn recompute_single_barframe_input(
    summary: &serde_json::Value,
    verify_run_record: bool,
) -> Result<(), String> {
    let input = summary.get("input").ok_or("腿级摘要缺少 input")?;
    let field = |name: &str| {
        input
            .get(name)
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| format!("腿级摘要 input 缺 {name}"))
    };
    let kind = field("kind")?;
    if kind != "barframe" {
        return Err(format!("多腿腿级摘要输入类型不支持: {kind}"));
    }
    let path = field("path")?;
    let frame_path = Path::new(path);
    let frame = read_bar_frame(frame_path)?;
    let manifest = if field("dataset_version")? == qx_data::BARFRAME_DATASET_VERSION {
        legacy_barframe_dataset_identity(frame_path, &frame)?
    } else {
        barframe_dataset_identity(frame_path, &frame)?
    };
    for (name, actual) in [
        ("dataset_id", manifest.dataset_id.as_str()),
        ("dataset_version", manifest.version.as_str()),
        ("fingerprint", manifest.fingerprint.as_str()),
    ] {
        let declared = field(name)?;
        if declared != actual {
            return Err(format!(
                "回测产物声明的输入与实况不符: {name} 声明={declared} 重算={actual}（输入文件 {}）",
                frame_path.display()
            ));
        }
    }
    verify_declared_run_manifest(summary)?;
    if verify_run_record {
        verify_declared_run_record(summary)?;
    }
    Ok(())
}

pub fn verify_barframe_dataset_quality(
    path: &Path,
    frame_path: &Path,
    frame: &BarFrame,
    input_manifest: &qx_data::DatasetManifest,
) -> Result<serde_json::Value, String> {
    let payload = std::fs::read_to_string(path)
        .map_err(|error| format!("读取数据集质量清单失败 {}: {error}", path.display()))?;
    let quality = qx_data::DatasetManifestV2::from_json(&payload)?;
    let expected_content_hash = qx_strategy::file_digest::sha256_file_hex(frame_path)?;
    let declared = &quality.dataset;
    for (field, actual, expected) in [
        (
            "dataset_id",
            declared.dataset_id.as_str(),
            input_manifest.dataset_id.as_str(),
        ),
        (
            "version",
            declared.version.as_str(),
            input_manifest.version.as_str(),
        ),
        (
            "fingerprint",
            declared.fingerprint.as_str(),
            input_manifest.fingerprint.as_str(),
        ),
        (
            "content_hash",
            quality.content_hash.as_str(),
            expected_content_hash.as_str(),
        ),
    ] {
        if actual != expected {
            return Err(format!(
                "数据集质量清单与本次 BarFrame 不一致: {field} 清单={actual} 实际={expected} ({})",
                path.display()
            ));
        }
    }
    let declared_instrument = qx_core::InstrumentId::parse(&quality.instrument)
        .ok_or_else(|| format!("数据集质量清单 instrument 非法: {}", quality.instrument))?;
    if declared_instrument != frame.instrument {
        return Err(format!(
            "数据集质量清单与本次 BarFrame 不一致: instrument 清单={} 实际={} ({})",
            quality.instrument,
            frame.instrument,
            path.display()
        ));
    }
    let Some((&first_ts, &last_ts)) = frame.ts.first().zip(frame.ts.last()) else {
        return Err(format!(
            "BarFrame 缺少时间戳，无法复核数据集质量清单 ({})",
            path.display()
        ));
    };
    if quality.quality_report.row_count != frame.ts.len() as u64
        || quality.tier != qx_data::DatasetTier::Bar
        || quality.provider_version != qx_data::BARFRAME_DATASET_VERSION
        || declared.start_timestamp != first_ts
        || declared.end_timestamp != last_ts
    {
        return Err(format!(
            "数据集质量清单的样本数或时间范围与本次 BarFrame 不一致 ({})",
            path.display()
        ));
    }
    let interval_ms = quality
        .quality_report
        .interval_ms
        .filter(|value| *value > 0)
        .ok_or_else(|| {
            format!(
                "数据集质量清单缺少可复核的 interval_ms ({})",
                path.display()
            )
        })?;
    let missing = frame
        .ts
        .windows(2)
        .try_fold(0_u64, |total, pair| {
            total.checked_add((pair[1] - pair[0]).div_ceil(interval_ms) - 1)
        })
        .ok_or_else(|| format!("按 interval_ms 复算缺口数溢出 ({})", path.display()))?;
    if quality.quality_report.missing_intervals != missing
        || quality.quality_report.duplicate_rows != 0
        || quality.quality_report.out_of_order_rows != 0
    {
        return Err(format!(
            "数据集质量报告与严格 BarFrame 实况不一致: missing_intervals 清单={} 重算={}，duplicate_rows={}，out_of_order_rows={} ({})",
            quality.quality_report.missing_intervals,
            missing,
            quality.quality_report.duplicate_rows,
            quality.quality_report.out_of_order_rows,
            path.display()
        ));
    }
    let manifest_fingerprint = quality.fingerprint()?;
    Ok(serde_json::json!({
        "manifest_path": path.to_string_lossy(),
        "manifest_fingerprint": manifest_fingerprint,
        "content_hash": quality.content_hash,
        "quality_report": quality.quality_report,
        "manifest": quality,
    }))
}

fn recompute_multi_barframe_input_inner(
    summary: &serde_json::Value,
    verify_run_record: bool,
    evidence_manifest: Option<&qx_core::RunManifest>,
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
        let frame_path = Path::new(path);
        let frame = read_bar_frame(frame_path)?;
        let dataset = barframe_dataset_identity(frame_path, &frame)?;
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
        let frame_path = Path::new(frame_path);
        let frame = read_bar_frame(frame_path)?;
        let dataset = barframe_dataset_identity(frame_path, &frame)?;
        let recomputed = verify_barframe_dataset_quality(
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
        let expected_quality_report = crate::aggregate_multi_leg_quality_report(
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
        || declared("dataset_version")? != qx_data::BARFRAME_DATASET_VERSION
    {
        return Err("组合级摘要的数据集身份或版本与两腿输入不一致".into());
    }
    if declared("path")? != declared_paths.join("|") {
        return Err("组合级摘要的输入路径与腿级输入声明不一致".into());
    }
    let mut leg_result_hashes = Vec::with_capacity(2);
    let mut verified_leg_exact_metrics = Vec::<Option<(ExactDecimal, ExactDecimal)>>::new();
    if verify_run_record {
        let leg_summaries = summary
            .get("leg_summaries")
            .and_then(serde_json::Value::as_array)
            .filter(|legs| legs.len() == 2)
            .ok_or("组合级摘要必须引用两份腿级摘要")?;
        for (index, pointer) in leg_summaries.iter().enumerate() {
            let pointer = pointer.as_str().ok_or("组合级腿摘要路径无效")?;
            let payload = std::fs::read_to_string(pointer)
                .map_err(|error| format!("读取组合级腿摘要失败 {pointer}: {error}"))?;
            let leg_summary: serde_json::Value = serde_json::from_str(&payload)
                .map_err(|error| format!("组合级腿摘要 JSON 无效 {pointer}: {error}"))?;
            if !leg_summary["schema_version"]
                .as_i64()
                .is_some_and(|version| {
                    (5..=i64::from(super::BACKTEST_SUMMARY_SCHEMA_VERSION)).contains(&version)
                })
                || leg_summary["input"]["kind"] != "barframe"
            {
                return Err(format!(
                    "组合级第 {} 腿引用的不是可复核的 v5/v6/v7 BarFrame 摘要",
                    index + 1
                ));
            }
            recompute_single_barframe_input(&leg_summary, true)?;
            let leg_exact = exact_equity_metrics(&leg_summary)?;
            verified_leg_exact_metrics.push(leg_exact);
            let result_hash = leg_summary
                .get("result_hash")
                .and_then(serde_json::Value::as_str)
                .ok_or("组合级腿摘要缺 result_hash")?;
            leg_result_hashes.push(result_hash.to_string());
        }
    } else {
        // Portable EvidenceBundles bind the parent RunManifest as a content-addressed artifact.
        // Its input_components carry both child result hashes, so recomputation does not need to
        // follow source-workspace leg-summary paths that are intentionally absent after export.
        let manifest = evidence_manifest.ok_or("EvidenceBundle 多腿复算缺少已验证 RunManifest")?;
        for leg in ["primary", "reference"] {
            let result_hash = manifest
                .input_components
                .get(&format!("{leg}_result"))
                .ok_or_else(|| format!("EvidenceBundle RunManifest 缺少 {leg}_result"))?;
            leg_result_hashes.push(result_hash.clone());
        }
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
    let account_exact = |field: &str| -> Result<Option<ExactDecimal>, String> {
        parse_exact_string(
            summary
                .get("accounts")
                .and_then(|accounts| accounts.get(field)),
            &format!("组合级摘要 accounts.{field}"),
        )
    };
    let declared_exact = [
        account_exact("primary_initial_cash_exact")?,
        account_exact("reference_initial_cash_exact")?,
        account_exact("primary_final_equity_exact")?,
        account_exact("reference_final_equity_exact")?,
    ];
    let exact_count = declared_exact
        .iter()
        .filter(|value| value.is_some())
        .count();
    let combined_return = if exact_count == 0 {
        multi_leg_combined_return_bps([
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
        ])?
    } else {
        if exact_count != declared_exact.len() {
            return Err("组合级摘要 exact 账户权益字段不完整".into());
        }
        let primary_initial = declared_exact[0].ok_or("组合 exact 主腿初始权益缺失")?;
        let reference_initial = declared_exact[1].ok_or("组合 exact 对冲腿初始权益缺失")?;
        let primary_final = declared_exact[2].ok_or("组合 exact 主腿期末权益缺失")?;
        let reference_final = declared_exact[3].ok_or("组合 exact 对冲腿期末权益缺失")?;
        if verify_run_record {
            if verified_leg_exact_metrics.len() != 2
                || verified_leg_exact_metrics.iter().any(Option::is_none)
            {
                return Err("组合 exact 账户权益缺少腿级 exact 指标来源".into());
            }
            let primary_leg = verified_leg_exact_metrics
                .first()
                .and_then(|facts| *facts)
                .ok_or("组合主腿 exact 指标缺失")?;
            let reference_leg = verified_leg_exact_metrics
                .get(1)
                .and_then(|facts| *facts)
                .ok_or("组合对冲腿 exact 指标缺失")?;
            if primary_initial != primary_leg.0
                || primary_final != primary_leg.1
                || reference_initial != reference_leg.0
                || reference_final != reference_leg.1
            {
                return Err("组合 exact 账户权益与两腿回测报告不一致".into());
            }
        }
        let initial = primary_initial
            .checked_add(reference_initial)
            .ok_or("组合 exact 初始权益求和溢出")?;
        let final_equity = primary_final
            .checked_add(reference_final)
            .ok_or("组合 exact 期末权益求和溢出")?;
        let expected_return =
            crate::report_analysis::exact_equity_total_return_bps_v1(initial, final_equity)?;
        let exact_metrics = summary
            .pointer("/metrics_exact/equity")
            .ok_or("组合 exact 摘要缺少 metrics_exact.equity")?;
        let exact_initial =
            parse_exact_string(exact_metrics.get("initial"), "metrics_exact.equity.initial")?
                .ok_or("组合 exact 初始权益缺失")?;
        let exact_final =
            parse_exact_string(exact_metrics.get("final"), "metrics_exact.equity.final")?
                .ok_or("组合 exact 期末权益缺失")?;
        let exact_return =
            parse_exact_string(exact_metrics.get("return"), "metrics_exact.equity.return")?
                .ok_or("组合 exact 收益缺失")?;
        let expected_pnl = final_equity
            .checked_sub(initial)
            .ok_or("组合 exact 收益金额溢出")?;
        let expected_return_text = expected_return.to_string();
        if exact_initial != initial
            || exact_final != final_equity
            || exact_return != expected_pnl
            || exact_metrics["return_bps"].as_str() != Some(expected_return_text.as_str())
        {
            return Err("组合 metrics_exact.equity 与两腿精确权益不一致".into());
        }
        i64::from(expected_return)
    };
    if summary["totals"]["combined_return_bps"].as_i64() != Some(combined_return)
        || summary["metrics"]["return_bps"].as_i64() != Some(combined_return)
    {
        return Err("组合级摘要的 combined_return_bps 未能由两腿账户权益重算".into());
    }
    if exact_count != 0 && verify_run_record {
        let exact_metrics = summary
            .pointer("/metrics_exact/equity")
            .ok_or("组合 exact 摘要缺少 metrics_exact.equity")?;
        let initial =
            parse_exact_string(exact_metrics.get("initial"), "metrics_exact.equity.initial")?
                .ok_or("组合 exact 初始权益缺失")?;
        let declared_final =
            parse_exact_string(exact_metrics.get("final"), "metrics_exact.equity.final")?
                .ok_or("组合 exact 期末权益缺失")?;
        let curve = read_combined_exact_equity_curve(summary)?;
        let expected_rows = summary["bars"].as_u64().ok_or("组合摘要缺少 bars")? as usize;
        if curve.len() != expected_rows || curve.last().copied() != Some(declared_final) {
            return Err("组合 exact 权益 CSV 长度或期末值与摘要不一致".into());
        }
        let drawdown_curve = std::iter::once(initial)
            .chain(curve.iter().copied())
            .collect::<Vec<_>>();
        let recomputed_drawdown =
            crate::report_analysis::exact_equity_max_drawdown_bps_v1(&drawdown_curve)?;
        let declared_drawdown = exact_metrics["max_drawdown_bps"]
            .as_str()
            .ok_or("组合 exact 最大回撤基点缺失")?
            .parse::<u32>()
            .map_err(|error| format!("组合 exact 最大回撤基点无效: {error}"))?;
        if recomputed_drawdown != declared_drawdown
            || summary["metrics"]["max_drawdown_bps"].as_u64()
                != Some(u64::from(recomputed_drawdown))
        {
            return Err("组合 exact 最大回撤与权益 CSV 不一致".into());
        }
    }
    let owned_manifest = if evidence_manifest.is_none() {
        let manifest_pointer = summary
            .get("run_manifest")
            .and_then(serde_json::Value::as_str)
            .ok_or("组合级摘要缺少 run_manifest")?;
        let manifest_payload = std::fs::read_to_string(manifest_pointer)
            .map_err(|error| format!("读取组合级 RunManifest 失败 {manifest_pointer}: {error}"))?;
        Some(qx_core::RunManifest::from_json(&manifest_payload)?)
    } else {
        None
    };
    let manifest = evidence_manifest
        .or(owned_manifest.as_ref())
        .ok_or("组合级摘要缺少可复核 RunManifest")?;
    verify_multi_leg_manifest(summary, &actual, &leg_result_hashes, &fingerprint, manifest)?;
    if verify_run_record {
        verify_declared_run_manifest(summary)?;
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

fn read_combined_exact_equity_curve(
    summary: &serde_json::Value,
) -> Result<Vec<ExactDecimal>, String> {
    let path = summary
        .pointer("/artifact_locations/equity")
        .and_then(serde_json::Value::as_str)
        .ok_or("组合摘要缺少权益 CSV 路径")?;
    let payload = std::fs::read_to_string(path)
        .map_err(|error| format!("读取组合 exact 权益 CSV 失败 {path}: {error}"))?;
    let mut lines = payload.lines();
    if lines.next()
        != Some("index,ts,primary_equity_raw,reference_equity_raw,equity_raw,equity_exact")
    {
        return Err("组合 exact 权益 CSV 表头不属于 schema v4".into());
    }
    lines
        .enumerate()
        .map(|(index, line)| {
            let fields = line.split(',').collect::<Vec<_>>();
            if fields.len() != 6 || fields[0].parse::<usize>().ok() != Some(index) {
                return Err(format!("组合 exact 权益 CSV 第 {} 行结构无效", index + 2));
            }
            let value = ExactDecimal::parse(fields[5]).map_err(|error| {
                format!("组合 exact 权益 CSV 第 {} 行数值无效: {error}", index + 2)
            })?;
            if value.to_string() != fields[5] {
                return Err(format!(
                    "组合 exact 权益 CSV 第 {} 行不是规范十进制",
                    index + 2
                ));
            }
            Ok(value)
        })
        .collect()
}

fn verify_multi_leg_manifest(
    summary: &serde_json::Value,
    actual: &BTreeMap<String, String>,
    leg_result_hashes: &[String],
    fingerprint: &str,
    manifest: &qx_core::RunManifest,
) -> Result<(), String> {
    for leg in ["primary", "reference"] {
        if manifest.input_components.get(&format!("{leg}_barframe")) != actual.get(leg) {
            return Err(format!("组合级 RunManifest 的 {leg} 输入组件指纹不匹配"));
        }
    }
    for (leg, result_hash) in ["primary", "reference"]
        .into_iter()
        .zip(leg_result_hashes.iter())
    {
        if manifest.input_components.get(&format!("{leg}_result")) != Some(result_hash) {
            return Err(format!(
                "组合级 RunManifest 的 {leg} 结果组件与腿级摘要不匹配"
            ));
        }
    }
    let result_hash = summary
        .get("result_hash")
        .and_then(serde_json::Value::as_str)
        .ok_or("组合级摘要缺 result_hash")?;
    if manifest.data_fingerprint != fingerprint
        || manifest.result_hash != result_hash
        || manifest.input_event_hash != fingerprint
        || manifest.output_event_hash != result_hash
    {
        return Err("组合级 RunManifest 与组合摘要的输入或结果指纹不一致".into());
    }
    super::verify_canonical_run_spec_for_manifest(manifest, summary)
}
