use super::super::{
    check_report_identity, compare_single_market_experiment, ensure_matching_build,
    read_experiment, spec_fingerprint, with_isolated_output, PersistedExperimentInput,
};
use super::{
    copy_to, file_sha256, manifest_path, package_path, read_and_verify_bundle, write_json,
    BundleFile, BundleReference, ExperimentEvidenceBundleManifest, BUNDLE_FILE,
    BUNDLE_SCHEMA_VERSION,
};
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::BufReader;
use std::path::{Component, Path, PathBuf};

const BAR_KIND: &str = "bar-experiment";
const DEPTH_KIND: &str = "depth-experiment";

pub(crate) fn pack_bar_experiment(report_path: &Path, output_dir: &Path) -> Result<(), String> {
    pack_single_market_experiment(report_path, output_dir, false)
}

pub(crate) fn pack_depth_experiment(report_path: &Path, output_dir: &Path) -> Result<(), String> {
    pack_single_market_experiment(report_path, output_dir, true)
}

pub(crate) fn verify_bar_experiment_bundle(root: &Path, recompute: bool) -> Result<(), String> {
    verify_single_market_experiment_bundle(root, recompute, false)
}

pub(crate) fn verify_depth_experiment_bundle(root: &Path, recompute: bool) -> Result<(), String> {
    verify_single_market_experiment_bundle(root, recompute, true)
}

fn input_suffix(depth: bool) -> &'static str {
    if depth {
        ".depth-experiment.json"
    } else {
        ".experiment.json"
    }
}

fn kind(depth: bool) -> &'static str {
    if depth {
        DEPTH_KIND
    } else {
        BAR_KIND
    }
}

fn experiment_fingerprint(input: &PersistedExperimentInput, depth: bool) -> Result<String, String> {
    let spec_json = input.exact_spec_json()?;
    let fingerprinted = if depth {
        let spec = qx_app::RunDepthExperimentSpec::from_json(&spec_json)
            .map_err(|error| format!("深度实验身份规格无效: {error}"))?;
        spec.to_json().map_err(|error| error.to_json())?
    } else {
        let spec = qx_app::RunExperimentSpec::from_json(&spec_json)
            .map_err(|error| format!("Bar 实验身份规格无效: {error}"))?;
        spec.to_json().map_err(|error| error.to_json())?
    };
    Ok(spec_fingerprint(
        &fingerprinted,
        input.strategy_identity.as_ref(),
    ))
}

