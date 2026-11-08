//! Verification orchestration for portable multi-account depth Evidence packages.

use super::{
    account_directory_id, checked_package_file, read_run_bundle, recompute,
    validate_depth_batch_identity, validate_successful_outcomes, verify_bundle_summary_account,
    verify_digest, verify_member_evidence, verify_packaged_inputs, verify_portable_run_paths,
    MultiAccountEvidenceManifest, MANIFEST_NAME, RESULT_NAME, SCHEMA_VERSION, SPEC_NAME,
};
use qx_app::{MultiAccountDepthBacktestResult, MultiAccountDepthBacktestSpec};
use qx_spec::EvidenceGeneration;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub(super) fn verify_multi_account_depth_run(
    bundle_dir: &Path,
    recompute: bool,
) -> Result<(), String> {
    let root = bundle_dir
        .canonicalize()
        .map_err(|error| format!("解析多账户证据目录失败 {}: {error}", bundle_dir.display()))?;
    if !root.is_dir() {
        return Err("多账户便携证据路径必须是目录".into());
    }
    let manifest_path = root.join(MANIFEST_NAME);
    let manifest: MultiAccountEvidenceManifest = serde_json::from_slice(
        &std::fs::read(&manifest_path)
            .map_err(|error| format!("读取批次 Evidence manifest 失败: {error}"))?,
    )
    .map_err(|error| format!("批次 Evidence manifest JSON 无效: {error}"))?;
    if manifest.schema_version != SCHEMA_VERSION
        || manifest.kind != "qianxing-multi-account-depth-run-evidence"
        || manifest.result.path != RESULT_NAME
        || manifest.spec.path != SPEC_NAME
    {
        return Err("多账户 Evidence manifest 版本、类型或固定文件路径无效".into());
    }
    let result_path = checked_package_file(&root, &manifest.result.path)?;
    let spec_path = checked_package_file(&root, &manifest.spec.path)?;
    verify_digest(&result_path, &manifest.result.sha256, "多账户结果")?;
    verify_digest(&spec_path, &manifest.spec.sha256, "多账户规格")?;
    let result: MultiAccountDepthBacktestResult = serde_json::from_slice(
        &std::fs::read(&result_path).map_err(|error| format!("读取便携多账户结果失败: {error}"))?,
    )
    .map_err(|error| format!("便携多账户结果 JSON 无效: {error}"))?;
    let spec_text = std::fs::read_to_string(&spec_path)
        .map_err(|error| format!("读取便携多账户规格失败: {error}"))?;
    let spec =
        MultiAccountDepthBacktestSpec::from_json(&spec_text).map_err(|error| error.to_string())?;
    validate_depth_batch_identity(&result, &spec)?;
    if manifest.batch_id != result.batch_id
        || manifest.settlement_currency != result.settlement_currency
        || manifest.spec_fingerprint != result.spec_fingerprint
    {
        return Err("Evidence manifest 与批次结果/规格身份不一致".into());
    }
    validate_successful_outcomes(&result, &spec)?;
    verify_packaged_inputs(&root, &manifest, &spec, &result)?;

    let expected = result
        .runs
        .iter()
        .filter_map(|run| run.outcome.as_ref().map(|outcome| (run, outcome)))
        .map(|(run, outcome)| (run.account_id.as_str(), (run, outcome)))
        .collect::<BTreeMap<_, _>>();
    let mut seen = BTreeSet::new();
    let members = spec
        .runs
        .iter()
        .map(|member| (member.account_id.as_deref().unwrap_or_default(), member))
        .collect::<BTreeMap<_, _>>();
    for account in &manifest.accounts {
        if !seen.insert(account.account_id.as_str()) {
            return Err(format!(
                "Evidence manifest 重复账户: {}",
                account.account_id
            ));
        }
        let (batch_run, outcome) = expected.get(account.account_id.as_str()).ok_or_else(|| {
            format!(
                "Evidence manifest 包含未知/失败账户: {}",
                account.account_id
            )
        })?;
        let member = members
            .get(account.account_id.as_str())
            .ok_or("多账户规格缺少 Evidence manifest 账户")?;
        if account.run_id != outcome.run_id
            || account.result_hash != outcome.result_hash
            || account.instrument != outcome.instrument
            || account.account_id != batch_run.account_id
            || account.evidence_dir
                != format!("accounts/{}", account_directory_id(&account.account_id))
        {
            return Err(format!(
                "账户 {} 的 Evidence manifest 身份不一致",
                account.account_id
            ));
        }
        let portable_dir = checked_package_file(&root, &account.evidence_dir)?;
        if !portable_dir.is_dir() {
            return Err(format!("账户 {} 的便携证据目录缺失", account.account_id));
        }
        let portable_bundle = portable_dir.join("evidence.json");
        verify_digest(
            &portable_bundle,
            &account.evidence_sha256,
            "账户 EvidenceBundle",
        )?;
        crate::run_evidence_verify::verify_run_bundle_quiet(&portable_bundle, false).map_err(
            |error| {
                format!(
                    "账户 {} 的便携 EvidenceBundle 验真失败: {error}",
                    account.account_id
                )
            },
        )?;
        let bundle = read_run_bundle(&portable_bundle)?;
        let EvidenceGeneration::Run(evidence) = &bundle.generation else {
            return Err(format!(
                "账户 {} 的 EvidenceBundle 类型无效",
                account.account_id
            ));
        };
        verify_member_evidence(batch_run, outcome, evidence)?;
        verify_portable_run_paths(&account.evidence_dir, member, outcome, evidence)?;
        if account.strategy_id != evidence.identity.strategy_id
            || account.data_fingerprint != evidence.dataset.content_fingerprint
        {
            return Err(format!(
                "账户 {} 的策略或数据指纹与 manifest 不一致",
                account.account_id
            ));
        }
        verify_bundle_summary_account(&portable_bundle, &bundle, &account.account_id)?;
    }
    if seen.len() != expected.len() {
        return Err("Evidence manifest 未覆盖全部成功账户".into());
    }
    if recompute {
        recompute::recompute_multi_account_depth_run(&root, &manifest, &spec, &result)?;
    }
    crate::print_machine_json(&serde_json::json!({
        "ok": true,
        "operation": "multi-account-depth-run-evidence-verified",
        "batch_id": result.batch_id,
        "verified_account_count": seen.len(),
        "failed_account_count": result.summary.accounts_failed,
        "input_identity_recomputed": true,
        "result_recomputed": recompute,
        "signed": false
    }))
}
