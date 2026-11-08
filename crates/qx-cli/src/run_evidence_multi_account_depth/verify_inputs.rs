//! Validate per-account portable input coverage and content digests.

use super::{
    account_directory_id, checked_missing_package_file, checked_package_file,
    validate_portable_relative, verify_digest, MultiAccountEvidenceManifest, SCHEMA_VERSION,
};
use qx_app::{MultiAccountDepthBacktestResult, MultiAccountDepthBacktestSpec};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub(super) fn verify_packaged_inputs(
    root: &Path,
    manifest: &MultiAccountEvidenceManifest,
    spec: &MultiAccountDepthBacktestSpec,
    result: &MultiAccountDepthBacktestResult,
) -> Result<(), String> {
    if manifest.schema_version != SCHEMA_VERSION || manifest.inputs.len() != spec.runs.len() {
        return Err("多账户 Evidence 输入清单必须逐账户覆盖整个批次".into());
    }
    let members = spec
        .runs
        .iter()
        .map(|member| (member.account_id.as_deref().unwrap_or_default(), member))
        .collect::<BTreeMap<_, _>>();
    let outcomes = result
        .runs
        .iter()
        .map(|run| (run.account_id.as_str(), run.outcome.is_some()))
        .collect::<BTreeMap<_, _>>();
    let mut seen = BTreeSet::new();
    for input in &manifest.inputs {
        if !seen.insert(input.account_id.as_str()) {
            return Err(format!(
                "多账户 Evidence 输入清单重复账户: {}",
                input.account_id
            ));
        }
        let member = members
            .get(input.account_id.as_str())
            .ok_or_else(|| format!("多账户 Evidence 输入包含未知账户: {}", input.account_id))?;
        if input.path != member.depth_path {
            return Err(format!(
                "账户 {} 的输入路径与便携规格不一致",
                input.account_id
            ));
        }
        let relative = validate_portable_relative(&input.path)?;
        let succeeded = outcomes
            .get(input.account_id.as_str())
            .copied()
            .ok_or_else(|| format!("多账户结果缺少输入清单账户: {}", input.account_id))?;
        if !succeeded
            && input.path
                != format!(
                    "accounts/{}/inputs/depth.json",
                    account_directory_id(&input.account_id)
                )
        {
            return Err(format!(
                "失败账户 {} 的便携输入路径不符合规范",
                input.account_id
            ));
        }
        match (input.missing, input.sha256.as_deref(), succeeded) {
            (true, None, false) => {
                checked_missing_package_file(root, &relative)?;
            }
            (false, Some(digest), _) if digest.len() == 64 => {
                if !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                    return Err(format!("账户 {} 的输入 SHA-256 格式无效", input.account_id));
                }
                let path = checked_package_file(root, &relative)?;
                if !path.is_file() {
                    return Err(format!(
                        "账户 {} 的便携 DepthFrame 不是文件",
                        input.account_id
                    ));
                }
                verify_digest(&path, digest, "账户 DepthFrame 输入")?;
            }
            _ => {
                return Err(format!(
                    "账户 {} 的输入存在状态与结果状态不一致",
                    input.account_id
                ));
            }
        }
        if input.quality_manifest_path != member.quality_manifest_path {
            return Err(format!(
                "账户 {} 的质量清单路径与便携规格不一致",
                input.account_id
            ));
        }
        match (
            input.quality_manifest_path.as_deref(),
            input.quality_manifest_sha256.as_deref(),
            input.quality_manifest_missing,
        ) {
            (None, None, None) => {}
            (Some(path), Some(digest), Some(false)) => {
                let relative = validate_portable_relative(path)?;
                let path = checked_package_file(root, &relative)?;
                if !path.is_file() {
                    return Err(format!("账户 {} 的便携质量清单不是文件", input.account_id));
                }
                verify_digest(&path, digest, "账户质量清单")?;
            }
            (Some(path), None, Some(true)) if !succeeded => {
                let relative = validate_portable_relative(path)?;
                checked_missing_package_file(root, &relative)?;
            }
            _ => {
                return Err(format!(
                    "账户 {} 的质量清单路径、摘要、缺失状态不一致",
                    input.account_id
                ));
            }
        }
    }
    if seen.len() != members.len() {
        return Err("多账户 Evidence 输入清单未覆盖全部账户".into());
    }
    Ok(())
}
