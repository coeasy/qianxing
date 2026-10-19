//! Quality and portfolio readout for persisted multi-market depth runs.

use crate::report_multi_market_depth_render::{
    render_comparison_html, render_html, strval, workbook_rows,
};
use crate::report_output_paths::same_output_target;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

fn read_artifact(path: &Path) -> Result<Option<Value>, String> {
    if !path.is_file() {
        return Ok(None);
    }
    let payload = std::fs::read(path)
        .map_err(|error| format!("读取报告输入失败 {}: {error}", path.display()))?;
    let Ok(value) = serde_json::from_slice::<Value>(&payload) else {
        return Ok(None);
    };
    if value.pointer("/outcome/shared_result_hash").is_some()
        && value.get("market_data").and_then(Value::as_array).is_some()
        && value.pointer("/spec/markets").is_some()
    {
        Ok(Some(value))
    } else {
        Ok(None)
    }
}

pub(crate) fn maybe_run(
    path: &Path,
    as_json: bool,
    html: bool,
    evidence: bool,
    output: Option<&Path>,
    against: &[PathBuf],
    xlsx: Option<&Path>,
) -> Result<bool, String> {
    let Some(artifact) = read_artifact(path)? else {
        return Ok(false);
    };
    if evidence {
        return Err("多市场深度运行请使用 `evidence pack-multi-market-run` 生成运行证据包".into());
    }
    if output.is_some() && !html {
        return Err("--output 只能与 --html 一起使用".into());
    }
    if output.is_some_and(|html_path| {
        xlsx.is_some_and(|xlsx_path| same_output_target(html_path, xlsx_path))
    }) {
        return Err("HTML 与 XLSX 报告必须使用不同输出路径".into());
    }
    let mut reports = vec![build_report(path, &artifact)?];
    let mut seen = std::collections::BTreeSet::from([path
        .canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())]);
    for candidate in against {
        let canonical = candidate
            .canonicalize()
            .map_err(|error| format!("解析对照运行路径失败 {}: {error}", candidate.display()))?;
        if !seen.insert(canonical) {
            return Err(format!(
                "多市场深度对照不能重复输入: {}",
                candidate.display()
            ));
        }
        let Some(value) = read_artifact(candidate)? else {
            return Err(format!(
                "--against 只接受多市场深度运行产物: {}",
                candidate.display()
            ));
        };
        reports.push(build_report(candidate, &value)?);
    }
    let source_paths = reports
        .iter()
        .filter_map(|report| report["summary_path"].as_str().map(PathBuf::from))
        .collect::<Vec<_>>();
    let comparison = reports.len() > 1;
    if comparison {
        let fingerprint = reports[0]["data_fingerprint"].clone();
        let instruments = market_instruments(&reports[0])?;
        for report in &reports {
            if report["data_fingerprint"] != fingerprint
                || market_instruments(report)? != instruments
            {
                return Err("多市场深度对照要求市场顺序和数据指纹完全相同".into());
            }
        }
        reports.sort_by(|left, right| {
            right["return_bps"]
                .as_i64()
                .unwrap_or_default()
                .cmp(&left["return_bps"].as_i64().unwrap_or_default())
                .then_with(|| {
                    left["max_drawdown_bps"]
                        .as_u64()
                        .cmp(&right["max_drawdown_bps"].as_u64())
                })
                .then_with(|| left["run_id"].as_str().cmp(&right["run_id"].as_str()))
        });
    }
    let report = if comparison {
        json!({"comparison_kind":"same_multi_market_depth_input","data_fingerprint":reports[0]["data_fingerprint"],"markets":market_instruments(&reports[0])?,"ranking":reports.iter().enumerate().map(|(index, report)|json!({"rank":index+1,"run_id":report["run_id"],"return_bps":report["return_bps"],"max_drawdown_bps":report["max_drawdown_bps"],"successful_fills":report["successful_fills"],"result_hash":report["result_hash"],"summary_path":report["summary_path"]})).collect::<Vec<_>>()})
    } else {
        reports.remove(0)
    };
    let html_path = if html {
        let target = output.map(Path::to_path_buf).unwrap_or_else(|| {
            let name = path
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("multi-market-depth.json");
            path.with_file_name(name.replace(
                ".multi-market-depth.json",
                if comparison {
                    ".multi-market-depth.comparison.html"
                } else {
                    ".multi-market-depth.report.html"
                },
            ))
        });
        if !target
            .extension()
            .and_then(|value| value.to_str())
            .is_some_and(|value| {
                value.eq_ignore_ascii_case("html") || value.eq_ignore_ascii_case("htm")
            })
        {
            return Err(format!(
                "HTML 报告输出路径必须以 .html 或 .htm 结尾: {}",
                target.display()
            ));
        }
        if source_paths
            .iter()
            .any(|source| same_output_target(&target, source))
        {
            return Err("HTML 输出不能覆盖多市场深度输入产物".into());
        }
        let rendered = if comparison {
            render_comparison_html(&report)
        } else {
            render_html(&report)
        };
        if let Some(parent) = target
            .parent()
            .filter(|value| !value.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("创建报告目录失败: {error}"))?;
        }
        qx_app::artifact_store::sink::publish(
            &target,
            &rendered,
            qx_app::artifact_store::sink::PublishMode::Overwrite,
        )
        .map_err(|error| format!("发布多市场深度 HTML 失败: {error}"))?;
        Some(target)
    } else {
        None
    };
    let xlsx_path = if let Some(target) = xlsx {
        if source_paths
            .iter()
            .any(|source| same_output_target(target, source))
        {
            return Err("XLSX 输出不能覆盖多市场深度输入产物".into());
        }
        if comparison {
            let overview = vec![
                vec!["Field".into(), "Value".into()],
                vec![
                    "Data fingerprint".into(),
                    strval(&report["data_fingerprint"]),
                ],
                vec![
                    "Comparison kind".into(),
                    "same_multi_market_depth_input".into(),
                ],
            ];
            let mut ranking = vec![vec![
                "rank".into(),
                "run_id".into(),
                "return_bps".into(),
                "max_drawdown_bps".into(),
                "successful_fills".into(),
                "result_hash".into(),
                "summary_path".into(),
            ]];
            for row in report["ranking"].as_array().into_iter().flatten() {
                ranking.push(
                    [
                        "rank",
                        "run_id",
                        "return_bps",
                        "max_drawdown_bps",
                        "successful_fills",
                        "result_hash",
                        "summary_path",
                    ]
                    .into_iter()
                    .map(|key| strval(&row[key]))
                    .collect(),
                );
            }
            Some(
                crate::report_xlsx::write_multi_market_depth_comparison_xlsx(
                    target, overview, ranking,
                )?,
            )
        } else {
            let (overview, quality) = workbook_rows(&report);
            Some(crate::report_xlsx::write_multi_market_depth_xlsx(
                target, overview, quality,
            )?)
        }
    } else {
        None
    };
    if as_json {
        let mut value = json!({"schema_version":1,"report_kind":if comparison {"multi_market_depth_comparison"} else {"multi_market_depth"},"report":report});
        if let Some(path) = html_path {
            value["generated_artifacts"]["html"] = json!(path);
        }
        if let Some(path) = xlsx_path {
            value["generated_artifacts"]["xlsx"] = json!(path);
        }
        print!(
            "{}\n",
            serde_json::to_string_pretty(&value).map_err(|error| error.to_string())?
        );
    } else {
        if let Some(path) = html_path {
            println!("[Report] html={}", path.display());
        }
        if let Some(path) = xlsx_path {
            println!("[Report] xlsx={}", path.display());
        }
        if comparison {
            println!(
                "[Report] comparison_fingerprint={} compared_runs={}",
                report["data_fingerprint"],
                report["ranking"].as_array().map_or(0, Vec::len)
            );
        } else {
            println!(
                "[Report] run_id={} return_bps={} max_drawdown_bps={} markets={}",
                report["run_id"],
                report["return_bps"],
                report["max_drawdown_bps"],
                report["markets"].as_array().map_or(0, Vec::len)
            );
        }
    }
    Ok(true)
}

