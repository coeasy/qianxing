//! Isolated replay verification for portable multi-account Bar evidence.

use super::{
    account_directory_id, checked_missing_package_file, checked_package_file,
    validate_portable_relative, MultiAccountEvidenceManifest, StagingDirectory,
};
use qx_app::{MultiAccountBacktestResult, MultiAccountBacktestSpec};
use std::path::Path;

pub(super) fn recompute_multi_account_run(
    package_root: &Path,
    manifest: &MultiAccountEvidenceManifest,
    packaged_spec: &MultiAccountBacktestSpec,
    expected: &MultiAccountBacktestResult,
) -> Result<(), String> {
    let expected_manifest = expected
        .run_manifest
        .as_ref()
        .ok_or("全失败或历史无父 RunManifest 的多账户批次不能声明独立复算")?;
    if !expected_manifest
        .code_commit
        .eq_ignore_ascii_case(crate::build_identity::BUILD_REVISION)
    {
        return Err(format!(
            "当前构建 revision={} 与多账户 Evidence 要求的 code_commit={} 不一致，拒绝独立复算",
            crate::build_identity::BUILD_REVISION,
            expected_manifest.code_commit
        ));
    }

    let temp_root = std::env::temp_dir()
        .canonicalize()
        .map_err(|error| format!("解析多账户复算临时目录失败: {error}"))?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| format!("读取多账户复算时钟失败: {error}"))?
        .as_nanos();
    let work_candidate = temp_root.join(format!(
        "qx-multi-account-recompute-{}-{stamp}",
        std::process::id()
    ));
    std::fs::create_dir(&work_candidate).map_err(|error| {
        format!(
            "创建多账户隔离复算目录失败 {}: {error}",
            work_candidate.display()
        )
    })?;
    let staging = StagingDirectory(work_candidate.clone());
    let work = work_candidate
        .canonicalize()
        .map_err(|error| format!("解析多账户隔离复算目录失败: {error}"))?;
    if !work.starts_with(&temp_root) {
        return Err("多账户隔离复算目录越出临时目录".into());
    }

    let mut spec = packaged_spec.clone();
    for member in &mut spec.runs {
        let account_id = member.account_id.as_deref().unwrap_or_default();
        let input_evidence = manifest
            .inputs
            .iter()
            .find(|input| input.account_id == account_id);
        if expected.summary.accounts_failed > 0 && input_evidence.is_none() {
            return Err("旧版多账户证据包没有失败账户输入清单，拒绝执行不完整的批次复算".into());
        }
        let relative = validate_portable_relative(&member.bars_path)?;
        let input = match input_evidence {
            Some(input) if input.missing => checked_missing_package_file(package_root, &relative)?,
            Some(_) => checked_package_file(package_root, &relative)?,
            None if expected.summary.accounts_failed == 0 => {
                checked_package_file(package_root, &relative)?
            }
            None => return Err("多账户 Evidence 缺少账户输入清单".into()),
        };
        if !input_evidence.is_some_and(|evidence| evidence.missing) && !input.is_file() {
            return Err(format!("账户 {} 的包内 BarFrame 不存在", account_id));
        }
        member.bars_path = input.to_string_lossy().into_owned();
        member.output_dir = work
            .join("accounts")
            .join(account_directory_id(account_id))
            .to_string_lossy()
            .into_owned();
    }

    let context =
        qx_app::RunContext::new(qx_app::CallerCapability::Research, spec.batch_id.clone())
            .with_code_commit(crate::build_identity::BUILD_REVISION);
    let actual = qx_app::run_multi_account_backtest(&spec, &context)
        .map_err(|error| format!("重新运行多账户批次失败: {}", error.to_json()))?;
    qx_app::verify_multi_account_backtest_result(&actual)
        .map_err(|error| format!("多账户复算结果自校验失败: {}", error.to_json()))?;
    if actual.summary != expected.summary
        || actual.spec_fingerprint != expected.spec_fingerprint
        || actual.run_manifest.as_ref() != Some(expected_manifest)
    {
        return Err(format!(
            "多账户 Evidence 独立复算不一致: expected={} actual={}",
            expected_manifest.digest(),
            actual
                .run_manifest
                .as_ref()
                .map(|manifest| manifest.digest().to_string())
                .unwrap_or_else(|| "missing".into())
        ));
    }
    if actual.runs.len() != expected.runs.len()
        || actual
            .runs
            .iter()
            .zip(&expected.runs)
            .any(|(actual, expected)| {
                actual.account_id != expected.account_id
                    || actual.outcome.as_ref().map(|outcome| {
                        (
                            outcome.run_id.as_str(),
                            outcome.result_hash.as_str(),
                            outcome.data_fingerprint.as_str(),
                        )
                    }) != expected.outcome.as_ref().map(|outcome| {
                        (
                            outcome.run_id.as_str(),
                            outcome.result_hash.as_str(),
                            outcome.data_fingerprint.as_str(),
                        )
                    })
            })
    {
        return Err("多账户独立复算的账户顺序、状态或子结果身份不一致".into());
    }
    drop(staging);
    Ok(())
}
