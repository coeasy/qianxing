use std::path::{Path, PathBuf};

pub(super) fn checked_missing_package_file(root: &Path, relative: &str) -> Result<PathBuf, String> {
    let resolved = crate::run_evidence_recompute::resolve_reference(root, relative)?;
    let parent = resolved
        .parent()
        .ok_or_else(|| format!("包内缺失输入路径没有父目录: {relative}"))?;
    let canonical_parent = parent
        .canonicalize()
        .map_err(|error| format!("解析包内缺失输入父目录失败 {}: {error}", parent.display()))?;
    if !canonical_parent.starts_with(root) {
        return Err(format!("包内缺失输入路径逃逸出根目录: {relative}"));
    }
    match std::fs::symlink_metadata(&resolved) {
        Ok(_) => return Err(format!("清单声明输入缺失，但包内文件仍存在: {relative}")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!(
                "检查包内缺失输入路径失败 {}: {error}",
                resolved.display()
            ));
        }
    }
    Ok(canonical_parent.join(
        resolved
            .file_name()
            .ok_or_else(|| format!("包内缺失输入路径文件名无效: {relative}"))?,
    ))
}

pub(super) fn checked_package_file(root: &Path, relative: &str) -> Result<PathBuf, String> {
    let resolved = crate::run_evidence_recompute::resolve_reference(root, relative)?;
    let canonical = resolved
        .canonicalize()
        .map_err(|error| format!("解析证据包文件失败 {}: {error}", resolved.display()))?;
    if !canonical.starts_with(root) {
        return Err(format!("证据包路径逃逸出根目录: {relative}"));
    }
    Ok(canonical)
}

pub(super) fn verify_portable_run_paths(
    evidence_dir: &str,
    member: &qx_app::DepthBacktestSpec,
    outcome: &qx_app::DepthBacktestOutcome,
    evidence: &qx_spec::RunEvidencePayload,
) -> Result<(), String> {
    let dataset_path = validate_portable_relative(&evidence.dataset.path)?;
    let expected_dataset = format!("{evidence_dir}/{dataset_path}");
    if member.depth_path != expected_dataset {
        return Err(format!(
            "账户 {} 的便携规格未指向本包中的数据输入",
            member.account_id.as_deref().unwrap_or_default()
        ));
    }
    let expected_quality_manifest = portable_quality_manifest_path(
        evidence_dir,
        evidence.dataset.quality_manifest_path.as_deref(),
    )?;
    if member.quality_manifest_path != expected_quality_manifest {
        return Err(format!(
            "账户 {} 的便携规格未指向本包中的质量清单",
            member.account_id.as_deref().unwrap_or_default()
        ));
    }
    for (name, actual) in [
        ("run_manifest", outcome.artifacts.run_manifest.as_str()),
        ("summary", outcome.artifacts.summary.as_str()),
        ("equity", outcome.artifacts.equity.as_str()),
        ("fills", outcome.artifacts.fills.as_str()),
    ] {
        let expected = portable_artifact_path(evidence_dir, evidence, name)?;
        if actual != expected {
            return Err(format!(
                "账户 {} 的便携结果 {name} 路径与 EvidenceBundle 不一致",
                member.account_id.as_deref().unwrap_or_default()
            ));
        }
    }
    let expected_trace = evidence
        .artifacts
        .iter()
        .find(|artifact| artifact.name == "event_trace")
        .map(|artifact| format!("{evidence_dir}/{}", artifact.path));
    if outcome.artifacts.event_trace != expected_trace {
        return Err(format!(
            "账户 {} 的便携结果 event_trace 路径与 EvidenceBundle 不一致",
            member.account_id.as_deref().unwrap_or_default()
        ));
    }
    Ok(())
}

pub(super) fn portable_quality_manifest_path(
    evidence_dir: &str,
    path: Option<&str>,
) -> Result<Option<String>, String> {
    path.map(|path| {
        let relative = validate_portable_relative(path)?;
        Ok(format!("{evidence_dir}/{relative}"))
    })
    .transpose()
}

pub(super) fn portable_artifact_path(
    evidence_dir: &str,
    evidence: &qx_spec::RunEvidencePayload,
    name: &str,
) -> Result<String, String> {
    let artifact = evidence
        .artifacts
        .iter()
        .find(|artifact| artifact.name == name)
        .ok_or_else(|| format!("账户 EvidenceBundle 缺少 {name} 产物"))?;
    Ok(format!(
        "{evidence_dir}/{}",
        validate_portable_relative(&artifact.path)?
    ))
}

pub(super) fn validate_portable_relative(value: &str) -> Result<String, String> {
    let path = Path::new(value);
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir | std::path::Component::RootDir
            )
        })
    {
        return Err(format!("便携运行证据包含非相对或越界路径: {value}"));
    }
    Ok(path.to_string_lossy().replace('\\', "/"))
}

pub(super) fn locate_account_bundle(summary_path: &Path) -> Result<PathBuf, String> {
    let file_name = summary_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("账户 summary 文件名无效: {}", summary_path.display()))?;
    let stem = file_name
        .strip_suffix(".summary.json")
        .ok_or_else(|| format!("账户 summary 文件名必须以 .summary.json 结尾: {file_name}"))?;
    for suffix in [
        ".strategy-diagnostics.evidence.json",
        ".report-analysis.evidence.json",
        ".evidence.json",
    ] {
        let candidate = summary_path.with_file_name(format!("{stem}{suffix}"));
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err(format!(
        "账户 {} 缺少运行 EvidenceBundle；先对该批次执行 report --evidence",
        summary_path.display()
    ))
}

pub(super) fn account_directory_id(account_id: &str) -> String {
    qx_strategy::sha256_hex(account_id.as_bytes())
}
