//! 回测完成记录：把已重放验证的运行产物挂到 qx-spec 的 RunRecord。
//!
//! 这里只消费由同一轮回测产出的 RunManifest 与文件，不补算交易指标、不读取目录猜最新运行。

use super::*;
use qx_spec::{ArtifactRef, FoundationDocument, ReplayVerdict, RunRecord, RunStatus};

/// 流式读取产物摘要，避免大权益曲线/成交文件被整份装入内存。
fn artifact_digest(path: &Path) -> Result<String, String> {
    qx_strategy::file_digest::sha256_file_hex(path)
}

/// 仅在摘要、权益曲线和成交文件都已写完后调用，生成并验证一条 completed RunRecord。
pub(crate) fn persist_verified_run_record(
    manifest_path: &Path,
    summary_path: &Path,
    equity_path: &Path,
    fills_path: &Path,
) -> Result<PathBuf, String> {
    let manifest_payload = std::fs::read_to_string(manifest_path)
        .map_err(|error| format!("读取 RunRecord 对应 RunManifest 失败: {error}"))?;
    let manifest = qx_core::RunManifest::from_json(&manifest_payload)?;
    let artifact_refs = [
        ("summary", summary_path),
        ("equity", equity_path),
        ("fills", fills_path),
        ("run_manifest", manifest_path),
    ]
    .into_iter()
    .map(|(name, path)| {
        Ok(ArtifactRef {
            name: name.to_string(),
            path: path.to_string_lossy().into_owned(),
            digest: artifact_digest(path)?,
        })
    })
    .collect::<Result<Vec<_>, String>>()?;
    let run_record = RunRecord {
        schema_version: qx_spec::RUN_RECORD_SCHEMA_VERSION,
        run_id: manifest.run_id,
        status: RunStatus::Completed,
        input_digest: manifest.data_fingerprint,
        code_identity: format!("{}:{}", manifest.code_commit, manifest.runtime_version),
        config_fingerprint: manifest.config_hash,
        started_at_ms: manifest.clock_start,
        finished_at_ms: Some(manifest.clock_end),
        artifact_refs,
        replay_verdict: ReplayVerdict::Verified,
        capability_level: qx_spec::CapabilityLevel::L1,
    };
    let payload = run_record.to_json()?;
    // 走统一规格漏斗再读一次，避免写侧绕过对象校验或 schema 版本漂移。
    let readback = RunRecord::from_json(&payload)?;
    if readback != run_record {
        return Err("RunRecord 规范化读回与写入对象不一致".into());
    }
    let path = summary_path.with_file_name(
        summary_path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_suffix(".summary.json"))
            .map(|stem| format!("{stem}.record.json"))
            .ok_or_else(|| format!("回测摘要文件名无效: {}", summary_path.display()))?,
    );
    super::write_backtest_artifact(&path, &payload, "RunRecord")?;
    Ok(path)
}

/// 验证摘要声明的 completed RunRecord，并逐个重算其引用产物的 SHA-256。
/// v5 起 RunRecord 是摘要协议的一部分；旧世代没有该字段时继续兼容。
pub(crate) fn verify_declared_run_record(summary: &serde_json::Value) -> Result<(), String> {
    let Some(pointer) = summary
        .get("run_record")
        .and_then(serde_json::Value::as_str)
    else {
        if summary_schema_version(summary).is_some_and(|version| version >= 5) {
            return Err("v5 回测摘要缺少 run_record 指针".into());
        }
        return Ok(());
    };
    let record_path = PathBuf::from(pointer);
    let payload = std::fs::read_to_string(&record_path)
        .map_err(|error| format!("读取回测 RunRecord 失败 {pointer}: {error}"))?;
    let record = RunRecord::from_json(&payload)?;
    let summary_path = sibling_summary_from_record(&record_path)?;
    let summary_payload = std::fs::read_to_string(&summary_path).map_err(|error| {
        format!(
            "读取 RunRecord 引用的摘要失败 {}: {error}",
            summary_path.display()
        )
    })?;
    let stored_summary: serde_json::Value = serde_json::from_str(&summary_payload)
        .map_err(|error| format!("RunRecord 引用的摘要 JSON 无效: {error}"))?;
    if stored_summary != *summary {
        return Err("报告载入的摘要与 RunRecord 所在目录的摘要不一致".into());
    }
    let manifest_path = summary
        .get("run_manifest")
        .and_then(serde_json::Value::as_str)
        .ok_or("v5 回测摘要缺少 run_manifest 指针")?;
    let manifest_payload = std::fs::read_to_string(manifest_path)
        .map_err(|error| format!("读取 RunRecord 的 RunManifest 失败: {error}"))?;
    let manifest = qx_core::RunManifest::from_json(&manifest_payload)?;
    if record.run_id != manifest.run_id
        || record.input_digest != manifest.data_fingerprint
        || record.config_fingerprint != manifest.config_hash
        || record.code_identity != format!("{}:{}", manifest.code_commit, manifest.runtime_version)
        || record.capability_level != qx_spec::CapabilityLevel::L1
    {
        return Err("RunRecord 身份字段或能力等级与本地回测 RunManifest 不一致".into());
    }
    if record.artifact_refs.len() != 4 {
        return Err(format!(
            "本地回测 RunRecord 必须恰有 4 个工件引用，实际 {}",
            record.artifact_refs.len()
        ));
    }
    let stem = summary_path
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_suffix(".summary.json"))
        .ok_or_else(|| format!("回测摘要文件名无效: {}", summary_path.display()))?;
    let expected = [
        ("summary", summary_path.clone()),
        (
            "equity",
            summary_path.with_file_name(format!("{stem}.equity.csv")),
        ),
        (
            "fills",
            summary_path.with_file_name(format!("{stem}.fills.csv")),
        ),
        ("run_manifest", PathBuf::from(manifest_path)),
    ];
    for (name, path) in expected {
        let reference = record
            .artifact_refs
            .iter()
            .find(|artifact| artifact.name == name)
            .ok_or_else(|| format!("RunRecord 缺少 {name} 产物引用"))?;
        if Path::new(&reference.path) != path.as_path() {
            return Err(format!("RunRecord {name} 引用路径与摘要声明不一致"));
        }
    }
    for artifact in &record.artifact_refs {
        let actual = artifact_digest(Path::new(&artifact.path))?;
        if actual != artifact.digest {
            return Err(format!(
                "RunRecord 产物摘要不匹配: {} 声明={} 实际={actual}",
                artifact.path, artifact.digest
            ));
        }
    }
    Ok(())
}

fn sibling_summary_from_record(record_path: &Path) -> Result<PathBuf, String> {
    let name = record_path
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_suffix(".record.json"))
        .ok_or_else(|| format!("RunRecord 文件名无效: {}", record_path.display()))?;
    Ok(record_path.with_file_name(format!("{name}.summary.json")))
}
