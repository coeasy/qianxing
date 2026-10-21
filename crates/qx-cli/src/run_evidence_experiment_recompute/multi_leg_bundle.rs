use super::{
    check_report_identity, compare_multi_leg_experiment, copy_to, ensure_matching_build,
    file_sha256, manifest_path, package_path, read_and_verify_bundle, source_file,
    spec_fingerprint, with_isolated_output, write_json, BundleFile,
    ExperimentEvidenceBundleManifest, PersistedExperimentInput, BUNDLE_FILE, BUNDLE_SCHEMA_VERSION,
};
use serde_json::Value;
use std::path::{Path, PathBuf};

fn identity_fingerprint(input: &PersistedExperimentInput) -> Result<String, String> {
    let spec = qx_app::RunMultiLegExperimentSpec::from_json(
        &serde_json::to_string(&input.spec).map_err(|error| error.to_string())?,
    )
    .map_err(|error| format!("多腿实验身份规格无效: {error}"))?;
    Ok(super::spec_fingerprint(
        &spec.to_json().map_err(|error| error.to_string())?,
        input.strategy_identity.as_ref(),
    ))
}

fn artifact_name(path: &str, ordinal: usize) -> Result<PathBuf, String> {
    let source = Path::new(path);
    let extension = source
        .extension()
        .and_then(|value| value.to_str())
        .filter(|value| value.chars().all(|ch| ch.is_ascii_alphanumeric()))
        .unwrap_or("json");
    Ok(PathBuf::from(format!(
        "artifacts/candidate-{ordinal:04}.{extension}"
    )))
}

