use crate::*;
use qx_spec::{EvidenceBundle, EvidenceGeneration, FoundationDocument};
use std::path::Path;

fn is_path_like_python_module(reference: &str) -> bool {
    reference.ends_with(".py")
        || reference.contains('/')
        || reference.contains('\\')
        || reference.starts_with('.')
}

fn verify_declared_strategy_artifacts(
    config: &qx_runtime::RuntimeConfig,
    config_root: &Path,
) -> Result<(), String> {
    for strategy in std::iter::once(&config.strategy).chain(config.strategies.iter()) {
        let label = strategy.id.as_deref().unwrap_or(&strategy.version);
        for (reference, expected, kind) in [
            (
                strategy
                    .python_module
                    .as_deref()
                    .filter(|path| is_path_like_python_module(path)),
                strategy.strategy_artifact_sha256.as_deref(),
                "strategy_artifact_sha256",
            ),
            (
                strategy.external_executable.as_deref(),
                strategy.strategy_artifact_sha256.as_deref(),
                "strategy_artifact_sha256",
            ),
            (
                strategy.c_abi_library.as_deref(),
                strategy.c_abi_sha256.as_deref(),
                "c_abi_sha256",
            ),
        ] {
            let (Some(reference), Some(expected)) = (reference, expected) else {
                if kind == "c_abi_sha256" && reference.is_some() {
                    return Err(format!(
                        "策略 {label} 配置了 c_abi_library 但缺少 c_abi_sha256"
                    ));
                }
                continue;
            };
            qx_app::artifact_store::verify_portable_run_dependency_digest(
                config_root,
                reference,
                expected,
                |actual| {
                    format!(
                        "策略 {label} 的 {kind} 不匹配: {reference} 声明={expected} 实际={actual}"
                    )
                },
            )?;
        }
    }
    Ok(())
}

pub(crate) fn config_for_strategy_run(
    config: &qx_runtime::RuntimeConfig,
    runtime_config_path: &Path,
    run_id: &str,
) -> Result<qx_runtime::RuntimeConfig, String> {
    let mut effective = declared_config_for_strategy_run(config, run_id)?;
    if run_id.starts_with("strategy-backtest:") {
        resolve_strategy_runtime_paths(&mut effective.strategy, runtime_config_path);
    }
    Ok(effective)
}

/// Strategy identity fingerprints the config as declared, so moving the portable bundle does
/// not change the run id. Runtime consumers use `config_for_strategy_run` for resolved paths.
pub(crate) fn declared_config_for_strategy_run(
    config: &qx_runtime::RuntimeConfig,
    run_id: &str,
) -> Result<qx_runtime::RuntimeConfig, String> {
    let Some(strategy_run) = run_id.strip_prefix("strategy-backtest:") else {
        return Ok(config.clone());
    };
    let strategies = if config.strategies.is_empty() {
        vec![&config.strategy]
    } else {
        config.strategies.iter().collect()
    };
    let matches: Vec<_> = strategies
        .into_iter()
        .filter(|strategy| {
            let strategy_id = strategy.id.as_deref().unwrap_or(&strategy.version);
            strategy_run
                .strip_prefix(&format!("{strategy_id}:"))
                .is_some_and(|instrument| !instrument.is_empty())
        })
        .collect();
    let [strategy] = matches.as_slice() else {
        return Err(format!(
            "runtime 配置中无法唯一定位 RunManifest 策略身份 {run_id}"
        ));
    };
    let mut effective = config.clone();
    effective.strategy = (*strategy).clone();
    effective.strategies.clear();
    Ok(effective)
}

fn configured_file_references(config: &qx_runtime::RuntimeConfig) -> Vec<&str> {
    let mut references = Vec::new();
    for strategy in std::iter::once(&config.strategy).chain(config.strategies.iter()) {
        references.extend(
            [
                strategy.cost_rules_path.as_deref(),
                strategy.target_snapshot_path.as_deref(),
                strategy.research_snapshot_path.as_deref(),
                strategy.dataset_bundle_path.as_deref(),
                strategy.builtin_reference_bars_snapshot_path.as_deref(),
                strategy.bars_snapshot_path.as_deref(),
                strategy.ashare_rules_path.as_deref(),
                strategy.ashare_actions_path.as_deref(),
                strategy.ashare_calendar_path.as_deref(),
                strategy.external_executable.as_deref(),
                strategy
                    .python_module
                    .as_deref()
                    .filter(|reference| is_path_like_python_module(reference)),
                strategy.c_abi_library.as_deref(),
            ]
            .into_iter()
            .flatten(),
        );
        references.extend(
            strategy
                .dataset_component_paths
                .values()
                .map(String::as_str),
        );
    }
    references
}

