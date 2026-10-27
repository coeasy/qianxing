//! Build and verify completed-run records over already-persisted backtest artifacts.

use super::reader::{
    read_text_bounded, read_text_bounded_by_id, sha256_artifact_bounded, ArtifactId,
    ArtifactLocatorV1, ArtifactReader, FileArtifactReader,
};
use super::sink;
use crate::{AppError, AppErrorCategory};
use qx_spec::{
    ArtifactRef, ArtifactRefV2, FoundationDocument, ReplayVerdict, RunRecord, RunRecordV2,
    RunStatus,
};
use std::io::{BufRead, Read};
use std::path::{Path, PathBuf};

const MAX_RUN_RECORD_ARTIFACT_BYTES: u64 = 128 * 1024 * 1024;

/// Version of the complete parent portfolio envelope persisted by joint exchange backtests.
pub const MULTI_ACCOUNT_JOINT_BACKTEST_ENVELOPE_SCHEMA_VERSION: u32 = 2;
/// RunRecord kind for the complete joint portfolio envelope, distinct from its outcome DTO.
pub const MULTI_ACCOUNT_JOINT_BACKTEST_ENVELOPE_KIND: &str =
    "multi-account-joint-backtest-envelope-json";
const LEGACY_MULTI_ACCOUNT_JOINT_BACKTEST_KIND: &str = "multi-account-joint-backtest-result-json";
const JOINT_ENVELOPE_VERSION_REFUSAL: &str = "联合账户父产物 envelope schema_version 不受支持";

pub(crate) struct RunRecordArtifactLocators {
    pub summary: ArtifactLocatorV1,
    pub run_manifest: ArtifactLocatorV1,
    pub equity: ArtifactLocatorV1,
    pub fills: ArtifactLocatorV1,
}

/// `RunRecord` 产物的人类可读名，只用于错误文案。
const LABEL: &str = "RunRecord";

/// Persist a completed RunRecord after the summary, equity and fills artifacts exist.
pub fn persist_verified_run_record(
    manifest_path: &Path,
    summary_path: &Path,
    equity_path: &Path,
    fills_path: &Path,
) -> Result<PathBuf, String> {
    let manifest_payload = std::fs::read_to_string(manifest_path)
        .map_err(|error| format!("读取 RunRecord 对应 RunManifest 失败: {error}"))?;
    let manifest = qx_core::RunManifest::from_json(&manifest_payload)?;
    let summary: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(summary_path)
            .map_err(|error| format!("读取 RunRecord 对应摘要失败: {error}"))?,
    )
    .map_err(|error| format!("RunRecord 对应摘要 JSON 无效: {error}"))?;
    let locations = summary
        .get("artifact_locations")
        .and_then(serde_json::Value::as_object)
        .ok_or("RunRecord V2 对应摘要缺少 artifact_locations")?;
    let artifact_refs = [
        (
            "summary",
            "backtest-summary-json",
            summary
                .get("schema_version")
                .and_then(serde_json::Value::as_u64)
                .and_then(|version| u32::try_from(version).ok())
                .filter(|version| *version > 0)
                .ok_or("RunRecord V2 摘要 schema_version 无效")?,
            summary_path,
        ),
        (
            "equity",
            "backtest-equity-csv",
            equity_csv_schema_version(equity_path)?,
            equity_path,
        ),
        ("fills", "backtest-fills-csv", 2, fills_path),
        ("run_manifest", "run-manifest-json", 1, manifest_path),
    ]
    .into_iter()
    .map(|(name, kind, schema_version, path)| {
        let declared_location = locations
            .get(name)
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| format!("RunRecord V2 摘要缺少 {name} 存储定位"))?;
        if Path::new(declared_location) != path {
            return Err(format!(
                "RunRecord V2 摘要的 {name} 存储定位与实际产物不一致"
            ));
        }
        let byte_len = std::fs::metadata(path)
            .map_err(|error| format!("读取 RunRecord {name} 工件长度失败: {error}"))?
            .len();
        Ok(ArtifactRefV2 {
            name: name.to_string(),
            artifact_id: artifact_digest(path)?,
            kind: kind.to_string(),
            schema_version,
            byte_len,
        })
    })
    .collect::<Result<Vec<_>, String>>()?;
    let run_record = RunRecordV2 {
        schema_version: qx_spec::RUN_RECORD_V2_SCHEMA_VERSION,
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
    let readback = RunRecordV2::from_json(&payload)?;
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
    write_run_record(&path, &payload)?;
    Ok(path)
}

fn equity_csv_schema_version(path: &Path) -> Result<u32, String> {
    let file =
        std::fs::File::open(path).map_err(|error| format!("读取权益 CSV 表头失败: {error}"))?;
    let mut line = String::new();
    std::io::BufReader::new(file)
        .read_line(&mut line)
        .map_err(|error| format!("读取权益 CSV 表头失败: {error}"))?;
    equity_csv_header_schema_version(line.trim_end_matches(['\r', '\n']))
}

fn equity_csv_header_schema_version(headers: &str) -> Result<u32, String> {
    match headers {
        "ts,equity_raw,position_raw" => Ok(1),
        "ts,equity_raw,equity_exact,position_raw" => Ok(2),
        "index,ts,equity_raw,position_raw" => Ok(3),
        "index,ts,primary_equity_raw,reference_equity_raw,equity_raw,equity_exact" => Ok(4),
        "index,ts,equity_raw,equity_exact,position_raw" => Ok(5),
        _ => Err("权益 CSV 表头不属于受支持的 schema v1/v2/v3/v4/v5".into()),
    }
}

/// Verify a summary's completed RunRecord and recompute every referenced artifact digest.
pub fn verify_declared_run_record(summary: &serde_json::Value) -> Result<(), String> {
    verify_declared_run_record_with_reader(summary, &FileArtifactReader)
}

