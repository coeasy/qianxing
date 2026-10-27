//! Portable evidence packages for multi-exchange joint Bar runs.

use qx_app::{MultiAccountJointBacktestOutcome, MultiAccountJointBacktestSpec};
use qx_spec::{EvidenceBundle, EvidenceGeneration, FoundationDocument};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const MANIFEST_NAME: &str = "multi-account-joint-evidence.json";
const RESULT_NAME: &str = "result.json";
const SPEC_NAME: &str = "spec.json";
const SCHEMA_VERSION: u32 = 1;
static STAGING_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileDigest {
    path: String,
    sha256: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct JointAccountEvidence {
    exchange_id: String,
    account_id: String,
    run_id: String,
    result_hash: String,
    markets_total: usize,
    evidence_dir: String,
    evidence_sha256: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct JointEvidenceManifest {
    schema_version: u32,
    kind: String,
    run_id: String,
    settlement_currency: String,
    spec_fingerprint: String,
    result_hash: String,
    result: FileDigest,
    spec: FileDigest,
    parent_artifacts: BTreeMap<String, FileDigest>,
    accounts: Vec<JointAccountEvidence>,
}

struct StagingDirectory(PathBuf);

impl Drop for StagingDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub(crate) fn pack_multi_account_joint_run(
    result_path: &Path,
    spec_path: &Path,
    output_dir: &Path,
) -> Result<(), String> {
    if output_dir.exists() {
        return Err(format!("导出目录已存在，不覆盖: {}", output_dir.display()));
    }
    let result_bytes = std::fs::read(result_path)
        .map_err(|error| format!("读取联合账户结果失败 {}: {error}", result_path.display()))?;
    let result: MultiAccountJointBacktestOutcome = serde_json::from_slice(&result_bytes)
        .map_err(|error| format!("联合账户结果 JSON 无效: {error}"))?;
    let spec_bytes = std::fs::read(spec_path)
        .map_err(|error| format!("读取联合账户规格失败 {}: {error}", spec_path.display()))?;
    let spec_text = std::str::from_utf8(&spec_bytes)
        .map_err(|error| format!("联合账户规格不是 UTF-8 {}: {error}", spec_path.display()))?;
    let spec =
        MultiAccountJointBacktestSpec::from_json(spec_text).map_err(|error| error.to_string())?;
    validate_joint_spec_identity(&result, &spec)?;
    let source_report =
        qx_app::read_multi_account_joint_report_v1(&result, &joint_context(&result.run_id))
            .map_err(|error| format!("来源联合账户报告复核失败: {}", error.to_json()))?;
    if source_report.run_id != result.run_id
        || source_report.summary.accounts_total != result.accounts_total
        || source_report.summary.markets_total != result.markets_total
    {
        return Err("来源联合账户报告与结果摘要不一致".into());
    }

    let output_parent = output_dir
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(output_parent)
        .map_err(|error| format!("创建证据包父目录失败 {}: {error}", output_parent.display()))?;
    let staging = reserve_staging_directory(output_parent, output_dir)?;
    let mut portable_spec = spec.clone();
    portable_spec.output_dir = "recompute-output".into();
    let mut accounts = Vec::with_capacity(spec.accounts.len());
    let mut packaged_bundles = BTreeMap::<(String, String), PathBuf>::new();

    for account in &spec.accounts {
        let outcome = result
            .accounts
            .iter()
            .find(|member| {
                member.exchange_id == account.exchange_id && member.account_id == account.account_id
            })
            .ok_or_else(|| {
                format!(
                    "联合结果缺少账户 {}:{}",
                    account.exchange_id, account.account_id
                )
            })?;
        let child_artifact = Path::new(&outcome.outcome.artifact_path);
        let source_evidence =
            crate::run_evidence_multi_market::write_multi_market_run_evidence(child_artifact, None)
                .map_err(|error| {
                    format!(
                        "生成账户 {}:{} 的多市场 EvidenceBundle 失败: {error}",
                        account.exchange_id, account.account_id
                    )
                })?;
        crate::run_evidence_verify::verify_run_bundle_quiet(&source_evidence, false).map_err(
            |error| {
                format!(
                    "来源账户 {}:{} EvidenceBundle 未通过复核: {error}",
                    account.exchange_id, account.account_id
                )
            },
        )?;
        let source_bundle = read_run_bundle(&source_evidence)?;
        let EvidenceGeneration::Run(source_run) = &source_bundle.generation else {
            return Err("来源多市场 EvidenceBundle 不是 run 类型".into());
        };
        verify_child_bundle_identity(outcome, source_run)?;

        let evidence_dir = format!(
            "accounts/{}",
            account_directory_id(&account.exchange_id, &account.account_id)
        );
        let package_account_dir = staging.0.join(&evidence_dir);
        if let Some(parent) = package_account_dir.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("创建联合账户证据目录失败: {error}"))?;
        }
        crate::run_evidence_export::export_run_bundle_quiet(&source_evidence, &package_account_dir)
            .map_err(|error| {
                format!(
                    "导出账户 {}:{} 的便携 EvidenceBundle 失败: {error}",
                    account.exchange_id, account.account_id
                )
            })?;
        let portable_evidence_path = package_account_dir.join("evidence.json");
        crate::run_evidence_verify::verify_run_bundle_quiet(&portable_evidence_path, false)
            .map_err(|error| {
                format!(
                    "便携账户 {}:{} EvidenceBundle 验真失败: {error}",
                    account.exchange_id, account.account_id
                )
            })?;
        let portable_bundle = read_run_bundle(&portable_evidence_path)?;
        let EvidenceGeneration::Run(portable_run) = &portable_bundle.generation else {
            return Err("便携账户 EvidenceBundle 类型改变".into());
        };
        verify_child_bundle_identity(outcome, portable_run)?;
        bind_portable_market_paths(&mut portable_spec, account, &evidence_dir, portable_run)?;

        let evidence_sha256 = qx_strategy::file_digest::sha256_file_hex(&portable_evidence_path)?;
        packaged_bundles.insert(
            (account.exchange_id.clone(), account.account_id.clone()),
            portable_evidence_path,
        );
        accounts.push(JointAccountEvidence {
            exchange_id: account.exchange_id.clone(),
            account_id: account.account_id.clone(),
            run_id: outcome.outcome.run_id.clone(),
            result_hash: outcome.outcome.shared_result_hash.clone(),
            markets_total: outcome.outcome.markets_total,
            evidence_dir,
            evidence_sha256,
        });
    }
    accounts.sort_by(|left, right| {
        left.exchange_id
            .cmp(&right.exchange_id)
            .then_with(|| left.account_id.cmp(&right.account_id))
    });

    let mut parent_artifacts = BTreeMap::new();
    for (name, source, relative) in parent_artifact_sources(&result)? {
        let bytes = std::fs::read(&source).map_err(|error| {
            format!("读取联合父级 {name} 产物失败 {}: {error}", source.display())
        })?;
        let destination = staging.0.join(&relative);
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("创建联合父级工件目录失败: {error}"))?;
        }
        write_new(&destination, &bytes, &format!("联合父级 {name}"))?;
        parent_artifacts.insert(
            name,
            FileDigest {
                path: relative,
                sha256: qx_strategy::sha256_hex(&bytes),
            },
        );
    }

    let portable_spec_bytes = serde_json::to_vec_pretty(&portable_spec)
        .map_err(|error| format!("序列化便携联合账户规格失败: {error}"))?;
    write_new(
        &staging.0.join(SPEC_NAME),
        &portable_spec_bytes,
        "联合账户规格",
    )?;
    write_new(&staging.0.join(RESULT_NAME), &result_bytes, "联合账户结果")?;
    let manifest = JointEvidenceManifest {
        schema_version: SCHEMA_VERSION,
        kind: "qianxing-multi-account-joint-run-evidence".into(),
        run_id: result.run_id.clone(),
        settlement_currency: result.settlement_currency.clone(),
        spec_fingerprint: result.spec_fingerprint.clone(),
        result_hash: result
            .result_hash
            .clone()
            .ok_or("联合结果缺少 result_hash")?,
        result: FileDigest {
            path: RESULT_NAME.into(),
            sha256: qx_strategy::sha256_hex(&result_bytes),
        },
        spec: FileDigest {
            path: SPEC_NAME.into(),
            sha256: qx_strategy::sha256_hex(&portable_spec_bytes),
        },
        parent_artifacts,
        accounts,
    };
    let manifest_bytes = serde_json::to_vec_pretty(&manifest)
        .map_err(|error| format!("序列化联合账户 Evidence manifest 失败: {error}"))?;
    write_new(
        &staging.0.join(MANIFEST_NAME),
        &manifest_bytes,
        "联合账户 Evidence manifest",
    )?;
    std::fs::rename(&staging.0, output_dir).map_err(|error| {
        format!(
            "提交联合账户便携证据包失败 {} -> {}: {error}",
            staging.0.display(),
            output_dir.display()
        )
    })?;
    crate::print_machine_json(&serde_json::json!({
        "ok": true,
        "operation": "multi-account-joint-run-evidence-packed",
        "run_id": result.run_id,
        "directory": output_dir.display().to_string(),
        "manifest": output_dir.join(MANIFEST_NAME).display().to_string(),
        "accounts_verified": result.accounts_total,
        "markets_verified": result.markets_total,
        "portable": true,
        "signed": false
    }))
}

