//! Prepare a run EvidenceBundle and its input files for portable offline verification.

use super::portable_build_attestation::{
    verify_portable_build_attestation_artifact_refs, StrategyBuildAttestation,
};
use qx_spec::{ArtifactRef, EvidenceBundle, EvidenceGeneration, FoundationDocument};
use std::collections::HashSet;
use std::io;
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Debug)]
pub struct PortableRunEvidenceSource {
    pub source: PathBuf,
    pub relative: String,
}

pub struct PortableRunEvidenceExportPlan {
    pub bundle: EvidenceBundle,
    pub sources: Vec<PortableRunEvidenceSource>,
}

pub struct PortableRunEvidenceGeneratedFile {
    pub relative: String,
    pub payload: Vec<u8>,
    pub label: String,
}

#[derive(Default)]
pub struct PortableRunEvidenceSupplements {
    pub runtime_config: Option<ArtifactRef>,
    pub runtime_dependencies: Vec<ArtifactRef>,
    pub market_spec: Option<ArtifactRef>,
    pub build_attestation: Option<StrategyBuildAttestation>,
}

pub struct PortableRunEvidenceDependencyPlan {
    pub sources: Vec<PortableRunEvidenceSource>,
    pub artifacts: Vec<ArtifactRef>,
}

pub struct PortableRunEvidencePreparedFile {
    pub source: PortableRunEvidenceSource,
    pub artifact: ArtifactRef,
}

/// Bind a selected source file to its portable bundle path and content digest.
pub fn prepare_portable_run_evidence_file(
    source: &Path,
    artifact_name: &str,
    relative: &str,
) -> Result<PortableRunEvidencePreparedFile, String> {
    let digest = qx_strategy::file_digest::sha256_file_hex(source)?;
    Ok(PortableRunEvidencePreparedFile {
        source: PortableRunEvidenceSource {
            source: source.to_path_buf(),
            relative: relative.to_string(),
        },
        artifact: ArtifactRef {
            name: artifact_name.to_string(),
            path: relative.to_string(),
            digest,
        },
    })
}

/// Resolve, deduplicate, hash, and name runtime files declared by an already-parsed config.
/// The caller owns config parsing; portable path and ArtifactRef rules live in this layer.
pub fn prepare_portable_run_evidence_dependencies(
    config_root: &Path,
    references: &[&str],
) -> Result<PortableRunEvidenceDependencyPlan, String> {
    let mut seen = HashSet::with_capacity(references.len());
    let mut plan = PortableRunEvidenceDependencyPlan {
        sources: Vec::with_capacity(references.len()),
        artifacts: Vec::with_capacity(references.len()),
    };
    for reference in references {
        let source = resolve_portable_run_dependency(config_root, reference)?;
        let relative = PathBuf::from(reference);
        let target = PathBuf::from("runtime")
            .join(&relative)
            .to_string_lossy()
            .replace('\\', "/");
        if !seen.insert(target.clone()) {
            continue;
        }
        let file_name = Path::new(&target)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("dependency");
        let prepared = prepare_portable_run_evidence_file(
            &source,
            &format!("runtime_dependency_{}_{}", plan.artifacts.len(), file_name),
            &target,
        )?;
        plan.artifacts.push(prepared.artifact);
        plan.sources.push(prepared.source);
    }
    Ok(plan)
}

