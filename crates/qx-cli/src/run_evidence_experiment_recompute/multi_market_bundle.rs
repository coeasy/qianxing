use super::super::{with_isolated_output, PersistedExperimentInput};
use serde_json::Value;
use std::path::{Path, PathBuf};
#[path = "multi_market_pack.rs"]
mod multi_market_pack;
pub(crate) use multi_market_pack::pack_multi_market_experiment;

fn spec_fingerprint(input: &PersistedExperimentInput, depth: bool) -> Result<String, String> {
    let canonical = if depth {
        qx_app::RunMultiMarketDepthExperimentSpec::from_json(
            &serde_json::to_string(&input.spec).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_json())?
        .to_json()
        .map_err(|error| error.to_json())?
    } else {
        qx_app::RunMultiMarketExperimentSpec::from_json(
            &serde_json::to_string(&input.spec).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_json())?
        .to_json()
        .map_err(|error| error.to_json())?
    };
    Ok(super::super::spec_fingerprint(
        &canonical,
        input.strategy_identity.as_ref(),
    ))
}

fn input_suffix(depth: bool) -> &'static str {
    if depth {
        ".multi-market-depth-experiment.json"
    } else {
        ".multi-market-experiment.json"
    }
}

fn input_role(instrument: &str) -> String {
    format!("input:market:{instrument}")
}

fn candidate_path(role: &str, ordinal: usize) -> String {
    format!("artifacts/candidate-{ordinal:04}.{role}.json")
}

fn source_reference(reference: &str, base: &Path) -> Result<PathBuf, String> {
    let path = Path::new(reference);
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    };
    path.canonicalize()
        .map_err(|error| format!("解析实验产物引用失败 {}: {error}", path.display()))
}

fn fields(depth: bool) -> (&'static str, &'static str) {
    if depth {
        ("depth_path", "multi-market-depth-experiment")
    } else {
        ("bars_path", "multi-market-experiment")
    }
}