/// Verify a RunRecord through the caller's storage port. V1 paths remain compatibility locators;
/// V2 content IDs are resolved through the summary's separately authorized artifact_locations.
pub fn verify_declared_run_record_with_reader(
    summary: &serde_json::Value,
    reader: &dyn ArtifactReader,
) -> Result<(), String> {
    let Some(pointer) = summary
        .get("run_record")
        .and_then(serde_json::Value::as_str)
    else {
        if summary
            .get("schema_version")
            .and_then(serde_json::Value::as_i64)
            .is_some_and(|version| version >= 5)
        {
            return Err("v5 回测摘要缺少 run_record 指针".into());
        }
        return Ok(());
    };
    if summary
        .get("artifact_locations")
        .and_then(serde_json::Value::as_object)
        .is_some_and(|locations| locations.contains_key("joint_portfolio"))
    {
        return verify_multi_account_joint_run_record(summary, pointer, reader);
    }
    if summary
        .get("experiment_id")
        .and_then(serde_json::Value::as_str)
        .is_some()
    {
        return verify_experiment_run_record(summary, pointer, reader);
    }
    let payload = read_text_bounded(reader, pointer, MAX_RUN_RECORD_ARTIFACT_BYTES, LABEL)
        .map_err(|error| format!("读取回测 RunRecord 失败 {pointer}: {error}"))?;
    let (record, v2_refs) = normalize_run_record(&payload, summary)?;
    let record_path = PathBuf::from(pointer);
    let summary_path = sibling_summary_from_record(&record_path)?;
    let summary_ref = unique_artifact_ref(&record, "summary")?;
    let summary_payload = read_artifact_text(reader, summary_ref)?;
    let stored_summary: serde_json::Value = serde_json::from_str(&summary_payload)
        .map_err(|error| format!("RunRecord 引用的摘要 JSON 无效: {error}"))?;
    if stored_summary != *summary {
        return Err("报告载入的摘要与 RunRecord 所在目录的摘要不一致".into());
    }
    let manifest_path = summary
        .get("run_manifest")
        .and_then(serde_json::Value::as_str)
        .ok_or("v5 回测摘要缺少 run_manifest 指针")?;
    let manifest_ref = unique_artifact_ref(&record, "run_manifest")?;
    if manifest_ref.path != manifest_path {
        return Err("RunRecord 的 RunManifest 路径与摘要声明不一致".into());
    }
    let manifest_payload = read_artifact_text(reader, manifest_ref)?;
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
    for name in ["summary", "equity", "fills", "run_manifest"] {
        let _ = unique_artifact_ref(&record, name)?;
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
        let reference = unique_artifact_ref(&record, name)?;
        if Path::new(&reference.path) != path.as_path() {
            return Err(format!("RunRecord {name} 引用路径与摘要声明不一致"));
        }
    }
    for artifact in &record.artifact_refs {
        let artifact_id = ArtifactId::from_sha256_hex(artifact.digest.clone())
            .map_err(|error| format!("RunRecord {} 的 ArtifactId 无效: {error}", artifact.name))?;
        let actual = sha256_artifact_bounded(
            reader,
            &artifact_id,
            Some(&artifact.path),
            MAX_RUN_RECORD_ARTIFACT_BYTES,
        )
        .map_err(|error| format!("RunRecord 产物读取失败 {}: {error}", artifact.name))?;
        if actual != artifact.digest {
            return Err(format!(
                "RunRecord 产物摘要不匹配: {} 声明={} 实际={actual}",
                artifact.path, artifact.digest
            ));
        }
    }
    if let Some(v2_refs) = v2_refs.as_deref() {
        verify_v2_artifact_metadata(summary, v2_refs, reader)?;
    }
    Ok(())
}

