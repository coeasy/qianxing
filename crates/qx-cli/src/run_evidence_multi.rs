use qx_spec::RunEvidenceDataset;
use std::path::{Path, PathBuf};
use std::process::Command;

fn resolve(root: &Path, value: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        return Ok(path);
    }
    if path.components().any(|part| {
        matches!(
            part,
            std::path::Component::ParentDir | std::path::Component::RootDir
        )
    }) {
        return Err(format!("多腿证据包输入路径越界: {value}"));
    }
    Ok(root.join(path))
}

pub(crate) fn rerun_multi_barframe(
    root: &Path,
    summary_path: &Path,
    dataset: &RunEvidenceDataset,
    runtime_config: Option<&Path>,
    primary_spec: Option<&Path>,
    reference_spec: Option<&Path>,
    expected: &qx_core::RunManifest,
) -> Result<(), String> {
    if !expected
        .code_commit
        .eq_ignore_ascii_case(crate::build_identity::BUILD_REVISION)
    {
        return Err(format!(
            "当前构建 revision={} 与证据要求的 code_commit={} 不一致，拒绝复算",
            crate::build_identity::BUILD_REVISION,
            expected.code_commit
        ));
    }
    if dataset.components.len() != 2 {
        return Err("multi-barframe 复算需要 primary 与 reference 两条输入".into());
    }
    let mut summary: serde_json::Value = serde_json::from_slice(
        &std::fs::read(summary_path).map_err(|error| format!("读取多腿摘要失败: {error}"))?,
    )
    .map_err(|error| format!("多腿摘要 JSON 无效: {error}"))?;
    for component in &dataset.components {
        let Some(manifest_path) = component.quality_manifest_path.as_deref() else {
            continue;
        };
        let path = resolve(root, manifest_path)?
            .canonicalize()
            .map_err(|error| format!("解析 {} 数据质量清单失败: {error}", component.role))?;
        let actual = qx_strategy::file_digest::sha256_file_hex(&path)?;
        if component.quality_manifest_sha256.as_deref() != Some(actual.as_str()) {
            return Err(format!(
                "{} 数据质量清单 SHA-256 与 EvidenceBundle 不一致",
                component.role
            ));
        }
        let role = match component.role.as_str() {
            "primary" => "primary",
            "reference" => "reference",
            role => return Err(format!("未知多腿输入角色: {role}")),
        };
        summary["dataset_quality"]["legs"][role]["manifest_path"] =
            serde_json::Value::String(path.to_string_lossy().to_string());
    }
    // Bundle verification already checked the immutable source RunRecord and summary digest.
    // A portable bundle relocates quality-manifest paths in this in-memory copy, so it must not
    // compare that relocated copy byte-for-byte with the source directory's RunRecord summary.
    crate::recompute_multi_barframe_input_from_evidence(&summary)?;
    let strategy = summary
        .pointer("/strategy/kind")
        .and_then(serde_json::Value::as_str)
        .ok_or("多腿摘要缺少 strategy.kind")?;
    let quantity = summary
        .pointer("/strategy/quantity")
        .and_then(serde_json::Value::as_i64)
        .filter(|value| *value > 0)
        .ok_or("多腿摘要缺少正整数 strategy.quantity")?;
    let funding_bps = summary
        .get("funding_bps")
        .and_then(serde_json::Value::as_i64)
        .ok_or("多腿摘要缺少 funding_bps")?;
    let primary = resolve(root, &dataset.components[0].path)?
        .canonicalize()
        .map_err(|error| format!("解析 primary BarFrame 失败: {error}"))?;
    let reference = resolve(root, &dataset.components[1].path)?
        .canonicalize()
        .map_err(|error| format!("解析 reference BarFrame 失败: {error}"))?;
    let mut args = vec![
        "backtest".to_string(),
        "multi-builtin".to_string(),
        strategy.to_string(),
        primary.to_string_lossy().to_string(),
        reference.to_string_lossy().to_string(),
    ];
    for component in &dataset.components {
        let Some(path) = component.quality_manifest_path.as_deref() else {
            continue;
        };
        let role_flag = match component.role.as_str() {
            "primary" => "--primary-dataset-manifest",
            "reference" => "--reference-dataset-manifest",
            role => return Err(format!("未知多腿输入角色: {role}")),
        };
        args.push(role_flag.to_string());
        args.push(
            resolve(root, path)?
                .canonicalize()
                .map_err(|error| format!("解析 {role_flag} 路径失败: {error}"))?
                .to_string_lossy()
                .to_string(),
        );
    }
    if let Some(path) = primary_spec {
        args.push(
            path.canonicalize()
                .map_err(|error| format!("解析 primary market spec 失败: {error}"))?
                .to_string_lossy()
                .to_string(),
        );
    }
    if let Some(path) = reference_spec {
        if primary_spec.is_none() {
            return Err("reference market spec 单独出现，当前多腿 CLI 无法表达该参数形状".into());
        }
        args.push(
            path.canonicalize()
                .map_err(|error| format!("解析 reference market spec 失败: {error}"))?
                .to_string_lossy()
                .to_string(),
        );
    }
    args.extend([
        "--quantity".into(),
        quantity.to_string(),
        "--funding-bps".into(),
        funding_bps.to_string(),
    ]);
    if let Some(path) = runtime_config {
        args.extend([
            "--config".into(),
            path.canonicalize()
                .map_err(|error| format!("解析 runtime config 失败: {error}"))?
                .to_string_lossy()
                .to_string(),
        ]);
    }
    let executable =
        std::env::current_exe().map_err(|error| format!("定位 qx-cli 当前构建失败: {error}"))?;
    let target = executable
        .parent()
        .and_then(Path::parent)
        .ok_or("当前 qx-cli 不在 target/<profile> 下，无法确定隔离复算目录")?
        .to_path_buf();
    std::fs::create_dir_all(&target)
        .map_err(|error| format!("创建复算 target 目录失败: {error}"))?;
    let target = target
        .canonicalize()
        .map_err(|error| format!("解析复算 target 目录失败: {error}"))?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| format!("读取系统时钟失败: {error}"))?
        .as_nanos();
    let work = target.join(format!("qx-multi-recompute-{}-{stamp}", std::process::id()));
    std::fs::create_dir(&work).map_err(|error| format!("创建多腿复算目录失败: {error}"))?;
    let work = work
        .canonicalize()
        .map_err(|error| format!("解析多腿复算目录失败: {error}"))?;
    if !work.starts_with(&target) {
        return Err("多腿复算目录越出 target".into());
    }
    args.extend(["--root".into(), work.to_string_lossy().to_string()]);
    let result = (|| {
        let mut command = Command::new(executable);
        crate::run_evidence_recompute::sanitize_recompute_environment(&mut command);
        let output = command
            .args(&args)
            .current_dir(&work)
            .output()
            .map_err(|error| format!("启动多腿离线复算失败: {error}"))?;
        if !output.status.success() {
            return Err(format!(
                "多腿离线复算进程失败 ({}): {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
                    .chars()
                    .take(1200)
                    .collect::<String>()
            ));
        }
        let runs = work.join("runs");
        for entry in
            std::fs::read_dir(&runs).map_err(|error| format!("读取多腿复算产物失败: {error}"))?
        {
            let path = entry
                .map_err(|error| format!("读取多腿复算目录项失败: {error}"))?
                .path();
            if !path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(".run.json"))
            {
                continue;
            }
            let payload = std::fs::read_to_string(&path)
                .map_err(|error| format!("读取复算 RunManifest 失败: {error}"))?;
            let actual = qx_core::RunManifest::from_json(&payload)?;
            if actual.run_id == expected.run_id {
                if actual == *expected {
                    return Ok(());
                }
                return Err(format!(
                    "多腿复算 RunManifest 不一致: expected={} actual={}",
                    expected.digest(),
                    actual.digest()
                ));
            }
        }
        Err("多腿复算没有生成匹配的组合 RunManifest".into())
    })();
    let cleanup = std::fs::remove_dir_all(&work)
        .map_err(|error| format!("清理多腿复算目录失败 {}: {error}", work.display()));
    result?;
    cleanup?;
    Ok(())
}
