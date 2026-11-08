//! Independent recomputation for portable multi-account depth evidence.

use super::io::STAGING_SEQUENCE;
use super::{
    account_directory_id, checked_missing_package_file, checked_package_file,
    validate_portable_relative, verify_digest, MultiAccountEvidenceManifest, StagingDirectory,
};
use qx_app::{MultiAccountDepthBacktestResult, MultiAccountDepthBacktestSpec};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

pub(super) fn recompute_multi_account_depth_run(
    package_root: &Path,
    manifest: &MultiAccountEvidenceManifest,
    packaged_spec: &MultiAccountDepthBacktestSpec,
    expected: &MultiAccountDepthBacktestResult,
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

    let (staging, work) = reserve_recompute_directory(package_root)?;

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
        let relative = validate_portable_relative(&member.depth_path)?;
        let input = match input_evidence {
            Some(input) if input.missing => checked_missing_package_file(package_root, &relative)?,
            Some(_) => checked_package_file(package_root, &relative)?,
            None if expected.summary.accounts_failed == 0 => {
                checked_package_file(package_root, &relative)?
            }
            None => return Err("多账户 Evidence 缺少账户输入清单".into()),
        };
        if !input_evidence.is_some_and(|evidence| evidence.missing) && !input.is_file() {
            return Err(format!("账户 {} 的包内 DepthFrame 不存在", account_id));
        }
        member.depth_path = input.to_string_lossy().into_owned();
        match input_evidence.and_then(|evidence| {
            evidence
                .quality_manifest_path
                .as_deref()
                .map(|path| (evidence, path))
        }) {
            Some((evidence, relative)) => {
                let relative = validate_portable_relative(relative)?;
                let missing = evidence.quality_manifest_missing == Some(true);
                let path = if missing {
                    if evidence.quality_manifest_sha256.is_some() {
                        return Err(format!(
                            "账户 {} 的缺失质量清单不应声明 SHA-256",
                            account_id
                        ));
                    }
                    checked_missing_package_file(package_root, &relative)?
                } else {
                    let path = checked_package_file(package_root, &relative)?;
                    if !path.is_file() {
                        return Err(format!("账户 {} 的包内质量清单不是文件", account_id));
                    }
                    let expected = evidence
                        .quality_manifest_sha256
                        .as_deref()
                        .ok_or_else(|| format!("账户 {} 的质量清单缺少 SHA-256", account_id))?;
                    verify_digest(&path, expected, "账户质量清单")?;
                    path
                };
                member.quality_manifest_path = Some(path.to_string_lossy().into_owned());
            }
            None if member.quality_manifest_path.is_some() => {
                return Err(format!(
                    "账户 {} 的规格声明了质量清单，但证据输入清单缺少它",
                    account_id
                ));
            }
            None => {}
        }
        member.output_dir = work
            .join("accounts")
            .join(account_directory_id(account_id))
            .to_string_lossy()
            .into_owned();
    }

    let context =
        qx_app::RunContext::new(qx_app::CallerCapability::Research, spec.batch_id.clone())
            .with_code_commit(crate::build_identity::BUILD_REVISION);
    let actual = qx_app::run_multi_account_depth_backtest(&spec, &context)
        .map_err(|error| format!("重新运行多账户批次失败: {}", error.to_json()))?;
    qx_app::artifact_store::verify_multi_account_depth_backtest_result(&actual)
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

fn reserve_recompute_directory(package_root: &Path) -> Result<(StagingDirectory, PathBuf), String> {
    let mut bases = Vec::new();
    if let Ok(temp_root) = std::env::temp_dir().canonicalize() {
        bases.push(temp_root);
    }
    if let Some(parent) = package_root.parent() {
        if let Ok(parent) = parent.canonicalize() {
            if !bases.contains(&parent) {
                bases.push(parent);
            }
        }
    }
    if bases.is_empty() {
        return Err("无法解析多账户深度复算的临时目录或证据包父目录".into());
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| format!("读取多账户复算时钟失败: {error}"))?
        .as_nanos();
    let mut last_error = None;
    for base in bases {
        for _ in 0..16 {
            let sequence = STAGING_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let candidate = base.join(format!(
                "qx-multi-account-depth-recompute-{}-{stamp}-{sequence}",
                std::process::id()
            ));
            match std::fs::create_dir(&candidate) {
                Ok(()) => {
                    let work = candidate.canonicalize().map_err(|error| {
                        format!(
                            "解析多账户隔离复算目录失败 {}: {error}",
                            candidate.display()
                        )
                    })?;
                    if !work.starts_with(&base) {
                        let _ = std::fs::remove_dir_all(&candidate);
                        return Err("多账户隔离复算目录越出其授权父目录".into());
                    }
                    return Ok((StagingDirectory(candidate), work));
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    last_error = Some(error);
                }
                Err(error) => {
                    last_error = Some(error);
                    break;
                }
            }
        }
    }
    Err(format!(
        "创建多账户隔离复算目录失败: {}",
        last_error
            .map(|error| error.to_string())
            .unwrap_or_else(|| "没有可用的复算目录".into())
    ))
}