fn verify_multi_account_joint_run_record(
    summary: &serde_json::Value,
    pointer: &str,
    reader: &dyn ArtifactReader,
) -> Result<(), String> {
    let record_payload =
        read_text_bounded(reader, pointer, MAX_RUN_RECORD_ARTIFACT_BYTES, LABEL)
            .map_err(|error| format!("读取联合账户 RunRecord V2 失败 {pointer}: {error}"))?;
    let record = RunRecordV2::from_json(&record_payload)
        .map_err(|error| format!("联合账户 RunRecord V2 无效 {pointer}: {error}"))?;
    let run_id = summary
        .get("run_id")
        .and_then(serde_json::Value::as_str)
        .ok_or("联合账户结果缺少 run_id")?;
    let result_hash = summary
        .get("result_hash")
        .and_then(serde_json::Value::as_str)
        .ok_or("联合账户结果缺少 result_hash")?;
    let manifest_path = summary
        .get("run_manifest_path")
        .and_then(serde_json::Value::as_str)
        .ok_or("联合账户结果缺少 RunManifest 路径")?;
    let locations = summary
        .get("artifact_locations")
        .and_then(serde_json::Value::as_object)
        .ok_or("联合账户结果缺少 artifact_locations")?;
    let expected_locations = [
        (
            "joint_portfolio",
            summary
                .get("artifact_path")
                .and_then(serde_json::Value::as_str)
                .ok_or("联合账户结果缺少 artifact_path")?,
        ),
        (
            "joint_equity",
            summary
                .get("equity_path")
                .and_then(serde_json::Value::as_str)
                .ok_or("联合账户结果缺少 equity_path")?,
        ),
        (
            "joint_fills",
            summary
                .get("fills_path")
                .and_then(serde_json::Value::as_str)
                .ok_or("联合账户结果缺少 fills_path")?,
        ),
        ("run_manifest", manifest_path),
    ];
    if locations.len() != expected_locations.len()
        || expected_locations.iter().any(|(name, expected)| {
            locations.get(*name).and_then(serde_json::Value::as_str) != Some(*expected)
        })
    {
        return Err("联合账户 RunRecord V2 ArtifactLocations 与结果路径不一致".into());
    }
    let manifest_payload = read_text_bounded(
        reader,
        manifest_path,
        MAX_RUN_RECORD_ARTIFACT_BYTES,
        "联合账户 RunManifest",
    )
    .map_err(|error| format!("读取联合账户 RunManifest 失败: {error}"))?;
    let manifest = qx_core::RunManifest::from_json(&manifest_payload)?;
    manifest.validate()?;
    if record.schema_version != qx_spec::RUN_RECORD_V2_SCHEMA_VERSION
        || record.run_id != run_id
        || record.status != RunStatus::Completed
        || record.input_digest != manifest.data_fingerprint
        || record.code_identity != format!("{}:{}", manifest.code_commit, manifest.runtime_version)
        || record.config_fingerprint != manifest.config_hash
        || record.started_at_ms != manifest.clock_start
        || record.finished_at_ms != Some(manifest.clock_end)
        || record.replay_verdict != ReplayVerdict::Verified
        || record.capability_level != qx_spec::CapabilityLevel::L1
    {
        return Err("联合账户 RunRecord V2 身份字段与父 RunManifest 不一致".into());
    }
    let expected_artifacts = [
        (
            "joint_portfolio",
            MULTI_ACCOUNT_JOINT_BACKTEST_ENVELOPE_KIND,
            MULTI_ACCOUNT_JOINT_BACKTEST_ENVELOPE_SCHEMA_VERSION,
        ),
        ("joint_equity", "joint-equity-json", 1),
        ("joint_fills", "joint-fills-json", 1),
        ("run_manifest", "run-manifest-json", 1),
    ];
    if record.artifact_refs.len() != expected_artifacts.len() {
        return Err("联合账户 RunRecord V2 必须恰有 4 个工件引用".into());
    }
    let mut portfolio = None;
    for (name, kind, schema_version) in expected_artifacts {
        let mut refs = record
            .artifact_refs
            .iter()
            .filter(|artifact| artifact.name == name);
        let reference = refs
            .next()
            .ok_or_else(|| format!("联合账户 RunRecord V2 缺少 {name}"))?;
        if refs.next().is_some() {
            return Err(format!("联合账户 RunRecord V2 {name} 存在重复工件引用"));
        }
        let normalized_reference = if name == "joint_portfolio" {
            normalize_legacy_joint_portfolio_artifact_ref(reference)
        } else if reference.kind == kind && reference.schema_version == schema_version {
            Some(reference.clone())
        } else {
            None
        }
        .ok_or_else(|| format!("联合账户 RunRecord V2 {name} 类型或 schema 版本无效"))?;
        let location = locations
            .get(name)
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| format!("联合账户 RunRecord V2 缺少 {name} 定位"))?;
        let artifact_id = ArtifactId::from_sha256_hex(normalized_reference.artifact_id.clone())
            .map_err(|error| format!("联合账户 RunRecord V2 {name} ArtifactId 无效: {error}"))?;
        let stream = reader
            .open_by_id(&artifact_id, Some(location))
            .map_err(|error| format!("读取联合账户 RunRecord V2 {name} 失败: {error}"))?;
        if stream.byte_len != normalized_reference.byte_len
            || stream.byte_len > MAX_RUN_RECORD_ARTIFACT_BYTES
        {
            return Err(format!("联合账户 RunRecord V2 {name} 工件长度无效"));
        }
        let payload = read_text_bounded_by_id(
            reader,
            &artifact_id,
            Some(location),
            MAX_RUN_RECORD_ARTIFACT_BYTES,
            "联合账户 RunRecord 工件",
        )
        .map_err(|error| format!("读取联合账户 RunRecord V2 {name} 内容失败: {error}"))?;
        if payload.len() as u64 != normalized_reference.byte_len
            || qx_strategy::sha256_hex(payload.as_bytes()) != normalized_reference.artifact_id
        {
            return Err(format!("联合账户 RunRecord V2 {name} 摘要或字节长度不匹配"));
        }
        if name == "run_manifest" && payload != manifest_payload {
            return Err("联合账户 RunRecord V2 manifest 内容与声明路径不一致".into());
        }
        if name == "joint_portfolio" {
            let artifact = verify_joint_portfolio_envelope_schema(&payload)?;
            if artifact.get("outcome") != Some(summary)
                || artifact
                    .get("result_hash")
                    .and_then(serde_json::Value::as_str)
                    != Some(result_hash)
            {
                return Err("联合账户 RunRecord V2 父产物与返回结果不一致".into());
            }
            portfolio = Some(artifact);
        }
    }
    let portfolio = portfolio.ok_or("联合账户 RunRecord V2 缺少父产物")?;
    crate::artifact_store::verify_declared_run_manifest_with_reader(&portfolio, reader)
        .map_err(|error| format!("联合账户 RunRecord V2 父身份复核失败: {error}"))?;
    Ok(())
}

/// Convert the pre-envelope RunRecord label into its explicit current contract. The referenced
/// bytes are still independently checked against the v2 envelope schema and content digest.
fn normalize_legacy_joint_portfolio_artifact_ref(
    reference: &ArtifactRefV2,
) -> Option<ArtifactRefV2> {
    if reference.kind == MULTI_ACCOUNT_JOINT_BACKTEST_ENVELOPE_KIND
        && reference.schema_version == MULTI_ACCOUNT_JOINT_BACKTEST_ENVELOPE_SCHEMA_VERSION
    {
        return Some(reference.clone());
    }
    if reference.kind == LEGACY_MULTI_ACCOUNT_JOINT_BACKTEST_KIND && reference.schema_version == 1 {
        return Some(ArtifactRefV2 {
            kind: MULTI_ACCOUNT_JOINT_BACKTEST_ENVELOPE_KIND.to_string(),
            schema_version: MULTI_ACCOUNT_JOINT_BACKTEST_ENVELOPE_SCHEMA_VERSION,
            ..reference.clone()
        });
    }
    None
}

fn verify_joint_portfolio_envelope_schema(payload: &str) -> Result<serde_json::Value, String> {
    let artifact: serde_json::Value = serde_json::from_str(payload)
        .map_err(|error| format!("联合账户父产物 JSON 无效: {error}"))?;
    let version = artifact
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
        .and_then(|version| u32::try_from(version).ok())
        .unwrap_or_default();
    if version != MULTI_ACCOUNT_JOINT_BACKTEST_ENVELOPE_SCHEMA_VERSION {
        return Err(format!(
            "{JOINT_ENVELOPE_VERSION_REFUSAL}: 实际 {:?}，本构建只认 {}",
            version, MULTI_ACCOUNT_JOINT_BACKTEST_ENVELOPE_SCHEMA_VERSION
        ));
    }
    Ok(artifact)
}

