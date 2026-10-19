use crate::*;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub(crate) fn recompute_multi_market_depth_identity(
    root: &Path,
    dataset: &qx_spec::RunEvidenceDataset,
    manifest: &qx_core::RunManifest,
) -> Result<(BacktestInputProvenance, u64), String> {
    if dataset.components.len() < 2 {
        return Err("multi-market-depthframe 至少需要两个市场输入".into());
    }
    let mut components = Vec::new();
    let mut instruments = Vec::new();
    let mut paths = Vec::new();
    let mut count = 0_u64;
    let mut hasher = qx_core::Fnv1a::new();
    let mut manifest_inputs = BTreeMap::new();
    for component in &dataset.components {
        let instrument = component
            .role
            .strip_prefix("market:")
            .ok_or("多市场深度 role 非法")?;
        let path = crate::run_evidence_recompute::resolve_reference(root, &component.path)?;
        let frame = read_depth_frame_for_backtest(&path)?;
        if frame.instrument.to_string() != instrument {
            return Err(format!("{} 标的不一致", component.role));
        }
        let hash = format!("{:016x}", frame.input_hash());
        let fingerprint = ["l1", "l2"]
            .into_iter()
            .map(|tier| format!("depth:{tier}:{}:{hash}", frame.source))
            .find(|candidate| candidate == &component.content_fingerprint)
            .ok_or_else(|| format!("{} 内容身份不匹配", component.role))?;
        if component.version != "depthframe-v1"
            || component.dataset_id != format!("depth:{instrument}")
            || component.content_fingerprint != fingerprint
        {
            return Err(format!("{} 内容身份不匹配", component.role));
        }
        let rows = u64::try_from(frame.snapshots.len()).map_err(|_| "快照数溢出")?;
        if rows != component.row_count {
            return Err(format!("{} 快照数不匹配", component.role));
        }
        count = count.checked_add(rows).ok_or("多市场深度快照总数溢出")?;
        let tier = if fingerprint.starts_with("depth:l1:") {
            "l1"
        } else {
            "l2"
        };
        if tier == "l1"
            && frame
                .snapshots
                .iter()
                .any(|snapshot| snapshot.bids.len() != 1 || snapshot.asks.len() != 1)
        {
            return Err(format!("{} 声明 L1 但不是单档深度", component.role));
        }
        hasher.write_text(instrument);
        hasher.write_text(tier);
        hasher.write_text(&frame.source);
        hasher.write_u64(frame.input_hash());
        manifest_inputs.insert(format!("depthframe:{instrument}"), fingerprint.clone());
        if let Some(manifest_fingerprint) = &component.manifest_fingerprint {
            let quality_path = crate::run_evidence_recompute::resolve_reference(
                root,
                component
                    .quality_manifest_path
                    .as_deref()
                    .ok_or("深度质量清单缺少路径")?,
            )?;
            let bytes = std::fs::read(&quality_path).map_err(|error| error.to_string())?;
            if component.quality_manifest_sha256.as_deref()
                != Some(qx_data::sha256_hex_bytes(&bytes).as_str())
            {
                return Err(format!("{} 质量清单 SHA-256 不匹配", component.role));
            }
            let quality = qx_data::DatasetManifestV2::from_json(
                std::str::from_utf8(&bytes).map_err(|error| error.to_string())?,
            )?;
            let tier_value = if tier == "l1" {
                qx_data::DatasetTier::L1
            } else {
                qx_data::DatasetTier::L2
            };
            let rows = u64::try_from(frame.snapshots.len()).map_err(|_| "深度快照数溢出")?;
            let interval = quality
                .quality_report
                .interval_ms
                .filter(|value| *value > 0)
                .ok_or("深度质量清单缺少正 interval_ms")?;
            let (duplicates, out_of_order, missing) = frame
                .snapshots
                .windows(2)
                .try_fold(
                    (0_u64, 0_u64, 0_u64),
                    |(duplicates, out_of_order, missing), pair| {
                        if pair[1].ts < pair[0].ts {
                            Some((duplicates, out_of_order.checked_add(1)?, missing))
                        } else if pair[1].ts == pair[0].ts {
                            Some((duplicates.checked_add(1)?, out_of_order, missing))
                        } else {
                            Some((
                                duplicates,
                                out_of_order,
                                missing.checked_add(
                                    (pair[1].ts - pair[0].ts).div_ceil(interval) - 1,
                                )?,
                            ))
                        }
                    },
                )
                .ok_or("深度质量统计溢出")?;
            let report = component
                .quality_report
                .as_ref()
                .ok_or("深度证据缺少质量报告")?;
            if quality.fingerprint()? != *manifest_fingerprint
                || quality.instrument != instrument
                || quality.tier != tier_value
                || quality.provider_version != qx_data::DEPTHFRAME_DATASET_VERSION
                || quality.dataset.dataset_id != format!("depth-bars:{instrument}")
                || quality.dataset.version != qx_data::DEPTHFRAME_DATASET_VERSION
                || quality.dataset.source != frame.source
                || quality.dataset.schema_version != frame.schema_version
                || quality.source_lineage.primary_source != frame.source
                || quality.content_hash != qx_strategy::file_digest::sha256_file_hex(&path)?
                || quality.dataset.fingerprint != format!("{:016x}", frame.input_hash())
                || quality.dataset.start_timestamp
                    != frame.snapshots.first().ok_or("DepthFrame 为空")?.ts
                || quality.dataset.end_timestamp
                    != frame.snapshots.last().ok_or("DepthFrame 为空")?.ts
                || quality.quality_report.row_count != rows
                || quality.quality_report.usable_tiers != [tier]
                || quality.quality_report.duplicate_rows != duplicates
                || quality.quality_report.out_of_order_rows != out_of_order
                || quality.quality_report.missing_intervals != missing
                || report.duplicate_rows != duplicates
                || report.out_of_order_rows != out_of_order
                || report.missing_intervals != missing
                || report.interval_ms != quality.quality_report.interval_ms
                || report.timezone != quality.quality_report.timezone
                || report.usable_tiers != quality.quality_report.usable_tiers
            {
                return Err(format!("{} 质量报告与 DepthFrame 不匹配", component.role));
            }
            hasher.write_text(manifest_fingerprint);
            manifest_inputs.insert(
                format!("dataset-quality:{instrument}"),
                manifest_fingerprint.clone(),
            );
        }
        components.push(format!("{}={fingerprint}", component.role));
        instruments.push(instrument.to_string());
        paths.push(path.to_string_lossy().to_string());
    }
    let dataset_id = format!("multi-market-depth:{}", instruments.join("+"));
    let fingerprint = qx_strategy::sha256_hex(components.join("|").as_bytes());
    let composed = format!("{:016x}", hasher.finish());
    if dataset.dataset_id != dataset_id
        || dataset.version != "multi-market-depthframe-v1"
        || dataset.content_fingerprint != fingerprint
        || dataset.composed_fingerprint != composed
        || dataset.row_count != count
        || manifest.data_fingerprint != composed
        || manifest.input_components != manifest_inputs
    {
        return Err("多市场深度组合身份与输入/Manifest 重算不一致".into());
    }
    Ok((
        BacktestInputProvenance {
            kind: "multi-market-depthframe",
            path: paths.join("|"),
            dataset_id,
            dataset_version: "multi-market-depthframe-v1".into(),
            fingerprint,
        },
        count,
    ))
}

