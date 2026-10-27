//! Application-owned facts for verifying multi-account research report summaries.

use crate::artifact_store::{ArtifactReader, FileArtifactReader};
use crate::report_analysis::WeightedReturnAccumulatorV1;
use crate::report_read::{
    read_depth_run_report_v1_with_reader, read_run_report_v1_with_reader, ResearchRunReportV1,
};
use crate::{
    AppError, AppErrorCategory, CallerCapability, MultiAccountBacktestResult,
    MultiAccountDepthBacktestResult, RunContext,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;

pub const MULTI_ACCOUNT_RESEARCH_REPORT_SCHEMA_VERSION_V1: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MultiAccountResearchStatusV1 {
    Succeeded,
    Failed,
}

/// One independently verified child report in a multi-account batch.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MultiAccountResearchAccountReportV1 {
    pub account_id: String,
    pub status: MultiAccountResearchStatusV1,
    pub run_id: Option<String>,
    pub report: Option<ResearchRunReportV1>,
    pub error_json: Option<String>,
}

/// Shared bounded report DTO for independently funded Bar or Depth account batches.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MultiAccountResearchReportV1 {
    pub schema_version: u32,
    pub run_id: String,
    pub settlement_currency: String,
    pub spec_fingerprint: String,
    pub instrument: String,
    pub verification: crate::VerificationResult,
    pub summary: MultiAccountResearchSummaryV1,
    pub accounts: Vec<MultiAccountResearchAccountReportV1>,
}

/// Summary fields declared by Bar and Depth multi-account outcomes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MultiAccountResearchSummaryV1 {
    pub accounts_total: usize,
    pub accounts_succeeded: usize,
    pub accounts_failed: usize,
    pub successful_initial_cash_raw: i128,
    pub successful_fills: u64,
    pub weighted_return_bps: Option<i32>,
    pub complete: bool,
}

/// Facts from one child report after its artifacts and replay have been verified.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MultiAccountResearchMemberV1 {
    Failed {
        account_id: String,
    },
    Succeeded {
        account_id: String,
        initial_cash_raw: i128,
        return_bps: i32,
        fills: u64,
        data_fingerprint: String,
        result_hash: String,
    },
}

/// Canonical, application-verified facts used by multi-account report adapters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MultiAccountResearchFactsV1 {
    pub summary: MultiAccountResearchSummaryV1,
    pub data_fingerprint: String,
    pub result_hash: String,
}

/// Read the shared multi-account report with the default filesystem artifact reader.
pub fn read_multi_account_report_v1(
    outcome: &Value,
    context: &RunContext,
) -> Result<MultiAccountResearchReportV1, AppError> {
    read_multi_account_report_v1_with_reader(outcome, context, &FileArtifactReader)
}

