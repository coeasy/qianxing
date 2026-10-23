//! Portable, batch-level evidence for isolated multi-account Bar runs.

use qx_app::{MultiAccountBacktestResult, MultiAccountBacktestSpec};
use qx_spec::{EvidenceBundle, EvidenceGeneration, FoundationDocument};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const SCHEMA_VERSION: u32 = 1;
const MANIFEST_NAME: &str = "multi-account-evidence.json";
const RESULT_NAME: &str = "multi-account-result.json";
const SPEC_NAME: &str = "multi-account-spec.json";
static STAGING_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileDigest {
    path: String,
    sha256: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AccountEvidence {
    account_id: String,
    run_id: String,
    result_hash: String,
    instrument: String,
    strategy_id: String,
    data_fingerprint: String,
    evidence_dir: String,
    evidence_sha256: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MultiAccountEvidenceManifest {
    schema_version: u32,
    kind: String,
    batch_id: String,
    settlement_currency: String,
    spec_fingerprint: String,
    result: FileDigest,
    spec: FileDigest,
    accounts: Vec<AccountEvidence>,
}

struct StagingDirectory(PathBuf);

impl Drop for StagingDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub(crate) fn pack_multi_account_run(
    result_path: &Path,
    spec_path: &Path,
    output_dir: &Path,
) -> Result<(), String> {
    if output_dir.exists() {
        return Err(format!("导出目录已存在，不覆盖: {}", output_dir.display()));
    }
    let result_bytes = std::fs::read(result_path)
        .map_err(|error| format!("读取多账户结果失败 {}: {error}", result_path.display()))?;
    let spec_bytes = std::fs::read(spec_path)
        .map_err(|error| format!("读取多账户规格失败 {}: {error}", spec_path.display()))?;
    let result: MultiAccountBacktestResult = serde_json::from_slice(&result_bytes)
        .map_err(|error| format!("多账户结果 JSON 无效: {error}"))?;
    let spec_text = std::str::from_utf8(&spec_bytes)
        .map_err(|error| format!("多账户规格不是 UTF-8: {error}"))?;
    let spec = MultiAccountBacktestSpec::from_json(spec_text).map_err(|error| error.to_string())?;
    crate::strategy_multi_account_diagnostics::validate_batch_identity(&result, &spec)?;
    validate_successful_outcomes(&result, &spec)?;

    let parent = output_dir
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("创建批次证据包父目录失败 {}: {error}", parent.display()))?;
    let staging = reserve_staging_directory(parent, output_dir)?;
    let members = spec
        .runs
        .iter()
        .map(|member| (member.account_id.as_deref().unwrap_or_default(), member))
        .collect::<BTreeMap<_, _>>();
    let mut accounts = Vec::with_capacity(result.summary.accounts_succeeded);
    let mut portable_result = result.clone();
    let mut portable_spec = spec.clone();
    for run in result.runs.iter().filter(|run| run.outcome.is_some()) {
        let outcome = run.outcome.as_ref().expect("filtered successful run");
        let member = members
            .get(run.account_id.as_str())
            .ok_or_else(|| format!("成功账户 {} 不在批次规格中", run.account_id))?;
        let source_bundle = locate_account_bundle(Path::new(&outcome.artifacts.summary))?;
        crate::run_evidence_verify::verify_run_bundle_quiet(&source_bundle, false).map_err(
            |error| {
                format!(
                    "账户 {} 的来源 EvidenceBundle 未通过复核: {error}",
                    run.account_id
                )
            },
        )?;
        let bundle = read_run_bundle(&source_bundle)?;
        let EvidenceGeneration::Run(evidence) = &bundle.generation else {
            return Err(format!(
                "账户 {} 的来源 EvidenceBundle 不是 run 类型",
                run.account_id
            ));
        };
        verify_member_evidence(run, outcome, member, evidence)?;
        verify_bundle_summary_account(&source_bundle, &bundle, &run.account_id)?;

        let evidence_dir = format!("accounts/{}", account_directory_id(&run.account_id));
        let portable_dir = staging.0.join(&evidence_dir);
        if let Some(parent) = portable_dir.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("创建账户证据子目录失败 {}: {error}", parent.display()))?;
        }
        crate::run_evidence_export::export_run_bundle_quiet(&source_bundle, &portable_dir)
            .map_err(|error| format!("导出账户 {} 的便携证据失败: {error}", run.account_id))?;
        let portable_bundle = portable_dir.join("evidence.json");
        crate::run_evidence_verify::verify_run_bundle_quiet(&portable_bundle, false).map_err(
            |error| {
                format!(
                    "账户 {} 的便携 EvidenceBundle 未通過复核: {error}",
                    run.account_id
                )
            },
        )?;
        let portable = read_run_bundle(&portable_bundle)?;
        let EvidenceGeneration::Run(portable_evidence) = &portable.generation else {
            return Err("便携账户 EvidenceBundle 类型改变".into());
        };
        verify_member_evidence(run, outcome, member, portable_evidence)?;
        let relative_dataset = validate_portable_relative(&portable_evidence.dataset.path)?;
        let packaged_member = portable_spec
            .runs
            .iter_mut()
            .find(|candidate| candidate.account_id.as_deref() == Some(run.account_id.as_str()))
            .ok_or("便携规格中缺少成功账户")?;
        packaged_member.bars_path = format!("{evidence_dir}/{relative_dataset}");
        packaged_member.output_dir = format!("{evidence_dir}/replay-output");
        let packaged_run = portable_result
            .runs
            .iter_mut()
            .find(|candidate| candidate.account_id == run.account_id)
            .and_then(|candidate| candidate.outcome.as_mut())
            .ok_or("便携结果中缺少成功账户")?;
        packaged_run.artifacts.run_manifest =
            portable_artifact_path(&evidence_dir, portable_evidence, "run_manifest")?;
        packaged_run.artifacts.summary =
            portable_artifact_path(&evidence_dir, portable_evidence, "summary")?;
        packaged_run.artifacts.equity =
            portable_artifact_path(&evidence_dir, portable_evidence, "equity")?;
        packaged_run.artifacts.fills =
            portable_artifact_path(&evidence_dir, portable_evidence, "fills")?;
        packaged_run.artifacts.event_trace = portable_evidence
            .artifacts
            .iter()
            .find(|artifact| artifact.name == "event_trace")
            .map(|artifact| format!("{evidence_dir}/{}", artifact.path));
        verify_portable_run_paths(
            &evidence_dir,
            packaged_member,
            packaged_run,
            portable_evidence,
        )?;
        let evidence_sha256 = qx_strategy::file_digest::sha256_file_hex(&portable_bundle)?;
        accounts.push(AccountEvidence {
            account_id: run.account_id.clone(),
            run_id: outcome.run_id.clone(),
            result_hash: outcome.result_hash.clone(),
            instrument: outcome.instrument.clone(),
            strategy_id: portable_evidence.identity.strategy_id.clone(),
            data_fingerprint: portable_evidence.dataset.content_fingerprint.clone(),
            evidence_dir,
            evidence_sha256,
        });
    }
    accounts.sort_by(|left, right| left.account_id.cmp(&right.account_id));
    let portable_result_bytes = serde_json::to_vec_pretty(&portable_result)
        .map_err(|error| format!("序列化便携多账户结果失败: {error}"))?;
    let portable_spec_bytes = serde_json::to_vec_pretty(&portable_spec)
        .map_err(|error| format!("序列化便携多账户规格失败: {error}"))?;
    write_new(
        &staging.0.join(RESULT_NAME),
        &portable_result_bytes,
        "多账户结果",
    )?;
    write_new(
        &staging.0.join(SPEC_NAME),
        &portable_spec_bytes,
        "多账户规格",
    )?;
    let manifest = MultiAccountEvidenceManifest {
        schema_version: SCHEMA_VERSION,
        kind: "qianxing-multi-account-run-evidence".into(),
        batch_id: result.batch_id.clone(),
        settlement_currency: result.settlement_currency.clone(),
        spec_fingerprint: result.spec_fingerprint.clone(),
        result: FileDigest {
            path: RESULT_NAME.into(),
            sha256: qx_strategy::sha256_hex(&portable_result_bytes),
        },
        spec: FileDigest {
            path: SPEC_NAME.into(),
            sha256: qx_strategy::sha256_hex(&portable_spec_bytes),
        },
        accounts,
    };
    let manifest_bytes = serde_json::to_vec_pretty(&manifest)
        .map_err(|error| format!("序列化多账户 Evidence manifest 失败: {error}"))?;
    write_new(
        &staging.0.join(MANIFEST_NAME),
        &manifest_bytes,
        "批次 Evidence manifest",
    )?;
    std::fs::rename(&staging.0, output_dir).map_err(|error| {
        format!(
            "提交多账户便携证据包失败 {} -> {}: {error}",
            staging.0.display(),
            output_dir.display()
        )
    })?;
    crate::print_machine_json(&serde_json::json!({
        "ok": true,
        "operation": "multi-account-run-evidence-packed",
        "batch_id": result.batch_id,
        "directory": output_dir.display().to_string(),
        "manifest": output_dir.join(MANIFEST_NAME).display().to_string(),
        "accounts_verified": result.summary.accounts_succeeded,
        "accounts_failed": result.summary.accounts_failed,
        "portable": true,
        "signed": false
    }))
}