fn verify_experiment_run_record(
    summary: &serde_json::Value,
    pointer: &str,
    reader: &dyn ArtifactReader,
) -> Result<(), String> {
    let payload = read_text_bounded(reader, pointer, MAX_RUN_RECORD_ARTIFACT_BYTES, LABEL)
        .map_err(|error| format!("读取参数实验 RunRecord 失败 {pointer}: {error}"))?;
    let record = RunRecordV2::from_json(&payload)
        .map_err(|error| format!("参数实验 RunRecord V2 无效 {pointer}: {error}"))?;
    let experiment_id = summary
        .get("experiment_id")
        .and_then(serde_json::Value::as_str)
        .ok_or("参数实验报告缺少 experiment_id")?;
    let config_fingerprint = summary
        .get("canonical_run_spec_fingerprint")
        .and_then(serde_json::Value::as_str)
        .ok_or("参数实验报告缺少 CanonicalRunSpec 指纹")?;
    let input_digest = summary
        .get("input_data_fingerprint")
        .and_then(serde_json::Value::as_str)
        .ok_or("参数实验报告缺少输入数据指纹")?;
    let canonical_payload = summary
        .get("canonical_run_spec_json")
        .and_then(serde_json::Value::as_str)
        .ok_or("参数实验报告缺少 CanonicalRunSpec")?;
    let canonical = qx_spec::CanonicalRunSpec::from_json(canonical_payload)?;
    let manifest_path = summary
        .get("run_manifest")
        .and_then(serde_json::Value::as_str)
        .ok_or("参数实验报告缺少 RunManifest 指针")?;
    let locations = summary
        .get("artifact_locations")
        .and_then(serde_json::Value::as_object)
        .ok_or("参数实验报告缺少 ArtifactLocations")?;
    let report_index_path = summary
        .get("artifact_path")
        .and_then(serde_json::Value::as_str)
        .filter(|path| !path.trim().is_empty())
        .ok_or("参数实验报告缺少稳定报告索引路径")?;
    let report_path = locations
        .get("experiment_report")
        .and_then(serde_json::Value::as_str)
        .filter(|path| !path.trim().is_empty())
        .ok_or("参数实验 ArtifactLocations 缺少不可变报告路径")?;
    let locations_manifest = locations
        .get("run_manifest")
        .and_then(serde_json::Value::as_str);
    let expected_code_identity = format!(
        "{}:{}",
        canonical.code_revision,
        canonical
            .execution_model
            .get("runtime_version")
            .and_then(serde_json::Value::as_str)
            .ok_or("参数实验执行模型缺少 runtime_version")?
    );
    let has_input_spec = locations.contains_key("experiment_input");
    let expected_artifact_count = if has_input_spec { 3 } else { 2 };
    if record.schema_version != qx_spec::RUN_RECORD_V2_SCHEMA_VERSION
        || record.run_id != experiment_id
        || record.status != RunStatus::Completed
        || record.replay_verdict != ReplayVerdict::Verified
        || record.capability_level != qx_spec::CapabilityLevel::L1
        || record.input_digest != input_digest
        || record.config_fingerprint != config_fingerprint
        || record.code_identity != expected_code_identity
        || record.started_at_ms != canonical.clock.start_ts_ms
        || record.finished_at_ms != Some(canonical.clock.end_ts_ms)
        || locations.len() != expected_artifact_count
        || report_index_path == report_path
        || locations_manifest != Some(manifest_path)
        || record.artifact_refs.len() != expected_artifact_count
    {
        return Err("参数实验 RunRecord V2 身份字段或报告产物定位不一致".into());
    }

    let report_kind = match canonical.run_type.as_str() {
        "experiment" => "run-experiment-result-json",
        "depth_experiment" => "run-depth-experiment-result-json",
        "multi_market_experiment" => "run-multi-market-experiment-result-json",
        "multi_market_depth_experiment" => "run-multi-market-depth-experiment-result-json",
        "multi_leg_experiment" => "run-multi-leg-experiment-result-json",
        other => return Err(format!("RunRecord V2 不支持实验报告类型: {other}")),
    };
    let mut expected = vec![
        ("experiment_report", report_kind, 1_u32),
        ("run_manifest", "run-manifest-json", 1_u32),
    ];
    if has_input_spec {
        expected.push(("experiment_input", "run-experiment-input-json", 1_u32));
    }
    let mut report_bytes = None;
    let mut input_spec_bytes = None;
    for (name, kind, schema_version) in expected {
        let mut matches = record
            .artifact_refs
            .iter()
            .filter(|artifact| artifact.name == name);
        let artifact = matches
            .next()
            .ok_or_else(|| format!("参数实验 RunRecord V2 缺少 {name} 工件"))?;
        if matches.next().is_some()
            || artifact.kind != kind
            || artifact.schema_version != schema_version
        {
            return Err(format!("参数实验 RunRecord V2 {name} 工件类型或版本无效"));
        }
        let location = match name {
            "experiment_report" => report_path,
            "run_manifest" => manifest_path,
            "experiment_input" => locations
                .get("experiment_input")
                .and_then(serde_json::Value::as_str)
                .filter(|path| !path.trim().is_empty())
                .ok_or("参数实验 ArtifactLocations 缺少输入规格 sidecar")?,
            _ => return Err("参数实验 RunRecord V2 出现未知工件名".into()),
        };
        if locations.get(name).and_then(serde_json::Value::as_str) != Some(location) {
            return Err(format!("参数实验 RunRecord V2 {name} 定位与报告不一致"));
        }
        let artifact_id = ArtifactId::from_sha256_hex(artifact.artifact_id.clone())
            .map_err(|error| format!("参数实验 RunRecord V2 {name} ArtifactId 无效: {error}"))?;
        let mut stream = reader
            .open_by_id(&artifact_id, Some(location))
            .map_err(|error| format!("读取参数实验 RunRecord V2 {name} 工件失败: {error}"))?;
        if stream.byte_len != artifact.byte_len || stream.byte_len > MAX_RUN_RECORD_ARTIFACT_BYTES {
            return Err(format!("参数实验 RunRecord V2 {name} 工件长度无效"));
        }
        let mut bytes = Vec::with_capacity(stream.byte_len.min(1024 * 1024) as usize);
        stream
            .reader
            .by_ref()
            .take(MAX_RUN_RECORD_ARTIFACT_BYTES.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|error| format!("读取参数实验 RunRecord V2 {name} 工件失败: {error}"))?;
        if bytes.len() as u64 != artifact.byte_len
            || bytes.len() as u64 > MAX_RUN_RECORD_ARTIFACT_BYTES
            || qx_strategy::sha256_hex(&bytes) != artifact.artifact_id
        {
            return Err(format!("参数实验 RunRecord V2 {name} 内容摘要或长度不匹配"));
        }
        if name == "experiment_report" {
            report_bytes = Some(bytes);
        } else if name == "experiment_input" {
            input_spec_bytes = Some(bytes);
        }
    }
    let stored_report_bytes = report_bytes.ok_or("参数实验 RunRecord V2 缺少报告内容")?;
    let stored_report_payload = std::str::from_utf8(&stored_report_bytes)
        .map_err(|error| format!("RunRecord V2 引用的实验报告不是 UTF-8: {error}"))?;
    if let Some(input_bytes) = input_spec_bytes.as_deref() {
        verify_experiment_input_artifact(input_bytes, &canonical, experiment_id)?;
    }
    let mut multi_leg_candidates = None;
    let (
        stored_experiment_id,
        stored_artifact_path,
        stored_report_result_hash,
        stored_manifest_path,
        stored_record_path,
        stored_canonical_json,
        stored_canonical_fingerprint,
        stored_input_fingerprint,
        stored_locations,
    ) = match canonical.run_type.as_str() {
        "experiment" => {
            let report = crate::cases::RunExperimentResult::from_json(stored_report_payload)
                .map_err(|error| format!("RunRecord V2 引用的参数实验报告无效: {error}"))?;
            (
                report.experiment_id,
                report.artifact_path,
                report.result_hash,
                report.run_manifest,
                report.run_record,
                report.canonical_run_spec_json,
                report.canonical_run_spec_fingerprint,
                report.input_data_fingerprint,
                serde_json::to_value(report.artifact_locations).ok(),
            )
        }
        "depth_experiment" => {
            let report =
                crate::cases::RunDepthExperimentResult::from_json(stored_report_payload)
                    .map_err(|error| format!("RunRecord V2 引用的深度实验报告无效: {error}"))?;
            (
                report.experiment_id,
                report.artifact_path,
                report.result_hash,
                report.run_manifest,
                report.run_record,
                report.canonical_run_spec_json,
                report.canonical_run_spec_fingerprint,
                report.input_data_fingerprint,
                serde_json::to_value(report.artifact_locations).ok(),
            )
        }
        "multi_market_depth_experiment" => {
            let report =
                crate::cases::RunMultiMarketDepthExperimentResult::from_json(stored_report_payload)
                    .map_err(|error| {
                        format!("RunRecord V2 引用的多市场深度实验报告无效: {error}")
                    })?;
            (
                report.experiment_id,
                report.artifact_path,
                report.result_hash,
                report.run_manifest,
                report.run_record,
                report.canonical_run_spec_json,
                report.canonical_run_spec_fingerprint,
                report.input_data_fingerprint,
                serde_json::to_value(report.artifact_locations).ok(),
            )
        }
        "multi_market_experiment" => {
            let report =
                crate::cases::RunMultiMarketExperimentResult::from_json(stored_report_payload)
                    .map_err(|error| format!("RunRecord V2 引用的多市场实验报告无效: {error}"))?;
            (
                report.experiment_id,
                report.artifact_path,
                report.result_hash,
                report.run_manifest,
                report.run_record,
                report.canonical_run_spec_json,
                report.canonical_run_spec_fingerprint,
                report.input_data_fingerprint,
                serde_json::to_value(report.artifact_locations).ok(),
            )
        }
        "multi_leg_experiment" => {
            let report =
                crate::cases::RunMultiLegExperimentResult::from_json(stored_report_payload)
                    .map_err(|error| format!("RunRecord V2 引用的多腿实验报告无效: {error}"))?;
            multi_leg_candidates = Some(report.candidates.clone());
            (
                report.experiment_id,
                report.artifact_path,
                report.result_hash,
                report.run_manifest,
                report.run_record,
                report.canonical_run_spec_json,
                report.canonical_run_spec_fingerprint,
                report.input_data_fingerprint,
                serde_json::to_value(report.artifact_locations).ok(),
            )
        }
        other => return Err(format!("RunRecord V2 不支持实验报告类型: {other}")),
    };
    let summary_string = |name: &str| summary.get(name).and_then(serde_json::Value::as_str);
    if stored_experiment_id != experiment_id
        || stored_artifact_path != report_index_path
        || stored_report_result_hash.as_deref() != summary_string("result_hash")
        || stored_manifest_path.as_deref() != Some(manifest_path)
        || stored_record_path.as_deref() != Some(pointer)
        || stored_canonical_json.as_deref() != summary_string("canonical_run_spec_json")
        || stored_canonical_fingerprint.as_deref()
            != summary_string("canonical_run_spec_fingerprint")
        || stored_input_fingerprint.as_deref() != summary_string("input_data_fingerprint")
        || stored_locations != summary.get("artifact_locations").cloned()
    {
        return Err("载入的参数实验报告与 RunRecord V2 绑定的报告身份不一致".into());
    }
    if let Some(candidates) = multi_leg_candidates {
        crate::cases::verify_candidate_run_manifests_with_reader(
            &candidates,
            input_digest,
            &canonical,
            reader,
        )
        .map_err(|error| format!("多腿实验候选 RunManifest 核验失败: {}", error.message()))?;
    }
    let stored_result_hash = crate::cases::experiment_result_hash_from_json(stored_report_payload)?;
    let report_index_payload = read_text_bounded(
        reader,
        report_index_path,
        MAX_RUN_RECORD_ARTIFACT_BYTES,
        "参数实验稳定报告索引",
    )
    .map_err(|error| format!("读取参数实验稳定报告索引失败 {report_index_path}: {error}"))?;
    let summary_result_hash =
        crate::cases::experiment_result_hash_from_json(&report_index_payload)?;
    if report_index_payload != stored_report_payload
        || summary_result_hash != stored_result_hash
        || stored_report_result_hash.as_deref() != Some(stored_result_hash.as_str())
    {
        return Err("载入的参数实验报告事实与 RunRecord V2 报告摘要不一致".into());
    }
    Ok(())
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ExperimentInputArtifactV1 {
    schema_version: u32,
    code_commit: String,
    #[serde(default)]
    strategy_identity: Option<crate::StrategyArtifactIdentity>,
    spec: serde_json::Value,
    #[serde(default)]
    spec_json: Option<String>,
}

fn verify_experiment_input_artifact(
    payload: &[u8],
    canonical: &qx_spec::CanonicalRunSpec,
    experiment_id: &str,
) -> Result<(), String> {
    let input: ExperimentInputArtifactV1 = serde_json::from_slice(payload)
        .map_err(|error| format!("参数实验输入规格 sidecar 无效: {error}"))?;
    if input.schema_version != 1 || input.code_commit != canonical.code_revision {
        return Err("参数实验输入规格 sidecar 版本或代码身份与 CanonicalRunSpec 不一致".into());
    }
    if !input.spec.is_object() {
        return Err("参数实验输入规格 sidecar 的 spec 必须是 JSON 对象".into());
    }
    let spec_json = input
        .spec_json
        .as_deref()
        .ok_or("参数实验输入规格 sidecar 缺少精确 spec_json")?;
    let (stored_id, strategy_id) = match canonical.run_type.as_str() {
        "experiment" => {
            let spec = crate::RunExperimentSpec::from_json(spec_json)
                .map_err(|error| format!("参数实验输入规格无效: {error}"))?;
            let canonical_json = spec
                .to_json()
                .map_err(|error| format!("参数实验输入规格规范化失败: {error}"))?;
            if canonical_json != spec_json {
                return Err("参数实验输入规格 sidecar 不是规范 JSON".into());
            }
            (spec.experiment_id, spec.base.strategy.strategy_id)
        }
        "depth_experiment" => {
            let spec = crate::RunDepthExperimentSpec::from_json(spec_json)
                .map_err(|error| format!("深度参数实验输入规格无效: {error}"))?;
            let canonical_json = spec
                .to_json()
                .map_err(|error| format!("深度参数实验输入规格规范化失败: {error}"))?;
            if canonical_json != spec_json {
                return Err("深度参数实验输入规格 sidecar 不是规范 JSON".into());
            }
            (spec.experiment_id, spec.base.strategy.strategy_id)
        }
        "multi_market_depth_experiment" => {
            let spec = crate::RunMultiMarketDepthExperimentSpec::from_json(spec_json)
                .map_err(|error| format!("多市场深度参数实验输入规格无效: {error}"))?;
            let canonical_json = spec
                .to_json()
                .map_err(|error| format!("多市场深度参数实验输入规格规范化失败: {error}"))?;
            if canonical_json != spec_json {
                return Err("多市场深度参数实验输入规格 sidecar 不是规范 JSON".into());
            }
            if !spec
                .base
                .markets
                .iter()
                .any(|market| market.strategy.strategy_id == spec.target_strategy_id)
            {
                return Err("多市场深度参数实验输入规格缺少目标策略槽".into());
            }
            (spec.experiment_id, spec.target_strategy_id)
        }
        "multi_market_experiment" => {
            let spec = crate::RunMultiMarketExperimentSpec::from_json(spec_json)
                .map_err(|error| format!("多市场参数实验输入规格无效: {error}"))?;
            let canonical_json = spec
                .to_json()
                .map_err(|error| format!("多市场参数实验输入规格规范化失败: {error}"))?;
            if canonical_json != spec_json {
                return Err("多市场参数实验输入规格 sidecar 不是规范 JSON".into());
            }
            (spec.experiment_id, spec.target_strategy_id)
        }
        "multi_leg_experiment" => {
            let spec = crate::RunMultiLegExperimentSpec::from_json(spec_json)
                .map_err(|error| format!("多腿参数实验输入规格无效: {error}"))?;
            let canonical_json = spec
                .to_json()
                .map_err(|error| format!("多腿参数实验输入规格规范化失败: {error}"))?;
            if canonical_json != spec_json {
                return Err("多腿参数实验输入规格 sidecar 不是规范 JSON".into());
            }
            (spec.experiment_id, spec.base.strategy.strategy_id)
        }
        other => return Err(format!("实验输入规格 sidecar 不支持 RunType={other}")),
    };
    if stored_id != experiment_id {
        return Err("参数实验输入规格 sidecar 的 experiment_id 与 RunRecord 不一致".into());
    }
    match input.strategy_identity {
        Some(identity)
            if identity.strategy_id == strategy_id
                && canonical.strategies.iter().any(|strategy| {
                    strategy.strategy_id == identity.strategy_id
                        && strategy.source_sha256.as_deref()
                            == Some(identity.source_sha256.as_str())
                }) => {}
        Some(_) => return Err("参数实验外部策略身份与 CanonicalRunSpec 不一致".into()),
        None if canonical
            .strategies
            .iter()
            .any(|strategy| strategy.source_sha256.is_some()) =>
        {
            return Err("参数实验 CanonicalRunSpec 声明外部策略但输入 sidecar 未绑定身份".into());
        }
        None => {}
    }
    Ok(())
}

/// Load content IDs and separately declared storage locations for primary report reads.
/// The later identity verification rechecks lengths, digests and cross-artifact facts.
pub(crate) fn declared_run_record_artifact_locators(
    summary: &serde_json::Value,
    reader: &dyn ArtifactReader,
) -> Result<Option<RunRecordArtifactLocators>, AppError> {
    let Some(pointer) = summary
        .get("run_record")
        .and_then(serde_json::Value::as_str)
    else {
        if summary
            .get("schema_version")
            .and_then(serde_json::Value::as_i64)
            .is_some_and(|version| version >= 5)
        {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "v5 回测摘要缺少 run_record 指针",
            ));
        }
        return Ok(None);
    };
    let payload = read_text_bounded(reader, pointer, MAX_RUN_RECORD_ARTIFACT_BYTES, LABEL)?;
    let (record, _) = normalize_run_record(&payload, summary).map_err(|error| {
        AppError::new(
            AppErrorCategory::InvalidInput,
            format!("回测 RunRecord 无效 {pointer}: {error}"),
        )
    })?;
    if record.artifact_refs.len() != 4 {
        return Err(AppError::new(
            AppErrorCategory::InvalidInput,
            format!(
                "本地回测 RunRecord 必须恰有 4 个工件引用，实际 {}",
                record.artifact_refs.len()
            ),
        ));
    }
    for name in ["summary", "equity", "fills", "run_manifest"] {
        let _ = unique_artifact_ref(&record, name)
            .map_err(|error| AppError::new(AppErrorCategory::InvalidInput, error))?;
    }
    let locator = |name: &str| -> Result<ArtifactLocatorV1, AppError> {
        let reference = unique_artifact_ref(&record, name)
            .map_err(|error| AppError::new(AppErrorCategory::InvalidInput, error))?;
        let id = ArtifactId::from_sha256_hex(reference.digest.clone()).map_err(|error| {
            AppError::new(
                AppErrorCategory::InvalidInput,
                format!("RunRecord {name} 的 ArtifactId 无效: {error}"),
            )
        })?;
        Ok(ArtifactLocatorV1 {
            id,
            legacy_reference: reference.path.clone(),
        })
    };
    Ok(Some(RunRecordArtifactLocators {
        summary: locator("summary")?,
        run_manifest: locator("run_manifest")?,
        equity: locator("equity")?,
        fills: locator("fills")?,
    }))
}

