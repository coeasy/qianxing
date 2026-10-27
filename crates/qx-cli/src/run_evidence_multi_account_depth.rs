//! Portable, batch-level evidence for independent multi-account Tick/OrderBook runs.

use qx_app::{MultiAccountDepthBacktestResult, MultiAccountDepthBacktestSpec};
use qx_core::Fnv1a;
use qx_spec::{EvidenceBundle, EvidenceGeneration, FoundationDocument};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const SCHEMA_VERSION: u32 = 1;
const MANIFEST_NAME: &str = "multi-account-depth-evidence.json";
const RESULT_NAME: &str = "multi-account-depth-result.json";
const SPEC_NAME: &str = "multi-account-depth-spec.json";
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
struct AccountInputEvidence {
    account_id: String,
    path: String,
    sha256: Option<String>,
    missing: bool,
    #[serde(default)]
    quality_manifest_path: Option<String>,
    #[serde(default)]
    quality_manifest_sha256: Option<String>,
    #[serde(default)]
    quality_manifest_missing: Option<bool>,
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
    #[serde(default)]
    inputs: Vec<AccountInputEvidence>,
}

struct StagingDirectory(PathBuf);

impl Drop for StagingDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub(crate) fn pack_multi_account_depth_run(
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
    let result: MultiAccountDepthBacktestResult = serde_json::from_slice(&result_bytes)
        .map_err(|error| format!("多账户结果 JSON 无效: {error}"))?;
    let spec_text = std::str::from_utf8(&spec_bytes)
        .map_err(|error| format!("多账户规格不是 UTF-8: {error}"))?;
    let spec =
        MultiAccountDepthBacktestSpec::from_json(spec_text).map_err(|error| error.to_string())?;
    validate_depth_batch_identity(&result, &spec)?;
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
    let mut inputs = Vec::with_capacity(spec.runs.len());
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
        verify_member_evidence(run, outcome, evidence)?;
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
        verify_member_evidence(run, outcome, portable_evidence)?;
        let relative_dataset = validate_portable_relative(&portable_evidence.dataset.path)?;
        let depth_path = format!("{evidence_dir}/{relative_dataset}");
        let quality_manifest_path = portable_quality_manifest_path(
            evidence_dir.as_str(),
            portable_evidence.dataset.quality_manifest_path.as_deref(),
        )?;
        if member.quality_manifest_path.is_some() != quality_manifest_path.is_some() {
            return Err(format!(
                "账户 {} 的质量清单规格与 EvidenceBundle 不一致",
                run.account_id
            ));
        }
        let packaged_member = portable_spec
            .runs
            .iter_mut()
            .find(|candidate| candidate.account_id.as_deref() == Some(run.account_id.as_str()))
            .ok_or("便携规格中缺少成功账户")?;
        packaged_member.depth_path = depth_path.clone();
        packaged_member.quality_manifest_path = quality_manifest_path.clone();
        packaged_member.runtime_config_path = None;
        packaged_member.market_spec_path = None;
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
        let packaged_depth = staging.0.join(&depth_path);
        let depth_sha256 = qx_strategy::file_digest::sha256_file_hex(&packaged_depth)?;
        let quality_manifest_sha256 = quality_manifest_path
            .as_deref()
            .map(|relative| {
                let path = staging.0.join(relative);
                let digest = qx_strategy::file_digest::sha256_file_hex(&path)?;
                if portable_evidence.dataset.quality_manifest_sha256.as_deref()
                    != Some(digest.as_str())
                {
                    return Err(format!(
                        "账户 {} 的便携质量清单摘要与 EvidenceBundle 不一致",
                        run.account_id
                    ));
                }
                Ok::<String, String>(digest)
            })
            .transpose()?;
        inputs.push(AccountInputEvidence {
            account_id: run.account_id.clone(),
            path: depth_path,
            sha256: Some(depth_sha256),
            missing: false,
            quality_manifest_path: quality_manifest_path.clone(),
            quality_manifest_sha256,
            quality_manifest_missing: quality_manifest_path.as_ref().map(|_| false),
        });
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
    for run in result.runs.iter().filter(|run| run.outcome.is_none()) {
        let member = members
            .get(run.account_id.as_str())
            .ok_or_else(|| format!("失败账户 {} 不在批次规格中", run.account_id))?;
        let evidence_dir = format!("accounts/{}/inputs", account_directory_id(&run.account_id));
        let relative_depth = format!("{evidence_dir}/depth.json");
        let packaged_path = staging.0.join(&relative_depth);
        let parent = packaged_path.parent().ok_or("失败账户输入路径缺少父目录")?;
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("创建失败账户输入目录失败 {}: {error}", parent.display()))?;
        let source = Path::new(&member.depth_path);
        let (sha256, missing) = match std::fs::read(source) {
            Ok(bytes) => {
                write_new(&packaged_path, &bytes, "失败账户原始 DepthFrame")?;
                (Some(qx_strategy::sha256_hex(&bytes)), false)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (None, true),
            Err(error) => {
                return Err(format!(
                    "读取失败账户 {} 的原始 DepthFrame 失败 {}: {error}",
                    run.account_id,
                    source.display()
                ));
            }
        };
        let packaged_member = portable_spec
            .runs
            .iter_mut()
            .find(|candidate| candidate.account_id.as_deref() == Some(run.account_id.as_str()))
            .ok_or("便携规格中缺少失败账户")?;
        packaged_member.depth_path = relative_depth.clone();
        let (quality_manifest_path, quality_manifest_sha256, quality_manifest_missing) =
            match member.quality_manifest_path.as_deref() {
                Some(source) => {
                    let relative = format!("{evidence_dir}/quality-manifest.json");
                    let packaged = staging.0.join(&relative);
                    match std::fs::read(source) {
                        Ok(bytes) => {
                            write_new(&packaged, &bytes, "失败账户质量清单")?;
                            (
                                Some(relative),
                                Some(qx_strategy::sha256_hex(&bytes)),
                                Some(false),
                            )
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                            (Some(relative), None, Some(true))
                        }
                        Err(error) => {
                            return Err(format!(
                                "读取失败账户 {} 的质量清单失败 {}: {error}",
                                run.account_id, source
                            ));
                        }
                    }
                }
                None => (None, None, None),
            };
        packaged_member.quality_manifest_path = quality_manifest_path.clone();
        packaged_member.runtime_config_path = None;
        packaged_member.market_spec_path = None;
        packaged_member.output_dir = format!(
            "accounts/{}/replay-output",
            account_directory_id(&run.account_id)
        );
        inputs.push(AccountInputEvidence {
            account_id: run.account_id.clone(),
            path: relative_depth,
            sha256,
            missing,
            quality_manifest_path,
            quality_manifest_sha256,
            quality_manifest_missing,
        });
    }
    inputs.sort_by(|left, right| left.account_id.cmp(&right.account_id));
    let portable_spec_fingerprint = stable_depth_spec_fingerprint(&portable_spec)?;
    if portable_result.spec_fingerprint != portable_spec_fingerprint {
        // Older depth batch fingerprints included local quality/config/spec paths. Normalize
        // the portable copy while preserving all behavior and child-run content identities.
        portable_result.spec_fingerprint = portable_spec_fingerprint.clone();
        if let Some(parent_manifest) = portable_result.run_manifest.as_mut() {
            parent_manifest.config_hash = portable_spec_fingerprint;
        }
    }
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
        kind: "qianxing-multi-account-depth-run-evidence".into(),
        batch_id: result.batch_id.clone(),
        settlement_currency: result.settlement_currency.clone(),
        spec_fingerprint: portable_result.spec_fingerprint.clone(),
        result: FileDigest {
            path: RESULT_NAME.into(),
            sha256: qx_strategy::sha256_hex(&portable_result_bytes),
        },
        spec: FileDigest {
            path: SPEC_NAME.into(),
            sha256: qx_strategy::sha256_hex(&portable_spec_bytes),
        },
        accounts,
        inputs,
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
        "operation": "multi-account-depth-run-evidence-packed",
        "batch_id": result.batch_id,
        "directory": output_dir.display().to_string(),
        "manifest": output_dir.join(MANIFEST_NAME).display().to_string(),
        "accounts_verified": result.summary.accounts_succeeded,
        "accounts_failed": result.summary.accounts_failed,
        "portable": true,
        "signed": false
    }))
}