pub(crate) fn verify_multi_account_run(bundle_dir: &Path) -> Result<(), String> {
    let root = bundle_dir
        .canonicalize()
        .map_err(|error| format!("解析多账户证据目录失败 {}: {error}", bundle_dir.display()))?;
    if !root.is_dir() {
        return Err("多账户便携证据路径必须是目录".into());
    }
    let manifest_path = root.join(MANIFEST_NAME);
    let manifest: MultiAccountEvidenceManifest = serde_json::from_slice(
        &std::fs::read(&manifest_path)
            .map_err(|error| format!("读取批次 Evidence manifest 失败: {error}"))?,
    )
    .map_err(|error| format!("批次 Evidence manifest JSON 无效: {error}"))?;
    if manifest.schema_version != SCHEMA_VERSION
        || manifest.kind != "qianxing-multi-account-run-evidence"
        || manifest.result.path != RESULT_NAME
        || manifest.spec.path != SPEC_NAME
    {
        return Err("多账户 Evidence manifest 版本、类型或固定文件路径无效".into());
    }
    let result_path = checked_package_file(&root, &manifest.result.path)?;
    let spec_path = checked_package_file(&root, &manifest.spec.path)?;
    verify_digest(&result_path, &manifest.result.sha256, "多账户结果")?;
    verify_digest(&spec_path, &manifest.spec.sha256, "多账户规格")?;
    let result: MultiAccountBacktestResult = serde_json::from_slice(
        &std::fs::read(&result_path).map_err(|error| format!("读取便携多账户结果失败: {error}"))?,
    )
    .map_err(|error| format!("便携多账户结果 JSON 无效: {error}"))?;
    let spec_text = std::fs::read_to_string(&spec_path)
        .map_err(|error| format!("读取便携多账户规格失败: {error}"))?;
    let spec =
        MultiAccountBacktestSpec::from_json(&spec_text).map_err(|error| error.to_string())?;
    crate::strategy_multi_account_diagnostics::validate_batch_identity(&result, &spec)?;
    if manifest.batch_id != result.batch_id
        || manifest.settlement_currency != result.settlement_currency
        || manifest.spec_fingerprint != result.spec_fingerprint
    {
        return Err("Evidence manifest 与批次结果/规格身份不一致".into());
    }
    validate_successful_outcomes(&result, &spec)?;

    let expected = result
        .runs
        .iter()
        .filter_map(|run| run.outcome.as_ref().map(|outcome| (run, outcome)))
        .map(|(run, outcome)| (run.account_id.as_str(), (run, outcome)))
        .collect::<BTreeMap<_, _>>();
    let mut seen = BTreeSet::new();
    let members = spec
        .runs
        .iter()
        .map(|member| (member.account_id.as_deref().unwrap_or_default(), member))
        .collect::<BTreeMap<_, _>>();
    for account in &manifest.accounts {
        if !seen.insert(account.account_id.as_str()) {
            return Err(format!(
                "Evidence manifest 重复账户: {}",
                account.account_id
            ));
        }
        let (batch_run, outcome) = expected.get(account.account_id.as_str()).ok_or_else(|| {
            format!(
                "Evidence manifest 包含未知/失败账户: {}",
                account.account_id
            )
        })?;
        let member = members
            .get(account.account_id.as_str())
            .ok_or("多账户规格缺少 Evidence manifest 账户")?;
        if account.run_id != outcome.run_id
            || account.result_hash != outcome.result_hash
            || account.instrument != outcome.instrument
            || account.account_id != batch_run.account_id
            || account.evidence_dir
                != format!("accounts/{}", account_directory_id(&account.account_id))
        {
            return Err(format!(
                "账户 {} 的 Evidence manifest 身份不一致",
                account.account_id
            ));
        }
        let portable_dir = checked_package_file(&root, &account.evidence_dir)?;
        if !portable_dir.is_dir() {
            return Err(format!("账户 {} 的便携证据目录缺失", account.account_id));
        }
        let portable_bundle = portable_dir.join("evidence.json");
        verify_digest(
            &portable_bundle,
            &account.evidence_sha256,
            "账户 EvidenceBundle",
        )?;
        crate::run_evidence_verify::verify_run_bundle_quiet(&portable_bundle, false).map_err(
            |error| {
                format!(
                    "账户 {} 的便携 EvidenceBundle 验真失败: {error}",
                    account.account_id
                )
            },
        )?;
        let bundle = read_run_bundle(&portable_bundle)?;
        let EvidenceGeneration::Run(evidence) = &bundle.generation else {
            return Err(format!(
                "账户 {} 的 EvidenceBundle 类型无效",
                account.account_id
            ));
        };
        verify_member_evidence(batch_run, outcome, member, evidence)?;
        verify_portable_run_paths(&account.evidence_dir, member, outcome, evidence)?;
        if account.strategy_id != evidence.identity.strategy_id
            || account.data_fingerprint != evidence.dataset.content_fingerprint
        {
            return Err(format!(
                "账户 {} 的策略或数据指纹与 manifest 不一致",
                account.account_id
            ));
        }
        verify_bundle_summary_account(&portable_bundle, &bundle, &account.account_id)?;
    }
    if seen.len() != expected.len() {
        return Err("Evidence manifest 未覆盖全部成功账户".into());
    }
    crate::print_machine_json(&serde_json::json!({
        "ok": true,
        "operation": "multi-account-run-evidence-verified",
        "batch_id": result.batch_id,
        "verified_account_count": seen.len(),
        "failed_account_count": result.summary.accounts_failed,
        "input_identity_recomputed": true,
        "result_recomputed": false,
        "signed": false
    }))
}

