use crate::*;
use qx_spec::ArtifactRef;
use std::path::Path;

pub(crate) fn prepare_build_attestation(
    path: &Path,
    runtime_config_path: &Path,
    config: &qx_runtime::RuntimeConfig,
    run_id: &str,
) -> Result<
    (
        qx_app::artifact_store::StrategyBuildAttestation,
        Vec<qx_app::artifact_store::PortableRunEvidenceSource>,
    ),
    String,
> {
    let (attestation, materials) =
        qx_app::artifact_store::prepare_portable_strategy_build_attestation(path)?;
    let runtime_strategy =
        crate::run_evidence_export::config_for_strategy_run(config, runtime_config_path, run_id)?;
    let strategy_id = runtime_strategy
        .strategy
        .id
        .as_deref()
        .unwrap_or(&runtime_strategy.strategy.version);
    if strategy_id != attestation.strategy_id {
        return Err(format!(
            "策略构建证明身份不匹配: RunManifest={strategy_id} attestation={}",
            attestation.strategy_id
        ));
    }
    let expected_artifact = runtime_strategy
        .strategy
        .strategy_artifact_sha256
        .as_deref()
        .or(runtime_strategy.strategy.c_abi_sha256.as_deref())
        .ok_or("所选策略没有声明可与构建证明绑定的策略工件摘要")?;
    if !expected_artifact.eq_ignore_ascii_case(&attestation.artifact_sha256) {
        return Err("策略构建证明 artifact_sha256 与 runtime 配置声明不一致".into());
    }
    Ok((attestation, materials))
}

pub(crate) fn verify_attached_build_attestation(
    root: &Path,
    artifacts: &[ArtifactRef],
    runtime_config_path: Option<&Path>,
    run_id: &str,
) -> Result<(), String> {
    let Some(attestation_ref) = artifacts
        .iter()
        .find(|artifact| artifact.name == "strategy_build_attestation")
    else {
        return qx_app::artifact_store::verify_portable_build_attestation_artifact_refs(
            None, artifacts,
        );
    };
    let path = crate::run_evidence_recompute::resolve_reference(root, &attestation_ref.path)?;
    let payload = std::fs::read_to_string(&path)
        .map_err(|error| format!("读取策略构建证明失败 {}: {error}", path.display()))?;
    let attestation: qx_app::artifact_store::StrategyBuildAttestation =
        serde_json::from_str(&payload).map_err(|error| format!("解析策略构建证明失败: {error}"))?;
    qx_app::artifact_store::validate_portable_strategy_build_attestation(&attestation)?;
    qx_app::artifact_store::verify_portable_build_attestation_artifact_refs(
        Some(&attestation),
        artifacts,
    )?;
    let config_path = runtime_config_path
        .ok_or("strategy build attestation requires an included runtime_config")?;
    let config = read_runtime_config(config_path)?;
    let effective =
        crate::run_evidence_export::config_for_strategy_run(&config, config_path, run_id)?;
    let strategy_id = effective
        .strategy
        .id
        .as_deref()
        .unwrap_or(&effective.strategy.version);
    let expected_artifact = effective
        .strategy
        .strategy_artifact_sha256
        .as_deref()
        .or(effective.strategy.c_abi_sha256.as_deref())
        .ok_or("attested strategy has no declared artifact digest")?;
    if strategy_id != attestation.strategy_id
        || !expected_artifact.eq_ignore_ascii_case(&attestation.artifact_sha256)
    {
        return Err(
            "strategy build attestation identity or artifact digest differs from runtime_config"
                .into(),
        );
    }
    Ok(())
}
