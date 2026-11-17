//! Verified multi-account Depth report projection and output publication.

use super::*;

pub(super) fn run_depth_report(
    path: &Path,
    value: &Value,
    as_json: bool,
    html: bool,
    evidence: bool,
    output: Option<&Path>,
    against: &[PathBuf],
    xlsx: Option<&Path>,
    has_benchmark: bool,
    strategy_diagnostics: Option<&Path>,
) -> Result<bool, String> {
    if !against.is_empty() || has_benchmark {
        return Err(
            "多账户深度报告暂不支持 --against 或基准参数；请对账户运行分别生成对照报告".into(),
        );
    }
    if output.is_some() && !html {
        return Err("--output 只能与 --html 一起使用".into());
    }
    if output.is_some_and(|html_path| {
        xlsx.is_some_and(|xlsx_path| {
            crate::report_output_paths::same_output_target(html_path, xlsx_path)
        })
    }) {
        return Err("HTML 与 XLSX 报告必须使用不同输出路径".into());
    }
    let result: qx_app::MultiAccountDepthBacktestResult = serde_json::from_value(value.clone())
        .map_err(|error| format!("多账户深度回测结果契约无效: {error}"))?;
    let verified_batch = qx_app::read_multi_account_report_v1(
        value,
        &qx_app::RunContext::new(
            qx_app::CallerCapability::Research,
            format!("report-{}", result.batch_id),
        ),
    )
    .map_err(|error| format!("多账户深度共享报告复核失败: {error}"))?;
    let verified_accounts = verified_batch
        .accounts
        .iter()
        .map(|account| (account.account_id.as_str(), account))
        .collect::<BTreeMap<_, _>>();
    let diagnostics = strategy_diagnostics
        .map(read_multi_account_diagnostics)
        .transpose()?;
    if let Some(diagnostics) = &diagnostics {
        let expected = result
            .runs
            .iter()
            .filter(|run| run.outcome.is_some())
            .map(|run| run.account_id.as_str())
            .collect::<BTreeSet<_>>();
        let supplied = diagnostics
            .reports
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        if expected != supplied {
            return Err(
                "多账户深度策略诊断必须为每个成功账户各提供且仅提供一份，且不得绑定失败账户".into(),
            );
        }
    }
    if let Some(xlsx_path) = xlsx {
        crate::report_xlsx::validate_xlsx_path(xlsx_path)?;
        validate_depth_report_output_target(
            xlsx_path,
            path,
            strategy_diagnostics,
            &result,
            evidence,
        )?;
    }
    let html_target = if html {
        let target = output.map(Path::to_path_buf).unwrap_or_else(|| {
            let name = path
                .file_stem()
                .and_then(|name| name.to_str())
                .unwrap_or("multi-account-depth");
            path.with_file_name(format!("{name}.depth.report.html"))
        });
        if !target
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                extension.eq_ignore_ascii_case("html") || extension.eq_ignore_ascii_case("htm")
            })
        {
            return Err(format!(
                "HTML 报告输出路径必须以 .html 或 .htm 结尾: {}",
                target.display()
            ));
        }
        validate_depth_report_output_target(
            &target,
            path,
            strategy_diagnostics,
            &result,
            evidence,
        )?;
        Some(target)
    } else {
        None
    };

    let accounts_total = result.runs.len();
    let fill_row_limit_max =
        MAX_REPORT_FILL_ROWS_PER_ACCOUNT.min(MAX_REPORT_FILL_ROWS_TOTAL.div_ceil(accounts_total));
    let event_row_limit_max =
        MAX_REPORT_EVENT_ROWS_PER_ACCOUNT.min(MAX_REPORT_EVENT_ROWS_TOTAL.div_ceil(accounts_total));
    let mut evidence_outputs = Vec::new();
    let mut accounts = Vec::with_capacity(accounts_total);
    for (account_index, run) in result.runs.iter().enumerate() {
        let Some(outcome) = run.outcome.as_ref() else {
            accounts.push(json!({
                "account_id": &run.account_id,
                "status": "failed",
                "error": &run.error_json,
                "run_id": null,
                "instrument": null,
                "tier": null,
                "result_hash": null,
                "data_fingerprint": null,
                "return_bps": null,
                "max_drawdown_bps": null,
                "fills": null,
                "equity_points": null,
                "snapshots": null,
                "fill_rows": [],
                "fill_rows_total": 0,
                "fill_rows_truncated": false,
                "event_rows": [],
                "event_rows_total": 0,
                "event_rows_truncated": false,
                "summary_path": null,
            }));
            continue;
        };
        let fill_row_limit = batch_row_budget(
            MAX_REPORT_FILL_ROWS_TOTAL,
            accounts_total,
            account_index,
            MAX_REPORT_FILL_ROWS_PER_ACCOUNT,
        );
        let event_row_limit = batch_row_budget(
            MAX_REPORT_EVENT_ROWS_TOTAL,
            accounts_total,
            account_index,
            MAX_REPORT_EVENT_ROWS_PER_ACCOUNT,
        );
        let verified_account = verified_accounts
            .get(run.account_id.as_str())
            .copied()
            .ok_or_else(|| format!("多账户深度共享报告缺少账户 {}", run.account_id))?;
        if verified_account.status != qx_app::MultiAccountResearchStatusV1::Succeeded {
            return Err(format!("账户 {} 的共享报告状态不匹配", run.account_id));
        }
        let verified_report = verified_account
            .report
            .as_ref()
            .ok_or_else(|| format!("账户 {} 缺少共享核心报告", run.account_id))?;
        let summary = verified_report.summary.clone();
        let verification = &verified_report.verification;
        if !verification.verified
            || verified_report.run_id != outcome.run_id
            || verified_report.instrument != outcome.instrument
            || verification.result_hash != outcome.result_hash
            || verification.data_fingerprint != outcome.data_fingerprint
            || outcome.account_id.as_deref() != Some(run.account_id.as_str())
        {
            return Err(format!("账户 {} 的深度报告身份不一致", run.account_id));
        }
        let summary_path = PathBuf::from(&outcome.artifacts.summary);
        let declared_input = qx_app::artifact_store::recompute_declared_backtest_input(&summary)?;
        if (evidence || diagnostics.is_some()) && declared_input.is_none() {
            return Err(format!(
                "账户 {} 的 summary 未声明可复核输入，不能绑定诊断或生成 EvidenceBundle",
                run.account_id
            ));
        }
        let bound_diagnostics = diagnostics
            .as_ref()
            .map(|attachment| {
                let diagnostic = attachment
                    .reports
                    .get(&run.account_id)
                    .ok_or_else(|| format!("账户 {} 缺少策略诊断", run.account_id))?;
                let strategy_id = summary
                    .pointer("/strategy/strategy_id")
                    .or_else(|| summary.pointer("/strategy_id"))
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        format!("账户 {} 的 summary 缺少 strategy_id", run.account_id)
                    })?;
                crate::report_command::validate_strategy_diagnostics_identity(
                    diagnostic,
                    strategy_id,
                    &outcome.instrument,
                    &declared_input
                        .as_ref()
                        .ok_or("策略诊断缺少重算后的数据输入身份")?
                        .fingerprint,
                )?;
                Ok::<_, String>(json!({
                    "identity_bound": true,
                    "account_id": &run.account_id,
                    "report": diagnostic,
                }))
            })
            .transpose()?;
        let account_evidence_path = if let Some(declared_input) =
            declared_input.as_ref().filter(|_| evidence)
        {
            let diagnostic_report = diagnostics
                .as_ref()
                .and_then(|attachment| attachment.reports.get(&run.account_id));
            let diagnostic_snapshot = diagnostic_report
                .map(|report| {
                    write_account_diagnostic_snapshot(&summary_path, &run.account_id, report)
                })
                .transpose()?;
            match crate::run_evidence::prepare_run_evidence(
                &summary_path,
                &summary,
                None,
                diagnostic_snapshot
                    .as_ref()
                    .zip(diagnostic_report)
                    .map(|((snapshot, _), report)| (snapshot.as_path(), report)),
            ) {
                Ok((evidence_path, payload)) => {
                    evidence_outputs.push((evidence_path.clone(), payload));
                    if let Some((snapshot_path, _)) = diagnostic_snapshot {
                        let snapshot_bytes = std::fs::read(&snapshot_path).map_err(|error| {
                            format!(
                                "读取账户策略诊断快照失败 {}: {error}",
                                snapshot_path.display()
                            )
                        })?;
                        evidence_outputs.push((snapshot_path, snapshot_bytes));
                    }
                    Some(evidence_path)
                }
                Err(error) => {
                    if let Some((snapshot, created)) = diagnostic_snapshot {
                        if created {
                            let _ = std::fs::remove_file(snapshot);
                        }
                    }
                    return Err(format!(
                        "账户 {} 生成运行 EvidenceBundle 失败（输入 {}）: {error}",
                        run.account_id, declared_input.path
                    ));
                }
            }
        } else {
            None
        };
        let (mut fill_rows, fill_rows_total) =
            read_depth_account_fills(verified_report, outcome.fills, fill_row_limit)?;
        let fill_rows_truncated =
            verified_report.fills.truncated || fill_rows_total > fill_rows.len() as u64;
        let (event_rows, event_trace_path, event_rows_total) = if let Some(trace_path) =
            &outcome.artifacts.event_trace
        {
            let event_page = qx_app::read_event_report_v1_with_reader(
                trace_path,
                &summary,
                &outcome.artifacts.run_manifest,
                None,
                &qx_app::artifact_store::FileArtifactReader,
            )
            .map_err(|error| format!("账户 {} 深度 EventLog 复核失败: {error}", run.account_id))?;
            let mut order_events = BTreeMap::new();
            let rows = event_page
                    .rows
                    .iter()
                    .skip(event_page.rows.len().saturating_sub(
                        event_row_limit.min(qx_app::MAX_EVENT_REPORT_PAGE_ROWS),
                    ))
                    .map(|event| {
                        let kind_value = event.get("kind").cloned().unwrap_or(Value::Null);
                        let (kind, order_id) = event
                            .get("kind")
                            .and_then(Value::as_object)
                            .and_then(|tagged| tagged.iter().next())
                            .map(|(kind, payload)| {
                                let order = payload
                                    .pointer("/fill/order_id")
                                    .or_else(|| payload.pointer("/order/client_order_id"))
                                    .or_else(|| payload.get("client_order_id"));
                                let order_id = order.map(|value| {
                                    value
                                        .as_str()
                                        .map(str::to_owned)
                                        .unwrap_or_else(|| value.to_string())
                                });
                                (kind.clone(), order_id)
                            })
                            .unwrap_or_else(|| ("unknown".into(), None));
                        if let (Some(order_id), Some(seq)) =
                            (order_id.as_ref(), event.get("seq").and_then(Value::as_u64))
                        {
                            order_events.entry(order_id.clone()).or_insert(seq);
                        }
                        json!({
                            "event_id": format!("account-{}-event-{}", run.account_id, event["seq"]),
                            "account_id": &run.account_id,
                            "seq": event["seq"],
                            "timestamp_ms": event["ts"],
                            "priority": event["prio"],
                            "source_seq": event["source_seq"],
                            "kind": kind,
                            "order_id": order_id,
                            "payload": serde_json::to_string(&kind_value).unwrap_or_else(|_| "{}".into()),
                        })
                    })
                    .collect::<Vec<_>>();
            for fill in &mut fill_rows {
                if let Some(seq) = fill
                    .get("order_id")
                    .and_then(Value::as_str)
                    .and_then(|order_id| order_events.get(order_id))
                {
                    fill["event_seq"] = json!(seq);
                    fill["event_id"] = json!(format!("account-{}-event-{}", run.account_id, seq));
                }
            }
            (rows, Some(PathBuf::from(trace_path)), event_page.total)
        } else {
            (Vec::new(), None, 0)
        };
        let event_rows_truncated =
            event_trace_path.is_some() && event_rows_total > event_rows.len() as u64;
        let equity_values =
            crate::report_html::read_equity_csv(Path::new(&outcome.artifacts.equity))?;
        let equity_series = downsample_equity(&equity_values, 2_000)
            .into_iter()
            .map(|(timestamp_ms, equity_raw)| {
                json!({"timestamp_ms": timestamp_ms, "equity_raw": equity_raw.to_string()})
            })
            .collect::<Vec<_>>();
        let equity_series_truncated =
            verified_report.equity.truncated || outcome.equity_points > equity_series.len() as u64;
        accounts.push(json!({
            "account_id": &run.account_id,
            "status": "succeeded",
            "run_id": &outcome.run_id,
            "instrument": &outcome.instrument,
            "tier": &outcome.tier,
            "snapshots": outcome.snapshots,
            "strategy_id": summary.pointer("/strategy/strategy_id").or_else(|| summary.pointer("/strategy_id")),
            "result_hash": &outcome.result_hash,
            "data_fingerprint": declared_input.as_ref().map(|input| input.fingerprint.as_str()).unwrap_or(&verification.data_fingerprint),
            "run_data_fingerprint": &verification.data_fingerprint,
            "strategy_bias_diagnostics": bound_diagnostics,
            "return_bps": outcome.return_bps,
            "max_drawdown_bps": outcome.max_drawdown_bps,
            "fills": outcome.fills,
            "fill_rows_total": fill_rows_total,
            "fill_rows": fill_rows,
            "fill_rows_truncated": fill_rows_truncated,
            "event_rows": event_rows,
            "event_rows_total": event_rows_total,
            "event_rows_truncated": event_rows_truncated,
            "event_trace_path": event_trace_path,
            "equity_points": outcome.equity_points,
            "equity_series": equity_series,
            "equity_series_truncated": equity_series_truncated,
            "summary_path": &summary_path,
            "run_evidence_path": account_evidence_path,
            "equity_path": &outcome.artifacts.equity,
            "fills_path": &outcome.artifacts.fills,
        }));
    }

    let report = json!({
        "schema_version": 1,
        "report_kind": "multi_account_depth_backtest",
        "source_path": path,
        "batch_id": &verified_batch.run_id,
        "settlement_currency": &verified_batch.settlement_currency,
        "spec_fingerprint": &verified_batch.spec_fingerprint,
        "summary": &verified_batch.summary,
        "account_semantics": "accounts_are_independent; cash_positions_risk_and_returns_are_not_netted",
        "verification_scope": "shared_qx_app_report_verified_parent_child_artifacts_and_replayed_depth_event_logs",
        "detail_limits": {
            "fill_rows_per_account_max": fill_row_limit_max,
            "fill_rows_batch_max": MAX_REPORT_FILL_ROWS_TOTAL,
            "event_rows_per_account_max": event_row_limit_max.min(qx_app::MAX_EVENT_REPORT_PAGE_ROWS),
            "event_rows_batch_max": MAX_REPORT_EVENT_ROWS_TOTAL.min(qx_app::MAX_EVENT_REPORT_PAGE_ROWS * accounts_total),
            "sampling": "verified_qx_app_rows_with_cli_tail_limits_and_equity_downsampling",
        },
        "accounts": accounts,
    });
    if let Some(xlsx_path) = xlsx {
        crate::report_xlsx::validate_xlsx_path(xlsx_path)?;
        validate_depth_report_output_target(
            xlsx_path,
            path,
            strategy_diagnostics,
            &result,
            evidence,
        )?;
    }
    let mut output_bundle = crate::report_publish::ReportOutputBundle::default();
    output_bundle.extend(evidence_outputs);
    let html_path = if let Some(target) = html_target {
        let mut rendered = render_html(&report)?
            .replace("牵星多账户回测研究报告", "牵星多账户深度回测研究报告")
            .replace(
                "<h1>多账户回测研究报告</h1>",
                "<h1>多账户深度回测研究报告</h1>",
            );
        let depth_rows = report["accounts"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|account| {
                format!(
                    "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
                    escape_html(account["account_id"].as_str().unwrap_or("")),
                    escape_html(account["instrument"].as_str().unwrap_or("")),
                    escape_html(account["tier"].as_str().unwrap_or("")),
                    escape_html(
                        account["snapshots"]
                            .as_u64()
                            .map(|value| value.to_string())
                            .as_deref()
                            .unwrap_or("absent")
                    ),
                )
            })
            .collect::<String>();
        rendered = rendered.replace(
            "<section class=\"cards\">",
            &format!("<section><h2>深度输入</h2><div class=\"scroll\"><table><thead><tr><th>账户</th><th>标的</th><th>档位</th><th>快照</th></tr></thead><tbody>{depth_rows}</tbody></table></div></section><section class=\"cards\">"),
        );
        output_bundle.add_bytes(&target, rendered.into_bytes());
        Some(target)
    } else {
        None
    };
    let xlsx_path = if let Some(target) = xlsx {
        let staged = output_bundle.stage_path(target)?;
        crate::report_xlsx::write_multi_account_report_xlsx(&report, &staged)?;
        output_bundle.add_staged(target, &staged)?;
        Some(target.to_path_buf())
    } else {
        None
    };
    output_bundle.commit()?;
    if as_json {
        let mut value = json!({"schema_version":1,"report":report});
        if let Some(path) = html_path {
            value["generated_artifacts"]["html"] = json!(path);
        }
        if let Some(path) = xlsx_path {
            value["generated_artifacts"]["xlsx"] = json!(path);
        }
        println!(
            "{}",
            serde_json::to_string_pretty(&value).map_err(|error| error.to_string())?
        );
    } else if let Some(html_path) = html_path {
        println!("[Report] html={}", html_path.display());
        if let Some(xlsx_path) = xlsx_path {
            println!("[Report] xlsx={}", xlsx_path.display());
        }
        println!(
            "[Report] batch_id={} accounts={} complete={}",
            result.batch_id,
            result.runs.len(),
            result.summary.complete
        );
    } else {
        if let Some(xlsx_path) = xlsx_path {
            println!("[Report] xlsx={}", xlsx_path.display());
        }
        println!(
            "[Report] batch_id={} accounts={} complete={}",
            result.batch_id,
            result.runs.len(),
            result.summary.complete
        );
    }
    Ok(true)
}