fn normalize_run_record(
    payload: &str,
    summary: &serde_json::Value,
) -> Result<(RunRecord, Option<Vec<ArtifactRefV2>>), String> {
    let value: serde_json::Value =
        serde_json::from_str(payload).map_err(|error| format!("运行记录解析失败: {error}"))?;
    match value
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
    {
        Some(version) if version == u64::from(qx_spec::RUN_RECORD_SCHEMA_VERSION) => {
            Ok((RunRecord::from_json(payload)?, None))
        }
        Some(version) if version == u64::from(qx_spec::RUN_RECORD_V2_SCHEMA_VERSION) => {
            let record = RunRecordV2::from_json(payload)?;
            let locations = summary
                .get("artifact_locations")
                .and_then(serde_json::Value::as_object)
                .ok_or("RunRecord V2 摘要缺少 artifact_locations")?;
            let artifact_refs = record
                .artifact_refs
                .iter()
                .map(|artifact| {
                    let path = locations
                        .get(&artifact.name)
                        .and_then(serde_json::Value::as_str)
                        .filter(|path| !path.trim().is_empty())
                        .ok_or_else(|| format!("RunRecord V2 缺少 {} 的存储定位", artifact.name))?;
                    Ok(ArtifactRef {
                        name: artifact.name.clone(),
                        path: path.to_string(),
                        digest: artifact.artifact_id.clone(),
                    })
                })
                .collect::<Result<Vec<_>, String>>()?;
            let normalized = RunRecord {
                schema_version: qx_spec::RUN_RECORD_SCHEMA_VERSION,
                run_id: record.run_id,
                status: record.status,
                input_digest: record.input_digest,
                code_identity: record.code_identity,
                config_fingerprint: record.config_fingerprint,
                started_at_ms: record.started_at_ms,
                finished_at_ms: record.finished_at_ms,
                artifact_refs,
                replay_verdict: record.replay_verdict,
                capability_level: record.capability_level,
            };
            normalized.validate()?;
            Ok((normalized, Some(record.artifact_refs)))
        }
        Some(version) => Err(format!("运行记录 schema_version={version} 不受支持")),
        None => Err("运行记录缺少有效 schema_version".into()),
    }
}