pub(crate) fn rerun_multi_market_depth(
    root: &Path,
    summary: &Path,
    dataset: &qx_spec::RunEvidenceDataset,
    expected: &qx_core::RunManifest,
) -> Result<(), String> {
    if !expected
        .code_commit
        .eq_ignore_ascii_case(crate::build_identity::BUILD_REVISION)
    {
        return Err("当前构建 revision 与证据 code_commit 不一致，拒绝多市场深度复算".into());
    }
    let value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(summary).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
    if value
        .get("strategy_identities")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|ids| !ids.is_empty())
    {
        return Err("多市场深度运行含外部策略，缺少第三方策略二进制，拒绝 builtin 代替复算".into());
    }
    let mut spec: qx_app::MultiMarketDepthBacktestSpec =
        serde_json::from_value(value.get("spec").cloned().ok_or("摘要缺少 spec")?)
            .map_err(|error| error.to_string())?;
    if spec.markets.len() != dataset.components.len() {
        return Err("市场数与输入组件数不一致".into());
    }
    for (market, component) in spec.markets.iter_mut().zip(&dataset.components) {
        if component.role != format!("market:{}", market.instrument) {
            return Err("市场顺序/身份与证据组件不一致".into());
        }
        market.depth_path =
            crate::run_evidence_recompute::resolve_reference(root, &component.path)?
                .canonicalize()
                .map_err(|error| error.to_string())?
                .to_string_lossy()
                .to_string();
        market.dataset_manifest_path = component
            .quality_manifest_path
            .as_deref()
            .map(|path| crate::run_evidence_recompute::resolve_reference(root, path))
            .transpose()?
            .map(|path| path.to_string_lossy().to_string());
    }
    let target = std::env::current_exe()
        .map_err(|error| error.to_string())?
        .parent()
        .and_then(Path::parent)
        .ok_or("无法确定隔离复算目录")?
        .to_path_buf();
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos();
    let work = target.join(format!(
        "qx-multi-market-depth-recompute-{}-{stamp}",
        std::process::id()
    ));
    std::fs::create_dir(&work).map_err(|error| error.to_string())?;
    let result: Result<(), String> = (|| {
        spec.output_dir = work.join("output").to_string_lossy().to_string();
        let context =
            qx_app::RunContext::new(qx_app::CallerCapability::Research, spec.run_id.clone())
                .with_code_commit(crate::build_identity::BUILD_REVISION);
        qx_app::run_multi_market_depth_backtest(&spec, &context)
            .map_err(|error| error.to_json())?;
        let path = PathBuf::from(&spec.output_dir)
            .join(format!("{}.multi-market-depth.run.json", spec.run_id));
        let actual = qx_core::RunManifest::from_json(
            &std::fs::read_to_string(path).map_err(|error| error.to_string())?,
        )?;
        if actual != *expected {
            return Err("多市场深度复算 RunManifest 不一致".into());
        }
        Ok(())
    })();
    let cleanup =
        std::fs::remove_dir_all(&work).map_err(|error| format!("清理深度复算目录失败: {error}"));
    result?;
    cleanup?;
    Ok(())
}