pub(crate) fn verify_multi_market_experiment_bundle(
    root: &Path,
    recompute: bool,
) -> Result<(), String> {
    let bundle = super::read_and_verify_bundle(root)?;
    let depth = match bundle.kind.as_str() {
        "multi-market-experiment" => false,
        "multi-market-depth-experiment" => true,
        _ => return Err("该实验包不是 multi-market experiment 类型".into()),
    };
    let report_path = super::package_path(root, super::manifest_path(&bundle, "experiment.json")?)?;
    let identity_path =
        super::package_path(root, super::manifest_path(&bundle, "spec.identity.json")?)?;
    let spec_path =
        super::package_path(root, super::manifest_path(&bundle, "experiment.spec.json")?)?;
    let report: Value =
        serde_json::from_slice(&std::fs::read(&report_path).map_err(|e| e.to_string())?)
            .map_err(|error| format!("多市场实验报告 JSON 无效: {error}"))?;
    let input: PersistedExperimentInput =
        serde_json::from_slice(&std::fs::read(&identity_path).map_err(|e| e.to_string())?)
            .map_err(|error| format!("多市场实验身份规格无效: {error}"))?;
    let spec: Value =
        serde_json::from_slice(&std::fs::read(&spec_path).map_err(|e| e.to_string())?)
            .map_err(|error| format!("多市场实验可搬运规格无效: {error}"))?;
    if input.strategy_identity.is_some() {
        return Err("实验包绑定外部策略，缺少第三方构建材料".into());
    }
    super::super::ensure_matching_build(&input)?;
    let fingerprint = spec_fingerprint(&input, depth)?;
    if input.code_commit != bundle.code_commit
        || fingerprint != bundle.spec_fingerprint
        || report.get("spec_fingerprint").and_then(Value::as_str) != Some(fingerprint.as_str())
        || report.get("experiment_id").and_then(Value::as_str)
            != Some(bundle.experiment_id.as_str())
    {
        return Err("多市场实验包报告、sidecar 与清单身份不一致".into());
    }
    super::super::check_report_identity(&report, &bundle.experiment_id, &fingerprint)?;
    let (path_field, _) = fields(depth);
    let mut expected_spec = input.spec.clone();
    let expected_base = expected_spec
        .get_mut("base")
        .and_then(Value::as_object_mut)
        .ok_or("身份规格缺少 base")?;
    expected_base.insert("output_dir".into(), Value::String("runs".into()));
    let expected_markets = expected_base
        .get_mut("markets")
        .and_then(Value::as_array_mut)
        .ok_or("身份规格缺少 markets")?;
    let markets = spec
        .pointer("/base/markets")
        .and_then(Value::as_array)
        .ok_or("可搬运规格缺少 markets")?;
    if expected_markets.len() != markets.len() {
        return Err("可搬运规格市场数与身份规格不一致".into());
    }
    for (index, market) in markets.iter().enumerate() {
        let instrument = market
            .get("instrument")
            .and_then(Value::as_str)
            .ok_or("可搬运市场缺少 instrument")?;
        let relative = market
            .get(path_field)
            .and_then(Value::as_str)
            .ok_or("可搬运市场缺少输入路径")?;
        let file = bundle
            .files
            .iter()
            .find(|file| file.role == input_role(instrument))
            .ok_or_else(|| format!("实验包缺少市场输入: {instrument}"))?;
        if file.path != relative
            || !relative.starts_with(&format!("inputs/market-{:02}.", index + 1))
        {
            return Err(format!("市场 {instrument} 的规格路径与包内输入不一致"));
        }
        expected_markets[index]
            .as_object_mut()
            .ok_or("身份规格市场项必须是 JSON 对象")?
            .insert(path_field.into(), Value::String(relative.into()));
        super::package_path(root, relative)?;
        let quality_relative = market.get("dataset_manifest_path").and_then(Value::as_str);
        let expected_market = expected_markets[index]
            .as_object_mut()
            .ok_or("身份规格市场项必须是 JSON 对象")?;
        match quality_relative {
            Some(relative) => {
                let quality_role = format!("market-quality:{instrument}");
                let quality_file = bundle
                    .files
                    .iter()
                    .find(|file| file.role == quality_role)
                    .ok_or_else(|| format!("实验包缺少市场 {instrument} 质量清单"))?;
                if quality_file.path != relative
                    || !relative.starts_with(&format!("inputs/market-{:02}-quality.", index + 1))
                {
                    return Err(format!("市场 {instrument} 质量清单路径与包内登记不一致"));
                }
                expected_market.insert(
                    "dataset_manifest_path".into(),
                    Value::String(relative.into()),
                );
                super::package_path(root, relative)?;
            }
            None if expected_market.contains_key("dataset_manifest_path") => {
                return Err(format!(
                    "市场 {instrument} 身份规格含质量清单但可搬运规格缺失"
                ));
            }
            None => {}
        }
    }
    if expected_spec != spec {
        return Err("可搬运实验规格与身份规格除输入路径/output_dir 外存在差异".into());
    }
    let candidates = report
        .get("candidates")
        .and_then(Value::as_array)
        .ok_or("实验报告缺少 candidates")?;
    if report.get("total_candidates").and_then(Value::as_u64) != Some(candidates.len() as u64) {
        return Err("多市场实验 candidates 数量与 total_candidates 不匹配".into());
    }
    let mut resolved_report = report.clone();
    let resolved_candidates = resolved_report
        .get_mut("candidates")
        .and_then(Value::as_array_mut)
        .ok_or("实验报告缺少 candidates")?;
    for (index, candidate) in candidates.iter().enumerate() {
        let Some(reference) = candidate
            .pointer("/outcome/artifact_path")
            .and_then(Value::as_str)
        else {
            continue;
        };
        let summary_role = format!("candidate-summary:{}", index + 1);
        if !bundle
            .files
            .iter()
            .any(|file| file.role == summary_role && file.path == reference)
        {
            return Err(format!("候选 {} 摘要路径与实验包清单不一致", index + 1));
        }
        let summary_path = super::package_path(root, reference)?;
        let summary: Value =
            serde_json::from_slice(&std::fs::read(&summary_path).map_err(|e| e.to_string())?)
                .map_err(|error| format!("候选 {} 运行产物 JSON 无效: {error}", index + 1))?;
        let manifest_ref = summary
            .get("run_manifest_path")
            .and_then(Value::as_str)
            .ok_or("候选产物缺少 run_manifest_path")?;
        let manifest_ref_path = Path::new(manifest_ref);
        if manifest_ref_path.is_absolute()
            || manifest_ref_path.components().count() != 1
            || manifest_ref_path.file_name().and_then(|name| name.to_str())
                != manifest_ref_path.to_str()
        {
            return Err(format!(
                "候选 RunManifest 路径必须是包内文件名: {manifest_ref}"
            ));
        }
        let manifest_role = format!("candidate-manifest:{}", index + 1);
        let manifest_entry = bundle
            .files
            .iter()
            .find(|file| file.role == manifest_role)
            .ok_or_else(|| format!("实验包缺少候选 RunManifest: {}", index + 1))?;
        let expected_manifest = super::package_path(root, &manifest_entry.path)?;
        let referenced_manifest = summary_path
            .parent()
            .unwrap_or(root)
            .join(manifest_ref_path)
            .canonicalize()
            .map_err(|error| format!("解析候选 RunManifest 失败: {error}"))?;
        if referenced_manifest != expected_manifest {
            return Err(format!(
                "候选 {} RunManifest 路径与清单登记不一致",
                index + 1
            ));
        }
        let manifest_path = expected_manifest;
        let manifest: qx_core::RunManifest = qx_core::RunManifest::from_json(
            &std::fs::read_to_string(&manifest_path).map_err(|error| error.to_string())?,
        )?;
        if manifest.code_commit != bundle.code_commit {
            return Err(format!(
                "候选 {} RunManifest code_commit 与包清单不一致",
                index + 1
            ));
        }
        super::super::verify_candidate_replay(&summary, &manifest)?;
        let outcome = resolved_candidates[index]
            .get_mut("outcome")
            .and_then(Value::as_object_mut)
            .ok_or("成功候选 outcome 无效")?;
        outcome.insert(
            "artifact_path".into(),
            Value::String(summary_path.to_string_lossy().to_string()),
        );
    }
    if recompute {
        recompute_bundle(root, &spec_path, depth, resolved_report)?;
    }
    println!(
        "[qx-cli · CLI] 多市场实验包已复核: {} 个文件，experiment_id={}{}",
        bundle.files.len(),
        bundle.experiment_id,
        if recompute {
            "，整网格复算一致"
        } else {
            ""
        }
    );
    Ok(())
}