/// Verify the parent batch and every successful Bar/Depth child, then return one bounded DTO.
///
/// The caller supplies `ArtifactReader` so an API can constrain all referenced artifacts to its
/// authorized root while CLI and SDK callers use the normal filesystem reader.
pub fn read_multi_account_report_v1_with_reader(
    outcome: &Value,
    context: &RunContext,
    reader: &dyn ArtifactReader,
) -> Result<MultiAccountResearchReportV1, AppError> {
    context.require(CallerCapability::Research, "读取多账户研究报告")?;
    let runs = outcome
        .get("runs")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid_report("多账户研究结果缺少 runs"))?;
    if !(2..=crate::MAX_MULTI_ACCOUNT_RUNS).contains(&runs.len()) {
        return Err(invalid_report(format!(
            "多账户研究结果数量必须在 2..={}",
            crate::MAX_MULTI_ACCOUNT_RUNS
        )));
    }
    let is_depth = outcome.get("result_kind").and_then(Value::as_str)
        == Some("multi_account_depth")
        || runs.iter().any(|run| {
            run.get("outcome")
                .and_then(Value::as_object)
                .is_some_and(|child| child.contains_key("tier"))
        });

    if is_depth {
        let result: MultiAccountDepthBacktestResult = serde_json::from_value(outcome.clone())
            .map_err(|error| invalid_report(format!("多账户深度结果结构无效: {error}")))?;
        if result.schema_version >= 2 {
            crate::artifact_store::verify_multi_account_depth_backtest_result_with_reader(
                &result, reader,
            )
            .map_err(|message| {
                report_conflict(format!("多账户 Depth 父运行身份复核失败: {message}"))
            })?;
        }
        let summary = MultiAccountResearchSummaryV1 {
            accounts_total: result.summary.accounts_total,
            accounts_succeeded: result.summary.accounts_succeeded,
            accounts_failed: result.summary.accounts_failed,
            successful_initial_cash_raw: result.summary.successful_initial_cash_raw,
            successful_fills: result.summary.successful_fills,
            weighted_return_bps: result.summary.weighted_return_bps,
            complete: result.summary.complete,
        };
        read_children(
            result.batch_id,
            result.settlement_currency,
            result.spec_fingerprint,
            summary,
            serde_json::to_value(result.runs).map_err(internal_report_error)?,
            true,
            context,
            reader,
        )
    } else {
        let result: MultiAccountBacktestResult = serde_json::from_value(outcome.clone())
            .map_err(|error| invalid_report(format!("多账户 Bar 结果结构无效: {error}")))?;
        if result.schema_version >= 2 {
            crate::artifact_store::verify_multi_account_backtest_result_with_reader(
                &result, reader,
            )
            .map_err(|message| {
                report_conflict(format!("多账户 Bar 父运行身份复核失败: {message}"))
            })?;
        }
        let summary = MultiAccountResearchSummaryV1 {
            accounts_total: result.summary.accounts_total,
            accounts_succeeded: result.summary.accounts_succeeded,
            accounts_failed: result.summary.accounts_failed,
            successful_initial_cash_raw: result.summary.successful_initial_cash_raw,
            successful_fills: result.summary.successful_fills,
            weighted_return_bps: result.summary.weighted_return_bps,
            complete: result.summary.complete,
        };
        read_children(
            result.batch_id,
            result.settlement_currency,
            result.spec_fingerprint,
            summary,
            serde_json::to_value(result.runs).map_err(internal_report_error)?,
            false,
            context,
            reader,
        )
    }
}

