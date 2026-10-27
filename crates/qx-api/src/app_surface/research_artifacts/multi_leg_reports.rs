//! Multi-leg backtest report verification and interactive projection.

use super::*;

pub(super) fn read_report(
    body: &str,
    paths: &AppPaths,
) -> Result<qx_app::MultiLegResearchReportV1, AppError> {
    let request: qx_app::MultiLegResearchReportReadRequestV1 = serde_json::from_str(body)
        .map_err(|error| invalid_report(format!("多腿研究报告读取请求无效: {error}")))?;
    let artifact_root = std::fs::canonicalize(&paths.artifact_root)
        .map_err(|error| AppError::from_io("解析 API 多腿研究产物根目录", &error))?;
    let data_root = std::fs::canonicalize(&paths.data_root)
        .map_err(|error| AppError::from_io("解析 API 多腿研究数据根目录", &error))?;
    let artifact_reader = RootedArtifactReader {
        root: artifact_root,
    };
    let data_reader = RootedArtifactReader { root: data_root };
    let context = RunContext::new(CallerCapability::Research, request.outcome.run_id.clone());
    qx_app::read_multi_leg_research_report_v1_with_readers(
        &request,
        &context,
        &artifact_reader,
        &data_reader,
    )
}

/// Verify a persisted two-leg run and project it into the interactive report surface. The
/// attribution artifact is treated as untrusted: source BarFrames, their optional quality
/// manifests, the shared event log, replay ledger, per-leg fills and equity metrics are checked
/// again before any report rows are returned.
pub(super) fn build(
    outcome_value: &Value,
    paths: &AppPaths,
    requested_page: Option<EventPage>,
    strategy_diagnostics: Option<&Value>,
    market_strategy_diagnostics: Option<&Value>,
) -> Result<ResearchArtifactView, AppError> {
    let requested: MultiLegBacktestOutcome = serde_json::from_value(outcome_value.clone())
        .map_err(|error| invalid_report(format!("多腿回测结果结构无效: {error}")))?;
    let artifact_root = std::fs::canonicalize(&paths.artifact_root)
        .map_err(|error| AppError::from_io("解析 API 研究产物根目录", &error))?;
    let data_root = std::fs::canonicalize(&paths.data_root)
        .map_err(|error| AppError::from_io("解析 API 数据根目录", &error))?;
    let artifact_reader = RootedArtifactReader {
        root: artifact_root,
    };
    let data_reader = RootedArtifactReader { root: data_root };
    let page = requested_page.unwrap_or(EventPage {
        offset: 0,
        limit: MAX_EVENT_ROWS,
    });
    let read_model = qx_app::read_multi_leg_report_read_model_v1_with_page_and_readers(
        &requested,
        &RunContext::new(CallerCapability::Research, requested.run_id.clone()),
        qx_app::MultiLegEventPageRequestV1 {
            offset: page.offset,
            limit: page.limit,
        },
        &artifact_reader,
        &data_reader,
    )?;
    let qx_app::MultiLegResearchReportReadModelV1 {
        frames,
        fingerprints,
        quality_fingerprints,
        data_fingerprint,
        facts: report_facts,
        report,
        ..
    } = read_model;
    let event_page = &report.events;
    let mut markets = [
        &requested.primary_instrument,
        &requested.reference_instrument,
    ]
    .into_iter()
    .map(|instrument| {
        let frame = &frames[instrument];
        MarketResearchView {
            instrument: instrument.clone(),
            data_fingerprint: fingerprints[instrument].clone(),
            fill_count: 0,
            quality_status: if quality_fingerprints.contains_key(instrument) {
                "verified"
            } else {
                "not_provided"
            },
            strategy_ids: Vec::new(),
            bars: barframe_view(frame),
            pnl_contribution: None,
            data_tier: None,
            depth: None,
        }
    })
    .collect::<Vec<_>>();
    let shared_result_hash = report.summary.event_hash.as_str();
    let order_markets = &report_facts.order_markets;
    let mut event_market_contexts = BTreeMap::<u64, Vec<Value>>::new();
    for row in &event_page.rows {
        let instruments = multi_leg_event_instruments(row, &order_markets, &frames);
        let Some(seq) = row.get("seq").and_then(Value::as_u64) else {
            continue;
        };
        let Some(ts) = row.get("ts").and_then(Value::as_u64) else {
            continue;
        };
        let Some(priority) = row
            .get("prio")
            .and_then(Value::as_u64)
            .and_then(|value| u8::try_from(value).ok())
        else {
            continue;
        };
        let mut contexts = Vec::new();
        for instrument in instruments {
            if let Some(frame) = frames.get(&instrument) {
                let (bar_context, status) =
                    match qx_app::multi_market_bar_context(frame, ts, priority) {
                        Some(context) => (context, "verified"),
                        None => (Value::Null, "before_first_visible_bar"),
                    };
                contexts.push(serde_json::json!({
                    "instrument": instrument,
                    "data_fingerprint": fingerprints.get(&instrument),
                    "bar_context": bar_context,
                    "bar_context_status": status
                }));
            }
        }
        if !contexts.is_empty() {
            event_market_contexts.insert(seq, contexts);
        }
    }
    let account_id = report.summary.account_id.as_str();
    let expected_ledger = report.summary.account_ledger_summary.clone();
    for market in &mut markets {
        market.fill_count = if market.instrument == report_facts.primary.instrument {
            report_facts.primary.fills
        } else if market.instrument == report_facts.reference.instrument {
            report_facts.reference.fills
        } else {
            return Err(invalid_report(format!(
                "多腿输入包含未声明标的: {}",
                market.instrument
            )));
        };
    }
    let effective_specs: Value = serde_json::from_str(&report.execution.market_specs_json)
        .map_err(|error| invalid_report(format!("多腿有效市场规格 JSON 无效: {error}")))?;
    let computed_return = report.summary.equity.return_bps;
    let computed_drawdown = report.summary.equity.max_drawdown_bps;

    let mut event_rows = event_page.rows.clone();
    for row in &mut event_rows {
        attach_research_fact_projection(row);
        let seq = row.get("seq").and_then(Value::as_u64);
        let contexts = seq.and_then(|seq| event_market_contexts.get(&seq));
        match contexts {
            Some(contexts) if contexts.len() == 1 => {
                row["instrument"] = contexts[0]["instrument"].clone();
                row["market_context"] = serde_json::json!({
                    "instrument": contexts[0]["instrument"],
                    "data_fingerprint": contexts[0]["data_fingerprint"]
                });
                row["bar_context"] = contexts[0]["bar_context"].clone();
                row["bar_context_status"] = contexts[0]["bar_context_status"].clone();
            }
            Some(contexts) => {
                row["market_contexts"] = Value::Array(contexts.clone());
                row["market_context"] = Value::Null;
                row["bar_context"] = Value::Null;
                row["bar_context_status"] = Value::String("multiple_legs".into());
            }
            None => {
                row["market_context"] = Value::Null;
                row["bar_context"] = Value::Null;
                row["bar_context_status"] = Value::String("input_unavailable".into());
            }
        }
    }
    let fills_total = report.summary.total_fills;
    let instrument = format!(
        "multi-leg[{},{}]",
        requested.primary_instrument, requested.reference_instrument
    );
    let strategy_id = report.strategy_id.as_str();
    let bound_strategy_diagnostics = strategy_diagnostics
        .map(|report| bind_multi_leg_diagnostic(report, strategy_id, &markets))
        .transpose()?;
    let bound_market_strategy_diagnostics = market_strategy_diagnostics
        .map(|reports| bind_multi_leg_diagnostics(reports, strategy_id, &markets))
        .transpose()?;
    let summary = serde_json::json!({
        "run_id": requested.run_id,
        "account_id": account_id,
        "markets_total": 2,
        "execution_assumptions": {
            "settlement_currency": report.execution.settlement_currency,
            "fill_model": report.execution.fill_model,
            "primary_maker_fee_bps": report.execution.primary_maker_fee_bps,
            "primary_taker_fee_bps": report.execution.primary_taker_fee_bps,
            "reference_maker_fee_bps": report.execution.reference_maker_fee_bps,
            "reference_taker_fee_bps": report.execution.reference_taker_fee_bps,
            "funding_bps": report.execution.funding_bps,
            "funding_events_count": report.execution.funding_events_count,
            "funding_events_sha256": report.execution.funding_events_sha256,
            "market_specs": effective_specs,
        },
        "successful_fills": fills_total,
        "primary_fills": requested.primary_fills,
        "reference_fills": requested.reference_fills,
        "event_count": report.summary.event_count,
        "account": expected_ledger,
        "metrics": {
            "return_bps": computed_return,
            "max_drawdown_bps": computed_drawdown,
            "final_equity_raw": report.summary.equity.final_equity_raw,
            "primary_return_bps": report.summary.primary.return_bps,
            "reference_return_bps": report.summary.reference.return_bps,
            "fees_raw": report.summary.costs.total_fees_raw,
            "funding_raw": report.summary.costs.total_funding_raw,
            "turnover_raw": report.summary.costs.total_turnover_raw,
            "cost_bps": report.summary.costs.cost_bps
        },
        "fills": fills_total,
        "result_hash": shared_result_hash,
        "data_fingerprint": report.data_fingerprint,
        "markets": markets.iter().map(|market| serde_json::json!({
            "instrument": market.instrument,
            "data_fingerprint": market.data_fingerprint,
            "fills": market.fill_count,
            "quality_status": market.quality_status,
            "bar_count": market.bars.total
        })).collect::<Vec<_>>()
    });
    let report_run_id = requested.run_id.clone();
    Ok(ResearchArtifactView {
        schema_version: 10,
        run_kind: Some("multi_leg"),
        run_id: report_run_id.clone(),
        instrument,
        verification: VerificationResult {
            run_id: report_run_id,
            result_hash: shared_result_hash.to_owned(),
            data_fingerprint: data_fingerprint.clone(),
            verified: true,
            checks: vec![
                "多腿 outcome、spec 与归因产物身份一致".into(),
                "两条 BarFrame 和可选数据质量清单已复算".into(),
                "共享 EventLog 已完整重放，digest 与账簿明细一致".into(),
                "逐腿成交数与成交指纹已从事件流复算".into(),
                "共享账户账簿及组合权益、收益和回撤已复核".into(),
            ],
            mismatches: Vec::new(),
        },
        summary,
        equity: CsvView {
            total: report.equity.total,
            truncated: report.equity.truncated,
            rows: report.equity.rows,
        },
        fills: CsvView {
            total: fills_total,
            truncated: report.fills.truncated,
            rows: report.fills.rows,
        },
        events: EventView {
            total: event_page.total,
            offset: event_page.offset,
            truncated: event_page.truncated,
            rows: event_rows,
        },
        bars: empty_bars("unsupported_input_kind"),
        depth: None,
        event_trace_available: Some(true),
        event_instrument: None,
        strategy_diagnostics: bound_strategy_diagnostics,
        market_strategy_diagnostics: bound_market_strategy_diagnostics,
        markets: Some(markets),
        accounts: None,
        account_event_offsets: None,
    })
}

