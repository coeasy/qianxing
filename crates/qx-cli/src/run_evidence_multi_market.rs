use crate::run_evidence_recompute::resolve_reference;
use crate::*;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[path = "run_evidence_multi_market_pack.rs"]
mod run_evidence_multi_market_pack;
pub(crate) use run_evidence_multi_market_pack::write_multi_market_run_evidence;

#[path = "run_evidence_multi_market_depth.rs"]
mod run_evidence_multi_market_depth;

pub(crate) fn write_multi_market_depth_run_evidence(
    artifact_path: &Path,
    diagnostics: Option<(&Path, &serde_json::Value)>,
) -> Result<PathBuf, String> {
    run_evidence_multi_market_depth::write_multi_market_depth_run_evidence(
        artifact_path,
        diagnostics,
    )
}

#[path = "run_evidence_multi_market_depth_recompute.rs"]
mod run_evidence_multi_market_depth_recompute;
pub(crate) use run_evidence_multi_market_depth_recompute::{
    recompute_multi_market_depth_identity, rerun_multi_market_depth,
};

pub(crate) fn recompute_multi_market_barframe_identity(
    root: &Path,
    dataset: &qx_spec::RunEvidenceDataset,
    manifest_components: &BTreeMap<String, String>,
    summary_path: &Path,
) -> Result<(BacktestInputProvenance, u64), String> {
    if dataset.components.len() < 3 {
        return Err("multi-market-barframe 至少要有三个市场输入".into());
    }
    let mut component_fingerprints = Vec::with_capacity(dataset.components.len());
    let mut instruments = Vec::with_capacity(dataset.components.len());
    let mut recomputed_components = BTreeMap::new();
    let mut paths = Vec::with_capacity(dataset.components.len());
    let mut input_hasher = qx_core::Fnv1a::new();
    let mut total_rows = 0_u64;
    for component in &dataset.components {
        let instrument = component
            .role
            .strip_prefix("market:")
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| format!("多市场输入 role 非法: {}", component.role))?;
        let path = resolve_reference(root, &component.path)?;
        let frame = read_bar_frame_for_backtest(&path)?;
        if frame.instrument.to_string() != instrument {
            return Err(format!(
                "EvidenceBundle {} 标的与输入 BarFrame 不一致: {} != {instrument}",
                component.role, frame.instrument
            ));
        }
        let fingerprint = qx_data::fingerprint_bar_frame(&frame)
            .map_err(|error| format!("重算 {} BarFrame 指纹失败: {error}", component.role))?;
        let expected_dataset_id = format!("market:{instrument}");
        for (field, declared, actual) in [
            (
                "dataset_id",
                component.dataset_id.as_str(),
                expected_dataset_id.as_str(),
            ),
            (
                "version",
                component.version.as_str(),
                qx_data::BARFRAME_DATASET_VERSION,
            ),
            (
                "content_fingerprint",
                component.content_fingerprint.as_str(),
                fingerprint.as_str(),
            ),
        ] {
            if declared != actual {
                return Err(format!(
                    "EvidenceBundle {role} 输入 {field} 不匹配: 声明={declared} 重算={actual}",
                    role = component.role
                ));
            }
        }
        let rows = u64::try_from(frame.ts.len()).map_err(|_| "BarFrame 行数超出 u64 范围")?;
        if component.row_count != rows {
            return Err(format!(
                "EvidenceBundle {} 行数不匹配: 声明={} 重算={rows}",
                component.role, component.row_count
            ));
        }
        total_rows = total_rows
            .checked_add(rows)
            .ok_or("multi-market-barframe 输入行数总和溢出")?;
        input_hasher.write_text(instrument);
        input_hasher.write_text(&fingerprint);
        recomputed_components.insert(format!("barframe:{instrument}"), fingerprint.clone());
        if let Some(manifest_fingerprint) = &component.manifest_fingerprint {
            let quality_path = resolve_reference(
                root,
                component
                    .quality_manifest_path
                    .as_deref()
                    .ok_or_else(|| format!("EvidenceBundle {} 缺少质量清单路径", component.role))?,
            )?;
            let quality_bytes = std::fs::read(&quality_path)
                .map_err(|error| format!("读取 {} 质量清单失败: {error}", component.role))?;
            let quality_sha256 = qx_data::sha256_hex_bytes(&quality_bytes);
            if component.quality_manifest_sha256.as_deref() != Some(quality_sha256.as_str()) {
                return Err(format!(
                    "EvidenceBundle {} 质量清单 SHA-256 不匹配",
                    component.role
                ));
            }
            let quality_json = std::str::from_utf8(&quality_bytes)
                .map_err(|error| format!("{} 质量清单不是 UTF-8: {error}", component.role))?;
            let quality = qx_data::DatasetManifestV2::from_json(quality_json)?;
            if quality.fingerprint()? != *manifest_fingerprint
                || quality.instrument != instrument
                || quality.provider_version != qx_data::BARFRAME_DATASET_VERSION
                || quality.tier != qx_data::DatasetTier::Bar
                || quality.content_hash != qx_strategy::file_digest::sha256_file_hex(&path)?
                || quality.quality_report.row_count != rows
                || quality.dataset.fingerprint != fingerprint
                || quality.dataset.start_timestamp != frame.ts[0]
                || quality.dataset.end_timestamp != *frame.ts.last().ok_or("BarFrame 为空")?
            {
                return Err(format!(
                    "EvidenceBundle {} 质量清单与 BarFrame 身份不一致",
                    component.role
                ));
            }
            let interval = quality
                .quality_report
                .interval_ms
                .filter(|value| *value > 0)
                .ok_or_else(|| {
                    format!("EvidenceBundle {} 质量清单缺少 interval_ms", component.role)
                })?;
            let missing = frame
                .ts
                .windows(2)
                .try_fold(0_u64, |total, pair| {
                    total.checked_add((pair[1] - pair[0]).div_ceil(interval).saturating_sub(1))
                })
                .ok_or("多市场质量缺口数复算溢出")?;
            if quality.quality_report.missing_intervals != missing
                || quality.quality_report.duplicate_rows != 0
                || quality.quality_report.out_of_order_rows != 0
            {
                return Err(format!(
                    "EvidenceBundle {} 质量报告与 BarFrame 实况不一致",
                    component.role
                ));
            }
            let report = component
                .quality_report
                .as_ref()
                .ok_or("多市场质量报告缺失")?;
            if report.duplicate_rows != quality.quality_report.duplicate_rows
                || report.out_of_order_rows != quality.quality_report.out_of_order_rows
                || report.missing_intervals != quality.quality_report.missing_intervals
                || report.interval_ms != quality.quality_report.interval_ms
                || report.timezone != quality.quality_report.timezone
                || report.corporate_action_coverage
                    != quality.quality_report.corporate_action_coverage
                || report.usable_tiers != quality.quality_report.usable_tiers
            {
                return Err(format!(
                    "EvidenceBundle {} 报告字段与质量清单不一致",
                    component.role
                ));
            }
            input_hasher.write_text(manifest_fingerprint);
            recomputed_components.insert(
                format!("dataset-quality:{instrument}"),
                manifest_fingerprint.clone(),
            );
        } else if component.quality_report.is_some()
            || component.quality_manifest_path.is_some()
            || component.quality_manifest_sha256.is_some()
        {
            return Err(format!(
                "EvidenceBundle {} 质量清单字段不成套",
                component.role
            ));
        }
        instruments.push(instrument.to_string());
        component_fingerprints.push(format!("{}={fingerprint}", component.role));
        paths.push(path.to_string_lossy().to_string());
    }
    let dataset_id = format!("multi-market:{}", instruments.join("+"));
    let content_fingerprint = qx_strategy::sha256_hex(component_fingerprints.join("|").as_bytes());
    let composed_fingerprint = format!("{:016x}", input_hasher.finish());
    let mut expected_input_components = manifest_components.clone();
    let derived_equity_hash =
        expected_input_components.remove("derived:instrument-equity-contributions");
    let summary: serde_json::Value =
        serde_json::from_slice(&std::fs::read(summary_path).map_err(|error| {
            format!(
                "读取多市场 EvidenceBundle 摘要失败 {}: {error}",
                summary_path.display()
            )
        })?)
        .map_err(|error| format!("多市场 EvidenceBundle 摘要 JSON 无效: {error}"))?;
    let derived_equity_path = summary
        .get("instrument_equity_contributions_path")
        .and_then(serde_json::Value::as_str)
        .map(|reference| resolve_reference(root, reference))
        .transpose()?;
    match (derived_equity_path, derived_equity_hash) {
        (Some(path), Some(expected)) => {
            let bytes = std::fs::read(&path).map_err(|error| {
                format!("读取多市场按标的权益贡献失败 {}: {error}", path.display())
            })?;
            let actual = qx_data::sha256_hex_bytes(&bytes);
            if actual != expected {
                return Err("EvidenceBundle 按标的权益贡献与 RunManifest SHA-256 不一致".into());
            }
        }
        (None, None) => {}
        _ => return Err("EvidenceBundle 按标的权益贡献路径与 RunManifest 组件不成对".into()),
    }
    let version = "multi-market-barframe-v1";
    if recomputed_components != expected_input_components
        || dataset.dataset_id != dataset_id
        || dataset.version != version
        || dataset.content_fingerprint != content_fingerprint
        || dataset.composed_fingerprint != composed_fingerprint
        || dataset.row_count != total_rows
    {
        return Err("EvidenceBundle 多市场组合身份与各市场输入重算结果不一致".into());
    }
    Ok((
        BacktestInputProvenance {
            kind: "multi-market-barframe",
            path: paths.join("|"),
            dataset_id,
            dataset_version: version.to_string(),
            fingerprint: content_fingerprint,
        },
        total_rows,
    ))
}

