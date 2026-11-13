//! Portable package assembly for multi-account depth Evidence.

use super::paths::{
    portable_artifact_path, portable_quality_manifest_path, validate_portable_relative,
    verify_portable_run_paths,
};
use super::{
    account_directory_id, locate_account_bundle, read_run_bundle, reserve_staging_directory,
    stable_depth_spec_fingerprint, validate_depth_batch_identity, validate_successful_outcomes,
    verify_bundle_summary_account, verify_member_evidence, write_new, AccountEvidence,
    AccountInputEvidence, FileDigest, MultiAccountEvidenceManifest, MANIFEST_NAME, RESULT_NAME,
    SCHEMA_VERSION, SPEC_NAME,
};
use qx_app::{MultiAccountDepthBacktestResult, MultiAccountDepthBacktestSpec};
use qx_spec::EvidenceGeneration;
use std::collections::BTreeMap;
use std::path::Path;

pub(super) fn pack_multi_account_depth_run(
    result_path: &Path,
    spec_path: &Path,
    output_dir: &Path,
) -> Result<(), String> {
    if output_dir.exists() {
        return Err(format!("导出目录已存在，不覆盖: {}", output_dir.display()));
    }
    let result_bytes = std::fs::read(result_path)
        .map_err(|error| format!("读取多账户结果失败 {}: {error}", result_path.display()))?;
    let spec_bytes = std::fs::read(spec_path)
        .map_err(|error| format!("读取多账户规格失败 {}: {error}", spec_path.display()))?;
    let result: MultiAccountDepthBacktestResult = serde_json::from_slice(&result_bytes)
        .map_err(|error| format!("多账户结果 JSON 无效: {error}"))?;
    let spec_text = std::str::from_utf8(&spec_bytes)
        .map_err(|error| format!("多账户规格不是 UTF-8: {error}"))?;
    let spec =
        MultiAccountDepthBacktestSpec::from_json(spec_text).map_err(|error| error.to_string())?;
    validate_depth_batch_identity(&result, &spec)?;
    validate_successful_outcomes(&result, &spec)?;

    let parent = output_dir
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("创建批次证据包父目录失败 {}: {error}", parent.display()))?;
    let staging = reserve_staging_directory(parent, output_dir)?;
    let members = spec
        .runs
        .iter()
        .map(|member| (member.account_id.as_deref().unwrap_or_default(), member))
        .collect::<BTreeMap<_, _>>();
    let mut accounts = Vec::with_capacity(result.summary.accounts_succeeded);
    let mut inputs = Vec::with_capacity(spec.runs.len());
    let mut portable_result = result.clone();
    let mut portable_spec = spec.clone();
    for run in result.runs.iter().filter(|run| run.outcome.is_some()) {
        let Some(outcome) = run.outcome.as_ref() else {
            continue;
        };
        let member = members
            .get(run.account_id.as_str())
            .ok_or_else(|| format!("成功账户 {} 不在批次规格中", run.account_id))?;
        let source_bundle = locate_account_bundle(Path::new(&outcome.artifacts.summary))?;
        crate::run_evidence_verify::verify_run_bundle_quiet(&source_bundle, false).map_err(
            |error| {
                format!(
                    "账户 {} 的来源 EvidenceBundle 未通过复核: {error}",
                    run.account_id
                )
            },
        )?;
        let bundle = read_run_bundle(&source_bundle)?;
        let EvidenceGeneration::Run(evidence) = &bundle.generation else {
            return Err(format!(
                "账户 {} 的来源 EvidenceBundle 不是 run 类型",
                run.account_id
            ));
        };
        verify_member_evidence(run, outcome, evidence)?;
        verify_bundle_summary_account(&source_bundle, &bundle, &run.account_id)?;

        let evidence_dir = format!("accounts/{}", account_directory_id(&run.account_id));
        let portable_dir = staging.0.join(&evidence_dir);
        if let Some(parent) = portable_dir.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("创建账户证据子目录失败 {}: {error}", parent.display()))?;
        }
        crate::run_evidence_export::export_run_bundle_quiet(&source_bundle, &portable_dir)
            .map_err(|error| format!("导出账户 {} 的便携证据失败: {error}", run.account_id))?;
        let portable_bundle = portable_dir.join("evidence.json");
        crate::run_evidence_verify::verify_run_bundle_quiet(&portable_bundle, false).map_err(
            |error| {
                format!(
                    "账户 {} 的便携 EvidenceBundle 未通過复核: {error}",
                    run.account_id
                )
            },
        )?;
        let portable = read_run_bundle(&portable_bundle)?;
        let EvidenceGeneration::Run(portable_evidence) = &portable.generation else {
            return Err("便携账户 EvidenceBundle 类型改变".into());
        };
        verify_member_evidence(run, outcome, portable_evidence)?;
        let relative_dataset = validate_portable_relative(&portable_evidence.dataset.path)?;
        let depth_path = format!("{evidence_dir}/{relative_dataset}");
        let quality_manifest_path = portable_quality_manifest_path(
            evidence_dir.as_str(),
            portable_evidence.dataset.quality_manifest_path.as_deref(),
        )?;
        if member.quality_manifest_path.is_some() != quality_manifest_path.is_some() {
            return Err(format!(
                "账户 {} 的质量清单规格与 EvidenceBundle 不一致",
                run.account_id
            ));
        }
        let packaged_member = portable_spec
            .runs
            .iter_mut()
            .find(|candidate| candidate.account_id.as_deref() == Some(run.account_id.as_str()))
            .ok_or("便携规格中缺少成功账户")?;
        packaged_member.depth_path = depth_path.clone();
        packaged_member.quality_manifest_path = quality_manifest_path.clone();
        packaged_member.runtime_config_path = None;
        packaged_member.market_spec_path = None;
        packaged_member.output_dir = format!("{evidence_dir}/replay-output");
        let packaged_run = portable_result
            .runs
            .iter_mut()
            .find(|candidate| candidate.account_id == run.account_id)
            .and_then(|candidate| candidate.outcome.as_mut())
            .ok_or("便携结果中缺少成功账户")?;
        packaged_run.artifacts.run_manifest =
            portable_artifact_path(&evidence_dir, portable_evidence, "run_manifest")?;
        packaged_run.artifacts.summary =
            portable_artifact_path(&evidence_dir, portable_evidence, "summary")?;
        packaged_run.artifacts.equity =
            portable_artifact_path(&evidence_dir, portable_evidence, "equity")?;
        packaged_run.artifacts.fills =
            portable_artifact_path(&evidence_dir, portable_evidence, "fills")?;
        packaged_run.artifacts.event_trace = portable_evidence
            .artifacts
            .iter()
            .find(|artifact| artifact.name == "event_trace")
            .map(|artifact| format!("{evidence_dir}/{}", artifact.path));
        verify_portable_run_paths(
            &evidence_dir,
            packaged_member,
            packaged_run,
            portable_evidence,
        )?;
        let packaged_depth = staging.0.join(&depth_path);
        let depth_sha256 = qx_strategy::file_digest::sha256_file_hex(&packaged_depth)?;
        let quality_manifest_sha256 = quality_manifest_path
            .as_deref()
            .map(|relative| {
                let path = staging.0.join(relative);
                let digest = qx_strategy::file_digest::sha256_file_hex(&path)?;
                if portable_evidence.dataset.quality_manifest_sha256.as_deref()
                    != Some(digest.as_str())
                {
                    return Err(format!(
                        "账户 {} 的便携质量清单摘要与 EvidenceBundle 不一致",
                        run.account_id
                    ));
                }
                Ok::<String, String>(digest)
            })
            .transpose()?;
        inputs.push(AccountInputEvidence {
            account_id: run.account_id.clone(),
            path: depth_path,
            sha256: Some(depth_sha256),
            missing: false,
            quality_manifest_path: quality_manifest_path.clone(),
            quality_manifest_sha256,
            quality_manifest_missing: quality_manifest_path.as_ref().map(|_| false),
        });
        let evidence_sha256 = qx_strategy::file_digest::sha256_file_hex(&portable_bundle)?;
        accounts.push(AccountEvidence {
            account_id: run.account_id.clone(),
            run_id: outcome.run_id.clone(),
            result_hash: outcome.result_hash.clone(),
            instrument: outcome.instrument.clone(),
            strategy_id: portable_evidence.identity.strategy_id.clone(),
            data_fingerprint: portable_evidence.dataset.content_fingerprint.clone(),
            evidence_dir,
            evidence_sha256,
        });
    }
    accounts.sort_by(|left, right| left.account_id.cmp(&right.account_id));
    for run in result.runs.iter().filter(|run| run.outcome.is_none()) {
        let member = members
            .get(run.account_id.as_str())
            .ok_or_else(|| format!("失败账户 {} 不在批次规格中", run.account_id))?;
        let evidence_dir = format!("accounts/{}/inputs", account_directory_id(&run.account_id));
        let relative_depth = format!("{evidence_dir}/depth.json");
        let packaged_path = staging.0.join(&relative_depth);
        let parent = packaged_path.parent().ok_or("失败账户输入路径缺少父目录")?;
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("创建失败账户输入目录失败 {}: {error}", parent.display()))?;
        let source = Path::new(&member.depth_path);
        let (sha256, missing) = match std::fs::read(source) {
            Ok(bytes) => {
                write_new(&packaged_path, &bytes, "失败账户原始 DepthFrame")?;
                (Some(qx_strategy::sha256_hex(&bytes)), false)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (None, true),
            Err(error) => {
                return Err(format!(
                    "读取失败账户 {} 的原始 DepthFrame 失败 {}: {error}",
                    run.account_id,
                    source.display()
                ));
            }
        };
        let packaged_member = portable_spec
            .runs
            .iter_mut()
            .find(|candidate| candidate.account_id.as_deref() == Some(run.account_id.as_str()))
            .ok_or("便携规格中缺少失败账户")?;
        packaged_member.depth_path = relative_depth.clone();
        let (quality_manifest_path, quality_manifest_sha256, quality_manifest_missing) =
            match member.quality_manifest_path.as_deref() {
                Some(source) => {
                    let relative = format!("{evidence_dir}/quality-manifest.json");
                    let packaged = staging.0.join(&relative);
                    match std::fs::read(source) {
                        Ok(bytes) => {
                            write_new(&packaged, &bytes, "失败账户质量清单")?;
                            (
                                Some(relative),
                                Some(qx_strategy::sha256_hex(&bytes)),
                                Some(false),
                            )
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                            (Some(relative), None, Some(true))
                        }
                        Err(error) => {
                            return Err(format!(
                                "读取失败账户 {} 的质量清单失败 {}: {error}",
                                run.account_id, source
                            ));
                        }
                    }
                }
                None => (None, None, None),
            };
        packaged_member.quality_manifest_path = quality_manifest_path.clone();
        packaged_member.runtime_config_path = None;
        packaged_member.market_spec_path = None;
        packaged_member.output_dir = format!(
            "accounts/{}/replay-output",
            account_directory_id(&run.account_id)
        );
        inputs.push(AccountInputEvidence {
            account_id: run.account_id.clone(),
            path: relative_depth,
            sha256,
            missing,
            quality_manifest_path,
            quality_manifest_sha256,
            quality_manifest_missing,
        });
    }
    inputs.sort_by(|left, right| left.account_id.cmp(&right.account_id));
    let portable_spec_fingerprint = stable_depth_spec_fingerprint(&portable_spec)?;
    if portable_result.spec_fingerprint != portable_spec_fingerprint {
        // Older depth batch fingerprints included local quality/config/spec paths. Normalize
        // the portable copy while preserving all behavior and child-run content identities.
        portable_result.spec_fingerprint = portable_spec_fingerprint.clone();
        if let Some(parent_manifest) = portable_result.run_manifest.as_mut() {
            parent_manifest.config_hash = portable_spec_fingerprint;
        }
    }
    let portable_result_bytes = serde_json::to_vec_pretty(&portable_result)
        .map_err(|error| format!("序列化便携多账户结果失败: {error}"))?;
    let portable_spec_bytes = serde_json::to_vec_pretty(&portable_spec)
        .map_err(|error| format!("序列化便携多账户规格失败: {error}"))?;
    write_new(
        &staging.0.join(RESULT_NAME),
        &portable_result_bytes,
        "多账户结果",
    )?;
    write_new(
        &staging.0.join(SPEC_NAME),
        &portable_spec_bytes,
        "多账户规格",
    )?;
    let manifest = MultiAccountEvidenceManifest {
        schema_version: SCHEMA_VERSION,
        kind: "qianxing-multi-account-depth-run-evidence".into(),
        batch_id: result.batch_id.clone(),
        settlement_currency: result.settlement_currency.clone(),
        spec_fingerprint: portable_result.spec_fingerprint.clone(),
        result: FileDigest {
            path: RESULT_NAME.into(),
            sha256: qx_strategy::sha256_hex(&portable_result_bytes),
        },
        spec: FileDigest {
            path: SPEC_NAME.into(),
            sha256: qx_strategy::sha256_hex(&portable_spec_bytes),
        },
        accounts,
        inputs,
    };
    let manifest_bytes = serde_json::to_vec_pretty(&manifest)
        .map_err(|error| format!("序列化多账户 Evidence manifest 失败: {error}"))?;
    write_new(
        &staging.0.join(MANIFEST_NAME),
        &manifest_bytes,
        "批次 Evidence manifest",
    )?;
    std::fs::rename(&staging.0, output_dir).map_err(|error| {
        format!(
            "提交多账户便携证据包失败 {} -> {}: {error}",
            staging.0.display(),
            output_dir.display()
        )
    })?;
    crate::print_machine_json(&serde_json::json!({
        "ok": true,
        "operation": "multi-account-depth-run-evidence-packed",
        "batch_id": result.batch_id,
        "directory": output_dir.display().to_string(),
        "manifest": output_dir.join(MANIFEST_NAME).display().to_string(),
        "accounts_verified": result.summary.accounts_succeeded,
        "accounts_failed": result.summary.accounts_failed,
        "portable": true,
        "signed": false
    }))
}
