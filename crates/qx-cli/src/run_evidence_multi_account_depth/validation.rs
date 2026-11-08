//! Identity and source Evidence validation shared by depth evidence pack and verify.

use qx_app::{MultiAccountDepthBacktestResult, MultiAccountDepthBacktestSpec};
use qx_core::Fnv1a;
use qx_spec::{EvidenceBundle, EvidenceGeneration};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub(crate) fn validate_depth_batch_identity(
    result: &MultiAccountDepthBacktestResult,
    spec: &MultiAccountDepthBacktestSpec,
) -> Result<(), String> {
    if result.batch_id != spec.batch_id
        || result.settlement_currency
            != spec
                .runs
                .first()
                .map(|run| run.settlement_currency.as_str())
                .unwrap_or_default()
    {
        return Err("多账户深度结果与规格的 batch_id/settlement_currency 不一致".into());
    }
    let expected = spec
        .runs
        .iter()
        .map(|run| run.account_id.as_deref().unwrap_or_default())
        .collect::<BTreeSet<_>>();
    let actual = result
        .runs
        .iter()
        .map(|run| run.account_id.as_str())
        .collect::<BTreeSet<_>>();
    let succeeded = result
        .runs
        .iter()
        .filter(|run| run.outcome.is_some())
        .count();
    if expected != actual
        || result.runs.len() != spec.runs.len()
        || result.summary.accounts_total != result.runs.len()
        || result.summary.accounts_succeeded != succeeded
        || result.summary.accounts_failed != result.runs.len() - succeeded
        || result.summary.complete != (succeeded == result.runs.len())
    {
        return Err("多账户深度结果的账户集合或汇总计数与规格/逐账户状态不一致".into());
    }
    let stable_fingerprint = stable_depth_spec_fingerprint(spec)?;
    let mut legacy = spec.clone();
    legacy
        .runs
        .sort_by(|left, right| left.account_id.cmp(&right.account_id));
    for run in &mut legacy.runs {
        run.depth_path.clear();
        run.output_dir.clear();
    }
    let legacy_json = legacy.to_json().map_err(|error| error.to_string())?;
    let mut legacy_hasher = Fnv1a::new();
    legacy_hasher.write_text(&legacy_json);
    let legacy_fingerprint = format!("{:016x}", legacy_hasher.finish());
    if result.spec_fingerprint != stable_fingerprint
        && result.spec_fingerprint != legacy_fingerprint
    {
        return Err("多账户深度结果 spec_fingerprint 与输入规格不一致".into());
    }
    Ok(())
}

pub(crate) fn stable_depth_spec_fingerprint(
    spec: &MultiAccountDepthBacktestSpec,
) -> Result<String, String> {
    let mut canonical = spec.clone();
    canonical
        .runs
        .sort_by(|left, right| left.account_id.cmp(&right.account_id));
    for run in &mut canonical.runs {
        run.depth_path.clear();
        run.output_dir.clear();
        run.quality_manifest_path = None;
        run.runtime_config_path = None;
        run.market_spec_path = None;
    }
    let canonical = canonical.to_json().map_err(|error| error.to_string())?;
    let mut hasher = Fnv1a::new();
    hasher.write_text(&canonical);
    Ok(format!("{:016x}", hasher.finish()))
}

pub(crate) fn validate_successful_outcomes(
    result: &MultiAccountDepthBacktestResult,
    spec: &MultiAccountDepthBacktestSpec,
) -> Result<(), String> {
    let members = spec
        .runs
        .iter()
        .map(|member| (member.account_id.as_deref().unwrap_or_default(), member))
        .collect::<BTreeMap<_, _>>();
    for run in result.runs.iter().filter(|run| run.outcome.is_some()) {
        let Some(outcome) = run.outcome.as_ref() else {
            continue;
        };
        let member = members
            .get(run.account_id.as_str())
            .ok_or_else(|| format!("成功账户 {} 不在批次规格中", run.account_id))?;
        if outcome.account_id.as_deref() != Some(run.account_id.as_str())
            || outcome.run_id != member.run_id
            || outcome.tier != member.tier
        {
            return Err(format!(
                "账户 {} 的成功结果与规格身份不一致",
                run.account_id
            ));
        }
    }
    Ok(())
}

pub(crate) fn verify_member_evidence(
    batch_run: &qx_app::MultiAccountDepthRunResult,
    outcome: &qx_app::DepthBacktestOutcome,
    evidence: &qx_spec::RunEvidencePayload,
) -> Result<(), String> {
    if batch_run.account_id != outcome.account_id.as_deref().unwrap_or_default()
        || evidence.run.run_id != outcome.run_id
        || evidence.run.result_hash != outcome.result_hash
        || evidence.run.data_fingerprint != outcome.data_fingerprint
        || evidence.identity.instrument != outcome.instrument
        || evidence.dataset.kind != "depth-frame"
    {
        return Err(format!(
            "账户 {} 的 EvidenceBundle 与批次结果不一致",
            batch_run.account_id
        ));
    }
    Ok(())
}

pub(crate) fn verify_bundle_summary_account(
    bundle_path: &Path,
    bundle: &EvidenceBundle,
    account_id: &str,
) -> Result<(), String> {
    let EvidenceGeneration::Run(evidence) = &bundle.generation else {
        return Err("账户 EvidenceBundle 类型无效".into());
    };
    let summary = evidence
        .artifacts
        .iter()
        .find(|artifact| artifact.name == "summary")
        .ok_or("账户 EvidenceBundle 缺少 summary")?;
    let root = bundle_path.parent().unwrap_or(Path::new("."));
    let path = crate::run_evidence_recompute::resolve_reference(root, &summary.path)?;
    let path = path
        .canonicalize()
        .map_err(|error| format!("解析账户 summary 路径失败: {error}"))?;
    let root = root
        .canonicalize()
        .map_err(|error| format!("解析账户 EvidenceBundle 目录失败: {error}"))?;
    if !path.starts_with(&root) {
        return Err(format!(
            "账户 {account_id} 的 summary 路径逃逸出 EvidenceBundle"
        ));
    }
    let payload =
        std::fs::read(&path).map_err(|error| format!("读取账户 summary 失败: {error}"))?;
    let value: serde_json::Value = serde_json::from_slice(&payload)
        .map_err(|error| format!("账户 summary JSON 无效: {error}"))?;
    if value.get("account_id").and_then(serde_json::Value::as_str) != Some(account_id) {
        return Err(format!("账户 {account_id} 的 summary 所属账户不一致"));
    }
    Ok(())
}