pub(crate) fn rerun_multi_market_barframe(
    root: &Path,
    summary_path: &Path,
    dataset: &qx_spec::RunEvidenceDataset,
    expected: &qx_core::RunManifest,
) -> Result<(), String> {
    if !expected
        .code_commit
        .eq_ignore_ascii_case(crate::build_identity::BUILD_REVISION)
    {
        return Err(format!(
            "当前构建 revision={} 与证据要求的 code_commit={} 不一致，拒绝多市场复算",
            crate::build_identity::BUILD_REVISION,
            expected.code_commit
        ));
    }
    let summary: serde_json::Value = serde_json::from_slice(
        &std::fs::read(summary_path).map_err(|error| format!("读取多市场摘要失败: {error}"))?,
    )
    .map_err(|error| format!("多市场摘要 JSON 无效: {error}"))?;
    if summary
        .get("strategy_identities")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|identities| !identities.is_empty())
    {
        return Err(
            "多市场运行绑定了外部策略；离线复算尚未重建外部策略二进制，拒绝以 builtin 结果代替"
                .into(),
        );
    }
    let mut spec: qx_app::MultiMarketBacktestSpec =
        serde_json::from_value(summary.get("spec").cloned().ok_or("多市场摘要缺少 spec")?)
            .map_err(|error| format!("多市场摘要的 spec 无效: {error}"))?;
    if spec.markets.len() != dataset.components.len() {
        return Err("多市场摘要 market 数与 EvidenceBundle components 数不一致".into());
    }
    for (market, component) in spec.markets.iter_mut().zip(&dataset.components) {
        let role = format!("market:{}", market.instrument);
        if component.role != role {
            return Err(format!(
                "多市场输入顺序/身份不一致: {} != {role}",
                component.role
            ));
        }
        market.bars_path = resolve_reference(root, &component.path)?
            .canonicalize()
            .map_err(|error| format!("解析 {role} 复算输入失败: {error}"))?
            .to_string_lossy()
            .to_string();
        market.dataset_manifest_path = component
            .quality_manifest_path
            .as_deref()
            .map(|path| resolve_reference(root, path))
            .transpose()?
            .map(|path| path.to_string_lossy().to_string());
    }
    let executable =
        std::env::current_exe().map_err(|error| format!("定位 qx-cli 当前构建失败: {error}"))?;
    let target = executable
        .parent()
        .and_then(Path::parent)
        .ok_or("当前 qx-cli 不在 target/<profile> 下，无法确定隔离复算目录")?
        .to_path_buf();
    std::fs::create_dir_all(&target)
        .map_err(|error| format!("创建复算 target 目录失败: {error}"))?;
    let target = target
        .canonicalize()
        .map_err(|error| format!("解析复算 target 目录失败: {error}"))?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| format!("读取系统时钟失败: {error}"))?
        .as_nanos();
    let work = target.join(format!(
        "qx-multi-market-recompute-{}-{stamp}",
        std::process::id()
    ));
    std::fs::create_dir(&work).map_err(|error| format!("创建多市场复算目录失败: {error}"))?;
    let work = work
        .canonicalize()
        .map_err(|error| format!("解析多市场复算目录失败: {error}"))?;
    if !work.starts_with(&target) {
        return Err("多市场复算目录越出 target".into());
    }
    let result = (|| {
        spec.output_dir = work.join("output").to_string_lossy().to_string();
        let context =
            qx_app::RunContext::new(qx_app::CallerCapability::Research, spec.run_id.clone())
                .with_code_commit(crate::build_identity::BUILD_REVISION);
        qx_app::run_multi_market_backtest(&spec, &context)
            .map_err(|error| format!("重跑多市场回测失败: {}", error.to_json()))?;
        let manifest_path =
            PathBuf::from(spec.output_dir).join(format!("{}.multi-market.run.json", spec.run_id));
        let actual = qx_core::RunManifest::from_json(
            &std::fs::read_to_string(&manifest_path)
                .map_err(|error| format!("读取多市场复算 RunManifest 失败: {error}"))?,
        )?;
        if actual != *expected {
            return Err(format!(
                "多市场复算 RunManifest 不一致: expected={} actual={}",
                expected.digest(),
                actual.digest()
            ));
        }
        Ok(())
    })();
    let cleanup = std::fs::remove_dir_all(&work)
        .map_err(|error| format!("清理多市场复算目录失败 {}: {error}", work.display()));
    result?;
    cleanup?;
    Ok(())
}