fn pack_single_market_experiment(
    report_path: &Path,
    output_dir: &Path,
    depth: bool,
) -> Result<(), String> {
    if output_dir.exists() {
        return Err(format!(
            "实验包输出目录已存在，不覆盖: {}",
            output_dir.display()
        ));
    }
    let report_path = report_path
        .canonicalize()
        .map_err(|error| format!("解析实验报告失败 {}: {error}", report_path.display()))?;
    let suffix = input_suffix(depth);
    let (report, input) = read_experiment(&report_path, suffix)?;
    if input.strategy_identity.is_some() {
        return Err("实验绑定外部策略；缺少第三方构建材料，拒绝打包为 builtin 证据".into());
    }
    ensure_matching_build(&input)?;
    let fingerprint = experiment_fingerprint(&input, depth)?;
    let spec_json = input.exact_spec_json()?;
    let experiment_id = if depth {
        qx_app::RunDepthExperimentSpec::from_json(&spec_json)
            .map_err(|error| error.to_json())?
            .experiment_id
    } else {
        qx_app::RunExperimentSpec::from_json(&spec_json)
            .map_err(|error| error.to_json())?
            .experiment_id
    };
    check_report_identity(&report, &experiment_id, &fingerprint)?;
    validate_parent_report(&report, &input)?;
    qx_app::artifact_store::verify_declared_run_manifest_with_reader(
        &report,
        &qx_app::artifact_store::FileArtifactReader,
    )
    .map_err(|error| format!("实验父 RunManifest 校验失败: {error}"))?;
    qx_app::artifact_store::verify_declared_run_record_with_reader(
        &report,
        &qx_app::artifact_store::FileArtifactReader,
    )
    .map_err(|error| format!("实验父 RunRecord V2 校验失败: {error}"))?;
    verify_outcomes(&report, &qx_app::artifact_store::FileArtifactReader, depth)?;

    let sidecar_name = format!(
        "{}.spec.json",
        report_path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("实验报告文件名不是 UTF-8")?
            .trim_end_matches(".json")
    );
    let sidecar_path = report_path.with_file_name(sidecar_name);
    let sidecar_bytes = std::fs::read(&sidecar_path).map_err(|error| {
        format!(
            "读取实验输入 sidecar 失败 {}: {error}",
            sidecar_path.display()
        )
    })?;

    let mut portable_spec: PortableSpec = if depth {
        let mut spec = qx_app::RunDepthExperimentSpec::from_json(&spec_json)
            .map_err(|error| error.to_json())?;
        spec.base.output_dir = "runs".into();
        spec.into()
    } else {
        let mut spec =
            qx_app::RunExperimentSpec::from_json(&spec_json).map_err(|error| error.to_json())?;
        spec.base.output_dir = "runs".into();
        spec.into()
    };

    let input_sources = if depth {
        let spec = portable_spec
            .as_mut_depth()
            .ok_or("内部实验规格类型不一致")?;
        let relative = input_relative("depth", &spec.base.depth_path);
        let source = resolve_source(&spec.base.depth_path, report_path.parent())?;
        spec.base.depth_path = relative.clone();
        let mut sources = vec![("input:depth", source, relative)];
        if let Some(path) = spec.base.quality_manifest_path.as_deref() {
            let source = resolve_source(path, report_path.parent())?;
            let relative = input_relative("quality-manifest", path);
            spec.base.quality_manifest_path = Some(relative.clone());
            sources.push(("input:quality-manifest", source, relative));
        }
        if let Some(path) = spec.base.runtime_config_path.as_deref() {
            let source = resolve_source(path, report_path.parent())?;
            let relative = input_relative("runtime-config", path);
            spec.base.runtime_config_path = Some(relative.clone());
            sources.push(("input:runtime-config", source, relative));
        }
        if let Some(path) = spec.base.market_spec_path.as_deref() {
            let source = resolve_source(path, report_path.parent())?;
            let relative = input_relative("market-spec", path);
            spec.base.market_spec_path = Some(relative.clone());
            sources.push(("input:market-spec", source, relative));
        }
        sources
    } else {
        let spec = portable_spec.as_mut_bar().ok_or("内部实验规格类型不一致")?;
        let relative = input_relative("bars", &spec.base.bars_path);
        let source = resolve_source(&spec.base.bars_path, report_path.parent())?;
        spec.base.bars_path = relative.clone();
        vec![("input:bars", source, relative)]
    };

    std::fs::create_dir(output_dir)
        .map_err(|error| format!("创建实验包目录失败 {}: {error}", output_dir.display()))?;
    let result = (|| {
        let mut writer = BundleWriter::new(output_dir);
        let report_file = copy_to(&report_path, output_dir, "experiment.json")?;
        writer.cache_source(&report_path, "experiment.json");
        writer.add_file(report_file);
        writer.add_reference(&report_path.to_string_lossy(), "experiment.json")?;
        if let Some(reference) = report.get("artifact_path").and_then(Value::as_str) {
            writer.add_reference(reference, "experiment.json")?;
        }

        let identity_path = output_dir.join("spec.identity.json");
        qx_app::artifact_store::sink::publish_bytes(
            &identity_path,
            &sidecar_bytes,
            qx_app::artifact_store::sink::PublishMode::CreateNew,
        )
        .map_err(|error| format!("写入实验身份 sidecar 失败: {error}"))?;
        writer.add_file(BundleFile {
            role: "spec.identity.json".into(),
            path: "spec.identity.json".into(),
            sha256: file_sha256(&identity_path)?,
        });
        writer.cache_source(&sidecar_path, "spec.identity.json");
        writer.add_reference(&sidecar_path.to_string_lossy(), "spec.identity.json")?;

        for (role, source, relative) in input_sources {
            let file = copy_to(&source, output_dir, &relative)?;
            writer.add_file(BundleFile {
                role: role.into(),
                ..file
            });
            writer.cache_source(&source, &relative);
        }
        let portable_path = output_dir.join("experiment.spec.json");
        write_json(&portable_path, &portable_spec)?;
        writer.add_file(BundleFile {
            role: "experiment.spec.json".into(),
            path: "experiment.spec.json".into(),
            sha256: file_sha256(&portable_path)?,
        });

        copy_parent_references(&report, &report_path, &mut writer)?;
        copy_outcome_artifacts(&report, &report_path, &mut writer)?;

        let bundle = ExperimentEvidenceBundleManifest {
            schema_version: BUNDLE_SCHEMA_VERSION,
            kind: kind(depth).into(),
            experiment_id: experiment_id.clone(),
            code_commit: input.code_commit.clone(),
            spec_fingerprint: fingerprint.clone(),
            files: writer.files,
            references: writer.references,
        };
        write_json(&output_dir.join(BUNDLE_FILE), &bundle)
    })();
    if result.is_err() {
        let _ = std::fs::remove_dir_all(output_dir);
    }
    result
}

