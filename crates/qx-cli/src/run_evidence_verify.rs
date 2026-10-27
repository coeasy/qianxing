use crate::run_evidence_multi_market::{
    recompute_multi_market_barframe_identity, recompute_multi_market_depth_identity,
    rerun_multi_market_barframe, rerun_multi_market_depth,
};
use crate::run_evidence_recompute::{
    recompute_multi_barframe_identity, rerun_depth_backtest, rerun_strategy, resolve_reference,
};
use crate::*;
use qx_spec::{EvidenceBundle, EvidenceGeneration, FoundationDocument};
use std::path::Path;

fn report_json_equivalent(left: &serde_json::Value, right: &serde_json::Value) -> bool {
    match (left, right) {
        (serde_json::Value::Object(left), serde_json::Value::Object(right)) => {
            left.len() == right.len()
                && left.iter().all(|(key, value)| {
                    right
                        .get(key)
                        .is_some_and(|other| report_json_equivalent(value, other))
                })
        }
        (serde_json::Value::Array(left), serde_json::Value::Array(right)) => {
            left.len() == right.len()
                && left
                    .iter()
                    .zip(right)
                    .all(|(left, right)| report_json_equivalent(left, right))
        }
        (serde_json::Value::Number(left), serde_json::Value::Number(right)) => {
            if left == right {
                true
            } else {
                left.as_f64()
                    .zip(right.as_f64())
                    .is_some_and(|(left, right)| (left - right).abs() <= 1e-9)
            }
        }
        _ => left == right,
    }
}

pub(crate) fn verify_run_bundle(bundle_path: &Path, recompute: bool) -> Result<(), String> {
    verify_run_bundle_impl(bundle_path, recompute, true)
}

pub(crate) fn verify_run_bundle_quiet(bundle_path: &Path, recompute: bool) -> Result<(), String> {
    verify_run_bundle_impl(bundle_path, recompute, false)
}

