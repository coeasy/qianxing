//! Verification orchestration for portable joint-account Evidence packages.

use super::{
    account_directory_id, build_path_map_reader, checked_package_dir, checked_package_file,
    joint_context, read_run_bundle, recompute, validate_joint_spec_identity,
    verify_child_bundle_identity, verify_digest, verify_portable_market_paths,
    JointEvidenceManifest, MANIFEST_NAME, RESULT_NAME, SCHEMA_VERSION, SPEC_NAME,
};
use qx_app::{MultiAccountJointBacktestOutcome, MultiAccountJointBacktestSpec};
use qx_spec::EvidenceGeneration;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub(super) fn verify_multi_account_joint_run(
    bundle_dir: &Path,
    recompute: bool,
) -> Result<(), String> {
    let root = bundle_dir
        .canonicalize()
        .map_err(|error| format!("解析联合账户证据目录失败 {}: {error}", bundle_dir.display()))?;
    if !root.is_dir() {
        return Err("联合账户便携证据路径必须是目录".into());
    }
    let manifest_path = root.join(MANIFEST_NAME);
    let manifest: JointEvidenceManifest = serde_json::from_slice(
        &std::fs::read(&manifest_path)
            .map_err(|error| format!("读取联合账户 Evidence manifest 失败: {error}"))?,
    )
    .map_err(|error| format!("联合账户 Evidence manifest JSON 无效: {error}"))?;
    if manifest.schema_version != SCHEMA_VERSION
        || manifest.kind != "qianxing-multi-account-joint-run-evidence"
        || manifest.result.path != RESULT_NAME
        || manifest.spec.path != SPEC_NAME
        || manifest
            .parent_artifacts
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>()
            != BTreeSet::from([
                "joint_equity",
                "joint_fills",
                "joint_portfolio",
                "run_manifest",
                "run_record",
            ])
    {
        return Err("联合账户 Evidence manifest 版本、类型或固定文件清单无效".into());
    }
    let result_path = checked_package_file(&root, &manifest.result.path)?;
    let spec_path = checked_package_file(&root, &manifest.spec.path)?;
    verify_digest(&result_path, &manifest.result.sha256, "联合账户结果")?;
    verify_digest(&spec_path, &manifest.spec.sha256, "联合账户规格")?;
    let outcome: MultiAccountJointBacktestOutcome = serde_json::from_slice(
        &std::fs::read(&result_path).map_err(|error| format!("读取便携联合结果失败: {error}"))?,
    )
    .map_err(|error| format!("便携联合结果 JSON 无效: {error}"))?;
    let portable_spec_text = std::fs::read_to_string(&spec_path)
        .map_err(|error| format!("读取便携联合规格失败: {error}"))?;
    let portable_spec = MultiAccountJointBacktestSpec::from_json(&portable_spec_text)
        .map_err(|error| error.to_string())?;
    validate_joint_spec_identity(&outcome, &portable_spec)?;
    if manifest.run_id != outcome.run_id
        || manifest.settlement_currency != outcome.settlement_currency
        || manifest.spec_fingerprint != outcome.spec_fingerprint
        || manifest.result_hash != outcome.result_hash.as_deref().unwrap_or_default()
        || manifest.accounts.len() != outcome.accounts_total
        || portable_spec.accounts.len() != outcome.accounts_total
    {
        return Err("Evidence manifest 与联合结果/便携规格身份不一致".into());
    }
    for digest in manifest.parent_artifacts.values() {
        let path = checked_package_file(&root, &digest.path)?;
        verify_digest(&path, &digest.sha256, "联合父级产物")?;
    }

    let expected_accounts = outcome
        .accounts
        .iter()
        .map(|account| {
            (
                (account.exchange_id.as_str(), account.account_id.as_str()),
                account,
            )
        })
        .collect::<BTreeMap<_, _>>();
    let portable_accounts = portable_spec
        .accounts
        .iter()
        .map(|account| {
            (
                (account.exchange_id.as_str(), account.account_id.as_str()),
                account,
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut seen = BTreeSet::new();
    let mut portable_bundles = BTreeMap::new();
    for entry in &manifest.accounts {
        let key = (entry.exchange_id.as_str(), entry.account_id.as_str());
        if !seen.insert(key) {
            return Err(format!("Evidence manifest 重复账户 {}:{}", key.0, key.1));
        }
        let member = expected_accounts
            .get(&key)
            .ok_or_else(|| format!("Evidence manifest 有未知账户 {}:{}", key.0, key.1))?;
        let portable_member = portable_accounts
            .get(&key)
            .ok_or_else(|| format!("便携规格缺少账户 {}:{}", key.0, key.1))?;
        if entry.run_id != member.outcome.run_id
            || entry.result_hash != member.outcome.shared_result_hash
            || entry.markets_total != member.outcome.markets_total
            || entry.evidence_dir != format!("accounts/{}", account_directory_id(key.0, key.1))
        {
            return Err(format!("账户 {}:{} 的证据身份不匹配", key.0, key.1));
        }
        let account_dir = checked_package_dir(&root, &entry.evidence_dir)
            .map_err(|error| format!("账户 {}:{} 的证据目录无效: {error}", key.0, key.1))?;
        let bundle_path = account_dir.join("evidence.json");
        verify_digest(&bundle_path, &entry.evidence_sha256, "账户 EvidenceBundle")?;
        crate::run_evidence_verify::verify_run_bundle_quiet(&bundle_path, false).map_err(
            |error| {
                format!(
                    "账户 {}:{} portable EvidenceBundle 验真失败: {error}",
                    key.0, key.1
                )
            },
        )?;
        let bundle = read_run_bundle(&bundle_path)?;
        let EvidenceGeneration::Run(run) = &bundle.generation else {
            return Err("联合账户子项 EvidenceBundle 不是 run 类型".into());
        };
        verify_child_bundle_identity(member, run)?;
        verify_portable_market_paths(&root, &entry.evidence_dir, portable_member, run)?;
        portable_bundles.insert((key.0.to_string(), key.1.to_string()), bundle_path);
    }
    if seen.len() != expected_accounts.len() {
        return Err("Evidence manifest 未覆盖全部联合账户".into());
    }

    let reader = build_path_map_reader(&root, &manifest, &outcome, &portable_bundles)?;
    let report = qx_app::read_multi_account_joint_report_v1_with_reader_and_options(
        &outcome,
        &joint_context(&outcome.run_id),
        &reader,
        &qx_app::MultiAccountJointReportReadOptionsV1::default(),
    )
    .map_err(|error| format!("便携联合账户报告复核失败: {}", error.to_json()))?;
    if report.run_id != outcome.run_id
        || report.summary.accounts_total != outcome.accounts_total
        || report.summary.markets_total != outcome.markets_total
        || report.summary.initial_cash_raw != outcome.initial_cash_raw.to_string()
        || report.summary.final_equity_raw != outcome.final_equity_raw.to_string()
        || report.summary.return_bps != outcome.return_bps
        || report.summary.max_drawdown_bps != outcome.max_drawdown_bps
        || report.summary.successful_fills != outcome.successful_fills
    {
        return Err("便携联合账户报告汇总与结果 DTO 不一致".into());
    }
    if recompute {
        recompute::recompute_joint_run(&root, &manifest, &portable_spec, &outcome)?;
    }
    crate::print_machine_json(&serde_json::json!({
        "ok": true,
        "operation": "multi-account-joint-run-evidence-verified",
        "run_id": outcome.run_id,
        "verified_account_count": seen.len(),
        "verified_market_count": outcome.markets_total,
        "input_identity_recomputed": true,
        "result_recomputed": recompute,
        "signed": false
    }))
}