fn market_instruments(report: &Value) -> Result<Vec<Value>, String> {
    report
        .get("markets")
        .and_then(Value::as_array)
        .ok_or_else(|| "多市场深度报告缺少 markets 列表".to_string())?
        .iter()
        .map(|market| {
            market
                .get("instrument")
                .cloned()
                .ok_or_else(|| "多市场深度报告的市场条目缺少 instrument".to_string())
        })
        .collect()
}

fn build_report(path: &Path, artifact: &Value) -> Result<Value, String> {
    let outcome = artifact
        .get("outcome")
        .ok_or("多市场深度产物缺少 outcome")?;
    let markets = artifact
        .get("market_data")
        .and_then(Value::as_array)
        .ok_or("多市场深度产物缺少 market_data")?;
    let mut rows = Vec::with_capacity(markets.len());
    let mut data_hasher = qx_core::Fnv1a::new();
    let mut input_components = std::collections::BTreeMap::new();
    for market in markets {
        let instrument = market
            .get("instrument")
            .and_then(Value::as_str)
            .ok_or("market_data 缺少 instrument")?;
        let depth_path = market
            .get("path")
            .and_then(Value::as_str)
            .ok_or("market_data 缺少 depth path")?;
        let frame = crate::read_depth_frame_for_backtest(Path::new(depth_path))?;
        if frame.instrument.to_string() != instrument
            || market.get("content_hash").and_then(Value::as_str)
                != Some(format!("{:016x}", frame.input_hash()).as_str())
            || market.get("source").and_then(Value::as_str) != Some(frame.source.as_str())
            || market.get("snapshot_count").and_then(Value::as_u64)
                != Some(frame.snapshots.len() as u64)
        {
            return Err(format!("{instrument} 深度输入身份与运行产物不一致"));
        }
        let tier = market
            .get("data_tier")
            .and_then(Value::as_str)
            .ok_or("market_data 缺少 data_tier")?;
        if !matches!(tier, "l1" | "l2") {
            return Err(format!("{instrument} 深度档位非法"));
        }
        data_hasher.write_text(instrument);
        data_hasher.write_text(tier);
        data_hasher.write_text(&frame.source);
        data_hasher.write_u64(frame.input_hash());
        input_components.insert(
            format!("depthframe:{instrument}"),
            format!("depth:{tier}:{}:{:016x}", frame.source, frame.input_hash()),
        );
        let quality = match market
            .get("dataset_quality")
            .filter(|value| !value.is_null())
        {
            Some(quality) => Some(verify_quality(
                instrument, depth_path, tier, &frame, quality,
            )?),
            None => None,
        };
        if let Some(quality) = &quality {
            let fingerprint = quality["manifest_fingerprint"]
                .as_str()
                .ok_or("质量清单缺少 fingerprint")?;
            data_hasher.write_text(fingerprint);
            input_components.insert(
                format!("dataset-quality:{instrument}"),
                fingerprint.to_string(),
            );
        }
        rows.push(json!({
            "instrument": instrument,
            "data_tier": tier,
            "source": frame.source,
            "content_hash": market["content_hash"],
            "snapshot_count": frame.snapshots.len(),
            "fills": artifact.pointer(&format!("/per_market_fills/{instrument}")).cloned().unwrap_or(json!(0)),
            "dataset_quality": quality,
        }));
    }
    let composed = format!("{:016x}", data_hasher.finish());
    let run_manifest_path = artifact["run_manifest_path"]
        .as_str()
        .ok_or("多市场深度产物缺少 RunManifest 路径")?;
    let manifest = qx_core::RunManifest::from_json(
        &std::fs::read_to_string(run_manifest_path)
            .map_err(|error| format!("读取 RunManifest 失败: {error}"))?,
    )?;
    if composed != outcome["data_fingerprint"].as_str().unwrap_or_default()
        || manifest.data_fingerprint != composed
        || manifest.input_components != input_components
        || manifest.result_hash != outcome["shared_result_hash"].as_str().unwrap_or_default()
    {
        return Err("多市场深度报告输入与 RunManifest 身份不一致".into());
    }
    Ok(json!({
        "run_id": outcome["run_id"], "account_id": outcome["account_id"],
        "result_hash": outcome["shared_result_hash"], "data_fingerprint": outcome["data_fingerprint"],
        "markets_total": outcome["markets_total"], "successful_fills": outcome["successful_fills"],
        "pending_orders": outcome["pending_orders"], "equity_points": outcome["equity_points"],
        "final_equity_raw": outcome["final_equity_raw"], "return_bps": outcome["return_bps"],
        "max_drawdown_bps": outcome["max_drawdown_bps"], "summary_path": path, "markets": rows
    }))
}