fn verify_run_bundle_impl(
    bundle_path: &Path,
    recompute: bool,
    emit_result: bool,
) -> Result<(), String> {
    let payload = std::fs::read_to_string(bundle_path).map_err(|error| {
        format!(
            "读取运行 EvidenceBundle 失败 {}: {error}",
            bundle_path.display()
        )
    })?;
    let bundle = EvidenceBundle::from_json(&payload)?;
    let EvidenceGeneration::Run(run) = &bundle.generation else {
        return Err("evidence verify-run 只接受 generation=run".into());
    };
    let root = bundle_path.parent().unwrap_or_else(|| Path::new("."));
    let mut manifest_path = None;
    let mut summary_path = None;
    let mut runtime_config_path = None;
    let mut runtime_config_fingerprint_matches_run_hash = None;
    let mut market_spec_path = None;
    let mut market_spec_primary_path = None;
    let mut market_spec_reference_path = None;
    let mut verified = 0usize;
    let mut strategy_diagnostics_verified = false;
    for artifact in &run.artifacts {
        let path = resolve_reference(root, &artifact.path)?;
        let digest = qx_strategy::file_digest::sha256_file_hex(&path)?;
        if digest != artifact.digest {
            return Err(format!(
                "EvidenceBundle 产物摘要不匹配: {} 声明={} 实际={digest}",
                artifact.path, artifact.digest
            ));
        }
        if artifact.name == "run_manifest" {
            manifest_path = Some(path);
        } else if artifact.name == "summary" {
            summary_path = Some(path);
        } else if artifact.name == "runtime_config" {
            runtime_config_path = Some(path);
        } else if artifact.name == "market_spec" {
            market_spec_path = Some(path);
        } else if artifact.name == "market_spec_primary" {
            market_spec_primary_path = Some(path);
        } else if artifact.name == "market_spec_reference" {
            market_spec_reference_path = Some(path);
        }
        verified += 1;
    }
    let manifest_path = manifest_path.ok_or("运行 EvidenceBundle 缺少 run_manifest 产物")?;
    let manifest_payload = std::fs::read_to_string(&manifest_path)
        .map_err(|error| format!("读取 RunManifest 失败 {}: {error}", manifest_path.display()))?;
    let manifest = qx_core::RunManifest::from_json(&manifest_payload)?;
    if manifest.run_id != run.run.run_id
        || manifest.code_commit != run.run.code_commit
        || manifest.runtime_version != run.run.runtime_version
        || manifest.config_hash != run.run.config_hash
        || manifest.data_fingerprint != run.run.data_fingerprint
        || manifest.input_components != run.run.input_components
        || manifest.result_hash != run.run.result_hash
        || manifest.strategy_version != run.run.strategy_version
        || manifest.instrument_spec_version != run.run.instrument_spec_version
        || manifest.model_fingerprint != run.run.model_fingerprint
        || manifest.input_event_hash != run.run.input_event_hash
        || manifest.output_event_hash != run.run.output_event_hash
        || manifest.clock_start != run.run.clock_start
        || manifest.clock_end != run.run.clock_end
        || manifest.global_seed != run.run.global_seed
        || manifest.determinism_mode != run.run.determinism_mode
    {
        return Err("RunManifest 与 EvidenceBundle.run 身份字段不一致".into());
    }
    if let Some(config_path) = &runtime_config_path {
        if !run.run.run_id.starts_with("depth-backtest:") {
            let config = read_runtime_config(config_path)?;
            let fingerprint = config.fingerprint()?;
            runtime_config_fingerprint_matches_run_hash = Some(fingerprint == run.run.config_hash);
            let strategy_run = run.run.run_id.starts_with("strategy-backtest:");
            if fingerprint != run.run.config_hash && !strategy_run {
                return Err(format!(
                    "EvidenceBundle runtime_config 指纹不匹配: {} != {}",
                    fingerprint, run.run.config_hash
                ));
            }
        }
    }
    crate::run_evidence_build_attestation::verify_attached_build_attestation(
        root,
        &run.artifacts,
        runtime_config_path.as_deref(),
        &run.run.run_id,
    )?;
    let input_path = if matches!(
        run.dataset.kind.as_str(),
        "multi-barframe" | "multi-market-barframe" | "multi-market-depthframe"
    ) {
        let primary = run
            .dataset
            .components
            .first()
            .ok_or("multi-barframe 缺少 primary 输入")?;
        resolve_reference(root, &primary.path)?
    } else {
        resolve_reference(root, &run.dataset.path)?
    };
    let quality_manifest_path = run
        .dataset
        .quality_manifest_path
        .as_deref()
        .map(|path| resolve_reference(root, path))
        .transpose()?;
    match (
        quality_manifest_path.as_deref(),
        run.dataset.quality_manifest_sha256.as_deref(),
    ) {
        (Some(path), Some(expected)) => {
            let actual = qx_strategy::file_digest::sha256_file_hex(path)?;
            if actual != expected {
                return Err(format!(
                    "EvidenceBundle 数据质量清单 SHA-256 不匹配: {} 声明={} 实际={actual}",
                    path.display(),
                    expected
                ));
            }
        }
        (None, None) => {}
        _ => return Err("EvidenceBundle 数据质量清单路径与 SHA-256 必须成对存在".into()),
    }
    let (recomputed, input_rows) = match run.dataset.kind.as_str() {
        "barframe" => {
            let frame = read_bar_frame_for_backtest(&input_path)?;
            let (_, manifest) = if run.dataset.version == qx_data::BARFRAME_DATASET_VERSION {
                qx_app::backtest_input_identity::legacy_barframe_dataset_identity(
                    &input_path,
                    &frame,
                )?
            } else {
                barframe_dataset_identity(&input_path, &frame)?
            };
            if let Some(quality_path) = quality_manifest_path.as_deref() {
                let quality = qx_app::artifact_store::verify_barframe_dataset_quality(
                    quality_path,
                    &input_path,
                    &frame,
                    &manifest,
                )?;
                let fingerprint = quality["manifest_fingerprint"]
                    .as_str()
                    .ok_or("复核后的数据质量清单缺少 fingerprint")?;
                if run.dataset.manifest_fingerprint.as_deref() != Some(fingerprint) {
                    return Err(
                        "EvidenceBundle 数据质量清单 fingerprint 与文件重算结果不一致".into(),
                    );
                }
                let mut report = quality["quality_report"].clone();
                if let Some(fields) = report.as_object_mut() {
                    fields.remove("row_count");
                }
                let declared_report = run
                    .dataset
                    .quality_report
                    .as_ref()
                    .map(serde_json::to_value)
                    .transpose()
                    .map_err(|error| format!("编码 EvidenceBundle 数据质量报告失败: {error}"))?;
                if declared_report.as_ref() != Some(&report) {
                    return Err("EvidenceBundle 数据质量报告与清单重算结果不一致".into());
                }
            }
            (
                qx_app::backtest_input_identity::barframe_input_provenance(&input_path, &manifest),
                frame.ts.len() as u64,
            )
        }
        "depth-frame" => {
            let frame = read_depth_frame_for_backtest(&input_path)?;
            (
                qx_app::backtest_input_identity::depth_frame_input_provenance(&input_path, &frame),
                frame.snapshots.len() as u64,
            )
        }
        "multi-barframe" => recompute_multi_barframe_identity(root, &run.dataset)?,
        "multi-market-barframe" => recompute_multi_market_barframe_identity(
            root,
            &run.dataset,
            &manifest.input_components,
            &run.artifacts,
        )?,
        "multi-market-depthframe" => {
            recompute_multi_market_depth_identity(root, &run.dataset, &manifest)?
        }
        kind => return Err(format!("未知的 EvidenceBundle 输入类型: {kind}")),
    };
    if recomputed.dataset_id != run.dataset.dataset_id
        || recomputed.dataset_version != run.dataset.version
        || recomputed.fingerprint != run.dataset.content_fingerprint
    {
        return Err(format!(
            "EvidenceBundle 输入身份与文件重算结果不一致: dataset_id={}/{}, version={}/{}, fingerprint={}/{}",
            run.dataset.dataset_id,
            recomputed.dataset_id,
            run.dataset.version,
            recomputed.dataset_version,
            run.dataset.content_fingerprint,
            recomputed.fingerprint
        ));
    }
    if input_rows != run.dataset.row_count {
        return Err(format!(
            "EvidenceBundle 输入行数与文件不一致: 声明={} 实际={input_rows}",
            run.dataset.row_count
        ));
    }
    if let Some(diagnostics) = &run.strategy_diagnostics {
        let diagnostics_path = resolve_reference(root, &diagnostics.input_path)?;
        let diagnostic_bytes = std::fs::read(&diagnostics_path).map_err(|error| {
            format!(
                "读取策略诊断附件失败 {}: {error}",
                diagnostics_path.display()
            )
        })?;
        if matches!(
            diagnostics.kind.as_str(),
            "strategy-bias-diagnostics-array" | "strategy-bias-diagnostics-depth-array"
        ) && diagnostic_bytes.len() > 1024 * 1024
        {
            return Err("EvidenceBundle 多市场策略诊断附件超过 1 MiB 上限".into());
        }
        let digest = qx_strategy::sha256_hex(&diagnostic_bytes);
        if digest != diagnostics.input_sha256 {
            return Err(format!(
                "EvidenceBundle 策略诊断 SHA-256 不匹配: {} 声明={} 实际={digest}",
                diagnostics.input_path, diagnostics.input_sha256
            ));
        }
        let diagnostic_report: serde_json::Value = serde_json::from_slice(&diagnostic_bytes)
            .map_err(|error| format!("EvidenceBundle 策略诊断 JSON 无效: {error}"))?;
        match diagnostics.kind.as_str() {
            "strategy-bias-diagnostics" if run.dataset.kind == "barframe" => {
                crate::report_command::validate_strategy_diagnostics_identity(
                    &diagnostic_report,
                    &run.identity.strategy_id,
                    &run.identity.instrument,
                    &run.dataset.content_fingerprint,
                )?;
            }
            "strategy-bias-diagnostics-array" if run.dataset.kind == "multi-market-barframe" => {
                let summary_path = summary_path
                    .as_deref()
                    .ok_or("多市场策略诊断证据缺少原始运行摘要")?;
                verify_multi_market_strategy_diagnostic_array(
                    root,
                    summary_path,
                    &run.dataset,
                    &diagnostic_report,
                )?;
            }
            "strategy-bias-diagnostics-depth-array"
                if run.dataset.kind == "multi-market-depthframe" =>
            {
                let summary_path = summary_path
                    .as_deref()
                    .ok_or("多市场深度策略诊断证据缺少原始运行摘要")?;
                crate::report_multi_market_depth::verify_strategy_diagnostics_for_evidence(
                    summary_path,
                    &diagnostic_report,
                )?;
            }
            _ => return Err("EvidenceBundle 策略诊断 kind 与输入形态不匹配".into()),
        }
        strategy_diagnostics_verified = true;
    }
    if run.recompute.expected_result_hash != run.run.result_hash {
        return Err("recompute.expected_result_hash 与 run.result_hash 不一致".into());
    }
    if let Some(analysis) = &run.report_analysis {
        let benchmark_path = resolve_reference(root, &analysis.input_path)?;
        let digest = qx_strategy::file_digest::sha256_file_hex(&benchmark_path)?;
        if digest != analysis.input_sha256 {
            return Err(format!(
                "EvidenceBundle 报告分析输入 SHA-256 不匹配: {}",
                analysis.input_path
            ));
        }
        let calendar_path = analysis
            .calendar_input_path
            .as_deref()
            .map(|path| resolve_reference(root, path))
            .transpose()?;
        match (&calendar_path, &analysis.calendar_input_sha256) {
            (Some(path), Some(expected)) => {
                let actual = qx_strategy::file_digest::sha256_file_hex(path)?;
                if &actual != expected {
                    return Err(format!(
                        "EvidenceBundle 报告分析交易日历 SHA-256 不匹配: {}",
                        analysis.calendar_input_path.as_deref().unwrap_or_default()
                    ));
                }
            }
            (None, None) => {}
            _ => return Err("EvidenceBundle 报告分析交易日历路径与 SHA-256 必须同时存在".into()),
        }
        let summary_path = summary_path
            .as_deref()
            .ok_or("报告分析证据缺少 summary 产物")?;
        let summary: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(summary_path)
                .map_err(|error| format!("读取报告分析 summary 失败: {error}"))?,
        )
        .map_err(|error| format!("报告分析 summary JSON 无效: {error}"))?;
        let portfolio_return_bps = i64::try_from(
            report_readout::summary_number(&summary, "/metrics/return_bps")
                .ok_or("报告分析 summary 缺少 return_bps")?,
        )
        .map_err(|_| "报告分析 return_bps 超出 i64 范围")?;
        let mut recomputed = crate::report_benchmark::benchmark_analysis(
            summary_path,
            &benchmark_path,
            portfolio_return_bps,
            calendar_path.as_deref(),
        )?;
        if let Some(object) = recomputed
            .pointer_mut("/benchmark")
            .and_then(serde_json::Value::as_object_mut)
        {
            object.remove("path");
        }
        if let Some(object) = recomputed
            .pointer_mut("/calendar")
            .and_then(serde_json::Value::as_object_mut)
        {
            object.remove("path");
        }
        let mut recorded: serde_json::Value = serde_json::from_str(&analysis.analysis)
            .map_err(|error| format!("EvidenceBundle 基准分析 JSON 无效: {error}"))?;
        if let Some(object) = recorded
            .pointer_mut("/benchmark")
            .and_then(serde_json::Value::as_object_mut)
        {
            object.remove("path");
        }
        if let Some(object) = recorded
            .pointer_mut("/calendar")
            .and_then(serde_json::Value::as_object_mut)
        {
            object.remove("path");
        }
        if analysis.kind != "benchmark"
            || analysis.instrument
                != recomputed
                    .pointer("/benchmark/instrument")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
            || analysis.input_fingerprint
                != recomputed
                    .pointer("/benchmark/fingerprint")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
            || !report_json_equivalent(&recorded, &recomputed)
        {
            return Err("EvidenceBundle 基准分析与输入行情重算结果不一致".into());
        }
    }
    if recompute {
        match run.dataset.kind.as_str() {
            "barframe" if run.run.run_id.starts_with("strategy-backtest:") => {
                if quality_manifest_path.is_none()
                    && (run.dataset.manifest_fingerprint.is_some()
                        || run.dataset.quality_report.is_some())
                {
                    return Err(
                        "EvidenceBundle 声明了数据质量但未绑定质量清单文件，无法独立复算".into(),
                    );
                }
                let config_path = runtime_config_path
                    .as_deref()
                    .ok_or("EvidenceBundle 未附带 runtime_config；无法执行离线策略复算")?;
                rerun_strategy(
                    config_path,
                    &input_path,
                    market_spec_path.as_deref(),
                    quality_manifest_path.as_deref(),
                    &manifest.run_id,
                    &run.recompute.expected_result_hash,
                    &run.run.code_commit,
                )?;
            }
            "depth-frame" if run.run.run_id.starts_with("depth-backtest:") => {
                let summary_path = summary_path
                    .as_deref()
                    .ok_or("深度运行 EvidenceBundle 缺少 summary 产物，无法重建复算参数")?;
                rerun_depth_backtest(
                    summary_path,
                    &input_path,
                    runtime_config_path.as_deref(),
                    market_spec_path.as_deref(),
                    quality_manifest_path.as_deref(),
                    &manifest,
                )?;
            }
            "multi-barframe" if run.run.run_id.starts_with("multi-composition:") => {
                let summary_path = summary_path
                    .as_deref()
                    .ok_or("多腿运行 EvidenceBundle 缺少 summary 产物")?;
                crate::run_evidence_multi::rerun_multi_barframe(
                    root,
                    summary_path,
                    &run.dataset,
                    runtime_config_path.as_deref(),
                    market_spec_primary_path.as_deref(),
                    market_spec_reference_path.as_deref(),
                    &manifest,
                )?;
            }
            "multi-market-barframe" => {
                let summary_path = summary_path
                    .as_deref()
                    .ok_or("多市场运行 EvidenceBundle 缺少 summary 产物")?;
                rerun_multi_market_barframe(root, summary_path, &run.dataset, &manifest)?;
            }
            "multi-market-depthframe" => {
                let summary_path = summary_path
                    .as_deref()
                    .ok_or("多市场深度 EvidenceBundle 缺少 summary 产物")?;
                rerun_multi_market_depth(root, summary_path, &run.dataset, &manifest)?;
            }
            "barframe" | "depth-frame" => {
                return Err(format!(
                    "当前离线复算不支持该 RunManifest 类型: {}",
                    run.run.run_id
                ));
            }
            kind => return Err(format!("当前离线复算不支持输入类型: {kind}")),
        }
    }
    if emit_result {
        print_machine_json(&serde_json::json!({
        "ok": true,
        "operation": "run-evidence-input-and-artifact-verified",
        "generation": "run",
        "run_id": run.run.run_id,
        "verified_artifact_count": verified,
        "verified_attachment_count": usize::from(strategy_diagnostics_verified),
        "strategy_diagnostics_verified": strategy_diagnostics_verified,
        "input_identity_recomputed": true,
        "input_row_count_recomputed": input_rows,
        "result_hash": run.run.result_hash,
        "result_recomputed": recompute,
        "runtime_config_fingerprint_matches_run_hash": runtime_config_fingerprint_matches_run_hash,
        "limitation": if recompute { "策略回测（含证据包内可执行的外部策略发布物）、单标的 L1/L2 深度回测、双腿 multi-builtin、多市场 builtin Bar 与多市场 builtin 深度回测已使用匹配 code_commit 的当前构建重跑；策略回测按目标 RunManifest.run_id 选取 result_hash。运行配置文件及其依赖摘要已核验，但路径搬运后的绝对路径配置指纹可能不等于来源运行的 path-resolved config_hash；--recompute 通过结果摘要补足行为核对。复算子进程会移除敏感凭据和代理环境变量，但这不构成操作系统级断网，也不验证第三方构建可重现性；实验入口尚不支持完整复算。" } else { "此命令核验工件摘要、RunManifest 身份与输入数据指纹；未启用 --recompute 时不声称独立复算 result_hash。运行配置 SHA-256 已核验；策略运行的路径解析可能使搬运后的配置指纹无法直接与来源 config_hash 对比。" }
        }))?;
    }
    Ok(())
}

