//! Identity checks shared by multi-account evidence packaging and verification.

use qx_app::{MultiAccountBacktestResult, MultiAccountBacktestSpec};
use qx_spec::{EvidenceBundle, EvidenceGeneration};
use std::collections::BTreeMap;
use std::path::Path;

pub(super) fn validate_successful_outcomes(
    result: &MultiAccountBacktestResult,
    spec: &MultiAccountBacktestSpec,
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
            || outcome.instrument != member.instrument
        {
            return Err(format!(
                "账户 {} 的成功结果与规格身份不一致",
                run.account_id
            ));
        }
    }
    Ok(())
}

pub(super) fn verify_member_evidence(
    batch_run: &qx_app::MultiAccountRunResult,
    outcome: &qx_app::BacktestOutcome,
    member: &qx_app::BacktestSpec,
    evidence: &qx_spec::RunEvidencePayload,
) -> Result<(), String> {
    if batch_run.account_id != outcome.account_id.as_deref().unwrap_or_default()
        || evidence.run.run_id != outcome.run_id
        || evidence.run.result_hash != outcome.result_hash
        || evidence.run.data_fingerprint != outcome.data_fingerprint
        || evidence.identity.instrument != outcome.instrument
        || evidence.identity.instrument != member.instrument
        || evidence.dataset.kind != "barframe"
    {
        return Err(format!(
            "账户 {} 的 EvidenceBundle 与批次结果不一致",
            batch_run.account_id
        ));
    }
    Ok(())
}

pub(super) fn verify_bundle_summary_account(
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
