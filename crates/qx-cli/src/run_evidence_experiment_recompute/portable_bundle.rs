use super::{
    check_report_identity, compare_multi_leg_experiment, ensure_matching_build, read_experiment,
    spec_fingerprint, with_isolated_output, PersistedExperimentInput,
};
use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};

const BUNDLE_SCHEMA_VERSION: u32 = 1;
const BUNDLE_FILE: &str = "experiment-evidence.json";

#[path = "multi_leg_bundle.rs"]
mod multi_leg_bundle;
pub(crate) use multi_leg_bundle::{pack_multi_leg_experiment, verify_multi_leg_experiment_bundle};

#[path = "multi_market_bundle.rs"]
mod multi_market_bundle;
pub(crate) use multi_market_bundle::{
    pack_multi_market_experiment, verify_multi_market_experiment_bundle,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BundleFile {
    role: String,
    path: String,
    sha256: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExperimentEvidenceBundleManifest {
    schema_version: u32,
    kind: String,
    experiment_id: String,
    code_commit: String,
    spec_fingerprint: String,
    files: Vec<BundleFile>,
}

fn package_path(root: &Path, relative: &str) -> Result<PathBuf, String> {
    let relative_path = Path::new(relative);
    if relative_path.as_os_str().is_empty()
        || relative_path.is_absolute()
        || relative_path.components().any(|part| {
            matches!(
                part,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(format!("实验包路径非法: {relative}"));
    }
    let path = root.join(relative_path);
    let resolved = path
        .canonicalize()
        .map_err(|error| format!("解析实验包文件失败 {}: {error}", path.display()))?;
    let root = root
        .canonicalize()
        .map_err(|error| format!("解析实验包目录失败 {}: {error}", root.display()))?;
    if !resolved.starts_with(root) || !resolved.is_file() {
        return Err(format!("实验包文件越界或不是普通文件: {relative}"));
    }
    Ok(resolved)
}

fn source_file(source: &str) -> Result<PathBuf, String> {
    let candidate = PathBuf::from(source);
    let path = if candidate.is_absolute() {
        candidate
    } else {
        std::env::current_dir()
            .map_err(|error| format!("读取当前目录失败: {error}"))?
            .join(candidate)
    };
    path.canonicalize()
        .map_err(|error| format!("解析实验源文件失败 {}: {error}", path.display()))
}

fn file_sha256(path: &Path) -> Result<String, String> {
    qx_strategy::file_digest::sha256_file_hex(path)
}

fn copy_to(source: &Path, root: &Path, relative: &str) -> Result<BundleFile, String> {
    let target = root.join(relative);
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("创建实验包目录失败 {}: {error}", parent.display()))?;
    }
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&target)
        .and_then(|mut output| {
            let mut input = std::fs::File::open(source)?;
            std::io::copy(&mut input, &mut output)?;
            output.sync_all()
        })
        .map_err(|error| format!("复制实验包文件失败 {}: {error}", target.display()))?;
    Ok(BundleFile {
        role: relative.to_string(),
        path: relative.replace('\\', "/"),
        sha256: file_sha256(&target)?,
    })
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<(), String> {
    let payload = serde_json::to_vec_pretty(value).map_err(|error| error.to_string())?;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .and_then(|mut file| {
            use std::io::Write;
            file.write_all(&payload)?;
            file.sync_all()
        })
        .map_err(|error| format!("写入实验包文件失败 {}: {error}", path.display()))
}

fn read_and_verify_bundle(root: &Path) -> Result<ExperimentEvidenceBundleManifest, String> {
    let bundle_path = root.join(BUNDLE_FILE);
    let payload = std::fs::read_to_string(&bundle_path)
        .map_err(|error| format!("读取实验包清单失败 {}: {error}", bundle_path.display()))?;
    let bundle: ExperimentEvidenceBundleManifest =
        serde_json::from_str(&payload).map_err(|error| format!("实验包清单无效: {error}"))?;
    if bundle.schema_version != BUNDLE_SCHEMA_VERSION
        || !matches!(
            bundle.kind.as_str(),
            "multi-leg-experiment" | "multi-market-experiment" | "multi-market-depth-experiment"
        )
    {
        return Err("实验包 schema_version 或 kind 不受支持".into());
    }
    let mut paths = std::collections::BTreeSet::<&str>::new();
    let mut roles = std::collections::BTreeSet::new();
    for file in &bundle.files {
        if file.role.trim().is_empty()
            || file.sha256.len() != 64
            || !paths.insert(file.path.as_str())
            || !roles.insert(&file.role)
        {
            return Err("实验包清单存在空字段、无效摘要或重复路径".into());
        }
        let path = package_path(root, &file.path)?;
        let actual = file_sha256(&path)?;
        if actual != file.sha256 {
            return Err(format!("实验包 SHA-256 不匹配: {}", file.path));
        }
    }
    for required in [
        "experiment.json",
        "experiment.spec.json",
        "spec.identity.json",
    ] {
        if !paths.contains(required) {
            return Err(format!("实验包缺少必需文件: {required}"));
        }
    }
    Ok(bundle)
}

fn manifest_path<'a>(
    bundle: &'a ExperimentEvidenceBundleManifest,
    role: &str,
) -> Result<&'a str, String> {
    bundle
        .files
        .iter()
        .find(|file| file.role == role || file.role.starts_with(role))
        .map(|file| file.path.as_str())
        .ok_or_else(|| format!("实验包缺少文件角色: {role}"))
}