/// Attach facade-resolved runtime files to a prepared run bundle and update its verification
/// contract in one place. The facade remains responsible for resolving and reading those files.
pub fn attach_portable_run_evidence_supplements(
    plan: &mut PortableRunEvidenceExportPlan,
    supplements: PortableRunEvidenceSupplements,
) -> Result<Vec<PortableRunEvidenceGeneratedFile>, String> {
    if supplements.runtime_config.is_none() && !supplements.runtime_dependencies.is_empty() {
        return Err("runtime dependencies require an included runtime_config".into());
    }
    if supplements.build_attestation.is_some() && supplements.runtime_config.is_none() {
        return Err("strategy build attestation requires an included runtime_config".into());
    }
    let EvidenceGeneration::Run(run) = &mut plan.bundle.generation else {
        return Err("evidence export-run 只接受 generation=run".into());
    };
    let mut generated_files = Vec::new();
    run.artifacts.extend(supplements.runtime_dependencies);
    if let Some(runtime_config) = supplements.runtime_config {
        run.recompute.inputs.extend(
            run.artifacts
                .iter()
                .filter(|artifact| artifact.name.starts_with("runtime_dependency_"))
                .map(|artifact| artifact.path.clone()),
        );
        run.recompute.inputs.push(runtime_config.path.clone());
        run.artifacts.push(runtime_config);
    }
    if let Some(market_spec) = supplements.market_spec {
        run.recompute.inputs.push(market_spec.path.clone());
        run.artifacts.push(market_spec);
    }
    if let Some(attestation) = supplements.build_attestation {
        let material_artifacts = attestation
            .materials
            .iter()
            .enumerate()
            .map(|(index, material)| ArtifactRef {
                name: format!("strategy_build_material_{index}"),
                path: material.path.clone(),
                digest: material.sha256.clone(),
            })
            .collect::<Vec<_>>();
        verify_portable_build_attestation_artifact_refs(Some(&attestation), &material_artifacts)?;
        let payload = serde_json::to_vec_pretty(&attestation)
            .map_err(|error| format!("序列化策略构建证明失败: {error}"))?;
        let relative = "build/attestation.json".to_string();
        let digest = qx_strategy::sha256_hex(&payload);
        generated_files.push(PortableRunEvidenceGeneratedFile {
            relative: relative.clone(),
            payload,
            label: "策略构建证明".to_string(),
        });
        run.artifacts.push(ArtifactRef {
            name: "strategy_build_attestation".into(),
            path: relative,
            digest,
        });
        run.recompute.inputs.extend(
            material_artifacts
                .iter()
                .map(|artifact| artifact.path.clone()),
        );
        run.artifacts.extend(material_artifacts);
    }
    run.verification.verified_artifact_count = run.artifacts.len() as u64;
    run.recompute.steps = vec![
        "在离线环境中运行 qx-cli evidence verify-run evidence.json，重核工件摘要、RunManifest 身份与数据指纹".into(),
        "若本包来自 strategy-backtest 或 depth-backtest，且附带所需配置/规格与匹配 code_commit 的构建，追加 --recompute 重跑并逐字段核对 RunManifest".into(),
    ];
    Ok(generated_files)
}

