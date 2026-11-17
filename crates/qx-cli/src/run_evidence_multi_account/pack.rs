//! Portable multi-account Bar Evidence package assembly.

use super::{
    account_directory_id, locate_account_bundle, portable_artifact_path, read_run_bundle,
    reserve_staging_directory, validate_portable_relative, validate_successful_outcomes,
    verify_bundle_summary_account, verify_member_evidence, verify_portable_run_paths, write_new,
    AccountEvidence, AccountInputEvidence, FileDigest, MultiAccountEvidenceManifest, MANIFEST_NAME,
    RESULT_NAME, SCHEMA_VERSION, SPEC_NAME,
};
use qx_app::{MultiAccountBacktestResult, MultiAccountBacktestSpec};
use qx_spec::EvidenceGeneration;
use std::collections::BTreeMap;
use std::path::Path;

pub(super) fn pack_multi_account_run(
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
    let result: MultiAccountBacktestResult = serde_json::from_slice(&result_bytes)
        .map_err(|error| format!("多账户结果 JSON 无效: {error}"))?;
    let spec_text = std::str::from_utf8(&spec_bytes)
        .map_err(|error| format!("多账户规格不是 UTF-8: {error}"))?;
    let spec = MultiAccountBacktestSpec::from_json(spec_text).map_err(|error| error.to_string())?;
    crate::strategy_multi_account_diagnostics::validate_batch_identity(&result, &spec)?;
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
        verify_member_evidence(run, outcome, member, evidence)?;
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
        verify_member_evidence(run, outcome, member, portable_evidence)?;
        let relative_dataset = validate_portable_relative(&portable_evidence.dataset.path)?;
        let bars_path = format!("{evidence_dir}/{relative_dataset}");
        let packaged_member = portable_spec
            .runs
            .iter_mut()
            .find(|candidate| candidate.account_id.as_deref() == Some(run.account_id.as_str()))
            .ok_or("便携规格中缺少成功账户")?;
        packaged_member.bars_path = bars_path.clone();
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
        let packaged_bars = staging.0.join(&bars_path);
        let bars_sha256 = qx_strategy::file_digest::sha256_file_hex(&packaged_bars)?;
        inputs.push(AccountInputEvidence {
            account_id: run.account_id.clone(),
            path: bars_path,
            sha256: Some(bars_sha256),
            missing: false,
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
        let relative_bars = format!("{evidence_dir}/bars.json");
        let packaged_path = staging.0.join(&relative_bars);
        let parent = packaged_path.parent().ok_or("失败账户输入路径缺少父目录")?;
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("创建失败账户输入目录失败 {}: {error}", parent.display()))?;
        let source = Path::new(&member.bars_path);
        let (sha256, missing) = match std::fs::read(source) {
            Ok(bytes) => {
                write_new(&packaged_path, &bytes, "失败账户原始 BarFrame")?;
                (Some(qx_strategy::sha256_hex(&bytes)), false)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (None, true),
            Err(error) => {
                return Err(format!(
                    "读取失败账户 {} 的原始 BarFrame 失败 {}: {error}",
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
        packaged_member.bars_path = relative_bars.clone();
        packaged_member.output_dir = format!(
            "accounts/{}/replay-output",
            account_directory_id(&run.account_id)
        );
        inputs.push(AccountInputEvidence {
            account_id: run.account_id.clone(),
            path: relative_bars,
            sha256,
            missing,
        });
    }
    inputs.sort_by(|left, right| left.account_id.cmp(&right.account_id));
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
        kind: "qianxing-multi-account-run-evidence".into(),
        batch_id: result.batch_id.clone(),
        settlement_currency: result.settlement_currency.clone(),
        spec_fingerprint: result.spec_fingerprint.clone(),
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
        "operation": "multi-account-run-evidence-packed",
        "batch_id": result.batch_id,
        "directory": output_dir.display().to_string(),
        "manifest": output_dir.join(MANIFEST_NAME).display().to_string(),
        "accounts_verified": result.summary.accounts_succeeded,
        "accounts_failed": result.summary.accounts_failed,
        "portable": true,
        "signed": false
    }))
}
