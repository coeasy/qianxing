//! Packaging orchestration for portable joint-account Evidence bundles.

use super::{
    account_directory_id, bind_portable_market_paths, joint_context, parent_artifact_sources,
    read_run_bundle, reserve_staging_directory, validate_joint_spec_identity,
    verify_child_bundle_identity, write_new, FileDigest, JointAccountEvidence,
    JointEvidenceManifest, MANIFEST_NAME, RESULT_NAME, SCHEMA_VERSION, SPEC_NAME,
};
use qx_app::{MultiAccountJointBacktestOutcome, MultiAccountJointBacktestSpec};
use qx_spec::EvidenceGeneration;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub(super) fn pack_multi_account_joint_run(
    result_path: &Path,
    spec_path: &Path,
    output_dir: &Path,
) -> Result<(), String> {
    if output_dir.exists() {
        return Err(format!("导出目录已存在，不覆盖: {}", output_dir.display()));
    }
    let result_bytes = std::fs::read(result_path)
        .map_err(|error| format!("读取联合账户结果失败 {}: {error}", result_path.display()))?;
    let result: MultiAccountJointBacktestOutcome = serde_json::from_slice(&result_bytes)
        .map_err(|error| format!("联合账户结果 JSON 无效: {error}"))?;
    let spec_bytes = std::fs::read(spec_path)
        .map_err(|error| format!("读取联合账户规格失败 {}: {error}", spec_path.display()))?;
    let spec_text = std::str::from_utf8(&spec_bytes)
        .map_err(|error| format!("联合账户规格不是 UTF-8 {}: {error}", spec_path.display()))?;
    let spec =
        MultiAccountJointBacktestSpec::from_json(spec_text).map_err(|error| error.to_string())?;
    validate_joint_spec_identity(&result, &spec)?;
    let source_report =
        qx_app::read_multi_account_joint_report_v1(&result, &joint_context(&result.run_id))
            .map_err(|error| format!("来源联合账户报告复核失败: {}", error.to_json()))?;
    if source_report.run_id != result.run_id
        || source_report.summary.accounts_total != result.accounts_total
        || source_report.summary.markets_total != result.markets_total
    {
        return Err("来源联合账户报告与结果摘要不一致".into());
    }

    let output_parent = output_dir
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(output_parent)
        .map_err(|error| format!("创建证据包父目录失败 {}: {error}", output_parent.display()))?;
    let staging = reserve_staging_directory(output_parent, output_dir)?;
    let mut portable_spec = spec.clone();
    portable_spec.output_dir = "recompute-output".into();
    let mut accounts = Vec::with_capacity(spec.accounts.len());
    let mut packaged_bundles = BTreeMap::<(String, String), PathBuf>::new();

    for account in &spec.accounts {
        let outcome = result
            .accounts
            .iter()
            .find(|member| {
                member.exchange_id == account.exchange_id && member.account_id == account.account_id
            })
            .ok_or_else(|| {
                format!(
                    "联合结果缺少账户 {}:{}",
                    account.exchange_id, account.account_id
                )
            })?;
        let child_artifact = Path::new(&outcome.outcome.artifact_path);
        let source_evidence =
            crate::run_evidence_multi_market::write_multi_market_run_evidence(child_artifact, None)
                .map_err(|error| {
                    format!(
                        "生成账户 {}:{} 的多市场 EvidenceBundle 失败: {error}",
                        account.exchange_id, account.account_id
                    )
                })?;
        crate::run_evidence_verify::verify_run_bundle_quiet(&source_evidence, false).map_err(
            |error| {
                format!(
                    "来源账户 {}:{} EvidenceBundle 未通过复核: {error}",
                    account.exchange_id, account.account_id
                )
            },
        )?;
        let source_bundle = read_run_bundle(&source_evidence)?;
        let EvidenceGeneration::Run(source_run) = &source_bundle.generation else {
            return Err("来源多市场 EvidenceBundle 不是 run 类型".into());
        };
        verify_child_bundle_identity(outcome, source_run)?;

        let evidence_dir = format!(
            "accounts/{}",
            account_directory_id(&account.exchange_id, &account.account_id)
        );
        let package_account_dir = staging.0.join(&evidence_dir);
        if let Some(parent) = package_account_dir.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("创建联合账户证据目录失败: {error}"))?;
        }
        crate::run_evidence_export::export_run_bundle_quiet(&source_evidence, &package_account_dir)
            .map_err(|error| {
                format!(
                    "导出账户 {}:{} 的便携 EvidenceBundle 失败: {error}",
                    account.exchange_id, account.account_id
                )
            })?;
        let portable_evidence_path = package_account_dir.join("evidence.json");
        crate::run_evidence_verify::verify_run_bundle_quiet(&portable_evidence_path, false)
            .map_err(|error| {
                format!(
                    "便携账户 {}:{} EvidenceBundle 验真失败: {error}",
                    account.exchange_id, account.account_id
                )
            })?;
        let portable_bundle = read_run_bundle(&portable_evidence_path)?;
        let EvidenceGeneration::Run(portable_run) = &portable_bundle.generation else {
            return Err("便携账户 EvidenceBundle 类型改变".into());
        };
        verify_child_bundle_identity(outcome, portable_run)?;
        bind_portable_market_paths(&mut portable_spec, account, &evidence_dir, portable_run)?;

        let evidence_sha256 = qx_strategy::file_digest::sha256_file_hex(&portable_evidence_path)?;
        packaged_bundles.insert(
            (account.exchange_id.clone(), account.account_id.clone()),
            portable_evidence_path,
        );
        accounts.push(JointAccountEvidence {
            exchange_id: account.exchange_id.clone(),
            account_id: account.account_id.clone(),
            run_id: outcome.outcome.run_id.clone(),
            result_hash: outcome.outcome.shared_result_hash.clone(),
            markets_total: outcome.outcome.markets_total,
            evidence_dir,
            evidence_sha256,
        });
    }
    accounts.sort_by(|left, right| {
        left.exchange_id
            .cmp(&right.exchange_id)
            .then_with(|| left.account_id.cmp(&right.account_id))
    });

    let mut parent_artifacts = BTreeMap::new();
    for (name, source, relative) in parent_artifact_sources(&result)? {
        let bytes = std::fs::read(&source).map_err(|error| {
            format!("读取联合父级 {name} 产物失败 {}: {error}", source.display())
        })?;
        let destination = staging.0.join(&relative);
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("创建联合父级工件目录失败: {error}"))?;
        }
        write_new(&destination, &bytes, &format!("联合父级 {name}"))?;
        parent_artifacts.insert(
            name,
            FileDigest {
                path: relative,
                sha256: qx_strategy::sha256_hex(&bytes),
            },
        );
    }

    let portable_spec_bytes = serde_json::to_vec_pretty(&portable_spec)
        .map_err(|error| format!("序列化便携联合账户规格失败: {error}"))?;
    write_new(
        &staging.0.join(SPEC_NAME),
        &portable_spec_bytes,
        "联合账户规格",
    )?;
    write_new(&staging.0.join(RESULT_NAME), &result_bytes, "联合账户结果")?;
    let manifest = JointEvidenceManifest {
        schema_version: SCHEMA_VERSION,
        kind: "qianxing-multi-account-joint-run-evidence".into(),
        run_id: result.run_id.clone(),
        settlement_currency: result.settlement_currency.clone(),
        spec_fingerprint: result.spec_fingerprint.clone(),
        result_hash: result
            .result_hash
            .clone()
            .ok_or("联合结果缺少 result_hash")?,
        result: FileDigest {
            path: RESULT_NAME.into(),
            sha256: qx_strategy::sha256_hex(&result_bytes),
        },
        spec: FileDigest {
            path: SPEC_NAME.into(),
            sha256: qx_strategy::sha256_hex(&portable_spec_bytes),
        },
        parent_artifacts,
        accounts,
    };
    let manifest_bytes = serde_json::to_vec_pretty(&manifest)
        .map_err(|error| format!("序列化联合账户 Evidence manifest 失败: {error}"))?;
    write_new(
        &staging.0.join(MANIFEST_NAME),
        &manifest_bytes,
        "联合账户 Evidence manifest",
    )?;
    std::fs::rename(&staging.0, output_dir).map_err(|error| {
        format!(
            "提交联合账户便携证据包失败 {} -> {}: {error}",
            staging.0.display(),
            output_dir.display()
        )
    })?;
    crate::print_machine_json(&serde_json::json!({
        "ok": true,
        "operation": "multi-account-joint-run-evidence-packed",
        "run_id": result.run_id,
        "directory": output_dir.display().to_string(),
        "manifest": output_dir.join(MANIFEST_NAME).display().to_string(),
        "accounts_verified": result.accounts_total,
        "markets_verified": result.markets_total,
        "portable": true,
        "signed": false
    }))
}
