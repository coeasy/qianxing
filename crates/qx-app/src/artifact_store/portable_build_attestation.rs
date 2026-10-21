//! Portable third-party strategy build evidence and its referenced material files.

use super::{
    resolve_portable_run_dependency, run_evidence_safe_component, PortableRunEvidenceSource,
};
use qx_spec::ArtifactRef;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StrategyBuildAttestation {
    pub schema_version: u32,
    pub strategy_id: String,
    pub source_revision: String,
    pub artifact_sha256: String,
    pub target_triple: String,
    pub build_command: Vec<String>,
    pub toolchain: Vec<String>,
    pub materials: Vec<StrategyBuildMaterial>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StrategyBuildMaterial {
    pub role: String,
    pub path: String,
    pub sha256: String,
}

pub fn validate_portable_strategy_build_attestation(
    attestation: &StrategyBuildAttestation,
) -> Result<(), String> {
    if attestation.schema_version != 1
        || attestation.strategy_id.trim().is_empty()
        || !matches!(attestation.source_revision.len(), 40 | 64)
        || !attestation
            .source_revision
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
        || attestation.artifact_sha256.len() != 64
        || !attestation
            .artifact_sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
        || attestation.target_triple.trim().is_empty()
        || attestation.build_command.is_empty()
        || attestation
            .build_command
            .iter()
            .any(|part| part.trim().is_empty())
        || attestation.toolchain.is_empty()
        || attestation
            .toolchain
            .iter()
            .any(|part| part.trim().is_empty())
    {
        return Err("策略构建证明的 schema 或身份字段无效".into());
    }
    let roles = ["source", "dependency_lock", "build_recipe", "toolchain"];
    if roles.iter().any(|role| {
        !attestation
            .materials
            .iter()
            .any(|material| material.role == *role)
    }) || attestation.materials.iter().any(|material| {
        let path = Path::new(&material.path);
        !roles.contains(&material.role.as_str())
            || material.path.trim().is_empty()
            || path.is_absolute()
            || path.components().any(|component| {
                matches!(
                    component,
                    std::path::Component::ParentDir
                        | std::path::Component::RootDir
                        | std::path::Component::Prefix(_)
                )
            })
            || material.sha256.len() != 64
            || !material.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
    }) {
        return Err("策略构建证明缺少必需材料，或材料角色/路径/SHA-256 无效".into());
    }
    if attestation
        .build_command
        .iter()
        .chain(attestation.toolchain.iter())
        .any(|part| {
            ["SECRET=", "TOKEN=", "PASSWORD=", "API_KEY=", "PRIVATE_KEY="]
                .iter()
                .any(|marker| part.to_ascii_uppercase().contains(marker))
        })
    {
        return Err("策略构建命令或工具链描述疑似包含明文凭据".into());
    }
    Ok(())
}

pub fn verify_portable_build_attestation_artifact_refs(
    attestation: Option<&StrategyBuildAttestation>,
    artifacts: &[ArtifactRef],
) -> Result<(), String> {
    let Some(attestation) = attestation else {
        if artifacts
            .iter()
            .any(|artifact| artifact.name.starts_with("strategy_build_material_"))
        {
            return Err(
                "EvidenceBundle has build materials without a strategy build attestation".into(),
            );
        }
        return Ok(());
    };
    if attestation.materials.len()
        != artifacts
            .iter()
            .filter(|artifact| artifact.name.starts_with("strategy_build_material_"))
            .count()
    {
        return Err(
            "strategy build attestation material roster differs from EvidenceBundle".into(),
        );
    }
    for (index, material) in attestation.materials.iter().enumerate() {
        let expected_name = format!("strategy_build_material_{index}");
        let Some(reference) = artifacts
            .iter()
            .find(|artifact| artifact.name == expected_name)
        else {
            return Err(format!("EvidenceBundle is missing {expected_name}"));
        };
        if reference.path != material.path
            || !reference.digest.eq_ignore_ascii_case(&material.sha256)
        {
            return Err(format!(
                "EvidenceBundle and build attestation disagree for {expected_name}"
            ));
        }
    }
    Ok(())
}

pub fn prepare_portable_strategy_build_attestation(
    path: &Path,
) -> Result<(StrategyBuildAttestation, Vec<PortableRunEvidenceSource>), String> {
    let payload = std::fs::read_to_string(path)
        .map_err(|error| format!("读取策略构建证明失败 {}: {error}", path.display()))?;
    let mut attestation: StrategyBuildAttestation =
        serde_json::from_str(&payload).map_err(|error| format!("解析策略构建证明失败: {error}"))?;
    validate_portable_strategy_build_attestation(&attestation)?;
    let attestation_root = path
        .canonicalize()
        .map_err(|error| format!("解析策略构建证明路径失败: {error}"))?;
    let attestation_root = attestation_root
        .parent()
        .unwrap_or(Path::new("."))
        .to_path_buf();
    let mut sources = Vec::with_capacity(attestation.materials.len());
    for (index, material) in attestation.materials.iter_mut().enumerate() {
        let source = resolve_portable_run_dependency(&attestation_root, &material.path)?;
        let digest = qx_strategy::file_digest::sha256_file_hex(&source)?;
        if !digest.eq_ignore_ascii_case(&material.sha256) {
            return Err(format!("策略构建材料摘要不匹配: {}", material.path));
        }
        let file_name = source
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("material");
        let relative = format!(
            "build/materials/{index:02}-{}",
            run_evidence_safe_component(file_name)
        );
        material.path = relative.clone();
        sources.push(PortableRunEvidenceSource { source, relative });
    }
    Ok((attestation, sources))
}