fn recompute_bundle(
    root: &Path,
    portable_spec_path: &Path,
    depth: bool,
    resolved_report: Value,
) -> Result<(), String> {
    let mut spec: Value =
        serde_json::from_slice(&std::fs::read(portable_spec_path).map_err(|e| e.to_string())?)
            .map_err(|error| format!("可搬运实验规格无效: {error}"))?;
    let (path_field, _) = fields(depth);
    let markets = spec
        .pointer_mut("/base/markets")
        .and_then(Value::as_array_mut)
        .ok_or("可搬运规格缺少 markets")?;
    for market in markets {
        let market = market.as_object_mut().ok_or("可搬运市场无效")?;
        let path = market
            .get(path_field)
            .and_then(Value::as_str)
            .ok_or("可搬运市场缺少输入路径")?;
        let absolute = root
            .join(path)
            .canonicalize()
            .map_err(|error| format!("解析复算输入失败 {path}: {error}"))?;
        market.insert(
            path_field.into(),
            Value::String(absolute.to_string_lossy().to_string()),
        );
        if let Some(manifest_path) = market
            .get("dataset_manifest_path")
            .and_then(Value::as_str)
            .map(str::to_string)
        {
            let absolute = root
                .join(&manifest_path)
                .canonicalize()
                .map_err(|error| format!("解析复算质量清单失败 {manifest_path}: {error}"))?;
            market.insert(
                "dataset_manifest_path".into(),
                Value::String(absolute.to_string_lossy().to_string()),
            );
        }
    }
    if depth {
        let mut spec: qx_app::RunMultiMarketDepthExperimentSpec = serde_json::from_value(spec)
            .map_err(|error| format!("可搬运深度实验规格无效: {error}"))?;
        with_isolated_output(|output| {
            spec.base.output_dir = output.to_string_lossy().into_owned();
            let context = qx_app::RunContext::new(
                qx_app::CallerCapability::Research,
                spec.experiment_id.clone(),
            )
            .with_code_commit(crate::build_identity::BUILD_REVISION);
            let rerun = qx_app::run_multi_market_depth_experiment(&spec, &context)
                .map_err(|error| error.to_json())
                .and_then(|result| {
                    serde_json::to_value(result).map_err(|error| error.to_string())
                })?;
            super::super::compare_experiment(&resolved_report, &rerun, "多市场深度", Some(root))
        })
    } else {
        let mut spec: qx_app::RunMultiMarketExperimentSpec = serde_json::from_value(spec)
            .map_err(|error| format!("可搬运 Bar 实验规格无效: {error}"))?;
        with_isolated_output(|output| {
            spec.base.output_dir = output.to_string_lossy().into_owned();
            let context = qx_app::RunContext::new(
                qx_app::CallerCapability::Research,
                spec.experiment_id.clone(),
            )
            .with_code_commit(crate::build_identity::BUILD_REVISION);
            let rerun = qx_app::run_multi_market_experiment(&spec, &context)
                .map_err(|error| error.to_json())
                .and_then(|result| {
                    serde_json::to_value(result).map_err(|error| error.to_string())
                })?;
            super::super::compare_experiment(&resolved_report, &rerun, "多市场 Bar", Some(root))
        })
    }
}