enum PortableSpec {
    Bar(qx_app::RunExperimentSpec),
    Depth(qx_app::RunDepthExperimentSpec),
}

impl PortableSpec {
    fn as_mut_bar(&mut self) -> Option<&mut qx_app::RunExperimentSpec> {
        match self {
            Self::Bar(spec) => Some(spec),
            Self::Depth(_) => None,
        }
    }

    fn as_mut_depth(&mut self) -> Option<&mut qx_app::RunDepthExperimentSpec> {
        match self {
            Self::Depth(spec) => Some(spec),
            Self::Bar(_) => None,
        }
    }
}

impl From<qx_app::RunExperimentSpec> for PortableSpec {
    fn from(value: qx_app::RunExperimentSpec) -> Self {
        Self::Bar(value)
    }
}

impl From<qx_app::RunDepthExperimentSpec> for PortableSpec {
    fn from(value: qx_app::RunDepthExperimentSpec) -> Self {
        Self::Depth(value)
    }
}

impl Serialize for PortableSpec {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::Bar(spec) => spec.serialize(serializer),
            Self::Depth(spec) => spec.serialize(serializer),
        }
    }
}

fn verify_single_market_experiment_bundle(
    root: &Path,
    recompute: bool,
    depth: bool,
) -> Result<(), String> {
    let bundle = read_and_verify_bundle(root)?;
    if bundle.kind != kind(depth) {
        return Err("实验包类型与要求的单市场实验类型不一致".into());
    }
    let report_path = package_path(root, manifest_path(&bundle, "experiment.json")?)?;
    let identity_path = package_path(root, manifest_path(&bundle, "spec.identity.json")?)?;
    let portable_spec_path = package_path(root, manifest_path(&bundle, "experiment.spec.json")?)?;
    let report_bytes = std::fs::read(&report_path).map_err(|error| error.to_string())?;
    let report: Value = serde_json::from_slice(&report_bytes)
        .map_err(|error| format!("实验报告 JSON 无效: {error}"))?;
    let identity: PersistedExperimentInput =
        serde_json::from_slice(&std::fs::read(&identity_path).map_err(|error| error.to_string())?)
            .map_err(|error| format!("实验身份规格 JSON 无效: {error}"))?;
    if identity.schema_version != 1 || identity.strategy_identity.is_some() {
        return Err("实验 sidecar 版本不受支持，或外部策略缺少构建材料".into());
    }
    ensure_matching_build(&identity)?;
    let fingerprint = experiment_fingerprint(&identity, depth)?;
    if identity.code_commit != bundle.code_commit
        || fingerprint != bundle.spec_fingerprint
        || report.get("experiment_id").and_then(Value::as_str)
            != Some(bundle.experiment_id.as_str())
    {
        return Err("实验包清单、报告与 sidecar 身份不一致".into());
    }
    check_report_identity(&report, &bundle.experiment_id, &fingerprint)?;
    if depth {
        let original = qx_app::RunDepthExperimentSpec::from_json(&identity.exact_spec_json()?)
            .map_err(|error| format!("深度实验身份规格无效: {error}"))?;
        let portable: qx_app::RunDepthExperimentSpec = serde_json::from_slice(
            &std::fs::read(&portable_spec_path).map_err(|error| error.to_string())?,
        )
        .map_err(|error| format!("深度实验可搬运规格无效: {error}"))?;
        let mut expected = original.clone();
        expected.base.output_dir = "runs".into();
        expected.base.depth_path = manifest_path(&bundle, "input:depth")?.to_string();
        verify_optional_depth_input(
            &bundle,
            &mut expected,
            "quality_manifest_path",
            "input:quality-manifest",
        )?;
        verify_optional_depth_input(
            &bundle,
            &mut expected,
            "runtime_config_path",
            "input:runtime-config",
        )?;
        verify_optional_depth_input(
            &bundle,
            &mut expected,
            "market_spec_path",
            "input:market-spec",
        )?;
        if expected != portable {
            return Err("深度可搬运规格与身份规格除输入路径/output_dir 外存在差异".into());
        }
    } else {
        let original = qx_app::RunExperimentSpec::from_json(&identity.exact_spec_json()?)
            .map_err(|error| format!("Bar 实验身份规格无效: {error}"))?;
        let portable: qx_app::RunExperimentSpec = serde_json::from_slice(
            &std::fs::read(&portable_spec_path).map_err(|error| error.to_string())?,
        )
        .map_err(|error| format!("Bar 实验可搬运规格无效: {error}"))?;
        let mut expected = original.clone();
        expected.base.output_dir = "runs".into();
        expected.base.bars_path = manifest_path(&bundle, "input:bars")?.to_string();
        if expected != portable {
            return Err("Bar 可搬运规格与身份规格除输入路径/output_dir 外存在差异".into());
        }
    }
    let reader = BundleArtifactReader::new(root, &bundle)?;
    qx_app::artifact_store::verify_declared_run_manifest_with_reader(&report, &reader)
        .map_err(|error| format!("实验包父 RunManifest 校验失败: {error}"))?;
    qx_app::artifact_store::verify_declared_run_record_with_reader(&report, &reader)
        .map_err(|error| format!("实验包父 RunRecord V2 校验失败: {error}"))?;
    verify_outcomes(&report, &reader, depth)?;
    if recompute {
        recompute_bundle(root, &portable_spec_path, &report, depth)?;
    }
    println!(
        "[qx-cli · CLI] 单市场实验包已复核: {} 个文件，experiment_id={}{}",
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

fn verify_optional_depth_input(
    bundle: &ExperimentEvidenceBundleManifest,
    expected: &mut qx_app::RunDepthExperimentSpec,
    field: &str,
    role: &str,
) -> Result<(), String> {
    let expected_base = serde_json::to_value(&expected.base).map_err(|error| error.to_string())?;
    let has_original = expected_base.get(field).and_then(Value::as_str).is_some();
    let entry = bundle.files.iter().find(|file| file.role == role);
    match (has_original, entry) {
        (false, None) => Ok(()),
        (true, Some(file)) => {
            match field {
                "quality_manifest_path" => {
                    expected.base.quality_manifest_path = Some(file.path.clone())
                }
                "runtime_config_path" => {
                    expected.base.runtime_config_path = Some(file.path.clone())
                }
                "market_spec_path" => expected.base.market_spec_path = Some(file.path.clone()),
                _ => return Err(format!("不支持的深度实验路径字段: {field}")),
            }
            Ok(())
        }
        (true, None) => Err(format!("实验包缺少 sidecar 声明的输入: {role}")),
        (false, Some(_)) => Err(format!("实验包包含规格未声明的输入: {role}")),
    }
}

fn recompute_bundle(
    root: &Path,
    portable_spec_path: &Path,
    original_report: &Value,
    depth: bool,
) -> Result<(), String> {
    if depth {
        let mut spec: qx_app::RunDepthExperimentSpec = serde_json::from_slice(
            &std::fs::read(portable_spec_path).map_err(|error| error.to_string())?,
        )
        .map_err(|error| format!("深度实验可搬运规格无效: {error}"))?;
        spec.base.depth_path = resolve_package_input(root, &spec.base.depth_path)?;
        resolve_optional_package_input(root, &mut spec.base.quality_manifest_path)?;
        resolve_optional_package_input(root, &mut spec.base.runtime_config_path)?;
        resolve_optional_package_input(root, &mut spec.base.market_spec_path)?;
        with_isolated_output(|output| {
            spec.base.output_dir = output.to_string_lossy().into_owned();
            let context = qx_app::RunContext::new(
                qx_app::CallerCapability::Research,
                spec.experiment_id.clone(),
            )
            .with_code_commit(crate::build_identity::BUILD_REVISION);
            let rerun = qx_app::run_depth_experiment(&spec, &context)
                .map_err(|error| error.to_json())
                .and_then(|result| {
                    serde_json::to_value(result).map_err(|error| error.to_string())
                })?;
            compare_single_market_experiment(original_report, &rerun, "深度")
        })
    } else {
        let mut spec: qx_app::RunExperimentSpec = serde_json::from_slice(
            &std::fs::read(portable_spec_path).map_err(|error| error.to_string())?,
        )
        .map_err(|error| format!("Bar 实验可搬运规格无效: {error}"))?;
        spec.base.bars_path = resolve_package_input(root, &spec.base.bars_path)?;
        with_isolated_output(|output| {
            spec.base.output_dir = output.to_string_lossy().into_owned();
            let context = qx_app::RunContext::new(
                qx_app::CallerCapability::Research,
                spec.experiment_id.clone(),
            )
            .with_code_commit(crate::build_identity::BUILD_REVISION);
            let rerun = qx_app::run_experiment(&spec, &context)
                .map_err(|error| error.to_json())
                .and_then(|result| {
                    serde_json::to_value(result).map_err(|error| error.to_string())
                })?;
            compare_single_market_experiment(original_report, &rerun, "Bar")
        })
    }
}

fn resolve_optional_package_input(
    root: &Path,
    reference: &mut Option<String>,
) -> Result<(), String> {
    if let Some(relative) = reference.as_deref() {
        *reference = Some(resolve_package_input(root, relative)?);
    }
    Ok(())
}

fn resolve_package_input(root: &Path, relative: &str) -> Result<String, String> {
    let path = Path::new(relative);
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(format!("实验包输入必须是包内相对路径: {relative}"));
    }
    package_path(root, relative).map(|path| path.to_string_lossy().into_owned())
}

fn validate_parent_report(report: &Value, input: &PersistedExperimentInput) -> Result<(), String> {
    let locations = report
        .get("artifact_locations")
        .and_then(Value::as_object)
        .ok_or("实验报告缺少 artifact_locations")?;
    for role in ["experiment_report", "run_manifest", "experiment_input"] {
        if !locations
            .get(role)
            .and_then(Value::as_str)
            .is_some_and(|path| !path.trim().is_empty())
        {
            return Err(format!("实验报告缺少父级 RunRecord 工件定位: {role}"));
        }
    }
    if !report
        .get("run_record")
        .and_then(Value::as_str)
        .is_some_and(|path| !path.trim().is_empty())
    {
        return Err("实验报告缺少父级 RunRecord 指针".into());
    }
    if input.schema_version != 1 {
        return Err(format!(
            "不支持的实验 sidecar schema_version={}",
            input.schema_version
        ));
    }
    Ok(())
}

fn verify_outcomes(
    report: &Value,
    reader: &dyn qx_app::artifact_store::ArtifactReader,
    depth: bool,
) -> Result<(), String> {
    let mut ordinal = 0usize;
    walk_outcomes(report, &mut |value| {
        ordinal += 1;
        let payload = serde_json::to_string(value).map_err(|error| error.to_string())?;
        let run_id = value
            .get("run_id")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let context = qx_app::RunContext::new(
            qx_app::CallerCapability::Research,
            format!("single-market-bundle:{ordinal}:{run_id}"),
        );
        let verification = if depth {
            let outcome = qx_app::DepthBacktestOutcome::from_json(&payload)
                .map_err(|error| format!("深度候选 outcome 无效: {error}"))?;
            qx_app::read_depth_run_report_v1_with_reader(&outcome, &context, reader)
                .map_err(|error| format!("深度候选报告读取失败: {}", error.to_json()))?
                .verification
        } else {
            let outcome = qx_app::BacktestOutcome::from_json(&payload)
                .map_err(|error| format!("Bar 候选 outcome 无效: {error}"))?;
            qx_app::read_run_report_v1_with_reader(&outcome, &context, reader)
                .map_err(|error| format!("Bar 候选报告读取失败: {}", error.to_json()))?
                .verification
        };
        if !verification.verified {
            return Err(format!(
                "候选 {} 运行产物未通过共享 reader: {}",
                run_id,
                verification.mismatches.join("; ")
            ));
        }
        Ok(())
    })?;
    if ordinal == 0 {
        return Err("实验报告没有可核验的候选运行产物".into());
    }
    Ok(())
}

fn walk_outcomes(
    value: &Value,
    visitor: &mut impl FnMut(&Value) -> Result<(), String>,
) -> Result<(), String> {
    match value {
        Value::Array(items) => {
            for item in items {
                walk_outcomes(item, visitor)?;
            }
        }
        Value::Object(fields) => {
            let is_outcome = fields.get("run_id").and_then(Value::as_str).is_some()
                && fields
                    .get("artifacts")
                    .and_then(Value::as_object)
                    .is_some_and(|artifacts| {
                        ["summary", "run_manifest", "equity", "fills"]
                            .iter()
                            .all(|key| artifacts.get(*key).and_then(Value::as_str).is_some())
                    });
            if is_outcome {
                visitor(value)?;
                return Ok(());
            }
            for child in fields.values() {
                walk_outcomes(child, visitor)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn copy_parent_references(
    report: &Value,
    report_path: &Path,
    writer: &mut BundleWriter<'_>,
) -> Result<(), String> {
    let base = report_path.parent().unwrap_or_else(|| Path::new("."));
    for field in ["run_record", "run_manifest"] {
        if let Some(reference) = report.get(field).and_then(Value::as_str) {
            writer.copy_reference(reference, base)?;
        }
    }
    if let Some(locations) = report.get("artifact_locations").and_then(Value::as_object) {
        for reference in locations.values().filter_map(Value::as_str) {
            writer.copy_reference(reference, base)?;
        }
    }
    Ok(())
}

fn copy_outcome_artifacts(
    report: &Value,
    report_path: &Path,
    writer: &mut BundleWriter<'_>,
) -> Result<(), String> {
    let base = report_path.parent().unwrap_or_else(|| Path::new("."));
    walk_outcomes(report, &mut |outcome| {
        let artifacts = outcome
            .get("artifacts")
            .and_then(Value::as_object)
            .ok_or("候选 outcome 缺少 artifacts")?;
        for reference in artifacts.values().filter_map(Value::as_str) {
            writer.copy_reference(reference, base)?;
        }
        let summary_reference = artifacts
            .get("summary")
            .and_then(Value::as_str)
            .ok_or("候选 outcome 缺少 summary artifact")?;
        let summary_path = resolve_source(summary_reference, Some(base))?;
        let summary: Value = serde_json::from_slice(
            &std::fs::read(&summary_path).map_err(|error| format!("读取候选摘要失败: {error}"))?,
        )
        .map_err(|error| format!("候选摘要 JSON 无效: {error}"))?;
        if let Some(reference) = summary.get("run_record").and_then(Value::as_str) {
            writer.copy_reference(reference, summary_path.parent().unwrap_or(base))?;
        }
        if let Some(reference) = summary.get("run_manifest").and_then(Value::as_str) {
            writer.copy_reference(reference, summary_path.parent().unwrap_or(base))?;
        }
        if let Some(locations) = summary.get("artifact_locations").and_then(Value::as_object) {
            for reference in locations.values().filter_map(Value::as_str) {
                writer.copy_reference(reference, summary_path.parent().unwrap_or(base))?;
            }
        }
        Ok(())
    })
}

fn resolve_source(reference: &str, relative_base: Option<&Path>) -> Result<PathBuf, String> {
    let candidate = PathBuf::from(reference);
    if candidate.is_absolute() {
        return candidate
            .canonicalize()
            .map_err(|error| format!("解析实验源文件失败 {}: {error}", candidate.display()));
    }
    let cwd = std::env::current_dir().map_err(|error| format!("读取当前目录失败: {error}"))?;
    let mut candidates = vec![cwd.join(&candidate)];
    if let Some(base) = relative_base {
        let path = base.join(&candidate);
        if !candidates.contains(&path) {
            candidates.push(path);
        }
    }
    let existing = candidates
        .into_iter()
        .filter_map(|path| path.canonicalize().ok())
        .filter(|path| path.is_file())
        .collect::<Vec<_>>();
    let first = existing
        .first()
        .cloned()
        .ok_or_else(|| format!("相对实验源文件不存在: {reference}（已尝试当前目录和报告目录）"))?;
    let first_digest = file_sha256(&first)?;
    for candidate in existing.iter().skip(1) {
        if file_sha256(candidate)? != first_digest {
            return Err(format!(
                "相对实验源文件解析存在歧义，候选内容不同: {reference}"
            ));
        }
    }
    Ok(first)
}

fn input_relative(role: &str, source: &str) -> String {
    let extension = Path::new(source)
        .extension()
        .and_then(|value| value.to_str())
        .filter(|extension| {
            !extension.is_empty() && extension.bytes().all(|byte| byte.is_ascii_alphanumeric())
        })
        .unwrap_or("data");
    format!("inputs/{role}.{extension}")
}

struct BundleWriter<'a> {
    root: &'a Path,
    files: Vec<BundleFile>,
    references: Vec<BundleReference>,
    reference_targets: BTreeMap<String, String>,
    source_targets: HashMap<PathBuf, String>,
    next_source: usize,
}

impl<'a> BundleWriter<'a> {
    fn new(root: &'a Path) -> Self {
        Self {
            root,
            files: Vec::new(),
            references: Vec::new(),
            reference_targets: BTreeMap::new(),
            source_targets: HashMap::new(),
            next_source: 1,
        }
    }

    fn add_file(&mut self, file: BundleFile) {
        self.files.push(file);
    }

    fn cache_source(&mut self, source: &Path, relative: &str) {
        if let Ok(source) = source.canonicalize() {
            self.source_targets.insert(source, relative.to_string());
        }
    }

    fn add_reference(&mut self, locator: &str, relative: &str) -> Result<(), String> {
        let digest = qx_strategy::sha256_hex(locator.as_bytes());
        match self.reference_targets.get(&digest) {
            Some(existing) if existing != relative => {
                Err("同一工件 locator 被映射到不同实验包文件".into())
            }
            Some(_) => Ok(()),
            None => {
                self.reference_targets
                    .insert(digest.clone(), relative.to_string());
                self.references.push(BundleReference {
                    locator_sha256: digest,
                    path: relative.to_string(),
                });
                Ok(())
            }
        }
    }

    fn copy_reference(&mut self, locator: &str, base: &Path) -> Result<String, String> {
        if locator.trim().is_empty() {
            return Err("实验报告含空工件 locator".into());
        }
        let source = resolve_source(locator, Some(base))?;
        let relative = if let Some(relative) = self.source_targets.get(&source) {
            relative.clone()
        } else {
            let extension = source
                .extension()
                .and_then(|value| value.to_str())
                .filter(|extension| {
                    !extension.is_empty()
                        && extension.bytes().all(|byte| byte.is_ascii_alphanumeric())
                })
                .unwrap_or("bin");
            let relative = format!("artifacts/source-{:05}.{extension}", self.next_source);
            self.next_source += 1;
            let file = copy_to(&source, self.root, &relative)?;
            self.files.push(file);
            self.source_targets.insert(source.clone(), relative.clone());
            relative
        };
        self.add_reference(locator, &relative)?;
        self.add_reference(&source.to_string_lossy(), &relative)?;
        Ok(relative)
    }
}

struct BundleArtifactReader {
    root: PathBuf,
    paths_by_locator: HashMap<String, String>,
    paths_by_digest: HashMap<String, String>,
    declared_paths: std::collections::HashSet<String>,
}

impl BundleArtifactReader {
    fn new(root: &Path, bundle: &ExperimentEvidenceBundleManifest) -> Result<Self, String> {
        let root = root
            .canonicalize()
            .map_err(|error| format!("解析实验包根目录失败: {error}"))?;
        let mut paths_by_digest = HashMap::new();
        let mut declared_paths = std::collections::HashSet::new();
        for file in &bundle.files {
            declared_paths.insert(file.path.clone());
            paths_by_digest
                .entry(file.sha256.clone())
                .or_insert_with(|| file.path.clone());
        }
        let paths_by_locator = bundle
            .references
            .iter()
            .map(|reference| (reference.locator_sha256.clone(), reference.path.clone()))
            .collect();
        Ok(Self {
            root,
            paths_by_locator,
            paths_by_digest,
            declared_paths,
        })
    }

    fn resolve_locator(&self, reference: &str) -> Result<PathBuf, qx_app::AppError> {
        let locator_digest = qx_strategy::sha256_hex(reference.as_bytes());
        if let Some(relative) = self.paths_by_locator.get(&locator_digest) {
            return self.resolve_relative(relative);
        }
        let relative = Path::new(reference);
        if relative.is_absolute()
            || relative.components().any(|component| {
                matches!(
                    component,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )
            })
        {
            return Err(qx_app::AppError::new(
                qx_app::AppErrorCategory::InvalidInput,
                "实验包 ArtifactReader 拒绝未登记的绝对或越界 locator",
            ));
        }
        if !self.declared_paths.contains(reference) {
            return Err(qx_app::AppError::new(
                qx_app::AppErrorCategory::StorageFailure,
                "实验包 ArtifactReader 拒绝未登记的 locator",
            ));
        }
        self.resolve_relative(reference)
    }

    fn resolve_relative(&self, reference: &str) -> Result<PathBuf, qx_app::AppError> {
        let path = Path::new(reference);
        if path.is_absolute()
            || path.components().any(|component| {
                matches!(
                    component,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )
            })
        {
            return Err(qx_app::AppError::new(
                qx_app::AppErrorCategory::InvalidInput,
                "实验包 ArtifactLocation 必须是包内相对路径",
            ));
        }
        let resolved =
            self.root.join(path).canonicalize().map_err(|error| {
                qx_app::AppError::from_io("解析实验包 ArtifactLocation", &error)
            })?;
        if !resolved.starts_with(&self.root) || !resolved.is_file() {
            return Err(qx_app::AppError::new(
                qx_app::AppErrorCategory::InvalidInput,
                "实验包 ArtifactLocation 越界或不是普通文件",
            ));
        }
        Ok(resolved)
    }

    fn open_path(
        &self,
        relative: &str,
    ) -> Result<qx_app::artifact_store::ArtifactStream, qx_app::AppError> {
        let path = self.resolve_relative(relative)?;
        let file = File::open(&path)
            .map_err(|error| qx_app::AppError::from_io("打开实验包 Artifact", &error))?;
        let metadata = file
            .metadata()
            .map_err(|error| qx_app::AppError::from_io("读取实验包 Artifact 元数据", &error))?;
        if !metadata.is_file() {
            return Err(qx_app::AppError::new(
                qx_app::AppErrorCategory::StorageFailure,
                "实验包 Artifact 必须是普通文件",
            ));
        }
        Ok(qx_app::artifact_store::ArtifactStream::new(
            metadata.len(),
            BufReader::new(file),
        ))
    }
}

impl qx_app::artifact_store::ArtifactReader for BundleArtifactReader {
    fn open(
        &self,
        reference: &str,
    ) -> Result<qx_app::artifact_store::ArtifactStream, qx_app::AppError> {
        let path = self.resolve_locator(reference)?;
        let relative = path
            .strip_prefix(&self.root)
            .map_err(|_| {
                qx_app::AppError::new(
                    qx_app::AppErrorCategory::InvalidInput,
                    "实验包 locator 越界",
                )
            })?
            .to_string_lossy()
            .replace('\\', "/");
        self.open_path(&relative)
    }

    fn open_by_id(
        &self,
        artifact_id: &qx_app::artifact_store::ArtifactId,
        legacy_reference: Option<&str>,
    ) -> Result<qx_app::artifact_store::ArtifactStream, qx_app::AppError> {
        if let Some(relative) = self.paths_by_digest.get(artifact_id.as_str()) {
            return self.open_path(relative);
        }
        let Some(reference) = legacy_reference else {
            return Err(qx_app::AppError::new(
                qx_app::AppErrorCategory::StorageFailure,
                format!("实验包中不存在 ArtifactId {}", artifact_id.as_str()),
            ));
        };
        self.open(reference)
    }
}