pub(crate) fn verify_multi_account_depth_run(
    bundle_dir: &Path,
    recompute: bool,
) -> Result<(), String> {
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
        || manifest.kind != "qianxing-multi-account-depth-run-evidence"
        || manifest.result.path != RESULT_NAME
        || manifest.spec.path != SPEC_NAME
    {
        return Err("多账户 Evidence manifest 版本、类型或固定文件路径无效".into());
    }
    let result_path = checked_package_file(&root, &manifest.result.path)?;
    let spec_path = checked_package_file(&root, &manifest.spec.path)?;
    verify_digest(&result_path, &manifest.result.sha256, "多账户结果")?;
    verify_digest(&spec_path, &manifest.spec.sha256, "多账户规格")?;
    let result: MultiAccountDepthBacktestResult = serde_json::from_slice(
        &std::fs::read(&result_path).map_err(|error| format!("读取便携多账户结果失败: {error}"))?,
    )
    .map_err(|error| format!("便携多账户结果 JSON 无效: {error}"))?;
    let spec_text = std::fs::read_to_string(&spec_path)
        .map_err(|error| format!("读取便携多账户规格失败: {error}"))?;
    let spec =
        MultiAccountDepthBacktestSpec::from_json(&spec_text).map_err(|error| error.to_string())?;
    validate_depth_batch_identity(&result, &spec)?;
    if manifest.batch_id != result.batch_id
        || manifest.settlement_currency != result.settlement_currency
        || manifest.spec_fingerprint != result.spec_fingerprint
    {
        return Err("Evidence manifest 与批次结果/规格身份不一致".into());
    }
    validate_successful_outcomes(&result, &spec)?;
    verify_packaged_inputs(&root, &manifest, &spec, &result)?;

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
        verify_member_evidence(batch_run, outcome, evidence)?;
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
    if recompute {
        recompute_multi_account_depth_run(&root, &manifest, &spec, &result)?;
    }
    crate::print_machine_json(&serde_json::json!({
        "ok": true,
        "operation": "multi-account-depth-run-evidence-verified",
        "batch_id": result.batch_id,
        "verified_account_count": seen.len(),
        "failed_account_count": result.summary.accounts_failed,
        "input_identity_recomputed": true,
        "result_recomputed": recompute,
        "signed": false
    }))
}