pub(crate) fn pack_multi_leg_experiment(
    report_path: &Path,
    output_dir: &Path,
) -> Result<(), String> {
    if output_dir.exists() {
        return Err(format!(
            "实验包输出目录已存在，不覆盖: {}",
            output_dir.display()
        ));
    }
    let report_path = report_path
        .canonicalize()
        .map_err(|error| format!("解析多腿实验报告失败 {}: {error}", report_path.display()))?;
    let (mut report, input) = super::read_experiment(&report_path, ".multi-leg-experiment.json")?;
    if input.schema_version != 1 {
        return Err(format!(
            "不支持的实验 sidecar schema_version={}",
            input.schema_version
        ));
    }
    if input.strategy_identity.is_some() {
        return Err(
            "多腿实验含外部策略身份；缺少可搬运的第三方构建材料，拒绝打包为 builtin 证据".into(),
        );
    }
    super::ensure_matching_build(&input)?;
    let fingerprint = identity_fingerprint(&input)?;
    let spec = qx_app::RunMultiLegExperimentSpec::from_json(
        &serde_json::to_string(&input.spec).map_err(|error| error.to_string())?,
    )
    .map_err(|error| format!("多腿实验规格无效: {error}"))?;
    if report.get("experiment_id").and_then(Value::as_str) != Some(spec.experiment_id.as_str())
        || report.get("spec_fingerprint").and_then(Value::as_str) != Some(fingerprint.as_str())
    {
        return Err("多腿实验报告与 sidecar 规格身份不一致，拒绝打包".into());
    }
    let total = report.get("total_candidates").and_then(Value::as_u64);
    let completed = report.get("completed_candidates").and_then(Value::as_u64);
    if total.is_none() || completed != total {
        return Err("多腿实验候选矩阵不完整，拒绝打包".into());
    }
    let original_spec_path = report_path.with_file_name(format!(
        "{}.spec.json",
        report_path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("实验报告文件名不是 UTF-8")?
            .trim_end_matches(".json")
    ));
    let primary_source = source_file(&spec.base.primary_bars_path)?;
    let reference_source = source_file(&spec.base.reference_bars_path)?;
    let primary_ext = primary_source
        .extension()
        .and_then(|v| v.to_str())
        .unwrap_or("data");
    let reference_ext = reference_source
        .extension()
        .and_then(|v| v.to_str())
        .unwrap_or("data");
    let primary_relative = format!("inputs/primary.{primary_ext}");
    let reference_relative = format!("inputs/reference.{reference_ext}");

    std::fs::create_dir(output_dir)
        .map_err(|error| format!("创建实验包目录失败 {}: {error}", output_dir.display()))?;
    let result = (|| {
        let report_out = output_dir.join("experiment.json");
        let spec_out = output_dir.join("experiment.spec.json");
        let files = report
            .get_mut("candidates")
            .and_then(Value::as_array_mut)
            .ok_or("多腿实验报告缺少 candidates")?;
        let mut manifest_files = Vec::new();
        manifest_files.push(copy_to(&primary_source, output_dir, &primary_relative)?);
        manifest_files.push(copy_to(&reference_source, output_dir, &reference_relative)?);
        manifest_files.push(copy_to(
            &original_spec_path,
            output_dir,
            "spec.identity.json",
        )?);

        let mut portable_spec = spec.clone();
        portable_spec.base.primary_bars_path = primary_relative;
        portable_spec.base.reference_bars_path = reference_relative;
        portable_spec.base.output_dir = "runs".into();
        write_json(&spec_out, &portable_spec)?;

        for (index, candidate) in files.iter_mut().enumerate() {
            let Some(outcome) = candidate.get_mut("outcome").and_then(Value::as_object_mut) else {
                continue;
            };
            let artifact = outcome
                .get("attribution_artifact")
                .and_then(Value::as_str)
                .ok_or_else(|| format!("成功候选 {} 缺少归因产物路径", index + 1))?;
            let source = source_file(artifact)?;
            let artifact_value: Value =
                serde_json::from_slice(&std::fs::read(&source).map_err(|error| error.to_string())?)
                    .map_err(|error| format!("候选 {} 归因产物 JSON 无效: {error}", index + 1))?;
            if artifact_value
                .get("strategy_source_sha256")
                .and_then(Value::as_str)
                .is_some_and(|hash| !hash.is_empty())
            {
                return Err(format!(
                    "候选 {} 归因产物含外部策略身份，缺少第三方构建材料，拒绝打包",
                    index + 1
                ));
            }
            let relative = artifact_name(artifact, index + 1)?;
            let relative_text = relative.to_string_lossy().replace('\\', "/");
            manifest_files.push(copy_to(&source, output_dir, &relative_text)?);
            outcome.insert("attribution_artifact".into(), Value::String(relative_text));
        }
        if let Some(object) = report.as_object_mut() {
            object.insert(
                "artifact_path".into(),
                Value::String("experiment.json".into()),
            );
        }
        write_json(&report_out, &report)?;
        manifest_files.push(BundleFile {
            role: "experiment.json".into(),
            path: "experiment.json".into(),
            sha256: file_sha256(&report_out)?,
        });
        manifest_files.push(BundleFile {
            role: "experiment.spec.json".into(),
            path: "experiment.spec.json".into(),
            sha256: file_sha256(&spec_out)?,
        });
        // `spec.identity.json` is the original canonical specification used to bind the report.
        let bundle = ExperimentEvidenceBundleManifest {
            schema_version: BUNDLE_SCHEMA_VERSION,
            kind: "multi-leg-experiment".into(),
            experiment_id: spec.experiment_id.clone(),
            code_commit: input.code_commit,
            spec_fingerprint: fingerprint,
            files: manifest_files,
        };
        write_json(&output_dir.join(BUNDLE_FILE), &bundle)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_dir_all(output_dir);
    }
    result
}

pub(crate) fn verify_multi_leg_experiment_bundle(
    root: &Path,
    recompute: bool,
) -> Result<(), String> {
    let bundle = read_and_verify_bundle(root)?;
    if bundle.kind != "multi-leg-experiment" {
        return Err("该实验包不是 multi-leg-experiment 类型".into());
    }
    let report_path = package_path(root, manifest_path(&bundle, "experiment.json")?)?;
    let identity_path = package_path(root, manifest_path(&bundle, "spec.identity.json")?)?;
    let portable_spec_path = package_path(root, manifest_path(&bundle, "experiment.spec.json")?)?;
    let report: Value =
        serde_json::from_slice(&std::fs::read(&report_path).map_err(|e| e.to_string())?)
            .map_err(|error| format!("实验报告 JSON 无效: {error}"))?;
    let identity: PersistedExperimentInput =
        serde_json::from_slice(&std::fs::read(&identity_path).map_err(|e| e.to_string())?)
            .map_err(|error| format!("实验身份规格 JSON 无效: {error}"))?;
    let portable_spec: qx_app::RunMultiLegExperimentSpec =
        serde_json::from_slice(&std::fs::read(&portable_spec_path).map_err(|e| e.to_string())?)
            .map_err(|error| format!("可搬运实验规格 JSON 无效: {error}"))?;
    if identity.code_commit != bundle.code_commit
        || report.get("experiment_id").and_then(Value::as_str)
            != Some(bundle.experiment_id.as_str())
        || report.get("spec_fingerprint").and_then(Value::as_str)
            != Some(bundle.spec_fingerprint.as_str())
        || identity_fingerprint(&identity)? != bundle.spec_fingerprint
    {
        return Err("实验包报告、sidecar 与清单身份不一致".into());
    }
    let file_paths = bundle
        .files
        .iter()
        .map(|file| file.path.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let primary = manifest_path(&bundle, "inputs/primary.")?;
    let reference = manifest_path(&bundle, "inputs/reference.")?;
    if !file_paths.contains(primary)
        || !file_paths.contains(reference)
        || portable_spec.base.primary_bars_path != primary
        || portable_spec.base.reference_bars_path != reference
        || portable_spec.experiment_id != bundle.experiment_id
    {
        return Err("实验包可搬运规格与双腿输入组件不一致".into());
    }
    let candidates = report
        .get("candidates")
        .and_then(Value::as_array)
        .ok_or("实验报告缺少 candidates")?;
    for candidate in candidates {
        if let Some(reference) = candidate
            .pointer("/outcome/attribution_artifact")
            .and_then(Value::as_str)
        {
            if !file_paths.contains(reference) {
                return Err(format!("实验包未登记候选归因产物: {reference}"));
            }
        }
    }
    if recompute {
        let primary = manifest_path(&bundle, "inputs/primary.")?;
        let reference = manifest_path(&bundle, "inputs/reference.")?;
        recompute_multi_leg_experiment_portable(
            &report_path,
            &identity_path,
            &portable_spec_path,
            root,
            primary,
            reference,
        )?;
    }
    println!(
        "[qx-cli · CLI] 多腿实验包已复核: {} 个文件，experiment_id={}{}",
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

fn recompute_multi_leg_experiment_portable(
    report_path: &Path,
    identity_path: &Path,
    portable_spec_path: &Path,
    root: &Path,
    primary_input: &str,
    reference_input: &str,
) -> Result<(), String> {
    let report: Value = serde_json::from_slice(
        &std::fs::read(report_path).map_err(|error| format!("读取实验报告失败: {error}"))?,
    )
    .map_err(|error| format!("实验报告 JSON 无效: {error}"))?;
    let identity: PersistedExperimentInput = serde_json::from_slice(
        &std::fs::read(identity_path).map_err(|error| format!("读取实验身份规格失败: {error}"))?,
    )
    .map_err(|error| format!("实验身份规格 JSON 无效: {error}"))?;
    if identity.strategy_identity.is_some() {
        return Err("实验绑定外部策略，缺少第三方构建材料，拒绝 builtin 代替复算".into());
    }
    ensure_matching_build(&identity)?;
    let fingerprint = spec_fingerprint(
        &qx_app::RunMultiLegExperimentSpec::from_json(
            &serde_json::to_string(&identity.spec).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_json())?
        .to_json()
        .map_err(|error| error.to_string())?,
        None,
    );
    let identity_spec: qx_app::RunMultiLegExperimentSpec = serde_json::from_value(identity.spec)
        .map_err(|error| format!("实验身份规格无效: {error}"))?;
    check_report_identity(&report, &identity_spec.experiment_id, &fingerprint)?;

    let mut spec: qx_app::RunMultiLegExperimentSpec = serde_json::from_slice(
        &std::fs::read(portable_spec_path)
            .map_err(|error| format!("读取可搬运实验规格失败: {error}"))?,
    )
    .map_err(|error| format!("可搬运实验规格无效: {error}"))?;
    if spec.experiment_id != identity_spec.experiment_id {
        return Err("可搬运规格 experiment_id 与身份规格不一致".into());
    }
    let resolve = |reference: &str| -> Result<String, String> {
        let path = root.join(reference);
        path.canonicalize()
            .map(|path| path.to_string_lossy().to_string())
            .map_err(|error| format!("解析打包输入失败 {}: {error}", path.display()))
    };
    spec.base.primary_bars_path = resolve(primary_input)?;
    spec.base.reference_bars_path = resolve(reference_input)?;
    let mut portable_report = report;
    let report_root = report_path.parent().unwrap_or_else(|| Path::new("."));
    if let Some(candidates) = portable_report
        .get_mut("candidates")
        .and_then(Value::as_array_mut)
    {
        for candidate in candidates {
            let Some(outcome) = candidate.get_mut("outcome").and_then(Value::as_object_mut) else {
                continue;
            };
            let Some(reference) = outcome.get("attribution_artifact").and_then(Value::as_str)
            else {
                continue;
            };
            if Path::new(reference).is_relative() {
                let path = report_root
                    .join(reference)
                    .canonicalize()
                    .map_err(|error| format!("解析打包归因产物失败 {reference}: {error}"))?;
                outcome.insert(
                    "attribution_artifact".into(),
                    Value::String(path.to_string_lossy().to_string()),
                );
            }
        }
    }
    with_isolated_output(|output| {
        spec.base.output_dir = output.to_string_lossy().into_owned();
        let context = qx_app::RunContext::new(
            qx_app::CallerCapability::Research,
            spec.experiment_id.clone(),
        )
        .with_code_commit(crate::build_identity::BUILD_REVISION);
        let rerun = qx_app::run_multi_leg_experiment(&spec, &context)
            .map_err(|error| error.to_json())
            .and_then(|result| serde_json::to_value(result).map_err(|error| error.to_string()))?;
        compare_multi_leg_experiment(&portable_report, &rerun)
    })
}