pub(crate) fn export_run_bundle(
    bundle_path: &Path,
    output_dir: &Path,
    runtime_config_path: Option<&Path>,
    market_spec_path: Option<&Path>,
    build_attestation_path: Option<&Path>,
) -> Result<(), String> {
    if output_dir.exists() {
        return Err(format!("导出目录已存在，不覆盖: {}", output_dir.display()));
    }
    let payload = std::fs::read_to_string(bundle_path).map_err(|error| {
        format!(
            "读取运行 EvidenceBundle 失败 {}: {error}",
            bundle_path.display()
        )
    })?;
    let bundle = EvidenceBundle::from_json(&payload)?;
    let source_root = bundle_path.parent().unwrap_or_else(|| Path::new("."));
    let mut plan =
        qx_app::artifact_store::prepare_portable_run_evidence_export(bundle, source_root)?;
    let EvidenceGeneration::Run(run) = &plan.bundle.generation else {
        unreachable!("application export planner accepts only run generation")
    };
    let mut runtime_config_fingerprint_matches_run_hash = None;
    let runtime_config = if let Some(config_path) = runtime_config_path {
        let config = read_runtime_config(config_path)?;
        let python_modules = std::iter::once(&config.strategy).chain(config.strategies.iter());
        let unresolved_python_modules: Vec<_> = python_modules
            .clone()
            .filter(|strategy| strategy.external_executable.is_none())
            .filter_map(|strategy| strategy.python_module.as_deref())
            .filter(|reference| !is_path_like_python_module(reference))
            .collect();
        if !unresolved_python_modules.is_empty() {
            return Err(format!(
                "无法导出自包含运行证据包：Python 策略使用无法定位源文件的 import 模块名 {}；请将 python_module 配置为配置目录内的 .py 相对路径。该包只收录直接策略文件，不打包 Python 环境依赖",
                unresolved_python_modules.join(", ")
            ));
        }
        let config_fingerprint =
            declared_config_for_strategy_run(&config, &run.run.run_id)?.fingerprint()?;
        // 深度回测的 config_hash 是撮合参数、风险规则版本与输入指纹的合成摘要，
        // 不是 RuntimeConfig 指纹；verify-run --recompute 会用完整 RunManifest 对拍。
        let depth_run = run.dataset.kind.contains("depth");
        runtime_config_fingerprint_matches_run_hash =
            Some(config_fingerprint == run.run.config_hash);
        if config_fingerprint != run.run.config_hash && !depth_run {
            return Err(format!(
                "运行时配置指纹与 EvidenceBundle.run.config_hash 不一致: 配置={config_fingerprint} 证据={}",
                run.run.config_hash
            ));
        }
        let config_path = config_path
            .canonicalize()
            .map_err(|error| format!("解析 runtime 配置路径失败: {error}"))?;
        let config_root = config_path.parent().unwrap_or_else(|| Path::new("."));
        verify_declared_strategy_artifacts(&config, config_root)?;
        let references = configured_file_references(&config);
        let dependencies = qx_app::artifact_store::prepare_portable_run_evidence_dependencies(
            config_root,
            &references,
        )?;
        plan.sources.extend(dependencies.sources);
        let extra_artifacts = dependencies.artifacts;
        let prepared_config = qx_app::artifact_store::prepare_portable_run_evidence_file(
            &config_path,
            "runtime_config",
            "runtime/runtime.json",
        )?;
        plan.sources.push(prepared_config.source);
        Some((
            config_path.to_path_buf(),
            prepared_config.artifact,
            extra_artifacts,
        ))
    } else {
        None
    };
    let prepared_attestation = if let Some(attestation_path) = build_attestation_path {
        let Some((config_path, _, _)) = runtime_config.as_ref() else {
            return Err(
                "--build-attestation requires --runtime-config for strategy identity verification"
                    .into(),
            );
        };
        let config = read_runtime_config(config_path)?;
        let (attestation, materials) =
            crate::run_evidence_build_attestation::prepare_build_attestation(
                attestation_path,
                config_path,
                &config,
                &run.run.run_id,
            )?;
        plan.sources.extend(materials);
        Some(attestation)
    } else {
        None
    };
    let market_spec = if let Some(path) = market_spec_path {
        let prepared = qx_app::artifact_store::prepare_portable_run_evidence_file(
            path,
            "market_spec",
            "runtime/market-spec.json",
        )?;
        plan.sources.push(prepared.source);
        Some(prepared.artifact)
    } else {
        None
    };

    let runtime_config =
        runtime_config.map(|(_, config, runtime_dependencies)| (config, runtime_dependencies));
    let generated_files = qx_app::artifact_store::attach_portable_run_evidence_supplements(
        &mut plan,
        qx_app::artifact_store::PortableRunEvidenceSupplements {
            runtime_config: runtime_config.as_ref().map(|(config, _)| config.clone()),
            runtime_dependencies: runtime_config
                .map(|(_, dependencies)| dependencies)
                .unwrap_or_default(),
            market_spec,
            build_attestation: prepared_attestation,
        },
    )?;
    let referenced_files = qx_app::artifact_store::commit_portable_run_evidence_export(
        plan,
        output_dir,
        generated_files,
    )?;
    print_machine_json(&serde_json::json!({
        "ok": true,
        "operation": "run-evidence-exported",
        "bundle": output_dir.join("evidence.json").display().to_string(),
        "directory": output_dir.display().to_string(),
        "referenced_files": referenced_files,
        "self_contained_files": true,
        "runtime_config_included": runtime_config_path.is_some(),
        "runtime_config_fingerprint_matches_run_hash": runtime_config_fingerprint_matches_run_hash,
        "market_spec_included": market_spec_path.is_some(),
        "full_result_recompute": false
    }))?;
    Ok(())
}