pub(crate) fn verify_multi_account_joint_run(
    bundle_dir: &Path,
    recompute: bool,
) -> Result<(), String> {
    let root = bundle_dir
        .canonicalize()
        .map_err(|error| format!("解析联合账户证据目录失败 {}: {error}", bundle_dir.display()))?;
    if !root.is_dir() {
        return Err("联合账户便携证据路径必须是目录".into());
    }
    let manifest_path = root.join(MANIFEST_NAME);
    let manifest: JointEvidenceManifest = serde_json::from_slice(
        &std::fs::read(&manifest_path)
            .map_err(|error| format!("读取联合账户 Evidence manifest 失败: {error}"))?,
    )
    .map_err(|error| format!("联合账户 Evidence manifest JSON 无效: {error}"))?;
    if manifest.schema_version != SCHEMA_VERSION
        || manifest.kind != "qianxing-multi-account-joint-run-evidence"
        || manifest.result.path != RESULT_NAME
        || manifest.spec.path != SPEC_NAME
        || manifest
            .parent_artifacts
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>()
            != BTreeSet::from([
                "joint_equity",
                "joint_fills",
                "joint_portfolio",
                "run_manifest",
                "run_record",
            ])
    {
        return Err("联合账户 Evidence manifest 版本、类型或固定文件清单无效".into());
    }
    let result_path = checked_package_file(&root, &manifest.result.path)?;
    let spec_path = checked_package_file(&root, &manifest.spec.path)?;
    verify_digest(&result_path, &manifest.result.sha256, "联合账户结果")?;
    verify_digest(&spec_path, &manifest.spec.sha256, "联合账户规格")?;
    let outcome: MultiAccountJointBacktestOutcome = serde_json::from_slice(
        &std::fs::read(&result_path).map_err(|error| format!("读取便携联合结果失败: {error}"))?,
    )
    .map_err(|error| format!("便携联合结果 JSON 无效: {error}"))?;
    let portable_spec_text = std::fs::read_to_string(&spec_path)
        .map_err(|error| format!("读取便携联合规格失败: {error}"))?;
    let portable_spec = MultiAccountJointBacktestSpec::from_json(&portable_spec_text)
        .map_err(|error| error.to_string())?;
    validate_joint_spec_identity(&outcome, &portable_spec)?;
    if manifest.run_id != outcome.run_id
        || manifest.settlement_currency != outcome.settlement_currency
        || manifest.spec_fingerprint != outcome.spec_fingerprint
        || manifest.result_hash != outcome.result_hash.as_deref().unwrap_or_default()
        || manifest.accounts.len() != outcome.accounts_total
        || portable_spec.accounts.len() != outcome.accounts_total
    {
        return Err("Evidence manifest 与联合结果/便携规格身份不一致".into());
    }
    for digest in manifest.parent_artifacts.values() {
        let path = checked_package_file(&root, &digest.path)?;
        verify_digest(&path, &digest.sha256, "联合父级产物")?;
    }

    let expected_accounts = outcome
        .accounts
        .iter()
        .map(|account| {
            (
                (account.exchange_id.as_str(), account.account_id.as_str()),
                account,
            )
        })
        .collect::<BTreeMap<_, _>>();
    let portable_accounts = portable_spec
        .accounts
        .iter()
        .map(|account| {
            (
                (account.exchange_id.as_str(), account.account_id.as_str()),
                account,
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut seen = BTreeSet::new();
    let mut portable_bundles = BTreeMap::new();
    for entry in &manifest.accounts {
        let key = (entry.exchange_id.as_str(), entry.account_id.as_str());
        if !seen.insert(key) {
            return Err(format!("Evidence manifest 重复账户 {}:{}", key.0, key.1));
        }
        let member = expected_accounts
            .get(&key)
            .ok_or_else(|| format!("Evidence manifest 有未知账户 {}:{}", key.0, key.1))?;
        let portable_member = portable_accounts
            .get(&key)
            .ok_or_else(|| format!("便携规格缺少账户 {}:{}", key.0, key.1))?;
        if entry.run_id != member.outcome.run_id
            || entry.result_hash != member.outcome.shared_result_hash
            || entry.markets_total != member.outcome.markets_total
            || entry.evidence_dir != format!("accounts/{}", account_directory_id(key.0, key.1))
        {
            return Err(format!("账户 {}:{} 的证据身份不匹配", key.0, key.1));
        }
        let account_dir = checked_package_dir(&root, &entry.evidence_dir)
            .map_err(|error| format!("账户 {}:{} 的证据目录无效: {error}", key.0, key.1))?;
        let bundle_path = account_dir.join("evidence.json");
        verify_digest(&bundle_path, &entry.evidence_sha256, "账户 EvidenceBundle")?;
        crate::run_evidence_verify::verify_run_bundle_quiet(&bundle_path, false).map_err(
            |error| {
                format!(
                    "账户 {}:{} portable EvidenceBundle 验真失败: {error}",
                    key.0, key.1
                )
            },
        )?;
        let bundle = read_run_bundle(&bundle_path)?;
        let EvidenceGeneration::Run(run) = &bundle.generation else {
            return Err("联合账户子项 EvidenceBundle 不是 run 类型".into());
        };
        verify_child_bundle_identity(member, run)?;
        verify_portable_market_paths(&root, &entry.evidence_dir, portable_member, run)?;
        portable_bundles.insert((key.0.to_string(), key.1.to_string()), bundle_path);
    }
    if seen.len() != expected_accounts.len() {
        return Err("Evidence manifest 未覆盖全部联合账户".into());
    }

    let reader = build_path_map_reader(&root, &manifest, &outcome, &portable_bundles)?;
    let report = qx_app::read_multi_account_joint_report_v1_with_reader_and_options(
        &outcome,
        &joint_context(&outcome.run_id),
        &reader,
        &qx_app::MultiAccountJointReportReadOptionsV1::default(),
    )
    .map_err(|error| format!("便携联合账户报告复核失败: {}", error.to_json()))?;
    if report.run_id != outcome.run_id
        || report.summary.accounts_total != outcome.accounts_total
        || report.summary.markets_total != outcome.markets_total
        || report.summary.initial_cash_raw != outcome.initial_cash_raw.to_string()
        || report.summary.final_equity_raw != outcome.final_equity_raw.to_string()
        || report.summary.return_bps != outcome.return_bps
        || report.summary.max_drawdown_bps != outcome.max_drawdown_bps
        || report.summary.successful_fills != outcome.successful_fills
    {
        return Err("便携联合账户报告汇总与结果 DTO 不一致".into());
    }
    if recompute {
        recompute_joint_run(&root, &manifest, &portable_spec, &outcome)?;
    }
    crate::print_machine_json(&serde_json::json!({
        "ok": true,
        "operation": "multi-account-joint-run-evidence-verified",
        "run_id": outcome.run_id,
        "verified_account_count": seen.len(),
        "verified_market_count": outcome.markets_total,
        "input_identity_recomputed": true,
        "result_recomputed": recompute,
        "signed": false
    }))
}

fn recompute_joint_run(
    package_root: &Path,
    manifest: &JointEvidenceManifest,
    portable_spec: &MultiAccountJointBacktestSpec,
    expected: &MultiAccountJointBacktestOutcome,
) -> Result<(), String> {
    let manifest_ref = manifest
        .parent_artifacts
        .get("run_manifest")
        .ok_or("联合 Evidence 缺少父级 RunManifest")?;
    let expected_manifest_path = checked_package_file(package_root, &manifest_ref.path)?;
    let expected_manifest: qx_core::RunManifest = serde_json::from_slice(
        &std::fs::read(&expected_manifest_path)
            .map_err(|error| format!("读取联合父级 RunManifest 失败: {error}"))?,
    )
    .map_err(|error| format!("联合父级 RunManifest JSON 无效: {error}"))?;
    if !expected_manifest
        .code_commit
        .eq_ignore_ascii_case(crate::build_identity::BUILD_REVISION)
    {
        return Err(format!(
            "当前构建 revision={} 与 Evidence 要求的 code_commit={} 不一致，拒绝联合回测复算",
            crate::build_identity::BUILD_REVISION,
            expected_manifest.code_commit
        ));
    }

    let (staging, work) = reserve_recompute_directory(package_root)?;
    let mut spec = portable_spec.clone();
    for account in &mut spec.accounts {
        for market in &mut account.markets {
            let component = market_component(
                portable_spec,
                account.exchange_id.as_str(),
                account.account_id.as_str(),
                &market.instrument,
                package_root,
            )?;
            market.bars_path = component.0;
            market.dataset_manifest_path = component.1;
        }
    }
    spec.output_dir = work.join("outputs").to_string_lossy().into_owned();
    let context =
        joint_context(&spec.run_id).with_code_commit(crate::build_identity::BUILD_REVISION);
    let actual = qx_app::run_multi_account_joint_backtest(&spec, &context)
        .map_err(|error| format!("重新运行联合账户回测失败: {}", error.to_json()))?;
    let actual_manifest_path = actual
        .run_manifest_path
        .as_deref()
        .ok_or("复算联合结果缺少 RunManifest")?;
    let actual_manifest: qx_core::RunManifest = serde_json::from_slice(
        &std::fs::read(actual_manifest_path)
            .map_err(|error| format!("读取复算 RunManifest 失败: {error}"))?,
    )
    .map_err(|error| format!("复算 RunManifest JSON 无效: {error}"))?;
    if actual_manifest != expected_manifest || !same_joint_result(&actual, expected) {
        return Err(format!(
            "联合账户 portable 复算不一致: expected={} actual={}",
            expected_manifest.digest(),
            actual_manifest.digest()
        ));
    }
    let actual_report = qx_app::read_multi_account_joint_report_v1(
        &actual,
        &joint_context(&actual.run_id).with_code_commit(crate::build_identity::BUILD_REVISION),
    )
    .map_err(|error| format!("复算联合账户报告自身复核失败: {}", error.to_json()))?;
    if actual_report.summary.initial_cash_raw != expected.initial_cash_raw.to_string()
        || actual_report.summary.final_equity_raw != expected.final_equity_raw.to_string()
        || actual_report.summary.return_bps != expected.return_bps
        || actual_report.summary.max_drawdown_bps != expected.max_drawdown_bps
    {
        return Err("联合账户复算报告汇总与目标结果不一致".into());
    }
    drop(staging);
    Ok(())
}

fn same_joint_result(
    actual: &MultiAccountJointBacktestOutcome,
    expected: &MultiAccountJointBacktestOutcome,
) -> bool {
    actual.run_id == expected.run_id
        && actual.settlement_currency == expected.settlement_currency
        && actual.spec_fingerprint == expected.spec_fingerprint
        && actual.accounts_total == expected.accounts_total
        && actual.markets_total == expected.markets_total
        && actual.initial_cash_raw == expected.initial_cash_raw
        && actual.final_equity_raw == expected.final_equity_raw
        && actual.return_bps == expected.return_bps
        && actual.max_drawdown_bps == expected.max_drawdown_bps
        && actual.successful_fills == expected.successful_fills
        && actual.result_hash == expected.result_hash
        && actual.accounts.len() == expected.accounts.len()
        && actual
            .accounts
            .iter()
            .zip(&expected.accounts)
            .all(|(actual, expected)| {
                actual.exchange_id == expected.exchange_id
                    && actual.account_id == expected.account_id
                    && actual.initial_cash_raw == expected.initial_cash_raw
                    && actual.outcome.run_id == expected.outcome.run_id
                    && actual.outcome.account_id == expected.outcome.account_id
                    && actual.outcome.spec_fingerprint == expected.outcome.spec_fingerprint
                    && actual.outcome.data_fingerprint == expected.outcome.data_fingerprint
                    && actual.outcome.shared_result_hash == expected.outcome.shared_result_hash
                    && actual.outcome.markets_total == expected.outcome.markets_total
                    && actual.outcome.strategies_total == expected.outcome.strategies_total
                    && actual.outcome.successful_fills == expected.outcome.successful_fills
                    && actual.outcome.equity_points == expected.outcome.equity_points
                    && actual.outcome.final_equity_raw == expected.outcome.final_equity_raw
                    && actual.outcome.return_bps == expected.outcome.return_bps
                    && actual.outcome.max_drawdown_bps == expected.outcome.max_drawdown_bps
            })
}

fn validate_joint_spec_identity(
    outcome: &MultiAccountJointBacktestOutcome,
    spec: &MultiAccountJointBacktestSpec,
) -> Result<(), String> {
    spec.validate().map_err(|error| error.to_string())?;
    let fingerprint = stable_joint_spec_fingerprint(spec)?;
    if outcome.run_id != spec.run_id
        || outcome.settlement_currency != spec.settlement_currency
        || outcome.accounts_total != spec.accounts.len()
        || outcome.spec_fingerprint != fingerprint
        || outcome.result_hash.is_none()
        || outcome.run_manifest_path.is_none()
        || outcome.run_record.is_none()
        || outcome.artifact_locations.is_none()
    {
        return Err("联合账户结果与规格/父级工件身份不一致或缺少 V2 工件指针".into());
    }
    Ok(())
}

fn stable_joint_spec_fingerprint(spec: &MultiAccountJointBacktestSpec) -> Result<String, String> {
    let mut canonical = spec.clone();
    canonical.accounts.sort_by(|left, right| {
        left.exchange_id
            .cmp(&right.exchange_id)
            .then_with(|| left.account_id.cmp(&right.account_id))
    });
    canonical.output_dir.clear();
    for account in &mut canonical.accounts {
        for market in &mut account.markets {
            market.bars_path.clear();
            market.dataset_manifest_path = None;
        }
    }
    let json = canonical
        .to_json()
        .map_err(|error| format!("规范化联合账户规格失败: {}", error.to_json()))?;
    let mut hasher = qx_core::Fnv1a::new();
    hasher.write_text(&json);
    Ok(format!("{:016x}", hasher.finish()))
}

fn verify_child_bundle_identity(
    account: &qx_app::JointAccountBacktestResult,
    evidence: &qx_spec::RunEvidencePayload,
) -> Result<(), String> {
    if evidence.run.run_id != account.outcome.run_id
        || evidence.run.result_hash != account.outcome.shared_result_hash
        || evidence.run.data_fingerprint != account.outcome.data_fingerprint
        || evidence.dataset.components.len() != account.outcome.markets_total
        || evidence.dataset.kind != "multi-market-barframe"
    {
        return Err(format!(
            "账户 {}:{} 的 EvidenceBundle 与子运行身份不匹配",
            account.exchange_id, account.account_id
        ));
    }
    Ok(())
}

fn bind_portable_market_paths(
    spec: &mut MultiAccountJointBacktestSpec,
    account: &qx_app::JointBacktestAccount,
    evidence_dir: &str,
    evidence: &qx_spec::RunEvidencePayload,
) -> Result<(), String> {
    let spec_account = spec
        .accounts
        .iter_mut()
        .find(|candidate| {
            candidate.exchange_id == account.exchange_id
                && candidate.account_id == account.account_id
        })
        .ok_or("便携规格缺少账户")?;
    for market in &mut spec_account.markets {
        let component = evidence
            .dataset
            .components
            .iter()
            .find(|component| component.role == format!("market:{}", market.instrument))
            .ok_or_else(|| format!("账户市场 {} 缺少 Evidence 输入组件", market.instrument))?;
        let relative = validate_portable_relative(&component.path)?;
        market.bars_path = format!("{evidence_dir}/{relative}");
        market.dataset_manifest_path = component
            .quality_manifest_path
            .as_deref()
            .map(|path| {
                validate_portable_relative(path)
                    .map(|relative| format!("{evidence_dir}/{relative}"))
            })
            .transpose()?;
    }
    Ok(())
}

fn verify_portable_market_paths(
    package_root: &Path,
    evidence_dir: &str,
    account: &qx_app::JointBacktestAccount,
    evidence: &qx_spec::RunEvidencePayload,
) -> Result<(), String> {
    if account.markets.len() != evidence.dataset.components.len() {
        return Err("联合账户规格市场数与 Evidence 组件数不一致".into());
    }
    for market in &account.markets {
        let component = evidence
            .dataset
            .components
            .iter()
            .find(|component| component.role == format!("market:{}", market.instrument))
            .ok_or_else(|| format!("Evidence 缺少市场 {}", market.instrument))?;
        let expected_bars = format!(
            "{evidence_dir}/{}",
            validate_portable_relative(&component.path)?
        );
        if market.bars_path != expected_bars {
            return Err(format!(
                "市场 {} 的便携 BarFrame 路径不一致",
                market.instrument
            ));
        }
        let expected_quality = component
            .quality_manifest_path
            .as_deref()
            .map(|path| {
                validate_portable_relative(path)
                    .map(|relative| format!("{evidence_dir}/{relative}"))
            })
            .transpose()?;
        if market.dataset_manifest_path != expected_quality {
            return Err(format!(
                "市场 {} 的便携质量清单路径不一致",
                market.instrument
            ));
        }
        checked_package_file(package_root, &market.bars_path)?;
        if let Some(path) = &market.dataset_manifest_path {
            checked_package_file(package_root, path)?;
        }
    }
    Ok(())
}

fn market_component(
    spec: &MultiAccountJointBacktestSpec,
    exchange_id: &str,
    account_id: &str,
    instrument: &str,
    package_root: &Path,
) -> Result<(String, Option<String>), String> {
    let account = spec
        .accounts
        .iter()
        .find(|account| account.exchange_id == exchange_id && account.account_id == account_id)
        .ok_or("联合便携规格缺少账户")?;
    let market = account
        .markets
        .iter()
        .find(|market| market.instrument == instrument)
        .ok_or("联合便携规格缺少市场")?;
    let bars = checked_package_file(package_root, &market.bars_path)?
        .canonicalize()
        .map_err(|error| format!("解析便携 BarFrame 失败: {error}"))?;
    let quality = market
        .dataset_manifest_path
        .as_deref()
        .map(|path| {
            checked_package_file(package_root, path)?
                .canonicalize()
                .map(|path| path.to_string_lossy().into_owned())
                .map_err(|error| format!("解析便携质量清单失败: {error}"))
        })
        .transpose()?;
    Ok((bars.to_string_lossy().into_owned(), quality))
}

fn parent_artifact_sources(
    outcome: &MultiAccountJointBacktestOutcome,
) -> Result<Vec<(String, PathBuf, String)>, String> {
    let locations = outcome
        .artifact_locations
        .as_ref()
        .ok_or("联合账户结果缺少 artifact_locations")?;
    let expected = [
        (
            "joint_portfolio",
            &outcome.artifact_path,
            "parent/portfolio.json",
        ),
        ("joint_equity", &outcome.equity_path, "parent/equity.json"),
        ("joint_fills", &outcome.fills_path, "parent/fills.json"),
        (
            "run_manifest",
            outcome
                .run_manifest_path
                .as_ref()
                .ok_or("缺少父级 RunManifest")?,
            "parent/run-manifest.json",
        ),
    ];
    let mut files = Vec::with_capacity(5);
    for (name, source, relative) in expected {
        if locations.get(name).map(String::as_str) != Some(source.as_str()) {
            return Err(format!(
                "联合账户 artifact_locations.{name} 与 outcome 不一致"
            ));
        }
        files.push((
            name.to_string(),
            PathBuf::from(source),
            relative.to_string(),
        ));
    }
    files.push((
        "run_record".into(),
        PathBuf::from(
            outcome
                .run_record
                .as_deref()
                .ok_or("缺少联合父级 RunRecord")?,
        ),
        "parent/run-record.json".into(),
    ));
    Ok(files)
}

fn build_path_map_reader(
    root: &Path,
    manifest: &JointEvidenceManifest,
    outcome: &MultiAccountJointBacktestOutcome,
    bundle_paths: &BTreeMap<(String, String), PathBuf>,
) -> Result<PortablePathMapReader, String> {
    let mut reader = PortablePathMapReader::new(root.to_path_buf());
    for (name, source, _) in parent_artifact_sources(outcome)? {
        let target = manifest
            .parent_artifacts
            .get(&name)
            .ok_or_else(|| format!("manifest 缺少父级产物 {name}"))?;
        reader.map_reference(&source, checked_package_file(root, &target.path)?)?;
    }
    for account in &outcome.accounts {
        let key = (account.exchange_id.clone(), account.account_id.clone());
        let bundle_path = bundle_paths
            .get(&key)
            .ok_or("账户便携 EvidenceBundle 缺失")?;
        let bundle = read_run_bundle(bundle_path)?;
        let EvidenceGeneration::Run(evidence) = &bundle.generation else {
            return Err("子账户便携 EvidenceBundle 类型无效".into());
        };
        let evidence_dir = format!("accounts/{}", account_directory_id(&key.0, &key.1));
        let summary_ref = evidence
            .artifacts
            .iter()
            .find(|artifact| artifact.name == "summary")
            .ok_or("子账户 EvidenceBundle 缺少 summary")?;
        let summary_path = checked_package_file(
            root,
            &format!(
                "{evidence_dir}/{}",
                validate_portable_relative(&summary_ref.path)?
            ),
        )?;
        reader.map_reference(&account.outcome.artifact_path, summary_path.clone())?;
        let summary: serde_json::Value = serde_json::from_slice(
            &std::fs::read(&summary_path)
                .map_err(|error| format!("读取子账户 summary 失败: {error}"))?,
        )
        .map_err(|error| format!("子账户 summary JSON 无效: {error}"))?;
        for (name, pointer) in [
            ("run_manifest", "/run_manifest_path"),
            ("equity", "/equity_path"),
            ("equity_exact", "/equity_exact_path"),
            ("fills", "/fills_path"),
            ("event_trace", "/event_trace_path"),
            (
                "instrument_equity_contributions",
                "/instrument_equity_contributions_path",
            ),
        ] {
            let Some(source) = summary.pointer(pointer).and_then(serde_json::Value::as_str) else {
                continue;
            };
            let Some(portable) = evidence
                .artifacts
                .iter()
                .find(|artifact| artifact.name == name)
            else {
                return Err(format!("子账户 Evidence 缺少 {name} 工件"));
            };
            let target = checked_package_file(
                root,
                &format!(
                    "{evidence_dir}/{}",
                    validate_portable_relative(&portable.path)?
                ),
            )?;
            reader.map_reference(source, target)?;
        }
        for component in &evidence.dataset.components {
            let instrument = component
                .role
                .strip_prefix("market:")
                .ok_or("子账户 Evidence market role 无效")?;
            let source = summary
                .get("market_data")
                .and_then(serde_json::Value::as_array)
                .and_then(|items| {
                    items.iter().find(|item| {
                        item.get("instrument").and_then(serde_json::Value::as_str)
                            == Some(instrument)
                    })
                })
                .and_then(|item| item.get("path"))
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| format!("子账户 summary 缺少 {instrument} 输入路径"))?;
            let target = checked_package_file(
                root,
                &format!(
                    "{evidence_dir}/{}",
                    validate_portable_relative(&component.path)?
                ),
            )?;
            reader.map_reference(source, target)?;
            if let Some(quality_source) = summary
                .pointer(&format!("/dataset_quality/{instrument}/manifest_path"))
                .and_then(serde_json::Value::as_str)
            {
                let quality = component
                    .quality_manifest_path
                    .as_deref()
                    .ok_or("子账户 summary 有质量清单，但 portable 组件没有路径")?;
                let target = checked_package_file(
                    root,
                    &format!("{evidence_dir}/{}", validate_portable_relative(quality)?),
                )?;
                reader.map_reference(quality_source, target)?;
            }
        }
    }
    Ok(reader)
}

struct PortablePathMapReader {
    root: PathBuf,
    paths: BTreeMap<String, PathBuf>,
}

impl PortablePathMapReader {
    fn new(root: PathBuf) -> Self {
        Self {
            root,
            paths: BTreeMap::new(),
        }
    }

    fn map_reference(&mut self, source: impl AsRef<Path>, target: PathBuf) -> Result<(), String> {
        let key = path_key(&source.as_ref().to_string_lossy());
        if let Some(existing) = self.paths.get(&key) {
            if existing != &target {
                return Err("一个来源工件路径映射到多个便携目标".into());
            }
        } else {
            self.paths.insert(key, target);
        }
        Ok(())
    }

    fn resolve(&self, reference: &str) -> Result<PathBuf, qx_app::AppError> {
        if let Some(path) = self.paths.get(&path_key(reference)) {
            return Ok(path.clone());
        }
        if Path::new(reference).is_absolute() {
            return Err(qx_app::AppError::new(
                qx_app::AppErrorCategory::StorageFailure,
                format!("便携报告引用了未登记的来源绝对路径: {reference}"),
            ));
        }
        checked_package_file(&self.root, reference)
            .map_err(|error| qx_app::AppError::new(qx_app::AppErrorCategory::StorageFailure, error))
    }
}

impl qx_app::artifact_store::ArtifactReader for PortablePathMapReader {
    fn open(
        &self,
        reference: &str,
    ) -> Result<qx_app::artifact_store::ArtifactStream, qx_app::AppError> {
        let path = self.resolve(reference)?;
        qx_app::artifact_store::ArtifactReader::open(
            &qx_app::artifact_store::FileArtifactReader,
            &path.to_string_lossy(),
        )
    }

    fn equivalent_references(&self, left: &str, right: &str) -> Result<bool, qx_app::AppError> {
        if path_key(left) == path_key(right) {
            return Ok(true);
        }
        let left = self
            .resolve(left)?
            .canonicalize()
            .map_err(|error| qx_app::AppError::from_io("解析左侧 portable 工件引用", &error))?;
        let right = self
            .resolve(right)?
            .canonicalize()
            .map_err(|error| qx_app::AppError::from_io("解析右侧 portable 工件引用", &error))?;
        Ok(left == right)
    }
}

fn path_key(value: &str) -> String {
    let normalized = value.replace('/', "\\");
    if cfg!(windows) {
        normalized.to_ascii_lowercase()
    } else {
        normalized
    }
}

fn joint_context(run_id: &str) -> qx_app::RunContext {
    qx_app::RunContext::new(qx_app::CallerCapability::Research, run_id.to_string())
}

fn account_directory_id(exchange_id: &str, account_id: &str) -> String {
    let value = format!("{exchange_id}\0{account_id}");
    qx_strategy::sha256_hex(value.as_bytes())
}

fn read_run_bundle(path: &Path) -> Result<EvidenceBundle, String> {
    let payload = std::fs::read_to_string(path)
        .map_err(|error| format!("读取账户 EvidenceBundle 失败 {}: {error}", path.display()))?;
    EvidenceBundle::from_json(&payload)
}

fn validate_portable_relative(value: &str) -> Result<String, String> {
    let path = Path::new(value);
    if value.trim().is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
        || value.contains('\\')
    {
        return Err(format!(
            "portable 证据引用必须是安全的包内相对路径: {value}"
        ));
    }
    Ok(value.to_string())
}

fn checked_package_file(root: &Path, relative: &str) -> Result<PathBuf, String> {
    let relative = validate_portable_relative(relative)?;
    let path = root
        .join(relative)
        .canonicalize()
        .map_err(|error| format!("解析包内文件失败: {error}"))?;
    if !path.starts_with(root) || !path.is_file() {
        return Err("便携证据引用越界或不是普通文件".into());
    }
    Ok(path)
}

fn checked_package_dir(root: &Path, relative: &str) -> Result<PathBuf, String> {
    let relative = validate_portable_relative(relative)?;
    let path = root
        .join(relative)
        .canonicalize()
        .map_err(|error| format!("解析包内目录失败: {error}"))?;
    if !path.starts_with(root) || !path.is_dir() {
        return Err("便携证据引用越界或不是目录".into());
    }
    Ok(path)
}

fn verify_digest(path: &Path, expected: &str, label: &str) -> Result<(), String> {
    let actual = qx_strategy::file_digest::sha256_file_hex(path)?;
    if actual != expected {
        return Err(format!(
            "{label} SHA-256 不匹配: expected={expected} actual={actual}"
        ));
    }
    Ok(())
}

fn write_new(path: &Path, bytes: &[u8], label: &str) -> Result<(), String> {
    let mut file = File::options()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| format!("创建 {label} 失败 {}: {error}", path.display()))?;
    file.write_all(bytes)
        .map_err(|error| format!("写入 {label} 失败 {}: {error}", path.display()))?;
    file.sync_all()
        .map_err(|error| format!("刷新 {label} 失败 {}: {error}", path.display()))
}

fn reserve_staging_directory(parent: &Path, target: &Path) -> Result<StagingDirectory, String> {
    for _ in 0..32 {
        let sequence = STAGING_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let candidate = parent.join(format!(
            ".{}.joint-evidence-staging-{}-{sequence}",
            target
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("package"),
            std::process::id()
        ));
        match std::fs::create_dir(&candidate) {
            Ok(()) => return Ok(StagingDirectory(candidate)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(format!("创建联合证据暂存目录失败: {error}")),
        }
    }
    Err("无法预留唯一联合证据暂存目录".into())
}

fn reserve_recompute_directory(package_root: &Path) -> Result<(StagingDirectory, PathBuf), String> {
    let mut bases = Vec::new();
    if let Ok(temp) = std::env::temp_dir().canonicalize() {
        bases.push(temp);
    }
    if let Some(parent) = package_root.parent() {
        if let Ok(parent) = parent.canonicalize() {
            if !bases.contains(&parent) {
                bases.push(parent);
            }
        }
    }
    for base in bases {
        for _ in 0..8 {
            let sequence = STAGING_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let candidate = base.join(format!(
                "qx-joint-recompute-{}-{sequence}",
                std::process::id()
            ));
            match std::fs::create_dir(&candidate) {
                Ok(()) => {
                    let canonical = candidate
                        .canonicalize()
                        .map_err(|error| format!("解析联合复算目录失败: {error}"))?;
                    if !canonical.starts_with(&base) {
                        let _ = std::fs::remove_dir_all(&canonical);
                        return Err("联合复算目录越出暂存根目录".into());
                    }
                    return Ok((StagingDirectory(canonical.clone()), canonical));
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(_) => break,
            }
        }
    }
    Err("无法创建联合账户隔离复算目录".into())
}