fn verify_v2_artifact_metadata(
    summary: &serde_json::Value,
    artifacts: &[ArtifactRefV2],
    reader: &dyn ArtifactReader,
) -> Result<(), String> {
    let locations = summary
        .get("artifact_locations")
        .and_then(serde_json::Value::as_object)
        .ok_or("RunRecord V2 摘要缺少 artifact_locations")?;
    let summary_schema_version = summary
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
        .and_then(|version| u32::try_from(version).ok())
        .filter(|version| *version > 0)
        .ok_or("RunRecord V2 摘要 schema_version 无效")?;
    let expected = [
        ("summary", "backtest-summary-json", summary_schema_version),
        ("equity", "backtest-equity-csv", 1_u32),
        ("fills", "backtest-fills-csv", 2_u32),
        ("run_manifest", "run-manifest-json", 1_u32),
    ];
    if artifacts.len() != expected.len() {
        return Err(format!(
            "本地回测 RunRecord V2 必须恰有 {} 个工件引用，实际 {}",
            expected.len(),
            artifacts.len()
        ));
    }
    for (name, kind, schema_version) in expected {
        let artifact = artifacts
            .iter()
            .find(|artifact| artifact.name == name)
            .ok_or_else(|| format!("RunRecord V2 缺少 {name} 产物引用"))?;
        let schema_matches = match name {
            "fills" => matches!(artifact.schema_version, 1 | 2 | 3),
            "equity" => matches!(artifact.schema_version, 1 | 2 | 3 | 4 | 5),
            _ => artifact.schema_version == schema_version,
        };
        if artifact.kind != kind || !schema_matches {
            return Err(format!("RunRecord V2 {name} 工件类型或 schema 版本不匹配"));
        }
        let location = locations
            .get(name)
            .and_then(serde_json::Value::as_str)
            .filter(|location| !location.trim().is_empty())
            .ok_or_else(|| format!("RunRecord V2 摘要缺少 {name} 存储定位"))?;
        let id = ArtifactId::from_sha256_hex(artifact.artifact_id.clone())
            .map_err(|error| format!("RunRecord V2 {name} 的 ArtifactId 无效: {error}"))?;
        let stream = reader
            .open_by_id(&id, Some(location))
            .map_err(|error| format!("读取 RunRecord V2 {name} 工件失败: {error}"))?;
        if stream.byte_len != artifact.byte_len {
            return Err(format!(
                "RunRecord V2 {name} 工件长度不匹配: 声明={} 实际={}",
                artifact.byte_len, stream.byte_len
            ));
        }
        if stream.byte_len > MAX_RUN_RECORD_ARTIFACT_BYTES {
            return Err(format!("RunRecord V2 {name} 工件超过读取上限"));
        }
        let actual = qx_strategy::file_digest::sha256_reader_hex_bounded(
            stream.reader,
            MAX_RUN_RECORD_ARTIFACT_BYTES,
        )
        .map_err(|error| format!("计算 RunRecord V2 {name} SHA-256 失败: {error}"))?;
        if actual != artifact.artifact_id {
            return Err(format!("RunRecord V2 {name} ArtifactId 内容摘要不匹配"));
        }
    }
    Ok(())
}