fn verify_multi_market_strategy_diagnostic_array(
    root: &Path,
    summary_path: &Path,
    dataset: &qx_spec::RunEvidenceDataset,
    diagnostics: &serde_json::Value,
) -> Result<(), String> {
    let items = diagnostics
        .as_array()
        .filter(|items| !items.is_empty() && items.len() <= 512)
        .ok_or("多市场 EvidenceBundle 策略诊断必须是 1..=512 项 JSON 数组")?;
    let summary: serde_json::Value = serde_json::from_slice(
        &std::fs::read(summary_path)
            .map_err(|error| format!("读取多市场诊断绑定摘要失败: {error}"))?,
    )
    .map_err(|error| format!("多市场诊断绑定摘要 JSON 无效: {error}"))?;
    let markets = summary
        .pointer("/spec/markets")
        .and_then(serde_json::Value::as_array)
        .ok_or("多市场诊断绑定摘要缺少 spec.markets")?;
    if markets.len() != dataset.components.len() {
        return Err("多市场诊断 spec 市场数与 EvidenceBundle 组件数不一致".into());
    }
    let mut seen = std::collections::BTreeSet::new();
    for diagnostic in items {
        let strategy_id = diagnostic
            .get("strategy_id")
            .and_then(serde_json::Value::as_str)
            .ok_or("多市场 EvidenceBundle 策略诊断缺少 strategy_id")?;
        let instrument = diagnostic
            .get("instrument")
            .and_then(serde_json::Value::as_str)
            .ok_or("多市场 EvidenceBundle 策略诊断缺少 instrument")?;
        if !seen.insert((instrument, strategy_id)) {
            return Err("多市场 EvidenceBundle 策略诊断包含重复策略/标的身份".into());
        }
        let configured = markets
            .iter()
            .find(|market| market["instrument"].as_str() == Some(instrument))
            .ok_or_else(|| format!("多市场诊断标的不在运行 spec 中: {instrument}"))?;
        let declared = configured["strategies"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|member| member["strategy"]["strategy_id"].as_str() == Some(strategy_id));
        if !declared {
            return Err(format!(
                "{instrument} EvidenceBundle 诊断不属于声明的策略槽"
            ));
        }
        let component = dataset
            .components
            .iter()
            .find(|component| component.role == format!("market:{instrument}"))
            .ok_or_else(|| format!("EvidenceBundle 缺少 {instrument} BarFrame 组件"))?;
        let frame_path = resolve_reference(root, &component.path)?;
        let frame = read_bar_frame_for_backtest(&frame_path)?;
        let fingerprint = qx_data::fingerprint_bar_frame(&frame)
            .map_err(|error| format!("重算 {instrument} BarFrame 指纹失败: {error}"))?;
        if frame.instrument.to_string() != instrument
            || frame.ts.len() as u64 != component.row_count
            || fingerprint != component.content_fingerprint
        {
            return Err(format!(
                "{instrument} EvidenceBundle BarFrame 组件身份不一致"
            ));
        }
        crate::report_command::validate_strategy_diagnostics_identity(
            diagnostic,
            strategy_id,
            instrument,
            &fingerprint,
        )?;
        if diagnostic["input_bars"].as_u64() != Some(component.row_count)
            || diagnostic.get("input_events").is_some()
            || !diagnostic
                .get("input_kind")
                .is_none_or(|kind| kind == "bar")
            || diagnostic["as_of"].as_u64() != frame.ts.last().copied()
        {
            return Err(format!(
                "{instrument} EvidenceBundle 诊断的 Bar 数量、输入形态或结束时点不匹配"
            ));
        }
    }
    Ok(())
}
