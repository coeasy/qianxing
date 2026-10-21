use super::super::{
    copy_to, file_sha256, source_file, write_json, BundleFile, ExperimentEvidenceBundleManifest,
    BUNDLE_FILE, BUNDLE_SCHEMA_VERSION,
};
use super::{candidate_path, fields, input_role, input_suffix, source_reference, spec_fingerprint};
use serde_json::Value;
use std::path::Path;

fn write_package(report_path: &Path, output_dir: &Path, depth: bool) -> Result<(), String> {
    if output_dir.exists() {
        return Err(format!(
            "实验包输出目录已存在，不覆盖: {}",
            output_dir.display()
        ));
    }
    let report_path = report_path
        .canonicalize()
        .map_err(|error| format!("解析多市场实验报告失败 {}: {error}", report_path.display()))?;
    let suffix = input_suffix(depth);
    let (mut report, input) = super::super::read_experiment(&report_path, suffix)?;
    if input.schema_version != 1 {
        return Err(format!(
            "不支持的实验 sidecar schema_version={}",
            input.schema_version
        ));
    }
    if input.strategy_identity.is_some() {
        return Err("实验绑定外部策略；缺少第三方构建材料，拒绝打包为 builtin 证据".into());
    }
    super::super::ensure_matching_build(&input)?;
    let fingerprint = spec_fingerprint(&input, depth)?;
    let mut portable_spec = input.spec.clone();
    let spec_obj = portable_spec
        .as_object_mut()
        .ok_or("多市场实验规格必须是 JSON 对象")?;
    let experiment_id = spec_obj
        .get("experiment_id")
        .and_then(Value::as_str)
        .ok_or("多市场实验规格缺少 experiment_id")?
        .to_string();
    let base = spec_obj
        .get_mut("base")
        .and_then(Value::as_object_mut)
        .ok_or("多市场实验规格缺少 base")?;
    let base_output = base
        .get_mut("output_dir")
        .ok_or("多市场实验规格缺少 base.output_dir")?;
    *base_output = Value::String("runs".into());
    let (path_field, kind) = fields(depth);
    let markets = base
        .get_mut("markets")
        .and_then(Value::as_array_mut)
        .ok_or("多市场实验规格缺少 markets")?;
    if markets.is_empty() {
        return Err("多市场实验规格 markets 不能为空".into());
    }
    let mut input_sources = Vec::with_capacity(markets.len() * 2);
    for (index, market) in markets.iter_mut().enumerate() {
        let market = market
            .as_object_mut()
            .ok_or("多市场规格项必须是 JSON 对象")?;
        let instrument = market
            .get("instrument")
            .and_then(Value::as_str)
            .ok_or("多市场规格项缺少 instrument")?
            .to_string();
        let source = market
            .get(path_field)
            .and_then(Value::as_str)
            .ok_or_else(|| format!("市场 {instrument} 缺少 {path_field}"))?;
        let source = source_file(source)?;
        let extension = source
            .extension()
            .and_then(|extension| extension.to_str())
            .filter(|extension| extension.chars().all(|ch| ch.is_ascii_alphanumeric()))
            .unwrap_or("data");
        let relative = format!("inputs/market-{:02}.{extension}", index + 1);
        market.insert(path_field.into(), Value::String(relative.clone()));
        input_sources.push((input_role(&instrument), source, relative.clone()));
        if let Some(manifest_path) = market
            .get("dataset_manifest_path")
            .and_then(Value::as_str)
            .map(str::to_string)
        {
            let manifest_source = source_file(&manifest_path)?;
            let manifest_extension = manifest_source
                .extension()
                .and_then(|extension| extension.to_str())
                .filter(|extension| extension.chars().all(|ch| ch.is_ascii_alphanumeric()))
                .unwrap_or("json");
            let manifest_relative = format!(
                "inputs/market-{:02}-quality.{manifest_extension}",
                index + 1
            );
            market.insert(
                "dataset_manifest_path".into(),
                Value::String(manifest_relative.clone()),
            );
            input_sources.push((
                format!("market-quality:{instrument}"),
                manifest_source,
                manifest_relative,
            ));
        }
    }
    if report.get("experiment_id").and_then(Value::as_str) != Some(experiment_id.as_str())
        || report.get("spec_fingerprint").and_then(Value::as_str) != Some(fingerprint.as_str())
    {
        return Err("多市场实验报告与 sidecar 规格身份不一致，拒绝打包".into());
    }
    let total = report.get("total_candidates").and_then(Value::as_u64);
    let completed = report.get("completed_candidates").and_then(Value::as_u64);
    if total.is_none() || completed != total {
        return Err("多市场实验候选矩阵不完整，拒绝打包".into());
    }
    let candidates = report
        .get("candidates")
        .and_then(Value::as_array)
        .ok_or("多市场实验报告缺少 candidates")?;
    if Some(candidates.len() as u64) != total {
        return Err("多市场实验 candidates 数量与 total_candidates 不匹配".into());
    }
    let name = report_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("多市场实验报告文件名不是 UTF-8")?;
    let original_spec_path =
        report_path.with_file_name(name.replace(suffix, &suffix.replace(".json", ".spec.json")));
    std::fs::create_dir(output_dir)
        .map_err(|error| format!("创建实验包目录失败 {}: {error}", output_dir.display()))?;
    let result = (|| {
        let mut files = Vec::new();
        for (role, source, relative) in &input_sources {
            let mut file = copy_to(source, output_dir, relative)?;
            file.role = role.clone();
            files.push(file);
        }
        files.push(copy_to(
            &original_spec_path,
            output_dir,
            "spec.identity.json",
        )?);
        let portable_spec_path = output_dir.join("experiment.spec.json");
        write_json(&portable_spec_path, &portable_spec)?;
        files.push(BundleFile {
            role: "experiment.spec.json".into(),
            path: "experiment.spec.json".into(),
            sha256: file_sha256(&portable_spec_path)?,
        });

        {
            let candidates = report
                .get_mut("candidates")
                .and_then(Value::as_array_mut)
                .ok_or("多市场实验报告缺少 candidates")?;
            for (index, candidate) in candidates.iter_mut().enumerate() {
                let Some(outcome) = candidate.get_mut("outcome").and_then(Value::as_object_mut)
                else {
                    continue;
                };
                let source_ref = outcome
                    .get("artifact_path")
                    .and_then(Value::as_str)
                    .ok_or_else(|| format!("成功候选 {} 缺少 artifact_path", index + 1))?;
                let source = source_reference(
                    source_ref,
                    report_path.parent().unwrap_or_else(|| Path::new(".")),
                )?;
                let mut summary: Value = serde_json::from_slice(
                    &std::fs::read(&source).map_err(|error| error.to_string())?,
                )
                .map_err(|error| format!("候选 {} 产物 JSON 无效: {error}", index + 1))?;
                if summary
                    .get("strategy_identities")
                    .and_then(Value::as_array)
                    .is_some_and(|identities| !identities.is_empty())
                {
                    return Err(format!(
                        "候选 {} 使用外部策略，拒绝 builtin 实验包",
                        index + 1
                    ));
                }
                let manifest_ref = summary
                    .get("run_manifest_path")
                    .and_then(Value::as_str)
                    .ok_or_else(|| format!("候选 {} 产物缺少 run_manifest_path", index + 1))?;
                let manifest_source = source_reference(
                    manifest_ref,
                    source.parent().unwrap_or_else(|| Path::new(".")),
                )?;
                let manifest: qx_core::RunManifest = qx_core::RunManifest::from_json(
                    &std::fs::read_to_string(&manifest_source).map_err(|error| {
                        format!("读取候选 {} RunManifest 失败: {error}", index + 1)
                    })?,
                )?;
                if manifest.code_commit != input.code_commit {
                    return Err(format!(
                        "候选 {} RunManifest code_commit 与实验 sidecar 不一致",
                        index + 1
                    ));
                }
                super::super::super::verify_candidate_replay(&summary, &manifest)?;
                let manifest_relative = candidate_path("manifest", index + 1);
                let summary_relative = candidate_path("summary", index + 1);
                let manifest_target_name = Path::new(&manifest_relative)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .ok_or("候选 Manifest 包内文件名非法")?;
                summary
                    .as_object_mut()
                    .ok_or("候选运行产物必须是 JSON 对象")?
                    .insert(
                        "run_manifest_path".into(),
                        Value::String(manifest_target_name.into()),
                    );
                let summary_target = output_dir.join(&summary_relative);
                if let Some(parent) = summary_target.parent() {
                    std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
                }
                write_json(&summary_target, &summary)?;
                let manifest_file = copy_to(&manifest_source, output_dir, &manifest_relative)?;
                let summary_file = BundleFile {
                    role: format!("candidate-summary:{}", index + 1),
                    path: summary_relative.clone(),
                    sha256: file_sha256(&summary_target)?,
                };
                files.push(summary_file);
                files.push(BundleFile {
                    role: format!("candidate-manifest:{}", index + 1),
                    ..manifest_file
                });
                outcome.insert("artifact_path".into(), Value::String(summary_relative));
            }
        }
        if let Some(object) = report.as_object_mut() {
            object.insert(
                "artifact_path".into(),
                Value::String("experiment.json".into()),
            );
        }
        let report_target = output_dir.join("experiment.json");
        write_json(&report_target, &report)?;
        files.push(BundleFile {
            role: "experiment.json".into(),
            path: "experiment.json".into(),
            sha256: file_sha256(&report_target)?,
        });
        let manifest = ExperimentEvidenceBundleManifest {
            schema_version: BUNDLE_SCHEMA_VERSION,
            kind: kind.into(),
            experiment_id,
            code_commit: input.code_commit,
            spec_fingerprint: fingerprint,
            files,
        };
        write_json(&output_dir.join(BUNDLE_FILE), &manifest)
    })();
    if result.is_err() {
        let _ = std::fs::remove_dir_all(output_dir);
    }
    result
}

pub(crate) fn pack_multi_market_experiment(
    report_path: &Path,
    output_dir: &Path,
    depth: bool,
) -> Result<(), String> {
    write_package(report_path, output_dir, depth)
}
