//! Independent replay and result identity checks for joint-account evidence.

use super::io::STAGING_SEQUENCE;
use super::{
    checked_package_file, joint_context, market_component, JointEvidenceManifest, StagingDirectory,
};
use qx_app::{MultiAccountJointBacktestOutcome, MultiAccountJointBacktestSpec};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

pub(super) fn recompute_joint_run(
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
