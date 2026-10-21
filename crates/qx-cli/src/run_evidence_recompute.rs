use crate::*;
use std::path::{Path, PathBuf};
use std::process::Command;

pub(crate) fn sanitize_recompute_environment(command: &mut Command) {
    const SENSITIVE_MARKERS: [&str; 6] = [
        "SECRET",
        "TOKEN",
        "PASSWORD",
        "API_KEY",
        "PRIVATE_KEY",
        "CREDENTIAL",
    ];
    for (key, _) in std::env::vars_os() {
        let upper = key.to_string_lossy().to_ascii_uppercase();
        if SENSITIVE_MARKERS
            .iter()
            .any(|marker| upper.contains(marker))
            || matches!(upper.as_str(), "HTTP_PROXY" | "HTTPS_PROXY" | "ALL_PROXY")
        {
            command.env_remove(key);
        }
    }
}

pub(crate) fn resolve_reference(root: &Path, reference: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(reference);
    if path.is_absolute() {
        return Ok(path);
    }
    if path.components().any(|component| {
        matches!(
            component,
            std::path::Component::ParentDir | std::path::Component::RootDir
        )
    }) {
        return Err(format!("证据包相对路径越界: {reference}"));
    }
    Ok(root.join(path))
}

pub(crate) fn recompute_multi_barframe_identity(
    root: &Path,
    dataset: &qx_spec::RunEvidenceDataset,
) -> Result<(BacktestInputProvenance, u64), String> {
    if dataset.components.len() != 2
        || dataset.components[0].role != "primary"
        || dataset.components[1].role != "reference"
    {
        return Err("multi-barframe 必须按 primary、reference 顺序登记两条输入腿".into());
    }
    let mut frames = Vec::with_capacity(2);
    let mut identities = Vec::with_capacity(2);
    let mut paths = Vec::with_capacity(2);
    for component in &dataset.components {
        let path = resolve_reference(root, &component.path)?;
        let frame = read_bar_frame_for_backtest(&path)?;
        let (_, identity) = barframe_dataset_identity(&path, &frame)?;
        for (field, declared, actual) in [
            (
                "dataset_id",
                component.dataset_id.as_str(),
                identity.dataset_id.as_str(),
            ),
            (
                "version",
                component.version.as_str(),
                identity.version.as_str(),
            ),
            (
                "content_fingerprint",
                component.content_fingerprint.as_str(),
                identity.fingerprint.as_str(),
            ),
        ] {
            if declared != actual {
                return Err(format!(
                    "EvidenceBundle {role} 输入 {field} 不匹配: 声明={declared} 重算={actual}",
                    role = component.role
                ));
            }
        }
        if component.row_count != frame.ts.len() as u64 {
            return Err(format!(
                "EvidenceBundle {} 输入行数不匹配: 声明={} 重算={}",
                component.role,
                component.row_count,
                frame.ts.len()
            ));
        }
        frames.push(frame);
        identities.push(identity);
        paths.push(path.to_string_lossy().to_string());
    }
    if frames[0].ts != frames[1].ts {
        return Err("multi-barframe 两条输入的时间戳不对齐".into());
    }
    let dataset_id = format!("{}+{}", identities[0].dataset_id, identities[1].dataset_id);
    let fingerprint = qx_strategy::sha256_hex(
        format!(
            "primary:{}|reference:{}",
            identities[0].fingerprint, identities[1].fingerprint
        )
        .as_bytes(),
    );
    if dataset.dataset_id != dataset_id
        || dataset.version != BARFRAME_DATASET_VERSION
        || dataset.content_fingerprint != fingerprint
        || dataset.row_count != frames[0].ts.len() as u64
    {
        return Err("EvidenceBundle 组合数据集身份与两条输入重算结果不一致".into());
    }
    Ok((
        BacktestInputProvenance {
            kind: "multi-barframe",
            path: paths.join("|"),
            dataset_id,
            dataset_version: BARFRAME_DATASET_VERSION.to_string(),
            fingerprint,
        },
        frames[0].ts.len() as u64,
    ))
}

