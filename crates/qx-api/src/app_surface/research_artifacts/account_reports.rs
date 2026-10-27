//! Independent multi-account report verification and projection.

use super::*;

/// Verify every successful account independently, then expose the batch as a bounded
/// interactive report. The batch's weighted summary is recomputed from the verified runs.
pub(super) fn build(
    outcome_value: &Value,
    paths: &AppPaths,
    requested_page: Option<EventPage>,
    requested_event_offsets: Option<&BTreeMap<String, u64>>,
    event_instrument: Option<&str>,
    account_strategy_diagnostics: Option<&Value>,
) -> Result<ResearchArtifactView, AppError> {
    let runs = outcome_value
        .get("runs")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid_report("多账户研究结果缺少 runs"))?;
    if !(2..=qx_app::MAX_MULTI_ACCOUNT_RUNS).contains(&runs.len()) {
        return Err(invalid_report(format!(
            "多账户研究结果数量必须在 2..={}",
            qx_app::MAX_MULTI_ACCOUNT_RUNS
        )));
    }
    let known_account_ids = runs
        .iter()
        .filter_map(|run| run.get("account_id").and_then(Value::as_str))
        .map(str::to_owned)
        .collect::<std::collections::BTreeSet<_>>();
    let mut account_diagnostic_by_id = BTreeMap::<String, &Value>::new();
    if let Some(entries) = account_strategy_diagnostics {
        let entries = entries
            .as_array()
            .ok_or_else(|| invalid_report("账户策略诊断必须是数组"))?;
        for entry in entries {
            let account_id = entry
                .get("account_id")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid_report("账户策略诊断缺少 account_id"))?;
            if !known_account_ids.contains(account_id) {
                return Err(invalid_report(format!(
                    "账户策略诊断引用未知账户 {account_id}"
                )));
            }
            let member = runs
                .iter()
                .find(|run| run.get("account_id").and_then(Value::as_str) == Some(account_id));
            if member
                .and_then(|run| run.get("outcome"))
                .is_none_or(Value::is_null)
            {
                return Err(invalid_report(format!(
                    "失败账户 {account_id} 不能附加策略诊断"
                )));
            }
            if account_diagnostic_by_id
                .insert(account_id.to_owned(), entry)
                .is_some()
            {
                return Err(invalid_report(format!("账户策略诊断重复引用 {account_id}")));
            }
        }
    }
    let matching_account_ids = if let Some(instrument) = event_instrument {
        let matching = runs
            .iter()
            .filter(|run| {
                run.get("outcome")
                    .and_then(|outcome| outcome.get("instrument"))
                    .and_then(Value::as_str)
                    == Some(instrument)
            })
            .filter_map(|run| run.get("account_id").and_then(Value::as_str))
            .map(str::to_owned)
            .collect::<std::collections::BTreeSet<_>>();
        if matching.is_empty() {
            return Err(invalid_report(format!(
                "多账户批次中不存在标的 {instrument}"
            )));
        }
        matching
    } else {
        known_account_ids.clone()
    };
    let page = requested_page.unwrap_or(EventPage {
        offset: 0,
        limit: MAX_EVENT_ROWS,
    });
    if requested_event_offsets.is_none() && page.offset != 0 {
        return Err(invalid_report(
            "多账户合并事件分页必须提供 account_event_offsets 游标",
        ));
    }
    if requested_event_offsets.is_some_and(|offsets| {
        offsets
            .keys()
            .any(|account_id| !known_account_ids.contains(account_id))
    }) {
        return Err(invalid_report("多账户事件分页游标包含未知 account_id"));
    }
    if requested_event_offsets.is_some_and(|offsets| {
        offsets
            .iter()
            .any(|(account_id, offset)| !matching_account_ids.contains(account_id) && *offset != 0)
    }) {
        return Err(invalid_report("被标的筛选排除的账户事件游标必须为 0"));
    }
    let requested_offset_sum = requested_event_offsets
        .into_iter()
        .flat_map(|offsets| {
            offsets
                .iter()
                .filter(|(account_id, _)| matching_account_ids.contains(*account_id))
                .map(|(_, value)| value)
        })
        .try_fold(0_u64, |total, value| total.checked_add(*value))
        .ok_or_else(|| invalid_report("多账户事件分页游标总数溢出"))?;
    if requested_event_offsets.is_some() && requested_offset_sum != page.offset {
        return Err(invalid_report(
            "多账户事件 offset 必须等于 account_event_offsets 游标总和",
        ));
    }
    let batch_id = outcome_value
        .get("batch_id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| invalid_report("多账户批次缺少 batch_id"))?;
    let artifact_root = std::fs::canonicalize(&paths.artifact_root)
        .map_err(|error| AppError::from_io("解析 API 研究产物根目录", &error))?;
    let artifact_reader = RootedArtifactReader {
        root: artifact_root,
    };
    let batch_report = qx_app::read_multi_account_report_v1_with_reader(
        outcome_value,
        &RunContext::new(CallerCapability::Research, batch_id),
        &artifact_reader,
    )?;
    let batch_id = batch_report.run_id.clone();
    let settlement_currency = batch_report.settlement_currency.clone();
    let spec_fingerprint = batch_report.spec_fingerprint.clone();
    let verified_accounts = batch_report
        .accounts
        .iter()
        .map(|account| (account.account_id.as_str(), account))
        .collect::<BTreeMap<_, _>>();

    let mut seen_accounts = std::collections::BTreeSet::new();
    let mut accounts = Vec::with_capacity(runs.len());
    let mut next_account_event_offsets = BTreeMap::new();
    let mut merged_event_rows = Vec::new();
    let mut merged_event_total = 0_u64;

    for run in runs {
        let account_id = run
            .get("account_id")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| invalid_report("多账户成员缺少 account_id"))?;
        if !seen_accounts.insert(account_id.to_owned()) {
            return Err(invalid_report("多账户研究结果含重复 account_id"));
        }
        let verified_account = verified_accounts
            .get(account_id)
            .copied()
            .ok_or_else(|| invalid_report(format!("共享报告缺少账户 {account_id}")))?;
        let Some(account_outcome) = run.get("outcome").filter(|value| !value.is_null()) else {
            if verified_account.status != qx_app::MultiAccountResearchStatusV1::Failed {
                return Err(invalid_report(format!(
                    "账户 {account_id} 的共享报告状态不匹配"
                )));
            }
            let event_offset = requested_event_offsets
                .and_then(|offsets| offsets.get(account_id))
                .copied()
                .unwrap_or(0);
            if event_offset != 0 {
                return Err(invalid_report(format!(
                    "失败账户 {account_id} 的事件游标必须为 0"
                )));
            }
            next_account_event_offsets.insert(account_id.to_owned(), 0);
            accounts.push(AccountResearchView {
                account_id: account_id.to_owned(),
                exchange_id: None,
                status: "failed",
                run_id: None,
                instrument: None,
                data_fingerprint: None,
                result_hash: None,
                return_bps: None,
                max_drawdown_bps: None,
                fills: None,
                equity_points: None,
                report: None,
            });
            continue;
        };
        let account_outcome_account = account_outcome.get("account_id").and_then(Value::as_str);
        if account_outcome_account != Some(account_id) {
            return Err(invalid_report(format!(
                "账户 {account_id} 的 outcome.account_id 不匹配"
            )));
        }
        if verified_account.status != qx_app::MultiAccountResearchStatusV1::Succeeded {
            return Err(invalid_report(format!(
                "账户 {account_id} 的共享报告状态不匹配"
            )));
        }
        let core_report = verified_account
            .report
            .clone()
            .ok_or_else(|| invalid_report(format!("账户 {account_id} 缺少共享核心报告")))?;
        let include_events = matching_account_ids.contains(account_id);
        let event_offset = if include_events {
            requested_event_offsets
                .and_then(|offsets| offsets.get(account_id))
                .copied()
                .unwrap_or(0)
        } else {
            0
        };
        let mut child_request = json!({
            "outcome": account_outcome,
            "event_page": {"offset": event_offset, "limit": page.limit}
        });
        if let Some(diagnostics) = account_diagnostic_by_id.get(account_id) {
            if let Some(strategy) = diagnostics.get("strategy_diagnostics") {
                child_request["strategy_diagnostics"] = (*strategy).clone();
            }
            if let Some(markets) = diagnostics.get("market_strategy_diagnostics") {
                child_request["market_strategy_diagnostics"] = (*markets).clone();
            }
        }
        let child_body = serde_json::to_string(&child_request)
            .map_err(|error| invalid_report(format!("账户 {account_id} 结果编码失败: {error}")))?;
        let child =
            super::build_with_preverified_core(&child_body, paths, Some(core_report.clone()))?;
        if !child.verification.verified {
            return Err(invalid_report(format!(
                "账户 {account_id} 运行产物复核未通过: {}",
                child.verification.mismatches.join("; ")
            )));
        }
        let verified_instrument = child.instrument.clone();
        if include_events
            && event_instrument.is_some_and(|instrument| instrument != verified_instrument)
        {
            return Err(invalid_report(format!(
                "账户 {account_id} 的复核标的与事件筛选标的不匹配"
            )));
        }
        if include_events
            && (event_offset > child.events.total || child.events.offset != event_offset)
        {
            return Err(invalid_report(format!(
                "账户 {account_id} 事件游标超出已复核事件范围"
            )));
        }
        if include_events {
            merged_event_total = merged_event_total
                .checked_add(child.events.total)
                .ok_or_else(|| invalid_report("多账户事件总数溢出"))?;
            next_account_event_offsets.insert(account_id.to_owned(), event_offset);
            for mut event in child.events.rows.iter().cloned() {
                let Some(event_object) = event.as_object_mut() else {
                    return Err(invalid_report(format!(
                        "账户 {account_id} 的事件行不是 JSON 对象"
                    )));
                };
                event_object.insert("account_id".into(), Value::String(account_id.to_owned()));
                event_object.insert(
                    "instrument".into(),
                    Value::String(verified_instrument.clone()),
                );
                merged_event_rows.push(event);
            }
        } else {
            next_account_event_offsets.insert(account_id.to_owned(), 0);
        }
        let return_bps = account_outcome
            .get("return_bps")
            .and_then(Value::as_i64)
            .and_then(|value| i32::try_from(value).ok())
            .ok_or_else(|| invalid_report(format!("账户 {account_id} return_bps 无效")))?;
        let fills = account_outcome
            .get("fills")
            .and_then(Value::as_u64)
            .ok_or_else(|| invalid_report(format!("账户 {account_id} fills 无效")))?;
        let report = serde_json::to_value(child)
            .map_err(|error| invalid_report(format!("账户 {account_id} 报告编码失败: {error}")))?;
        accounts.push(AccountResearchView {
            account_id: account_id.to_owned(),
            exchange_id: None,
            status: "succeeded",
            run_id: account_outcome
                .get("run_id")
                .and_then(Value::as_str)
                .map(str::to_owned),
            instrument: Some(verified_instrument),
            data_fingerprint: Some(core_report.verification.data_fingerprint.clone()),
            result_hash: Some(core_report.verification.result_hash.clone()),
            return_bps: Some(return_bps),
            max_drawdown_bps: account_outcome
                .get("max_drawdown_bps")
                .and_then(Value::as_u64)
                .and_then(|value| u32::try_from(value).ok()),
            fills: Some(fills),
            equity_points: account_outcome.get("equity_points").and_then(Value::as_u64),
            report: Some(report),
        });
    }

    merged_event_rows.sort_by(compare_merged_events);
    merged_event_rows.truncate(page.limit);
    for event in &merged_event_rows {
        let account_id = event
            .get("account_id")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid_report("合并事件缺少 account_id"))?;
        let offset = next_account_event_offsets
            .get_mut(account_id)
            .ok_or_else(|| invalid_report("合并事件引用未知账户"))?;
        *offset = offset
            .checked_add(1)
            .ok_or_else(|| invalid_report("账户事件游标溢出"))?;
    }

    let successful = batch_report.summary.accounts_succeeded;
    let failed = batch_report.summary.accounts_failed;
    let successful_initial_cash_raw = batch_report.summary.successful_initial_cash_raw;
    let successful_fills = batch_report.summary.successful_fills;
    let weighted_return_bps = batch_report.summary.weighted_return_bps;
    let event_trace_available = accounts.iter().any(|account| {
        account.status == "succeeded"
            && account.report.as_ref().is_some_and(|report| {
                report
                    .get("event_trace_available")
                    .and_then(Value::as_bool)
                    .unwrap_or_else(|| {
                        report
                            .pointer("/events/total")
                            .and_then(Value::as_u64)
                            .is_some()
                    })
            })
    });
    let event_offset = requested_offset_sum;
    let event_page_end = event_offset
        .checked_add(merged_event_rows.len() as u64)
        .ok_or_else(|| invalid_report("多账户事件分页结束位置溢出"))?;
    Ok(ResearchArtifactView {
        schema_version: 7,
        run_kind: Some("multi_account"),
        run_id: batch_id.clone(),
        instrument: format!("multi-account[{}]", settlement_currency),
        verification: VerificationResult {
            run_id: batch_id.clone(),
            result_hash: batch_report.verification.result_hash,
            data_fingerprint: batch_report.verification.data_fingerprint,
            verified: true,
            checks: vec![
                "多账户批次成员身份与汇总计数已核对".into(),
                "每个成功账户的输入、RunManifest、事件重放和副产物均已独立复核".into(),
                "成功本金、成交总数和加权收益已从账户结果复算".into(),
                "跨账户事件按时间、优先级、账户和序号稳定合并，所有账户事件页均已独立重放复核"
                    .into(),
            ],
            mismatches: Vec::new(),
        },
        summary: json!({
            "batch_id": batch_id,
            "settlement_currency": settlement_currency,
            "spec_fingerprint": spec_fingerprint,
            "accounts_total": runs.len(),
            "accounts_succeeded": successful,
            "accounts_failed": failed,
            "successful_initial_cash_raw": successful_initial_cash_raw.to_string(),
            "successful_fills": successful_fills,
            "weighted_return_bps": weighted_return_bps,
            "complete": failed == 0
        }),
        equity: empty_view(),
        fills: empty_view(),
        events: EventView {
            total: merged_event_total,
            offset: event_offset,
            truncated: event_page_end < merged_event_total,
            rows: merged_event_rows,
        },
        bars: empty_bars("unsupported_input_kind"),
        depth: None,
        event_trace_available: Some(event_trace_available),
        event_instrument: event_instrument.map(str::to_owned),
        strategy_diagnostics: None,
        market_strategy_diagnostics: None,
        markets: None,
        accounts: Some(accounts),
        account_event_offsets: Some(next_account_event_offsets),
    })
}