fn multi_leg_event_instruments(
    event: &Value,
    order_markets: &BTreeMap<u64, String>,
    frames: &BTreeMap<String, qx_datastruct::BarFrame>,
) -> Vec<String> {
    let intents = event
        .pointer("/kind/StrategyDecision/decision/intents")
        .and_then(Value::as_array);
    let mut instruments = intents
        .into_iter()
        .flatten()
        .filter_map(|intent| intent.get("instrument").and_then(canonical_instrument))
        .filter(|instrument| frames.contains_key(instrument))
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    if instruments.is_empty() {
        if let Some(instrument) = event_market_instrument(event, &BTreeMap::new(), order_markets)
            .filter(|instrument| frames.contains_key(instrument))
        {
            instruments.push(instrument);
        }
    }
    instruments
}

fn bind_multi_leg_diagnostic(
    report: &Value,
    strategy_id: &str,
    markets: &[MarketResearchView],
) -> Result<BoundStrategyDiagnostics, AppError> {
    let instrument = report
        .get("instrument")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid_report("多腿策略诊断缺少 instrument"))?;
    let market = markets
        .iter()
        .find(|market| market.instrument == instrument)
        .ok_or_else(|| invalid_report(format!("多腿策略诊断标的不属于本次运行: {instrument}")))?;
    if report.get("schema_version").and_then(Value::as_u64) != Some(1)
        || report.get("kind").and_then(Value::as_str) != Some("qianxing-strategy-bias-diagnostics")
        || report
            .get("input_bars")
            .and_then(Value::as_u64)
            .is_none_or(|count| count == 0)
        || report.get("as_of").and_then(Value::as_u64).is_none()
        || report.get("lookahead").and_then(Value::as_object).is_none()
        || report.get("startup").and_then(Value::as_object).is_none()
        || report.get("strategy_id").and_then(Value::as_str) != Some(strategy_id)
        || report.get("data_fingerprint").and_then(Value::as_str)
            != Some(market.data_fingerprint.as_str())
    {
        return Err(invalid_report("多腿策略诊断与策略身份或腿级输入指纹不匹配"));
    }
    Ok(BoundStrategyDiagnostics {
        identity_bound: true,
        computation: "client_computed_not_reexecuted_by_api",
        report: report.clone(),
    })
}

fn bind_multi_leg_diagnostics(
    reports: &Value,
    strategy_id: &str,
    markets: &[MarketResearchView],
) -> Result<Vec<BoundStrategyDiagnostics>, AppError> {
    let reports = reports
        .as_array()
        .ok_or_else(|| invalid_report("多腿 market_strategy_diagnostics 必须是数组"))?;
    if reports.is_empty() || reports.len() > markets.len() {
        return Err(invalid_report("多腿策略诊断数量必须为 1..=2"));
    }
    let mut seen = std::collections::BTreeSet::new();
    reports
        .iter()
        .map(|report| {
            let instrument = report
                .get("instrument")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid_report("多腿策略诊断缺少 instrument"))?;
            if !seen.insert(instrument.to_owned()) {
                return Err(invalid_report("多腿策略诊断重复绑定同一标的"));
            }
            bind_multi_leg_diagnostic(report, strategy_id, markets)
        })
        .collect()
}
