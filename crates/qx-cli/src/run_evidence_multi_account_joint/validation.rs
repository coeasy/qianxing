//! Joint batch and child Evidence identity validation shared by pack and verify.

use qx_app::{
    JointAccountBacktestResult, MultiAccountJointBacktestOutcome, MultiAccountJointBacktestSpec,
};
use qx_core::Fnv1a;
use qx_spec::RunEvidencePayload;

pub(super) fn validate_joint_spec_identity(
    outcome: &MultiAccountJointBacktestOutcome,
    spec: &MultiAccountJointBacktestSpec,
) -> Result<(), String> {
    spec.validate().map_err(|error| error.to_string())?;
    let fingerprint = stable_joint_spec_fingerprint(spec)?;
    if outcome.run_id != spec.run_id
        || outcome.settlement_currency != spec.settlement_currency
        || outcome.accounts_total != spec.accounts.len()
        || outcome.spec_fingerprint != fingerprint
        || outcome.result_hash.is_none()
        || outcome.run_manifest_path.is_none()
        || outcome.run_record.is_none()
        || outcome.artifact_locations.is_none()
    {
        return Err("联合账户结果与规格/父级工件身份不一致或缺少 V2 工件指针".into());
    }
    Ok(())
}

pub(super) fn stable_joint_spec_fingerprint(
    spec: &MultiAccountJointBacktestSpec,
) -> Result<String, String> {
    let mut canonical = spec.clone();
    canonical.accounts.sort_by(|left, right| {
        left.exchange_id
            .cmp(&right.exchange_id)
            .then_with(|| left.account_id.cmp(&right.account_id))
    });
    canonical.output_dir.clear();
    for account in &mut canonical.accounts {
        for market in &mut account.markets {
            market.bars_path.clear();
            market.dataset_manifest_path = None;
        }
    }
    let json = canonical
        .to_json()
        .map_err(|error| format!("规范化联合账户规格失败: {}", error.to_json()))?;
    let mut hasher = Fnv1a::new();
    hasher.write_text(&json);
    Ok(format!("{:016x}", hasher.finish()))
}

pub(super) fn verify_child_bundle_identity(
    account: &JointAccountBacktestResult,
    evidence: &RunEvidencePayload,
) -> Result<(), String> {
    if evidence.run.run_id != account.outcome.run_id
        || evidence.run.result_hash != account.outcome.shared_result_hash
        || evidence.run.data_fingerprint != account.outcome.data_fingerprint
        || evidence.dataset.components.len() != account.outcome.markets_total
        || evidence.dataset.kind != "multi-market-barframe"
    {
        return Err(format!(
            "账户 {}:{} 的 EvidenceBundle 与子运行身份不匹配",
            account.exchange_id, account.account_id
        ));
    }
    Ok(())
}