fn validate_successful_outcomes(
    result: &MultiAccountBacktestResult,
    spec: &MultiAccountBacktestSpec,
) -> Result<(), String> {
    let members = spec
        .runs
        .iter()
        .map(|member| (member.account_id.as_deref().unwrap_or_default(), member))
        .collect::<BTreeMap<_, _>>();
    for run in result.runs.iter().filter(|run| run.outcome.is_some()) {
        let outcome = run.outcome.as_ref().expect("filtered successful run");
        let member = members
            .get(run.account_id.as_str())
            .ok_or_else(|| format!("成功账户 {} 不在批次规格中", run.account_id))?;
        if outcome.account_id.as_deref() != Some(run.account_id.as_str())
            || outcome.run_id != member.run_id
            || outcome.instrument != member.instrument
        {
            return Err(format!(
                "账户 {} 的成功结果与规格身份不一致",
                run.account_id
            ));
        }
    }
    Ok(())
}

fn verify_member_evidence(
    batch_run: &qx_app::MultiAccountRunResult,
    outcome: &qx_app::BacktestOutcome,
    member: &qx_app::BacktestSpec,
    evidence: &qx_spec::RunEvidencePayload,
) -> Result<(), String> {
    if batch_run.account_id != outcome.account_id.as_deref().unwrap_or_default()
        || evidence.run.run_id != outcome.run_id
        || evidence.run.result_hash != outcome.result_hash
        || evidence.run.data_fingerprint != outcome.data_fingerprint
        || evidence.identity.instrument != outcome.instrument
        || evidence.identity.instrument != member.instrument
        || evidence.dataset.kind != "barframe"
    {
        return Err(format!(
            "账户 {} 的 EvidenceBundle 与批次结果不一致",
            batch_run.account_id
        ));
    }
    Ok(())
}