fn recompute_multi_account_depth_run(
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

fn verify_packaged_inputs(
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

fn checked_missing_package_file(root: &Path, relative: &str) -> Result<PathBuf, String> {
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

fn validate_depth_batch_identity(
    result: &MultiAccountDepthBacktestResult,
    spec: &MultiAccountDepthBacktestSpec,
) -> Result<(), String> {
    if result.batch_id != spec.batch_id
        || result.settlement_currency
            != spec
                .runs
                .first()
                .map(|run| run.settlement_currency.as_str())
                .unwrap_or_default()
    {
        return Err("多账户深度结果与规格的 batch_id/settlement_currency 不一致".into());
    }
    let expected = spec
        .runs
        .iter()
        .map(|run| run.account_id.as_deref().unwrap_or_default())
        .collect::<BTreeSet<_>>();
    let actual = result
        .runs
        .iter()
        .map(|run| run.account_id.as_str())
        .collect::<BTreeSet<_>>();
    let succeeded = result
        .runs
        .iter()
        .filter(|run| run.outcome.is_some())
        .count();
    if expected != actual
        || result.runs.len() != spec.runs.len()
        || result.summary.accounts_total != result.runs.len()
        || result.summary.accounts_succeeded != succeeded
        || result.summary.accounts_failed != result.runs.len() - succeeded
        || result.summary.complete != (succeeded == result.runs.len())
    {
        return Err("多账户深度结果的账户集合或汇总计数与规格/逐账户状态不一致".into());
    }
    let stable_fingerprint = stable_depth_spec_fingerprint(spec)?;
    let mut legacy = spec.clone();
    legacy
        .runs
        .sort_by(|left, right| left.account_id.cmp(&right.account_id));
    for run in &mut legacy.runs {
        run.depth_path.clear();
        run.output_dir.clear();
    }
    let legacy_json = legacy.to_json().map_err(|error| error.to_string())?;
    let mut legacy_hasher = Fnv1a::new();
    legacy_hasher.write_text(&legacy_json);
    let legacy_fingerprint = format!("{:016x}", legacy_hasher.finish());
    if result.spec_fingerprint != stable_fingerprint
        && result.spec_fingerprint != legacy_fingerprint
    {
        return Err("多账户深度结果 spec_fingerprint 与输入规格不一致".into());
    }
    Ok(())
}

fn stable_depth_spec_fingerprint(spec: &MultiAccountDepthBacktestSpec) -> Result<String, String> {
    let mut canonical = spec.clone();
    canonical
        .runs
        .sort_by(|left, right| left.account_id.cmp(&right.account_id));
    for run in &mut canonical.runs {
        run.depth_path.clear();
        run.output_dir.clear();
        run.quality_manifest_path = None;
        run.runtime_config_path = None;
        run.market_spec_path = None;
    }
    let canonical = canonical.to_json().map_err(|error| error.to_string())?;
    let mut hasher = Fnv1a::new();
    hasher.write_text(&canonical);
    Ok(format!("{:016x}", hasher.finish()))
}

fn validate_successful_outcomes(
    result: &MultiAccountDepthBacktestResult,
    spec: &MultiAccountDepthBacktestSpec,
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
            || outcome.tier != member.tier
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
    batch_run: &qx_app::MultiAccountDepthRunResult,
    outcome: &qx_app::DepthBacktestOutcome,
    evidence: &qx_spec::RunEvidencePayload,
) -> Result<(), String> {
    if batch_run.account_id != outcome.account_id.as_deref().unwrap_or_default()
        || evidence.run.run_id != outcome.run_id
        || evidence.run.result_hash != outcome.result_hash
        || evidence.run.data_fingerprint != outcome.data_fingerprint
        || evidence.identity.instrument != outcome.instrument
        || evidence.dataset.kind != "depth-frame"
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

fn portable_quality_manifest_path(
    evidence_dir: &str,
    path: Option<&str>,
) -> Result<Option<String>, String> {
    path.map(|path| {
        let relative = validate_portable_relative(path)?;
        Ok(format!("{evidence_dir}/{relative}"))
    })
    .transpose()
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