/// Resolve a bundle-relative path while rejecting `..` and root-prefix escapes.
pub fn run_evidence_source_path(root: &Path, reference: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(reference);
    if path.is_absolute() {
        Ok(path)
    } else if path.components().any(|part| {
        matches!(
            part,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        Err(format!("源 EvidenceBundle 的相对路径越界: {reference}"))
    } else {
        Ok(root.join(path))
    }
}

/// Resolve a runtime dependency inside its configuration directory, rejecting traversal and
/// symlink escapes before the file is added to a portable run bundle.
pub fn resolve_portable_run_dependency(
    config_root: &Path,
    reference: &str,
) -> Result<PathBuf, String> {
    let relative = PathBuf::from(reference);
    if relative.is_absolute()
        || relative.components().any(|part| {
            matches!(
                part,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(format!(
            "runtime 配置依赖必须是配置目录内的相对路径，无法生成可搬运包: {reference}"
        ));
    }
    let source = config_root.join(relative).canonicalize().map_err(|error| {
        format!(
            "解析 runtime 配置依赖失败 {}: {error}",
            config_root.join(reference).display()
        )
    })?;
    if !source.starts_with(config_root) {
        return Err(format!(
            "runtime 配置依赖经符号链接逃出配置目录: {reference}"
        ));
    }
    Ok(source)
}

/// Verify a portable runtime dependency against its declared digest and return its resolved path.
pub fn verify_portable_run_dependency_digest(
    config_root: &Path,
    reference: &str,
    expected: &str,
    mismatch: impl FnOnce(&str) -> String,
) -> Result<PathBuf, String> {
    let source = resolve_portable_run_dependency(config_root, reference)?;
    let actual = qx_strategy::file_digest::sha256_file_hex(&source)?;
    if actual != expected {
        return Err(mismatch(&actual));
    }
    Ok(source)
}

/// Make a stable filename component from an artifact/role name.
pub fn run_evidence_safe_component(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

/// Copy a portable artifact and preserve executable bits on Unix platforms.
pub fn copy_portable_run_evidence_file(source: &Path, target: &Path) -> Result<(), String> {
    let mut input = std::fs::File::open(source)
        .map_err(|error| format!("读取待导出文件失败 {}: {error}", source.display()))?;
    #[cfg(unix)]
    let executable = {
        use std::os::unix::fs::PermissionsExt;
        input
            .metadata()
            .map_err(|error| format!("读取源文件权限失败 {}: {error}", source.display()))?
            .permissions()
            .mode()
            & 0o111
            != 0
    };
    let mut output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(target)
        .map_err(|error| format!("创建导出文件失败 {}: {error}", target.display()))?;
    io::copy(&mut input, &mut output)
        .map_err(|error| format!("复制导出文件失败 {}: {error}", target.display()))?;
    output
        .sync_all()
        .map_err(|error| format!("同步导出文件失败 {}: {error}", target.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        drop(output);
        let mode = if executable { 0o755 } else { 0o644 };
        std::fs::set_permissions(target, std::fs::Permissions::from_mode(mode))
            .map_err(|error| format!("设置导出文件权限失败 {}: {error}", target.display()))?;
    }
    Ok(())
}

fn portable_extension(path: &Path, fallback: &str) -> String {
    path.extension()
        .and_then(|part| part.to_str())
        .filter(|part| part.chars().all(|ch| ch.is_ascii_alphanumeric()))
        .unwrap_or(fallback)
        .to_string()
}

fn push_source(sources: &mut Vec<PortableRunEvidenceSource>, source: PathBuf, relative: String) {
    sources.push(PortableRunEvidenceSource { source, relative });
}

fn verified_source(
    root: &Path,
    reference: &str,
    expected: &str,
    mismatch: impl FnOnce(&str) -> String,
) -> Result<PathBuf, String> {
    let source = run_evidence_source_path(root, reference)?;
    let actual = qx_strategy::file_digest::sha256_file_hex(&source)?;
    if actual != expected {
        return Err(mismatch(&actual));
    }
    Ok(source)
}

/// Validate source bytes, rewrite run-generation paths, and return the complete base-file plan.
/// Runtime configuration dependencies and build attestations are added by the CLI adapter.
pub fn prepare_portable_run_evidence_export(
    mut bundle: EvidenceBundle,
    source_root: &Path,
) -> Result<PortableRunEvidenceExportPlan, String> {
    let EvidenceGeneration::Run(run) = &mut bundle.generation else {
        return Err("evidence export-run 只接受 generation=run".into());
    };
    let artifact_count = run.artifacts.len();
    let mut sources = Vec::with_capacity(artifact_count + run.dataset.components.len() + 4);
    for artifact in &run.artifacts {
        let source = verified_source(source_root, &artifact.path, &artifact.digest, |actual| {
            format!(
                "导出前工件摘要不匹配: {} 声明={} 实际={actual}",
                artifact.path, artifact.digest
            )
        })?;
        let relative = format!(
            "artifacts/{}.{}",
            run_evidence_safe_component(&artifact.name),
            portable_extension(&source, "data")
        );
        push_source(&mut sources, source, relative);
    }

    let mut input_relatives = Vec::new();
    if run.dataset.components.is_empty() {
        if let Some(manifest_path) = run.dataset.quality_manifest_path.as_deref() {
            let expected = run
                .dataset
                .quality_manifest_sha256
                .as_deref()
                .ok_or("单输入数据质量清单缺少 SHA-256")?;
            let source = verified_source(source_root, manifest_path, expected, |actual| {
                format!("导出前数据质量清单摘要不匹配: 声明={expected} 实际={actual}")
            })?;
            let relative = "inputs/dataset-manifest.json".to_string();
            push_source(&mut sources, source, relative.clone());
            run.dataset.quality_manifest_path = Some(relative);
        }
        let source = run_evidence_source_path(source_root, &run.dataset.path)?;
        let relative = format!("inputs/dataset.{}", portable_extension(&source, "data"));
        push_source(&mut sources, source, relative.clone());
        input_relatives.push(relative);
    } else {
        for (index, component) in run.dataset.components.iter_mut().enumerate() {
            if let Some(manifest_path) = component.quality_manifest_path.as_deref() {
                let expected = component
                    .quality_manifest_sha256
                    .as_deref()
                    .ok_or_else(|| format!("{} 数据质量清单缺少 SHA-256", component.role))?;
                let source = verified_source(source_root, manifest_path, expected, |actual| {
                    format!(
                        "导出前 {} 数据质量清单摘要不匹配: 声明={} 实际={actual}",
                        component.role, expected
                    )
                })?;
                let relative = format!(
                    "inputs/{}.dataset-manifest.json",
                    run_evidence_safe_component(&component.role)
                );
                push_source(&mut sources, source, relative.clone());
                component.quality_manifest_path = Some(relative);
            }
            let source = run_evidence_source_path(source_root, &component.path)?;
            let name = if component.role.starts_with("market:") {
                format!("market-{:02}", index + 1)
            } else {
                run_evidence_safe_component(&component.role)
            };
            let relative = format!("inputs/{name}.{}", portable_extension(&source, "data"));
            push_source(&mut sources, source, relative.clone());
            component.path = relative.clone();
            input_relatives.push(relative);
        }
    }

    let mut report_analysis_relative = None;
    let mut report_calendar_relative = None;
    if let Some(analysis) = run.report_analysis.as_mut() {
        let source = verified_source(
            source_root,
            &analysis.input_path,
            &analysis.input_sha256,
            |actual| {
                format!(
                    "导出前报告分析输入摘要不匹配: {} 声明={} 实际={actual}",
                    analysis.input_path, analysis.input_sha256
                )
            },
        )?;
        let relative = format!("inputs/benchmark.{}", portable_extension(&source, "data"));
        push_source(&mut sources, source, relative.clone());
        analysis.input_path = relative.clone();
        report_analysis_relative = Some(relative);

        if let Some(calendar_path) = analysis.calendar_input_path.as_deref() {
            let expected = analysis
                .calendar_input_sha256
                .as_deref()
                .ok_or("报告分析声明了交易日历路径但缺少 SHA-256")?;
            let source = verified_source(source_root, calendar_path, expected, |actual| {
                format!(
                    "导出前报告交易日历 SHA-256 不匹配: {} 声明={} 实际={actual}",
                    calendar_path, expected
                )
            })?;
            let relative = format!(
                "inputs/benchmark-calendar.{}",
                portable_extension(&source, "json")
            );
            push_source(&mut sources, source, relative.clone());
            analysis.calendar_input_path = Some(relative.clone());
            report_calendar_relative = Some(relative);
        }
    }

    for (artifact, source) in run
        .artifacts
        .iter_mut()
        .zip(sources.iter().take(artifact_count))
    {
        artifact.path = source.relative.clone();
    }
    run.dataset.path = input_relatives.join("|");
    run.recompute.inputs = run
        .artifacts
        .iter()
        .map(|artifact| artifact.path.clone())
        .chain(input_relatives.iter().cloned())
        .collect();
    if let Some(path) = run.dataset.quality_manifest_path.as_ref() {
        run.recompute.inputs.push(path.clone());
    }
    if let Some(path) = report_analysis_relative {
        run.recompute.inputs.push(path);
    }
    if let Some(path) = report_calendar_relative {
        run.recompute.inputs.push(path);
    }

    Ok(PortableRunEvidenceExportPlan { bundle, sources })
}

/// Materialize every planned source and generated file, write the final bundle, then atomically
/// publish the staging directory at `output_dir`.
pub fn commit_portable_run_evidence_export(
    plan: PortableRunEvidenceExportPlan,
    output_dir: &Path,
    generated_files: Vec<PortableRunEvidenceGeneratedFile>,
) -> Result<usize, String> {
    let staging = output_dir.with_file_name(format!(
        ".{}.staging-{}",
        output_dir
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("导出目录名无效")?,
        std::process::id()
    ));
    std::fs::create_dir(&staging)
        .map_err(|error| format!("创建证据包暂存目录失败 {}: {error}", staging.display()))?;
    for source in &plan.sources {
        let target = staging.join(&source.relative);
        let parent = target.parent().ok_or("证据包导出目标缺少父目录")?;
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("创建证据包目录失败 {}: {error}", parent.display()))?;
        copy_portable_run_evidence_file(&source.source, &target)?;
    }
    for generated in generated_files {
        let target = staging.join(&generated.relative);
        let parent = target.parent().ok_or("证据包导出目标缺少父目录")?;
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("创建证据包目录失败 {}: {error}", parent.display()))?;
        std::fs::write(&target, generated.payload).map_err(|error| {
            format!("写入{}失败 {}: {error}", generated.label, target.display())
        })?;
    }
    let payload = plan.bundle.to_json()?;
    let bundle_target = staging.join("evidence.json");
    std::fs::write(&bundle_target, payload).map_err(|error| {
        format!(
            "写入导出 EvidenceBundle 失败 {}: {error}",
            bundle_target.display()
        )
    })?;
    std::fs::rename(&staging, output_dir).map_err(|error| {
        format!(
            "提交证据包目录失败 {} -> {}: {error}",
            staging.display(),
            output_dir.display()
        )
    })?;
    Ok(plan.sources.len())
}