fn verify_bundle_summary_account(
    bundle_path: &Path,
    bundle: &EvidenceBundle,
    account_id: &str,
) -> Result<(), String> {
    let EvidenceGeneration::Run(evidence) = &bundle.generation else {
        return Err("账户 EvidenceBundle 类型无效".into());
    };
    let summary = evidence
        .artifacts
        .iter()
        .find(|artifact| artifact.name == "summary")
        .ok_or("账户 EvidenceBundle 缺少 summary")?;
    let root = bundle_path.parent().unwrap_or(Path::new("."));
    let path = crate::run_evidence_recompute::resolve_reference(root, &summary.path)?;
    let path = path
        .canonicalize()
        .map_err(|error| format!("解析账户 summary 路径失败: {error}"))?;
    let root = root
        .canonicalize()
        .map_err(|error| format!("解析账户 EvidenceBundle 目录失败: {error}"))?;
    if !path.starts_with(&root) {
        return Err(format!(
            "账户 {account_id} 的 summary 路径逃逸出 EvidenceBundle"
        ));
    }
    let payload =
        std::fs::read(&path).map_err(|error| format!("读取账户 summary 失败: {error}"))?;
    let value: serde_json::Value = serde_json::from_slice(&payload)
        .map_err(|error| format!("账户 summary JSON 无效: {error}"))?;
    if value.get("account_id").and_then(serde_json::Value::as_str) != Some(account_id) {
        return Err(format!("账户 {account_id} 的 summary 所属账户不一致"));
    }
    Ok(())
}

