use super::paths::{
    account_directory_id, checked_missing_package_file, checked_package_file,
    validate_portable_relative,
};
use super::{verify_digest, MultiAccountEvidenceManifest, LEGACY_SCHEMA_VERSION, SCHEMA_VERSION};
use qx_app::{MultiAccountBacktestResult, MultiAccountBacktestSpec};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
pub(super) fn verify_packaged_inputs(
    root: &Path,
    manifest: &MultiAccountEvidenceManifest,
    spec: &MultiAccountBacktestSpec,
    result: &MultiAccountBacktestResult,
) -> Result<(), String> {
    if manifest.schema_version == LEGACY_SCHEMA_VERSION {
        if !manifest.inputs.is_empty() {
            return Err("旧版多账户 Evidence manifest 不允许声明 inputs".into());
        }
        return Ok(());
    }
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
        if input.path != member.bars_path {
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
                    "accounts/{}/inputs/bars.json",
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
                        "账户 {} 的便携 BarFrame 不是文件",
                        input.account_id
                    ));
                }
                verify_digest(&path, digest, "账户 BarFrame 输入")?;
            }
            _ => {
                return Err(format!(
                    "账户 {} 的输入存在状态与结果状态不一致",
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