pub(crate) fn rerun_strategy(
    runtime_path: &Path,
    input_path: &Path,
    market_spec_path: Option<&Path>,
    quality_manifest_path: Option<&Path>,
    expected_run_id: &str,
    expected_hash: &str,
    expected_commit: &str,
) -> Result<(), String> {
    if !expected_commit.eq_ignore_ascii_case(build_identity::BUILD_REVISION) {
        return Err(format!(
            "当前构建 revision={} 与证据要求的 code_commit={} 不一致，拒绝将结果称为独立复算",
            build_identity::BUILD_REVISION,
            expected_commit
        ));
    }
    let config = read_runtime_config(runtime_path)?;
    if Path::new(&config.storage.data_dir).is_absolute() {
        return Err("runtime.storage.data_dir 必须是相对路径，避免离线复算写入原运行目录".into());
    }
    if Path::new(&config.storage.data_dir)
        .components()
        .any(|part| {
            matches!(
                part,
                std::path::Component::ParentDir | std::path::Component::RootDir
            )
        })
    {
        return Err("runtime.storage.data_dir 不得越出离线复算工作目录".into());
    }
    let runtime_path = runtime_path
        .canonicalize()
        .map_err(|error| format!("解析证据包 runtime 配置失败: {error}"))?;
    let input_path = input_path
        .canonicalize()
        .map_err(|error| format!("解析证据包输入文件失败: {error}"))?;
    let market_spec_path = market_spec_path
        .map(Path::canonicalize)
        .transpose()
        .map_err(|error| format!("解析证据包市场规格失败: {error}"))?;
    let temp_root = std::env::temp_dir()
        .canonicalize()
        .map_err(|error| format!("解析系统临时目录失败: {error}"))?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| format!("读取系统时钟失败: {error}"))?
        .as_nanos();
    let work = temp_root.join(format!("qx-run-recompute-{}-{stamp}", std::process::id()));
    std::fs::create_dir(&work)
        .map_err(|error| format!("创建离线复算工作目录失败 {}: {error}", work.display()))?;
    let resolved_work = work
        .canonicalize()
        .map_err(|error| format!("解析离线复算工作目录失败: {error}"))?;
    if !resolved_work.starts_with(&temp_root) {
        return Err("离线复算工作目录越出系统临时目录".into());
    }
    let result = (|| {
        let executable = std::env::current_exe()
            .map_err(|error| format!("定位 qx-cli 当前构建失败: {error}"))?;
        let mut command = Command::new(executable);
        sanitize_recompute_environment(&mut command);
        command
            .args(["strategy", "backtest"])
            .arg(&runtime_path)
            .arg(&input_path);
        if let Some(quality_manifest) = quality_manifest_path {
            command.arg("--dataset-manifest").arg(quality_manifest);
        }
        if let Some(market_spec) = &market_spec_path {
            command.arg(market_spec);
        }
        let output = command
            .current_dir(&resolved_work)
            .output()
            .map_err(|error| format!("启动离线策略复算失败: {error}"))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!(
                "离线策略复算进程失败 ({}): {}",
                output.status,
                stderr.chars().take(1200).collect::<String>()
            ));
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let manifest_lines = stdout
            .lines()
            .filter_map(|line| line.strip_prefix("[RunManifest] "))
            .collect::<Vec<_>>();
        let manifest_line = manifest_lines
            .iter()
            .copied()
            .find(|line| {
                line.split_whitespace()
                    .any(|field| field.strip_prefix("run_id=") == Some(expected_run_id))
            })
            .ok_or_else(|| {
                format!(
                    "离线策略复算未输出目标 RunManifest: expected={expected_run_id} actual={}",
                    manifest_lines.join(" | ")
                )
            })?;
        let actual = manifest_line
            .split_whitespace()
            .find_map(|field| field.strip_prefix("result_hash="))
            .ok_or("目标 RunManifest 缺少 result_hash")?;
        if !actual.eq_ignore_ascii_case(expected_hash) {
            return Err(format!(
                "离线策略复算结果不一致 run_id={expected_run_id}: expected={expected_hash} actual={actual}"
            ));
        }
        Ok(())
    })();
    let cleanup = std::fs::remove_dir_all(&resolved_work).map_err(|error| {
        format!(
            "清理离线复算工作目录失败 {}: {error}",
            resolved_work.display()
        )
    });
    result?;
    cleanup?;
    Ok(())
}