fn read_children(
    batch_id: String,
    settlement_currency: String,
    spec_fingerprint: String,
    declared_summary: MultiAccountResearchSummaryV1,
    runs: Value,
    is_depth: bool,
    context: &RunContext,
    reader: &dyn ArtifactReader,
) -> Result<MultiAccountResearchReportV1, AppError> {
    if batch_id.trim().is_empty()
        || settlement_currency.trim().is_empty()
        || spec_fingerprint.len() != 16
        || !spec_fingerprint
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(invalid_report("多账户批次身份字段无效"));
    }
    let runs = runs
        .as_array()
        .ok_or_else(|| invalid_report("多账户研究结果 runs 不是数组"))?;
    let mut members = Vec::with_capacity(runs.len());
    let mut accounts = Vec::with_capacity(runs.len());
    let mut seen = BTreeSet::new();
    for run in runs {
        let account_id = run
            .get("account_id")
            .and_then(Value::as_str)
            .filter(|account_id| !account_id.is_empty())
            .ok_or_else(|| invalid_report("多账户成员缺少 account_id"))?;
        if !seen.insert(account_id.to_owned()) {
            return Err(invalid_report("多账户研究结果含重复 account_id"));
        }
        let error_json = run
            .get("error_json")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let Some(child_value) = run.get("outcome").filter(|value| !value.is_null()) else {
            members.push(MultiAccountResearchMemberV1::Failed {
                account_id: account_id.to_owned(),
            });
            accounts.push(MultiAccountResearchAccountReportV1 {
                account_id: account_id.to_owned(),
                status: MultiAccountResearchStatusV1::Failed,
                run_id: None,
                report: None,
                error_json,
            });
            continue;
        };

        let (run_id, reported_account_id, fills, return_bps, report) = if is_depth {
            let child: crate::DepthBacktestOutcome = serde_json::from_value(child_value.clone())
                .map_err(|error| {
                    invalid_report(format!("账户 {account_id} Depth outcome 无效: {error}"))
                })?;
            let child_context = child_context(context, &child.run_id);
            let report = read_depth_run_report_v1_with_reader(&child, &child_context, reader)?;
            (
                child.run_id,
                child.account_id,
                child.fills,
                child.return_bps,
                report,
            )
        } else {
            let child: crate::BacktestOutcome = serde_json::from_value(child_value.clone())
                .map_err(|error| {
                    invalid_report(format!("账户 {account_id} Bar outcome 无效: {error}"))
                })?;
            let child_context = child_context(context, &child.run_id);
            let report = read_run_report_v1_with_reader(&child, &child_context, reader)?;
            (
                child.run_id,
                child.account_id,
                child.fills,
                child.return_bps,
                report,
            )
        };
        if reported_account_id.as_deref() != Some(account_id) {
            return Err(invalid_report(format!(
                "账户 {account_id} 的 outcome.account_id 不匹配"
            )));
        }
        if !report.verification.verified {
            return Err(report_conflict(format!(
                "账户 {account_id} 运行产物复核未通过: {}",
                report.verification.mismatches.join("; ")
            )));
        }
        let initial_cash_raw = report
            .summary
            .pointer("/account/initial_cash_raw")
            .and_then(raw_i128)
            .or_else(|| {
                report
                    .equity
                    .rows
                    .first()
                    .and_then(|row| row.get("equity_raw"))
                    .and_then(raw_i128)
            })
            .ok_or_else(|| invalid_report(format!("账户 {account_id} summary 缺少有效初始资金")))?;
        members.push(MultiAccountResearchMemberV1::Succeeded {
            account_id: account_id.to_owned(),
            initial_cash_raw,
            return_bps,
            fills,
            data_fingerprint: report.verification.data_fingerprint.clone(),
            result_hash: report.verification.result_hash.clone(),
        });
        accounts.push(MultiAccountResearchAccountReportV1 {
            account_id: account_id.to_owned(),
            status: MultiAccountResearchStatusV1::Succeeded,
            run_id: Some(run_id),
            report: Some(report),
            error_json: None,
        });
    }

    let facts = verify_multi_account_research_facts_v1(
        &batch_id,
        &spec_fingerprint,
        &members,
        &declared_summary,
    )
    .map_err(|message| report_conflict(message))?;
    Ok(MultiAccountResearchReportV1 {
        schema_version: MULTI_ACCOUNT_RESEARCH_REPORT_SCHEMA_VERSION_V1,
        run_id: batch_id.clone(),
        settlement_currency: settlement_currency.clone(),
        spec_fingerprint,
        instrument: format!("multi-account[{settlement_currency}]"),
        verification: crate::VerificationResult {
            run_id: batch_id,
            result_hash: facts.result_hash,
            data_fingerprint: facts.data_fingerprint,
            verified: true,
            checks: vec![
                "多账户批次 summary 已由成功子报告复算".into(),
                "所有成功账户的 Bar/Depth 核心报告均已逐项复核".into(),
                "父 RunManifest 与 CanonicalRunSpec 已在适用版本下核对".into(),
            ],
            mismatches: Vec::new(),
        },
        summary: facts.summary,
        accounts,
    })
}

fn child_context(parent: &RunContext, run_id: &str) -> RunContext {
    RunContext::new(parent.capability(), run_id).with_code_commit(parent.code_commit())
}

fn raw_i128(value: &Value) -> Option<i128> {
    value
        .as_i64()
        .map(i128::from)
        .or_else(|| value.as_u64().map(i128::from))
        .or_else(|| value.as_str().and_then(|raw| raw.parse::<i128>().ok()))
}

fn invalid_report(message: impl Into<String>) -> AppError {
    AppError::new(AppErrorCategory::InvalidInput, message)
}

fn report_conflict(message: impl Into<String>) -> AppError {
    AppError::new(AppErrorCategory::Conflict, message)
}

fn internal_report_error(error: serde_json::Error) -> AppError {
    AppError::new(
        AppErrorCategory::InternalInvariant,
        format!("多账户报告 DTO 序列化失败: {error}"),
    )
}

