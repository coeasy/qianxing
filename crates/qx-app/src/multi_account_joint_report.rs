//! Verified report facts for synchronized, independently funded exchange accounts.

use crate::artifact_store::{
    read_text_bounded, verify_canonical_run_spec_for_manifest,
    verify_declared_run_manifest_with_reader, ArtifactReader, FileArtifactReader,
};
use crate::report_read::read_multi_market_event_report_v1_with_reader;
use crate::{
    AppError, AppErrorCategory, CallerCapability, MultiAccountJointBacktestOutcome,
    MultiAccountJointBacktestSpec, MultiMarketBacktestSpec, RunContext,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

pub const MULTI_ACCOUNT_JOINT_RESEARCH_REPORT_SCHEMA_VERSION_V1: u32 = 1;
pub const MAX_MULTI_ACCOUNT_JOINT_REPORT_ROWS: usize = 1_000_000;
pub const MAX_MULTI_ACCOUNT_JOINT_REPORT_EQUITY_SAMPLE: usize = 2_000;
pub const MAX_MULTI_ACCOUNT_JOINT_REPORT_FILL_SAMPLE: usize = 500;
pub const MAX_MULTI_ACCOUNT_JOINT_REPORT_EVENT_SAMPLE: usize = 50;
pub const MAX_MULTI_ACCOUNT_JOINT_REPORT_EVENT_PAGE: usize = 300;

const MAX_JOINT_REPORT_ARTIFACT_BYTES: u64 = 128 * 1024 * 1024;
const MAX_JOINT_REPORT_SIDECAR_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MultiAccountJointResearchReportV1 {
    pub schema_version: u32,
    pub run_id: String,
    pub settlement_currency: String,
    pub spec_fingerprint: String,
    pub verification: crate::VerificationResult,
    pub summary: MultiAccountJointResearchSummaryV1,
    pub equity: MultiAccountJointEquitySeriesV1,
    pub fills: MultiAccountJointFillSampleV1,
    pub accounts: Vec<MultiAccountJointAccountReportV1>,
}

/// Independent event pages keyed by exchange and account; offsets never imply cross-venue order.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MultiAccountJointReportReadOptionsV1 {
    #[serde(default)]
    pub joint_account_event_pages:
        BTreeMap<String, BTreeMap<String, MultiAccountJointEventPageRequestV1>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MultiAccountJointEventPageRequestV1 {
    pub offset: u64,
    pub limit: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MultiAccountJointResearchSummaryV1 {
    pub accounts_total: usize,
    pub markets_total: usize,
    pub initial_cash_raw: String,
    pub final_equity_raw: String,
    pub return_bps: i32,
    pub max_drawdown_bps: u32,
    pub successful_fills: u64,
    pub complete: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MultiAccountJointEquityPointV1 {
    pub ts: u64,
    pub equity_raw: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MultiAccountJointEquitySeriesV1 {
    pub total: u64,
    pub truncated: bool,
    pub rows: Vec<MultiAccountJointEquityPointV1>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MultiAccountJointFillSampleV1 {
    pub total: u64,
    pub truncated: bool,
    pub rows: Vec<Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MultiAccountJointAccountReportV1 {
    pub exchange_id: String,
    pub account_id: String,
    pub run_id: String,
    pub initial_cash_raw: String,
    pub final_equity_raw: String,
    pub return_bps: i32,
    pub max_drawdown_bps: u32,
    pub markets_total: usize,
    pub strategies_total: usize,
    pub successful_fills: u64,
    pub equity_points: u64,
    pub data_fingerprint: String,
    pub result_hash: String,
    pub fill_rows_total: u64,
    pub fill_rows_truncated: bool,
    pub fill_rows: Vec<Value>,
    pub event_rows_total: u64,
    pub event_rows_offset: u64,
    pub event_rows_limit: usize,
    pub event_rows_truncated: bool,
    pub event_rows_previous_offset: Option<u64>,
    pub event_rows_next_offset: Option<u64>,
    pub event_rows: Vec<Value>,
}

/// Read and verify a joint account report using the local filesystem adapter.
pub fn read_multi_account_joint_report_v1(
    outcome: &MultiAccountJointBacktestOutcome,
    context: &RunContext,
) -> Result<MultiAccountJointResearchReportV1, AppError> {
    read_multi_account_joint_report_v1_with_reader(outcome, context, &FileArtifactReader)
}

/// Read a joint account report through the caller's authorized artifact reader.
///
/// The parent RunManifest binds the canonical account/market set and child run identities. This
/// reader additionally reopens every child artifact, replays its EventLog, verifies its Bar and
/// fills sidecars, recomputes the synchronized aggregate equity curve, and checks the joint fills
/// file against the canonical child concatenation before returning bounded report samples.
pub fn read_multi_account_joint_report_v1_with_reader(
    outcome: &MultiAccountJointBacktestOutcome,
    context: &RunContext,
    reader: &dyn ArtifactReader,
) -> Result<MultiAccountJointResearchReportV1, AppError> {
    read_multi_account_joint_report_v1_with_reader_and_options(
        outcome,
        context,
        reader,
        &MultiAccountJointReportReadOptionsV1::default(),
    )
}

/// Read a report with explicit per-exchange/account event pages.
pub fn read_multi_account_joint_report_v1_with_options(
    outcome: &MultiAccountJointBacktestOutcome,
    context: &RunContext,
    options: &MultiAccountJointReportReadOptionsV1,
) -> Result<MultiAccountJointResearchReportV1, AppError> {
    read_multi_account_joint_report_v1_with_reader_and_options(
        outcome,
        context,
        &FileArtifactReader,
        options,
    )
}

/// Reader-aware version of [`read_multi_account_joint_report_v1_with_options`].
pub fn read_multi_account_joint_report_v1_with_reader_and_options(
    outcome: &MultiAccountJointBacktestOutcome,
    context: &RunContext,
    reader: &dyn ArtifactReader,
    options: &MultiAccountJointReportReadOptionsV1,
) -> Result<MultiAccountJointResearchReportV1, AppError> {
    context.require(CallerCapability::Research, "读取联合多账户研究报告")?;
    validate_event_page_options(outcome, options)?;
    let parent_artifact = read_json_artifact(
        reader,
        &outcome.artifact_path,
        MAX_JOINT_REPORT_ARTIFACT_BYTES,
        "联合账户组合产物",
    )?;
    if parent_artifact
        .get("schema_version")
        .and_then(Value::as_u64)
        != Some(2)
    {
        return Err(invalid_report("联合账户组合产物 schema_version 不受支持"));
    }
    let stored_outcome: MultiAccountJointBacktestOutcome = serde_json::from_value(
        parent_artifact
            .get("outcome")
            .cloned()
            .ok_or_else(|| invalid_report("联合账户组合产物缺少 outcome"))?,
    )
    .map_err(|error| invalid_report(format!("联合账户 outcome 无效: {error}")))?;
    if stored_outcome != *outcome
        || parent_artifact
            .get("joint_equity_path")
            .and_then(Value::as_str)
            != Some(outcome.equity_path.as_str())
        || parent_artifact
            .get("joint_fills_path")
            .and_then(Value::as_str)
            != Some(outcome.fills_path.as_str())
    {
        return Err(report_conflict(
            "联合账户 outcome 与父级产物路径/身份不一致",
        ));
    }
    let outcome_summary = serde_json::to_value(outcome)
        .map_err(|error| invalid_report(format!("联合账户 outcome 编码失败: {error}")))?;
    crate::artifact_store::verify_declared_run_record_with_reader(&outcome_summary, reader)
        .map_err(|error| report_conflict(format!("联合账户 RunRecord V2 复核失败: {error}")))?;
    verify_declared_run_manifest_with_reader(&parent_artifact, reader)
        .map_err(|error| report_conflict(format!("联合账户父级身份复核失败: {error}")))?;

    let spec: MultiAccountJointBacktestSpec = serde_json::from_value(
        parent_artifact
            .get("spec")
            .cloned()
            .ok_or_else(|| invalid_report("联合账户父级产物缺少 spec"))?,
    )
    .map_err(|error| invalid_report(format!("联合账户 spec 无效: {error}")))?;
    spec.validate()?;
    let verifications = parent_artifact
        .get("account_run_verifications")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid_report("联合账户父级产物缺少 account_run_verifications"))?;
    if spec.run_id != outcome.run_id
        || spec.settlement_currency != outcome.settlement_currency
        || outcome.accounts_total != spec.accounts.len()
        || outcome.accounts.len() != spec.accounts.len()
        || verifications.len() != spec.accounts.len()
        || outcome.run_manifest_path.as_deref()
            != parent_artifact
                .get("run_manifest_path")
                .and_then(Value::as_str)
    {
        return Err(report_conflict(
            "联合账户父规格、成员数或 RunManifest 身份不一致",
        ));
    }

    let parent_equity_value = read_json_artifact(
        reader,
        &outcome.equity_path,
        MAX_JOINT_REPORT_SIDECAR_BYTES,
        "联合账户权益曲线",
    )?;
    let parent_equity = parse_equity_curve(&parent_equity_value, "联合账户权益曲线")?;
    let mut expected_equity = vec![outcome.initial_cash_raw; parent_equity.len()];
    let mut timestamps_covered = vec![false; parent_equity.len()];
    let parent_timestamps = parent_equity.iter().map(|(ts, _)| *ts).collect::<Vec<_>>();
    let parent_fills_value = read_json_artifact(
        reader,
        &outcome.fills_path,
        MAX_JOINT_REPORT_SIDECAR_BYTES,
        "联合账户成交明细",
    )?;
    let parent_fills = parent_fills_value
        .as_array()
        .ok_or_else(|| invalid_report("联合账户成交明细必须是数组"))?;
    if parent_fills.len() > MAX_MULTI_ACCOUNT_JOINT_REPORT_ROWS
        || parent_fills.len() as u64 != outcome.successful_fills
    {
        return Err(report_conflict(
            "联合账户成交行数超过上限或与 outcome 不一致",
        ));
    }

    let mut canonical_accounts = spec.accounts.clone();
    canonical_accounts.sort_by(|left, right| {
        left.exchange_id
            .cmp(&right.exchange_id)
            .then_with(|| left.account_id.cmp(&right.account_id))
    });
    let mut accounts = Vec::with_capacity(canonical_accounts.len());
    let mut child_fill_cursor = 0_usize;
    let mut total_markets = 0_usize;
    let mut fills_digest = qx_strategy::file_digest::Sha256Accumulator::new();
    fills_digest.update(b"qx-multi-account-joint-fills-v1\0");
    for (index, (account, account_outcome)) in
        canonical_accounts.iter().zip(&outcome.accounts).enumerate()
    {
        let child = &account_outcome.outcome;
        let verification = &verifications[index];
        let child_artifact = read_json_artifact(
            reader,
            &child.artifact_path,
            MAX_JOINT_REPORT_ARTIFACT_BYTES,
            "联合账户成员多市场产物",
        )?;
        let actual_identity = serde_json::json!({
            "market_data": child_artifact.get("market_data").cloned().ok_or_else(|| invalid_report("成员产物缺少 market_data"))?,
            "dataset_quality": child_artifact.get("dataset_quality").cloned().ok_or_else(|| invalid_report("成员产物缺少 dataset_quality"))?,
            "strategy_identities": child_artifact.get("strategy_identities").cloned().ok_or_else(|| invalid_report("成员产物缺少 strategy_identities"))?,
            "outcome": child_artifact.get("outcome").cloned().ok_or_else(|| invalid_report("成员产物缺少 outcome"))?,
        });
        if verification.get("exchange_id").and_then(Value::as_str)
            != Some(account.exchange_id.as_str())
            || verification.get("account_id").and_then(Value::as_str)
                != Some(account.account_id.as_str())
            || verification.get("identity") != Some(&actual_identity)
            || child_artifact.get("outcome")
                != Some(
                    &serde_json::to_value(child)
                        .map_err(|error| internal_report_error(error.to_string()))?,
                )
        {
            return Err(report_conflict(format!(
                "{}:{} 成员产物与父级身份记录不一致",
                account.exchange_id, account.account_id
            )));
        }
        let child_spec: MultiMarketBacktestSpec = serde_json::from_value(
            child_artifact
                .get("spec")
                .cloned()
                .ok_or_else(|| invalid_report("成员多市场产物缺少 spec"))?,
        )
        .map_err(|error| invalid_report(format!("成员多市场 spec 无效: {error}")))?;
        let expected_child = crate::MultiMarketBacktestSpec {
            schema_version: crate::MULTI_MARKET_BACKTEST_SCHEMA_VERSION,
            run_id: crate::cases::joint_child_run_id(
                &spec.run_id,
                &account.exchange_id,
                &account.account_id,
            ),
            account_id: account.account_id.clone(),
            settlement_currency: spec.settlement_currency.clone(),
            initial_cash_raw: account.initial_cash_raw,
            initial_cash_exact: None,
            seed: spec.seed,
            output_dir: spec.output_dir.clone(),
            markets: account.markets.clone(),
        };
        if child_spec != expected_child {
            return Err(report_conflict(format!(
                "{}:{} 成员多市场 spec 与父级账户规格不一致",
                account.exchange_id, account.account_id
            )));
        }
        child_spec.validate()?;
        let manifest_path = child_artifact
            .get("run_manifest_path")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid_report("成员多市场产物缺少 RunManifest 路径"))?;
        let manifest_value = read_json_artifact(
            reader,
            manifest_path,
            MAX_JOINT_REPORT_SIDECAR_BYTES,
            "成员多市场 RunManifest",
        )?;
        let manifest: qx_core::RunManifest = serde_json::from_value(manifest_value)
            .map_err(|error| invalid_report(format!("成员 RunManifest 无效: {error}")))?;
        let embedded_manifest: qx_core::RunManifest = serde_json::from_value(
            verification
                .get("run_manifest")
                .cloned()
                .ok_or_else(|| invalid_report("父级成员身份缺少 RunManifest"))?,
        )
        .map_err(|error| invalid_report(format!("父级成员 RunManifest 无效: {error}")))?;
        if manifest != embedded_manifest {
            return Err(report_conflict("成员 RunManifest 与父级记录不一致"));
        }
        verify_canonical_run_spec_for_manifest(&manifest, &child_artifact)
            .map_err(|error| report_conflict(format!("成员 CanonicalRunSpec 复核失败: {error}")))?;

        let equity_path = child_artifact
            .get("equity_path")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid_report("成员产物缺少 equity_path"))?;
        let fills_path = child_artifact
            .get("fills_path")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid_report("成员产物缺少 fills_path"))?;
        if child_artifact
            .get("event_trace_path")
            .and_then(Value::as_str)
            .is_none()
        {
            return Err(report_conflict("成员产物缺少可重放的 EventLog"));
        }
        let equity_value = read_json_artifact(
            reader,
            equity_path,
            MAX_JOINT_REPORT_SIDECAR_BYTES,
            "成员权益曲线",
        )?;
        let exact_equity_value = child_artifact
            .get("equity_exact_path")
            .and_then(Value::as_str)
            .map(|path| {
                read_json_artifact(
                    reader,
                    path,
                    MAX_JOINT_REPORT_SIDECAR_BYTES,
                    "成员 exact 权益曲线",
                )
            })
            .transpose()?;
        let account_equity = parse_equity_curve(&equity_value, "成员权益曲线")?;
        let fills_value = read_json_artifact(
            reader,
            fills_path,
            MAX_JOINT_REPORT_SIDECAR_BYTES,
            "成员成交明细",
        )?;
        let event_count = child_artifact
            .get("event_count")
            .and_then(Value::as_u64)
            .ok_or_else(|| invalid_report("成员产物 event_count 无效"))?;
        let ledger_entries = child_artifact
            .get("ledger_entries")
            .cloned()
            .ok_or_else(|| invalid_report("成员产物缺少 ledger_entries"))?;
        let mut strategy_markets = BTreeMap::new();
        for market in &child_spec.markets {
            for member in &market.strategies {
                if strategy_markets
                    .insert(
                        member.strategy.strategy_id.clone(),
                        market.instrument.clone(),
                    )
                    .is_some()
                {
                    return Err(report_conflict("成员策略标识重复"));
                }
            }
        }
        let event_trace_path = child_artifact["event_trace_path"]
            .as_str()
            .ok_or_else(|| invalid_report("成员 EventLog 路径无效"))?;
        let requested_page = options
            .joint_account_event_pages
            .get(&account.exchange_id)
            .and_then(|accounts| accounts.get(&account.account_id))
            .map(|page| (page.offset, page.limit))
            .unwrap_or((
                event_count.saturating_sub(MAX_MULTI_ACCOUNT_JOINT_REPORT_EVENT_SAMPLE as u64),
                MAX_MULTI_ACCOUNT_JOINT_REPORT_EVENT_SAMPLE,
            ));
        let event_report = read_multi_market_event_report_v1_with_reader(
            event_trace_path,
            event_count,
            &manifest.output_event_hash,
            &ledger_entries,
            &strategy_markets,
            Some(requested_page),
            reader,
        )
        .map_err(|error| report_conflict(format!("成员 EventLog 重放失败: {error}")))?;
        let event_rows_offset = event_report.offset;
        let event_rows_limit = requested_page.1;
        let event_rows_end = event_rows_offset.saturating_add(event_report.rows.len() as u64);
        let event_rows_previous_offset = (event_rows_offset > 0)
            .then_some(event_rows_offset.saturating_sub(event_rows_limit as u64));
        let event_rows_next_offset =
            (event_rows_end < event_report.total).then_some(event_rows_end);
        let sidecars = crate::verify_multi_market_bar_sidecars_from_event_report_with_exact_v1(
            &equity_value,
            exact_equity_value.as_ref(),
            &fills_value,
            event_report.fill_count,
            &event_report.fill_sequence_sha256,
            &child_spec,
            child.equity_points,
            child.final_equity_raw,
            child.return_bps,
            child.max_drawdown_bps,
        )
        .map_err(|error| report_conflict(format!("成员权益/成交 sidecar 复核失败: {error}")))?;
        if account_equity.len() as u64 != child.equity_points
            || account_equity.first().map(|(_, equity)| *equity) != Some(account.initial_cash_raw)
            || sidecars.fill_rows != child.successful_fills
            || child.run_id != expected_child.run_id
            || child.account_id != account.account_id
            || child.markets_total != account.markets.len()
        {
            return Err(report_conflict("成员权益/成交汇总与账户 outcome 不一致"));
        }
        for (joint_index, (joint_ts, _)) in parent_equity.iter().enumerate() {
            let contribution = account_equity
                .partition_point(|(account_ts, _)| account_ts <= joint_ts)
                .checked_sub(1)
                .and_then(|point_index| account_equity.get(point_index))
                .map(|(_, equity)| *equity)
                .unwrap_or(account.initial_cash_raw);
            expected_equity[joint_index] = expected_equity[joint_index]
                .checked_add(
                    contribution
                        .checked_sub(account.initial_cash_raw)
                        .ok_or_else(|| invalid_report("成员权益贡献溢出"))?,
                )
                .ok_or_else(|| invalid_report("联合账户期望权益合计溢出"))?;
        }
        for (account_ts, _) in &account_equity {
            let joint_index = parent_timestamps
                .binary_search(account_ts)
                .map_err(|_| report_conflict("联合权益遗漏成员权益时间戳"))?;
            timestamps_covered[joint_index] = true;
        }

        let child_fills = fills_value
            .as_array()
            .ok_or_else(|| invalid_report("成员成交明细必须是数组"))?;
        if child_fills.len() > MAX_MULTI_ACCOUNT_JOINT_REPORT_ROWS {
            return Err(AppError::new(
                AppErrorCategory::Timeout,
                "成员成交明细超过联合报告 100 万行限制",
            ));
        }
        let next_cursor = child_fill_cursor
            .checked_add(child_fills.len())
            .ok_or_else(|| invalid_report("联合成交索引溢出"))?;
        if next_cursor > parent_fills.len()
            || parent_fills[child_fill_cursor..next_cursor] != *child_fills
        {
            return Err(report_conflict("联合成交明细与账户成员成交顺序/内容不一致"));
        }
        for row in child_fills {
            let fill: qx_core::Fill = serde_json::from_value(row.clone())
                .map_err(|error| invalid_report(format!("成员成交行无效: {error}")))?;
            let normalized = serde_json::to_value(&fill)
                .map_err(|error| internal_report_error(error.to_string()))?;
            if normalized != *row {
                return Err(report_conflict("成员成交行不是规范 Fill 序列化"));
            }
            let encoded = serde_json::to_vec(&fill)
                .map_err(|error| internal_report_error(error.to_string()))?;
            fills_digest.update(&(encoded.len() as u64).to_le_bytes());
            fills_digest.update(&encoded);
        }
        child_fill_cursor = next_cursor;
        total_markets = total_markets
            .checked_add(account.markets.len())
            .ok_or_else(|| invalid_report("联合市场数量溢出"))?;
        accounts.push(MultiAccountJointAccountReportV1 {
            exchange_id: account.exchange_id.clone(),
            account_id: account.account_id.clone(),
            run_id: child.run_id.clone(),
            initial_cash_raw: account.initial_cash_raw.to_string(),
            final_equity_raw: child.final_equity_raw.to_string(),
            return_bps: child.return_bps,
            max_drawdown_bps: child.max_drawdown_bps,
            markets_total: child.markets_total,
            strategies_total: child.strategies_total,
            successful_fills: child.successful_fills,
            equity_points: child.equity_points,
            data_fingerprint: child.data_fingerprint.clone(),
            result_hash: child.shared_result_hash.clone(),
            fill_rows_total: child_fills.len() as u64,
            fill_rows_truncated: child_fills.len() > MAX_MULTI_ACCOUNT_JOINT_REPORT_FILL_SAMPLE,
            fill_rows: tail_sample(child_fills, MAX_MULTI_ACCOUNT_JOINT_REPORT_FILL_SAMPLE),
            event_rows_total: event_report.total,
            event_rows_offset,
            event_rows_limit,
            event_rows_truncated: event_report.truncated,
            event_rows_previous_offset,
            event_rows_next_offset,
            event_rows: event_report.rows,
        });
    }
    if child_fill_cursor != parent_fills.len()
        || !timestamps_covered.iter().all(|covered| *covered)
        || expected_equity
            .iter()
            .zip(&parent_equity)
            .any(|(expected, (_, actual))| expected != actual)
    {
        return Err(report_conflict(
            "联合权益或成交明细与已复核的账户成员事实不一致",
        ));
    }
    let actual_max_drawdown =
        crate::report_analysis::joint_equity_max_drawdown_bps_v1(&parent_equity)
            .map_err(invalid_report)?;
    let actual_return = crate::report_analysis::equity_total_return_bps_v1(
        outcome.initial_cash_raw,
        outcome.final_equity_raw,
    )
    .map_err(invalid_report)?;
    if parent_equity.first().map(|(_, value)| *value) != Some(outcome.initial_cash_raw)
        || parent_equity.last().map(|(_, value)| *value) != Some(outcome.final_equity_raw)
        || outcome.markets_total != total_markets
        || fills_digest.finish_hex() != digest_fills(parent_fills)?
        || actual_return != outcome.return_bps
        || actual_max_drawdown != outcome.max_drawdown_bps
    {
        return Err(report_conflict(
            "联合账户权益、成交或性能指标与 outcome 不一致",
        ));
    }

    let fill_rows = tail_sample(parent_fills, MAX_MULTI_ACCOUNT_JOINT_REPORT_FILL_SAMPLE);
    let equity_rows =
        downsample_equity(&parent_equity, MAX_MULTI_ACCOUNT_JOINT_REPORT_EQUITY_SAMPLE)
            .into_iter()
            .map(|(ts, equity)| MultiAccountJointEquityPointV1 {
                ts,
                equity_raw: equity.to_string(),
            })
            .collect::<Vec<_>>();
    let result_hash = parent_artifact["result_hash"]
        .as_str()
        .ok_or_else(|| invalid_report("联合账户 result_hash 无效"))?
        .to_owned();
    let data_fingerprint = parent_artifact["data_fingerprint"]
        .as_str()
        .ok_or_else(|| invalid_report("联合账户 data_fingerprint 无效"))?
        .to_owned();
    Ok(MultiAccountJointResearchReportV1 {
        schema_version: MULTI_ACCOUNT_JOINT_RESEARCH_REPORT_SCHEMA_VERSION_V1,
        run_id: outcome.run_id.clone(),
        settlement_currency: outcome.settlement_currency.clone(),
        spec_fingerprint: outcome.spec_fingerprint.clone(),
        verification: crate::VerificationResult {
            run_id: outcome.run_id.clone(),
            result_hash,
            data_fingerprint,
            verified: true,
            checks: vec![
                "联合父 RunManifest 与 CanonicalRunSpec 已复核".into(),
                "所有成员 RunManifest、CanonicalRunSpec 与 EventLog 均已复核".into(),
                "成员权益/成交与联合权益/成交 sidecar 已交叉核验".into(),
                "账户隔离和联合汇总指标已重算".into(),
            ],
            mismatches: Vec::new(),
        },
        summary: MultiAccountJointResearchSummaryV1 {
            accounts_total: outcome.accounts_total,
            markets_total: outcome.markets_total,
            initial_cash_raw: outcome.initial_cash_raw.to_string(),
            final_equity_raw: outcome.final_equity_raw.to_string(),
            return_bps: outcome.return_bps,
            max_drawdown_bps: outcome.max_drawdown_bps,
            successful_fills: outcome.successful_fills,
            complete: true,
        },
        equity: MultiAccountJointEquitySeriesV1 {
            total: parent_equity.len() as u64,
            truncated: parent_equity.len() > equity_rows.len(),
            rows: equity_rows,
        },
        fills: MultiAccountJointFillSampleV1 {
            total: parent_fills.len() as u64,
            truncated: parent_fills.len() > fill_rows.len(),
            rows: fill_rows,
        },
        accounts,
    })
}

fn validate_event_page_options(
    outcome: &MultiAccountJointBacktestOutcome,
    options: &MultiAccountJointReportReadOptionsV1,
) -> Result<(), AppError> {
    let page_count = options
        .joint_account_event_pages
        .values()
        .try_fold(0_usize, |count, accounts| count.checked_add(accounts.len()))
        .ok_or_else(|| invalid_report("联合账户事件页数量溢出"))?;
    if page_count > outcome.accounts.len() || page_count > crate::MAX_MULTI_ACCOUNT_RUNS {
        return Err(invalid_report(format!(
            "联合账户事件页最多支持 {} 个已存在账户",
            outcome.accounts.len().min(crate::MAX_MULTI_ACCOUNT_RUNS)
        )));
    }
    for (exchange_id, accounts) in &options.joint_account_event_pages {
        if exchange_id.trim().is_empty() {
            return Err(invalid_report("联合账户事件页 exchange_id 不能为空"));
        }
        for (account_id, page) in accounts {
            if account_id.trim().is_empty() {
                return Err(invalid_report("联合账户事件页 account_id 不能为空"));
            }
            if !(1..=MAX_MULTI_ACCOUNT_JOINT_REPORT_EVENT_PAGE).contains(&page.limit) {
                return Err(invalid_report(format!(
                    "联合账户事件页 limit 必须在 1 到 {MAX_MULTI_ACCOUNT_JOINT_REPORT_EVENT_PAGE} 之间"
                )));
            }
            if !outcome.accounts.iter().any(|account| {
                account.exchange_id == *exchange_id && account.account_id == *account_id
            }) {
                return Err(invalid_report(format!(
                    "联合账户事件页不存在交易所账户 {exchange_id}:{account_id}"
                )));
            }
        }
    }
    Ok(())
}

fn read_json_artifact(
    reader: &dyn ArtifactReader,
    path: &str,
    byte_limit: u64,
    label: &str,
) -> Result<Value, AppError> {
    let payload = read_text_bounded(reader, path, byte_limit, label)?;
    serde_json::from_str(&payload)
        .map_err(|error| invalid_report(format!("{label} JSON 无效 {path}: {error}")))
}

fn parse_equity_curve(value: &Value, label: &str) -> Result<Vec<(u64, i128)>, AppError> {
    let rows = value
        .as_array()
        .ok_or_else(|| invalid_report(format!("{label} 必须是数组")))?;
    if rows.is_empty() || rows.len() > MAX_MULTI_ACCOUNT_JOINT_REPORT_ROWS {
        return Err(AppError::new(
            AppErrorCategory::Timeout,
            format!(
                "{label} 点数为空或超过 {} 行限制",
                MAX_MULTI_ACCOUNT_JOINT_REPORT_ROWS
            ),
        ));
    }
    let mut curve = Vec::with_capacity(rows.len());
    for row in rows {
        let pair = row
            .as_array()
            .filter(|pair| pair.len() == 2)
            .ok_or_else(|| invalid_report(format!("{label} 点必须是 [ts,equity_raw]")))?;
        let ts = pair[0]
            .as_u64()
            .ok_or_else(|| invalid_report(format!("{label} 时间戳无效")))?;
        let equity =
            raw_i128(&pair[1]).ok_or_else(|| invalid_report(format!("{label} raw 权益无效")))?;
        if curve.last().is_some_and(|(previous, _)| ts < *previous) {
            return Err(report_conflict(format!("{label} 时间戳倒序")));
        }
        curve.push((ts, equity));
    }
    Ok(curve)
}

fn downsample_equity(rows: &[(u64, i128)], limit: usize) -> Vec<(u64, i128)> {
    if rows.len() <= limit || limit < 2 {
        return rows.to_vec();
    }
    (0..limit)
        .map(|index| {
            let source = index * (rows.len() - 1) / (limit - 1);
            rows[source]
        })
        .collect()
}

fn tail_sample(rows: &[Value], limit: usize) -> Vec<Value> {
    rows.iter()
        .skip(rows.len().saturating_sub(limit))
        .cloned()
        .collect()
}

fn digest_fills(rows: &[Value]) -> Result<String, AppError> {
    let mut digest = qx_strategy::file_digest::Sha256Accumulator::new();
    digest.update(b"qx-multi-account-joint-fills-v1\0");
    for row in rows {
        let fill: qx_core::Fill = serde_json::from_value(row.clone())
            .map_err(|error| invalid_report(format!("联合成交行无效: {error}")))?;
        let normalized = serde_json::to_value(&fill)
            .map_err(|error| internal_report_error(error.to_string()))?;
        if normalized != *row {
            return Err(report_conflict("联合成交行不是规范 Fill 序列化"));
        }
        let encoded =
            serde_json::to_vec(&fill).map_err(|error| internal_report_error(error.to_string()))?;
        digest.update(&(encoded.len() as u64).to_le_bytes());
        digest.update(&encoded);
    }
    Ok(digest.finish_hex())
}

fn raw_i128(value: &Value) -> Option<i128> {
    value
        .as_i64()
        .map(i128::from)
        .or_else(|| value.as_u64().map(i128::from))
        .or_else(|| value.as_str().and_then(|text| text.parse::<i128>().ok()))
}

fn invalid_report(message: impl Into<String>) -> AppError {
    AppError::new(AppErrorCategory::InvalidInput, message)
}

fn report_conflict(message: impl Into<String>) -> AppError {
    AppError::new(AppErrorCategory::Conflict, message)
}

fn internal_report_error(message: String) -> AppError {
    AppError::new(AppErrorCategory::InternalInvariant, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        run_multi_account_joint_backtest, BuiltinStrategySpec, JointBacktestAccount,
        MultiMarketBacktestMarket, PortfolioStrategySpec,
    };
    use qx_strategy::BuiltinStrategyKind;

    fn mutate_json_and_expect_conflict(
        path: &std::path::Path,
        outcome: &MultiAccountJointBacktestOutcome,
        context: &RunContext,
        mutate: impl FnOnce(&mut Value),
    ) {
        let original = std::fs::read_to_string(path).expect("read JSON artifact before mutation");
        let mut value: Value = serde_json::from_str(&original).expect("parse JSON artifact");
        mutate(&mut value);
        std::fs::write(
            path,
            serde_json::to_vec(&value).expect("encode altered JSON artifact"),
        )
        .expect("write altered JSON artifact");
        let error = read_multi_account_joint_report_v1(outcome, context).unwrap_err();
        std::fs::write(path, original).expect("restore original JSON artifact");
        assert_eq!(error.category(), AppErrorCategory::Conflict);
    }

    #[test]
    fn joint_report_replays_children_and_rejects_modified_joint_curve() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("target/qx-app-integration-tmp")
            .join(format!("joint-report-unit-{}", std::process::id()));
        if root.exists() {
            std::fs::remove_dir_all(&root).expect("remove stale fixture root");
        }
        std::fs::create_dir_all(&root).expect("create fixture root");
        let make_market = |instrument: &str, filename: &str| {
            let frame_path = root.join(filename);
            let mut ts = Vec::new();
            let mut open = Vec::new();
            let mut high = Vec::new();
            let mut low = Vec::new();
            let mut close = Vec::new();
            let mut volume = Vec::new();
            for index in 0..80_i128 {
                let price = (100 + (index % 7) * 2) * 1_000_000_000;
                ts.push(1_000 + index as u64 * 60_000);
                open.push(price);
                high.push(price + 1_000_000_000);
                low.push(price - 1_000_000_000);
                close.push(price);
                volume.push(100_000_000_000_i128);
            }
            std::fs::write(
                &frame_path,
                serde_json::json!({
                    "schema_version": 1,
                    "instrument": instrument,
                    "source": "joint-report-test-v1",
                    "ts": ts,
                    "open_raw": open,
                    "high_raw": high,
                    "low_raw": low,
                    "close_raw": close,
                    "volume_raw": volume,
                })
                .to_string(),
            )
            .unwrap();
            frame_path.to_string_lossy().into_owned()
        };
        let mut strategy = BuiltinStrategySpec::new(BuiltinStrategyKind::SmaCross, "joint-sma");
        strategy.fast_window = 2;
        strategy.slow_window = 3;
        let spec = MultiAccountJointBacktestSpec {
            schema_version: 1,
            run_id: "joint-report-test".into(),
            settlement_currency: "USDT".into(),
            seed: 7,
            output_dir: root.join("out").to_string_lossy().into_owned(),
            accounts: ["binance", "okx"]
                .into_iter()
                .map(|exchange| JointBacktestAccount {
                    exchange_id: exchange.into(),
                    account_id: "primary".into(),
                    initial_cash_raw: 100_000 * 1_000_000_000,
                    markets: vec![MultiMarketBacktestMarket {
                        instrument: if exchange == "okx" {
                            "BTC-USDT.OKX"
                        } else {
                            "BTCUSDT.BINANCE"
                        }
                        .into(),
                        bars_path: if exchange == "okx" {
                            make_market("BTC-USDT.OKX", "okx-bars.json")
                        } else {
                            make_market("BTCUSDT.BINANCE", "binance-bars.json")
                        },
                        dataset_manifest_path: None,
                        priority: 0,
                        market_spec: None,
                        asset_class: None,
                        fee_schedule: None,
                        order_policy: None,
                        margin_model: None,
                        funding_bps: 0,
                        funding_events: Vec::new(),
                        strategies: vec![PortfolioStrategySpec {
                            priority: 0,
                            reserved_capital_raw: 20_000 * 1_000_000_000,
                            reserved_capital_exact: None,
                            strategy: strategy.clone(),
                        }],
                    }],
                })
                .collect(),
        };
        let context = RunContext::new(CallerCapability::Research, "joint-report-test");
        let outcome = run_multi_account_joint_backtest(&spec, &context).unwrap();
        let report = read_multi_account_joint_report_v1(&outcome, &context).unwrap();
        assert!(report.verification.verified);
        assert_eq!(report.summary.accounts_total, 2);
        assert_eq!(report.summary.markets_total, 2);
        assert_eq!(report.equity.total, 80);
        assert_eq!(report.accounts.len(), 2);

        let requested_pages = MultiAccountJointReportReadOptionsV1 {
            joint_account_event_pages: BTreeMap::from([
                (
                    "binance".into(),
                    BTreeMap::from([(
                        "primary".into(),
                        MultiAccountJointEventPageRequestV1 {
                            offset: 0,
                            limit: 2,
                        },
                    )]),
                ),
                (
                    "okx".into(),
                    BTreeMap::from([(
                        "primary".into(),
                        MultiAccountJointEventPageRequestV1 {
                            offset: 2,
                            limit: 2,
                        },
                    )]),
                ),
            ]),
        };
        let paged =
            read_multi_account_joint_report_v1_with_options(&outcome, &context, &requested_pages)
                .unwrap();
        let binance_page = paged
            .accounts
            .iter()
            .find(|account| account.exchange_id == "binance")
            .unwrap();
        let okx_page = paged
            .accounts
            .iter()
            .find(|account| account.exchange_id == "okx")
            .unwrap();
        assert_eq!(binance_page.account_id, okx_page.account_id);
        assert_eq!(binance_page.event_rows_offset, 0);
        assert_eq!(binance_page.event_rows.len(), 2);
        assert_eq!(binance_page.event_rows_next_offset, Some(2));
        assert_eq!(okx_page.event_rows_offset, 2);
        assert_eq!(okx_page.event_rows.len(), 2);
        assert_eq!(okx_page.event_rows_previous_offset, Some(0));

        let invalid_page = MultiAccountJointReportReadOptionsV1 {
            joint_account_event_pages: BTreeMap::from([(
                "unknown".into(),
                BTreeMap::from([(
                    "primary".into(),
                    MultiAccountJointEventPageRequestV1 {
                        offset: 0,
                        limit: 1,
                    },
                )]),
            )]),
        };
        let error =
            read_multi_account_joint_report_v1_with_options(&outcome, &context, &invalid_page)
                .unwrap_err();
        assert_eq!(error.category(), AppErrorCategory::InvalidInput);

        mutate_json_and_expect_conflict(
            std::path::Path::new(&outcome.equity_path),
            &outcome,
            &context,
            |equity| equity[10][1] = serde_json::json!(0),
        );

        let child = &outcome.accounts[0].outcome;
        let child_artifact: Value = serde_json::from_str(
            &std::fs::read_to_string(&child.artifact_path).expect("read child artifact"),
        )
        .expect("parse child artifact");
        let child_equity_path = child_artifact["equity_path"]
            .as_str()
            .expect("child equity path");
        mutate_json_and_expect_conflict(
            std::path::Path::new(child_equity_path),
            &outcome,
            &context,
            |equity| equity[0][1] = serde_json::json!(0),
        );

        let child_fills_path = child_artifact["fills_path"]
            .as_str()
            .expect("child fills path");
        mutate_json_and_expect_conflict(
            std::path::Path::new(child_fills_path),
            &outcome,
            &context,
            |fills| {
                let rows = fills.as_array_mut().expect("child fills rows");
                if rows.is_empty() {
                    rows.push(Value::Null);
                } else {
                    rows[0] = Value::Null;
                }
            },
        );

        let parent_manifest_path = outcome
            .run_manifest_path
            .as_deref()
            .expect("parent RunManifest path");
        mutate_json_and_expect_conflict(
            std::path::Path::new(parent_manifest_path),
            &outcome,
            &context,
            |manifest| manifest["config_hash"] = serde_json::json!("tampered"),
        );

        let event_log_path = child_artifact["event_trace_path"]
            .as_str()
            .expect("child EventLog path");
        let original_event_log = std::fs::read(event_log_path).expect("read child EventLog");
        let first_line_end = original_event_log
            .iter()
            .position(|byte| *byte == b'\n')
            .expect("EventLog line ending");
        let mut first_event: Value = serde_json::from_slice(&original_event_log[..first_line_end])
            .expect("parse first EventLog row");
        first_event["ts"] = serde_json::json!(first_event["ts"].as_u64().unwrap() + 1);
        let mut changed_event_log = serde_json::to_vec(&first_event).expect("encode changed event");
        changed_event_log.push(b'\n');
        changed_event_log.extend_from_slice(&original_event_log[first_line_end + 1..]);
        std::fs::write(event_log_path, changed_event_log).expect("tamper child EventLog digest");
        let event_error = read_multi_account_joint_report_v1(&outcome, &context).unwrap_err();
        std::fs::write(event_log_path, original_event_log).expect("restore child EventLog");
        assert_eq!(event_error.category(), AppErrorCategory::Conflict);
        let _ = std::fs::remove_dir_all(root);
    }
}