fn depth_descriptor_value<'a>(descriptor: &'a str, key: &str) -> Result<&'a str, String> {
    descriptor
        .split_whitespace()
        .find_map(|part| part.strip_prefix(&format!("{key}=")))
        .ok_or_else(|| format!("深度运行摘要的撮合描述缺少 {key}"))
}

pub(crate) fn rerun_depth_backtest(
    summary_path: &Path,
    input_path: &Path,
    runtime_config_path: Option<&Path>,
    market_spec_path: Option<&Path>,
    expected_manifest: &qx_core::RunManifest,
) -> Result<(), String> {
    if !expected_manifest
        .code_commit
        .eq_ignore_ascii_case(build_identity::BUILD_REVISION)
    {
        return Err(format!(
            "当前构建 revision={} 与证据要求的 code_commit={} 不一致，拒绝将结果称为独立复算",
            build_identity::BUILD_REVISION,
            expected_manifest.code_commit
        ));
    }
    let summary_path = summary_path
        .canonicalize()
        .map_err(|error| format!("解析深度回测摘要路径失败: {error}"))?;
    let input_path = input_path
        .canonicalize()
        .map_err(|error| format!("解析深度回测输入路径失败: {error}"))?;
    let runtime_config_path = runtime_config_path
        .map(Path::canonicalize)
        .transpose()
        .map_err(|error| format!("解析证据包 runtime 配置路径失败: {error}"))?;
    let market_spec_path = market_spec_path
        .map(Path::canonicalize)
        .transpose()
        .map_err(|error| format!("解析证据包 market spec 路径失败: {error}"))?;
    let summary_payload = std::fs::read_to_string(&summary_path)
        .map_err(|error| format!("读取深度回测摘要失败 {}: {error}", summary_path.display()))?;
    let summary: serde_json::Value = serde_json::from_str(&summary_payload)
        .map_err(|error| format!("深度回测摘要 JSON 无效: {error}"))?;
    let run_parts = expected_manifest.run_id.splitn(4, ':').collect::<Vec<_>>();
    if run_parts.len() != 4 || run_parts[0] != "depth-backtest" {
        return Err("当前深度离线复算只支持 depth-backtest RunManifest".into());
    }
    let tier = run_parts[1];
    if !matches!(tier, "l1" | "l2") {
        return Err(format!("深度 RunManifest 的 fill tier 非法: {tier}"));
    }
    let strategy = summary
        .get("signal")
        .and_then(|value| value.get("kind"))
        .and_then(serde_json::Value::as_str)
        .ok_or("深度回测摘要缺少内置策略 signal.kind")?;
    if strategy != run_parts[2]
        || summary
            .get("strategy_id")
            .and_then(serde_json::Value::as_str)
            != Some(format!("builtin-{strategy}").as_str())
    {
        return Err("深度摘要策略身份与 RunManifest.run_id 不一致".into());
    }
    if summary
        .get("instrument")
        .and_then(serde_json::Value::as_str)
        != Some(run_parts[3])
    {
        return Err("深度摘要标的与 RunManifest.run_id 不一致".into());
    }
    let quantity_raw = summary
        .get("signal")
        .and_then(|value| value.get("quantity_raw"))
        .and_then(serde_json::Value::as_str)
        .ok_or("深度回测摘要缺少 signal.quantity_raw")?
        .parse::<i128>()
        .map_err(|error| format!("深度回测摘要 signal.quantity_raw 非法: {error}"))?;
    if quantity_raw <= 0 || quantity_raw % qx_core::SCALE != 0 {
        return Err(format!(
            "深度回测摘要 quantity_raw 不能还原为整数下单量: {quantity_raw}"
        ));
    }
    let quantity = i64::try_from(quantity_raw / qx_core::SCALE)
        .map_err(|_| "深度回测摘要 quantity 超出 CLI 支持范围")?;
    let descriptors = summary
        .get("model_descriptors")
        .and_then(serde_json::Value::as_array)
        .ok_or("深度回测摘要缺少 model_descriptors")?;
    let descriptor = descriptors
        .iter()
        .filter_map(serde_json::Value::as_str)
        .find(|value| value.starts_with("orderbook-matching@v2 "))
        .ok_or("深度回测摘要缺少 orderbook-matching@v2 描述")?;
    let fee_bps = depth_descriptor_value(descriptor, "fee_bps")?
        .parse::<i64>()
        .map_err(|error| format!("深度撮合 fee_bps 非法: {error}"))?;
    let latency_snapshots = depth_descriptor_value(descriptor, "latency_snapshots")?
        .parse::<u64>()
        .map_err(|error| format!("深度撮合 latency_snapshots 非法: {error}"))?;
    let market_impact_bps = depth_descriptor_value(descriptor, "market_impact_bps")?
        .parse::<i64>()
        .map_err(|error| format!("深度撮合 market_impact_bps 非法: {error}"))?;
    if !descriptor.contains("queue_position=disabled") {
        return Err("当前深度离线复算不支持启用队列位置估算的产物".into());
    }
    let explicit_fee = summary
        .get("execution_costs")
        .and_then(|value| value.get("source"))
        .and_then(serde_json::Value::as_str)
        == Some("cli-flag");

    let temp_root = std::env::temp_dir()
        .canonicalize()
        .map_err(|error| format!("解析系统临时目录失败: {error}"))?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| format!("读取系统时钟失败: {error}"))?
        .as_nanos();
    let work = temp_root.join(format!("qx-depth-recompute-{}-{stamp}", std::process::id()));
    std::fs::create_dir(&work)
        .map_err(|error| format!("创建深度离线复算目录失败 {}: {error}", work.display()))?;
    let resolved_work = work
        .canonicalize()
        .map_err(|error| format!("解析深度离线复算目录失败: {error}"))?;
    if !resolved_work.starts_with(&temp_root) {
        return Err("深度离线复算目录越出系统临时目录".into());
    }
    let result = (|| {
        let executable = std::env::current_exe()
            .map_err(|error| format!("定位 qx-cli 当前构建失败: {error}"))?;
        let output_root = resolved_work.join("runs");
        let mut command = Command::new(executable);
        sanitize_recompute_environment(&mut command);
        command
            .args(["backtest", "book", "--fill-tier", tier, "--root"])
            .arg(&output_root);
        command.arg(strategy).arg(input_path);
        if let Some(market_spec) = market_spec_path {
            command.arg(market_spec).arg(quantity.to_string());
        } else if quantity != 1 {
            return Err(
                "深度回测使用了非默认 quantity，但离线包未附带 market_spec 位置参数".into(),
            );
        }
        if explicit_fee {
            command.args(["--fee-bps", &fee_bps.to_string()]);
        }
        command
            .args(["--market-impact-bps", &market_impact_bps.to_string()])
            .args(["--latency-snapshots", &latency_snapshots.to_string()]);
        if let Some(config) = runtime_config_path {
            command.arg("--config").arg(config);
        }
        let output = command
            .current_dir(&resolved_work)
            .output()
            .map_err(|error| format!("启动深度离线复算失败: {error}"))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!(
                "深度离线回测进程失败 ({}): {}",
                output.status,
                stderr.chars().take(1200).collect::<String>()
            ));
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let manifest_path = stdout
            .lines()
            .find_map(|line| line.strip_prefix("[RunManifest] path="))
            .map(PathBuf::from)
            .ok_or("深度离线回测输出缺少 RunManifest 路径")?;
        let manifest_path = manifest_path
            .canonicalize()
            .map_err(|error| format!("解析复算 RunManifest 路径失败: {error}"))?;
        if !manifest_path.starts_with(&resolved_work) {
            return Err("深度复算 RunManifest 路径越出隔离目录".into());
        }
        let actual = qx_core::RunManifest::from_json(
            &std::fs::read_to_string(&manifest_path)
                .map_err(|error| format!("读取复算 RunManifest 失败: {error}"))?,
        )?;
        if &actual != expected_manifest {
            return Err(format!(
                "深度离线复算 RunManifest 不一致: expected={} actual={}",
                expected_manifest.digest(),
                actual.digest()
            ));
        }
        Ok(())
    })();
    let cleanup = std::fs::remove_dir_all(&resolved_work).map_err(|error| {
        format!(
            "清理深度离线复算目录失败 {}: {error}",
            resolved_work.display()
        )
    });
    result?;
    cleanup?;
    Ok(())
}