fn verify_quality(
    instrument: &str,
    frame_path: &str,
    tier: &str,
    frame: &qx_xingban::DepthFrame,
    quality: &Value,
) -> Result<Value, String> {
    let manifest_path = quality
        .get("manifest_path")
        .and_then(Value::as_str)
        .ok_or("质量摘要缺少 manifest_path")?;
    let bytes = std::fs::read(manifest_path)
        .map_err(|error| format!("读取 {instrument} 质量清单失败: {error}"))?;
    let sha = qx_data::sha256_hex_bytes(&bytes);
    if quality.get("manifest_sha256").and_then(Value::as_str) != Some(sha.as_str()) {
        return Err(format!("{instrument} 清单 SHA-256 不匹配"));
    }
    let manifest = qx_data::DatasetManifestV2::from_json(
        std::str::from_utf8(&bytes).map_err(|error| error.to_string())?,
    )?;
    let expected_tier = if tier == "l1" {
        qx_data::DatasetTier::L1
    } else {
        qx_data::DatasetTier::L2
    };
    let input_hash = qx_strategy::file_digest::sha256_file_hex(Path::new(frame_path))?;
    let count = u64::try_from(frame.snapshots.len()).map_err(|_| "DepthFrame 快照数溢出")?;
    let interval = manifest
        .quality_report
        .interval_ms
        .filter(|value| *value > 0)
        .ok_or("质量清单缺少正 interval_ms")?;
    let (duplicates, out_of_order, missing) = frame
        .snapshots
        .windows(2)
        .try_fold((0_u64, 0_u64, 0_u64), |(d, o, m), pair| {
            if pair[1].ts < pair[0].ts {
                Some((d, o.checked_add(1)?, m))
            } else if pair[1].ts == pair[0].ts {
                Some((d.checked_add(1)?, o, m))
            } else {
                Some((
                    d,
                    o,
                    m.checked_add((pair[1].ts - pair[0].ts).div_ceil(interval) - 1)?,
                ))
            }
        })
        .ok_or("深度质量统计溢出")?;
    if manifest.instrument != instrument
        || manifest.tier != expected_tier
        || manifest.provider_version != qx_data::DEPTHFRAME_DATASET_VERSION
        || manifest.content_hash != input_hash
        || manifest.dataset.fingerprint != format!("{:016x}", frame.input_hash())
        || manifest.dataset.source != frame.source
        || manifest.dataset.schema_version != frame.schema_version
        || manifest.dataset.start_timestamp != frame.snapshots.first().ok_or("DepthFrame 为空")?.ts
        || manifest.dataset.end_timestamp != frame.snapshots.last().ok_or("DepthFrame 为空")?.ts
        || manifest.quality_report.row_count != count
        || manifest.quality_report.usable_tiers != [tier]
        || manifest.quality_report.duplicate_rows != duplicates
        || manifest.quality_report.out_of_order_rows != out_of_order
        || manifest.quality_report.missing_intervals != missing
        || quality.get("manifest_fingerprint").and_then(Value::as_str)
            != Some(manifest.fingerprint()?.as_str())
        || quality.get("quality_report")
            != Some(
                &serde_json::to_value(&manifest.quality_report)
                    .map_err(|error| error.to_string())?,
            )
    {
        return Err(format!("{instrument} 质量清单与 DepthFrame 不一致"));
    }
    Ok(
        json!({"manifest_fingerprint": manifest.fingerprint()?, "manifest_sha256": sha, "quality_report": manifest.quality_report, "status": if missing + duplicates + out_of_order == 0 {"clean"} else {"issues"}}),
    )
}