fn unique_artifact_ref<'a>(record: &'a RunRecord, name: &str) -> Result<&'a ArtifactRef, String> {
    let mut matches = record
        .artifact_refs
        .iter()
        .filter(|artifact| artifact.name == name);
    let reference = matches
        .next()
        .ok_or_else(|| format!("RunRecord 缺少 {name} 产物引用"))?;
    if matches.next().is_some() {
        return Err(format!("RunRecord 包含重复的 {name} 产物引用"));
    }
    Ok(reference)
}

fn read_artifact_text(
    reader: &dyn ArtifactReader,
    artifact: &ArtifactRef,
) -> Result<String, String> {
    let artifact_id = ArtifactId::from_sha256_hex(artifact.digest.clone())
        .map_err(|error| format!("RunRecord {} 的 ArtifactId 无效: {error}", artifact.name))?;
    read_text_bounded_by_id(
        reader,
        &artifact_id,
        Some(&artifact.path),
        MAX_RUN_RECORD_ARTIFACT_BYTES,
        &format!("RunRecord {}", artifact.name),
    )
    .map_err(|error| error.message().to_string())
}

fn artifact_digest(path: &Path) -> Result<String, String> {
    qx_strategy::file_digest::sha256_file_hex(path)
}

/// RunRecord 的落盘走全仓唯一发布出口（`artifact_store::sink`）：
/// 内容相同幂等、不同即冲突、崩溃不留半份。⚠️ 只写语义，不在这里重复临时文件逻辑。
fn write_run_record(path: &Path, payload: &str) -> Result<(), String> {
    sink::publish(path, payload, sink::PublishMode::Idempotent)
        .map_err(|error| format!("{LABEL}落盘失败: {error}"))
}