fn verify_portable_run_paths(
    evidence_dir: &str,
    member: &qx_app::BacktestSpec,
    outcome: &qx_app::BacktestOutcome,
    evidence: &qx_spec::RunEvidencePayload,
) -> Result<(), String> {
    let dataset_path = validate_portable_relative(&evidence.dataset.path)?;
    let expected_dataset = format!("{evidence_dir}/{dataset_path}");
    if member.bars_path != expected_dataset {
        return Err(format!(
            "账户 {} 的便携规格未指向本包中的数据输入",
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

fn portable_artifact_path(
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

fn validate_portable_relative(value: &str) -> Result<String, String> {
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

fn locate_account_bundle(summary_path: &Path) -> Result<PathBuf, String> {
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

fn read_run_bundle(path: &Path) -> Result<EvidenceBundle, String> {
    let payload = std::fs::read_to_string(path)
        .map_err(|error| format!("读取账户 EvidenceBundle 失败 {}: {error}", path.display()))?;
    EvidenceBundle::from_json(&payload)
}

fn account_directory_id(account_id: &str) -> String {
    qx_strategy::sha256_hex(account_id.as_bytes())
}

fn reserve_staging_directory(parent: &Path, target: &Path) -> Result<StagingDirectory, String> {
    let name = target
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("multi-account-evidence");
    for _ in 0..32 {
        let sequence = STAGING_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let candidate = parent.join(format!(".{name}.staging-{}-{sequence}", std::process::id()));
        match std::fs::create_dir(&candidate) {
            Ok(()) => return Ok(StagingDirectory(candidate)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(format!(
                    "创建多账户证据临时目录失败 {}: {error}",
                    candidate.display()
                ))
            }
        }
    }
    Err("无法保留唯一的多账户证据临时目录".into())
}

fn write_new(path: &Path, bytes: &[u8], label: &str) -> Result<(), String> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| format!("创建{label}失败 {}: {error}", path.display()))?;
    if let Err(error) = file.write_all(bytes) {
        drop(file);
        let _ = std::fs::remove_file(path);
        return Err(format!("写入{label}失败 {}: {error}", path.display()));
    }
    Ok(())
}

fn checked_package_file(root: &Path, relative: &str) -> Result<PathBuf, String> {
    let resolved = crate::run_evidence_recompute::resolve_reference(root, relative)?;
    let canonical = resolved
        .canonicalize()
        .map_err(|error| format!("解析证据包文件失败 {}: {error}", resolved.display()))?;
    if !canonical.starts_with(root) {
        return Err(format!("证据包路径逃逸出根目录: {relative}"));
    }
    Ok(canonical)
}

fn verify_digest(path: &Path, expected: &str, label: &str) -> Result<(), String> {
    let actual = qx_strategy::file_digest::sha256_file_hex(path)?;
    if actual != expected {
        return Err(format!(
            "{label} SHA-256 不匹配: 声明={expected} 实际={actual}"
        ));
    }
    Ok(())
}