/// Recompute the declared batch summary and stable child-identity digests.
///
/// Inputs must come from child reports whose artifacts and event streams have already passed
/// verification. This function owns the arithmetic, ordering, and digest contract for report
/// adapters so every entry point can consume the same application-layer result.
pub fn verify_multi_account_research_facts_v1(
    batch_id: &str,
    spec_fingerprint: &str,
    members: &[MultiAccountResearchMemberV1],
    declared_summary: &MultiAccountResearchSummaryV1,
) -> Result<MultiAccountResearchFactsV1, String> {
    if batch_id.is_empty() {
        return Err("多账户批次 ID 不能为空".into());
    }
    if spec_fingerprint.len() != 16
        || !spec_fingerprint
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err("多账户规格指纹必须是 16 位十六进制字符串".into());
    }
    if !(2..=crate::MAX_MULTI_ACCOUNT_RUNS).contains(&members.len()) {
        return Err(format!(
            "多账户报告成员数必须在 2..={}",
            crate::MAX_MULTI_ACCOUNT_RUNS
        ));
    }

    let mut seen_account_ids = BTreeSet::new();
    let mut successful = 0_usize;
    let mut successful_fills = 0_u64;
    let mut weighted_returns = WeightedReturnAccumulatorV1::default();
    let mut data_hasher = qx_core::Fnv1a::new();
    let mut result_hasher = qx_core::Fnv1a::new();
    data_hasher.write_text(batch_id);
    result_hasher.write_text(batch_id);
    result_hasher.write_text(spec_fingerprint);

    for member in members {
        let account_id = match member {
            MultiAccountResearchMemberV1::Failed { account_id }
            | MultiAccountResearchMemberV1::Succeeded { account_id, .. } => account_id,
        };
        if account_id.is_empty() || !seen_account_ids.insert(account_id.as_str()) {
            return Err("多账户报告包含空或重复的 account_id".into());
        }
        if let MultiAccountResearchMemberV1::Succeeded {
            initial_cash_raw,
            return_bps,
            fills,
            data_fingerprint,
            result_hash,
            ..
        } = member
        {
            if data_fingerprint.is_empty() || result_hash.is_empty() {
                return Err(format!("账户 {account_id} 的运行指纹无效"));
            }
            weighted_returns.push(*return_bps, *initial_cash_raw)?;
            successful = successful
                .checked_add(1)
                .ok_or_else(|| "账户成功数溢出".to_string())?;
            successful_fills = successful_fills
                .checked_add(*fills)
                .ok_or_else(|| "多账户成交总数溢出".to_string())?;
            data_hasher.write_text(account_id);
            data_hasher.write_text(data_fingerprint);
            result_hasher.write_text(account_id);
            result_hasher.write_text(result_hash);
        }
    }

    let failed = members.len() - successful;
    let summary = MultiAccountResearchSummaryV1 {
        accounts_total: members.len(),
        accounts_succeeded: successful,
        accounts_failed: failed,
        successful_initial_cash_raw: weighted_returns.initial_cash_raw(),
        successful_fills,
        weighted_return_bps: weighted_returns.finish()?,
        complete: failed == 0,
    };
    if &summary != declared_summary {
        return Err("多账户已声明汇总与逐账户复核结果不一致".into());
    }

    Ok(MultiAccountResearchFactsV1 {
        summary,
        data_fingerprint: format!("{:016x}", data_hasher.finish()),
        result_hash: format!("{:016x}", result_hasher.finish()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn success(
        account_id: &str,
        initial_cash_raw: i128,
        return_bps: i32,
        fills: u64,
        data_fingerprint: &str,
        result_hash: &str,
    ) -> MultiAccountResearchMemberV1 {
        MultiAccountResearchMemberV1::Succeeded {
            account_id: account_id.into(),
            initial_cash_raw,
            return_bps,
            fills,
            data_fingerprint: data_fingerprint.into(),
            result_hash: result_hash.into(),
        }
    }

    fn summary(
        accounts_total: usize,
        accounts_succeeded: usize,
        accounts_failed: usize,
        successful_initial_cash_raw: i128,
        successful_fills: u64,
        weighted_return_bps: Option<i32>,
        complete: bool,
    ) -> MultiAccountResearchSummaryV1 {
        MultiAccountResearchSummaryV1 {
            accounts_total,
            accounts_succeeded,
            accounts_failed,
            successful_initial_cash_raw,
            successful_fills,
            weighted_return_bps,
            complete,
        }
    }

    #[test]
    fn verifies_partial_batch_summary_and_child_identity_digests() {
        let members = vec![
            success(
                "alpha",
                10_000,
                100,
                2,
                "0000000000000001",
                "0000000000000002",
            ),
            MultiAccountResearchMemberV1::Failed {
                account_id: "beta".into(),
            },
            success(
                "gamma",
                30_000,
                -100,
                3,
                "0000000000000003",
                "0000000000000004",
            ),
        ];
        let declared = summary(3, 2, 1, 40_000, 5, Some(-50), false);
        let facts = verify_multi_account_research_facts_v1(
            "batch-1",
            "0123456789abcdef",
            &members,
            &declared,
        )
        .unwrap();

        let mut expected_data = qx_core::Fnv1a::new();
        expected_data.write_text("batch-1");
        expected_data.write_text("alpha");
        expected_data.write_text("0000000000000001");
        expected_data.write_text("gamma");
        expected_data.write_text("0000000000000003");
        let mut expected_result = qx_core::Fnv1a::new();
        expected_result.write_text("batch-1");
        expected_result.write_text("0123456789abcdef");
        expected_result.write_text("alpha");
        expected_result.write_text("0000000000000002");
        expected_result.write_text("gamma");
        expected_result.write_text("0000000000000004");
        assert_eq!(facts.summary, declared);
        assert_eq!(
            facts.data_fingerprint,
            format!("{:016x}", expected_data.finish())
        );
        assert_eq!(
            facts.result_hash,
            format!("{:016x}", expected_result.finish())
        );
    }

    #[test]
    fn all_failed_batch_has_no_weighted_return_and_stable_identity() {
        let members = vec![
            MultiAccountResearchMemberV1::Failed {
                account_id: "alpha".into(),
            },
            MultiAccountResearchMemberV1::Failed {
                account_id: "beta".into(),
            },
        ];
        let declared = summary(2, 0, 2, 0, 0, None, false);
        let facts = verify_multi_account_research_facts_v1(
            "batch-2",
            "0123456789abcdef",
            &members,
            &declared,
        )
        .unwrap();
        assert_eq!(facts.summary, declared);
        assert_ne!(facts.data_fingerprint, "0000000000000000");
    }

    #[test]
    fn rejects_tampered_summary_duplicate_accounts_and_overflow() {
        let account = success(
            "alpha",
            10_000,
            100,
            2,
            "0000000000000001",
            "0000000000000002",
        );
        let wrong_summary = summary(2, 1, 1, 10_000, 1, Some(100), false);
        assert!(verify_multi_account_research_facts_v1(
            "batch-3",
            "0123456789abcdef",
            &[
                account.clone(),
                MultiAccountResearchMemberV1::Failed {
                    account_id: "beta".into()
                }
            ],
            &wrong_summary,
        )
        .is_err());

        assert!(verify_multi_account_research_facts_v1(
            "batch-3",
            "0123456789abcdef",
            &[
                success("alpha", 10_000, 100, 2, "", "0000000000000002"),
                MultiAccountResearchMemberV1::Failed {
                    account_id: "beta".into(),
                },
            ],
            &summary(2, 1, 1, 10_000, 2, Some(100), false),
        )
        .is_err());

        let duplicate = vec![account.clone(), account.clone()];
        assert!(verify_multi_account_research_facts_v1(
            "batch-3",
            "0123456789abcdef",
            &duplicate,
            &summary(2, 2, 0, 20_000, 4, Some(100), true),
        )
        .is_err());

        let overflow = vec![
            success(
                "alpha",
                i128::MAX,
                0,
                0,
                "0000000000000001",
                "0000000000000002",
            ),
            success("beta", 1, 0, 0, "0000000000000003", "0000000000000004"),
        ];
        assert!(verify_multi_account_research_facts_v1(
            "batch-3",
            "0123456789abcdef",
            &overflow,
            &summary(2, 2, 0, 0, 0, Some(0), true),
        )
        .is_err());
    }
}