fn sibling_summary_from_record(record_path: &Path) -> Result<PathBuf, String> {
    let name = record_path
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_suffix(".record.json"))
        .ok_or_else(|| format!("RunRecord 文件名无效: {}", record_path.display()))?;
    Ok(record_path.with_file_name(format!("{name}.summary.json")))
}

#[cfg(test)]
mod joint_portfolio_envelope_schema_tests {
    use super::{
        verify_joint_portfolio_envelope_schema, JOINT_ENVELOPE_VERSION_REFUSAL,
        MULTI_ACCOUNT_JOINT_BACKTEST_ENVELOPE_SCHEMA_VERSION,
    };

    #[test]
    fn joint_portfolio_envelope_accepts_the_declared_schema_version() {
        let payload = serde_json::json!({
            "schema_version": MULTI_ACCOUNT_JOINT_BACKTEST_ENVELOPE_SCHEMA_VERSION
        })
        .to_string();
        assert!(verify_joint_portfolio_envelope_schema(&payload).is_ok());
    }

    #[test]
    fn joint_portfolio_envelope_refuses_unsupported_schema_version() {
        let payload = serde_json::json!({ "schema_version": 3 }).to_string();
        let error = verify_joint_portfolio_envelope_schema(&payload)
            .expect_err("future envelope versions must fail closed");
        assert!(error.contains(JOINT_ENVELOPE_VERSION_REFUSAL));
    }
}

#[cfg(test)]
mod equity_csv_schema_tests {
    use super::equity_csv_header_schema_version;

    #[test]
    fn recognizes_legacy_exact_and_indexed_equity_csv_headers() {
        assert_eq!(
            equity_csv_header_schema_version("ts,equity_raw,position_raw").unwrap(),
            1
        );
        assert_eq!(
            equity_csv_header_schema_version("ts,equity_raw,equity_exact,position_raw").unwrap(),
            2
        );
        assert_eq!(
            equity_csv_header_schema_version("index,ts,equity_raw,position_raw").unwrap(),
            3
        );
        assert_eq!(
            equity_csv_header_schema_version(
                "index,ts,primary_equity_raw,reference_equity_raw,equity_raw,equity_exact"
            )
            .unwrap(),
            4
        );
        assert_eq!(
            equity_csv_header_schema_version("index,ts,equity_raw,equity_exact,position_raw")
                .unwrap(),
            5
        );
    }

    #[test]
    fn rejects_unknown_equity_csv_headers() {
        assert!(equity_csv_header_schema_version("timestamp,equity").is_err());
    }
}
