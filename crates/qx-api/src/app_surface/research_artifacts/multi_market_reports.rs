//! Verified projection for shared-account multi-market Bar and depth reports.

use super::*;

/// Verify and project the shared-account multi-market artifact into the same bounded report
/// surface as single-market Bar/depth runs. The producer's `replay_verified` marker is ignored:
/// inputs, sidecars, manifest, ledger and fills are independently checked here.
pub(super) fn build(
    outcome_value: &Value,
    paths: &AppPaths,
    requested_page: Option<EventPage>,
    strategy_diagnostics: Option<&Value>,
    market_strategy_diagnostics: Option<&Value>,
) -> Result<ResearchArtifactView, AppError> {
    let requested: MultiMarketBacktestOutcome = serde_json::from_value(outcome_value.clone())
        .map_err(|error| invalid_report(format!("多市场回测结果结构无效: {error}")))?;
    let artifact_root = std::fs::canonicalize(&paths.artifact_root)
        .map_err(|error| AppError::from_io("解析 API 研究产物根目录", &error))?;
    let data_root = std::fs::canonicalize(&paths.data_root)
        .map_err(|error| AppError::from_io("解析 API 数据根目录", &error))?;
    let artifact_reader = RootedArtifactReader {
        root: artifact_root,
    };
    let data_reader = RootedArtifactReader { root: data_root };
    let context = RunContext::new(CallerCapability::Research, requested.run_id.clone());
    let page = requested_page.unwrap_or(EventPage {
        offset: 0,
        limit: MAX_EVENT_ROWS,
    });
    let report = qx_app::read_multi_market_research_report_v1_with_readers(
        &qx_app::MultiMarketResearchReportReadRequestV1 {
            schema_version: qx_app::MULTI_MARKET_RESEARCH_REPORT_SCHEMA_VERSION_V1,
            outcome: requested,
            page: Some(qx_app::MultiMarketEventReportPageRequestV1 {
                offset: page.offset,
                limit: page.limit,
            }),
        },
        &context,
        &artifact_reader,
        &data_reader,
    )?;
    if report.markets.len() > MAX_MULTI_MARKETS {
        return Err(invalid_report("多市场报告超出 API 市场数量上限"));
    }
    if strategy_diagnostics.is_some() && market_strategy_diagnostics.is_some() {
        return Err(AppError::new(
            AppErrorCategory::InvalidInput,
            "strategy_diagnostics 与 market_strategy_diagnostics 不能同时提供",
        ));
    }
    let settlement_currency = report
        .summary
        .pointer("/execution_assumptions/settlement_currency")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid_report("多市场报告缺少结算币种"))?
        .to_owned();
    let markets = report
        .markets
        .iter()
        .map(|market| {
            let bar_rows = market
                .bars
                .rows
                .iter()
                .cloned()
                .map(serde_json::from_value::<qx_app::artifact_store::VerifiedBarFramePoint>)
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| invalid_report(format!("BarFrame 预览结构无效: {error}")))?;
            Ok(MarketResearchView {
                instrument: market.instrument.clone(),
                data_fingerprint: market.data_fingerprint.clone(),
                fill_count: market.fill_count,
                quality_status: if market.quality_status == "verified" {
                    "verified"
                } else {
                    "not_provided"
                },
                strategy_ids: market.strategy_ids.clone(),
                bars: BarView {
                    source_status: "verified",
                    total: market.bars.total,
                    truncated: market.bars.truncated,
                    rows: bar_rows,
                },
                pnl_contribution: market.pnl_contribution.as_ref().map(|contribution| {
                    PnlContributionView {
                        currency: settlement_currency.clone(),
                        total: contribution.total,
                        truncated: contribution.truncated,
                        rows: contribution.rows.clone(),
                    }
                }),
                data_tier: None,
                depth: None,
            })
        })
        .collect::<Result<Vec<_>, AppError>>()?;
    let mut event_rows = report.events.rows.clone();
    for row in &mut event_rows {
        attach_research_fact_projection(row);
    }
    let bound_strategy_diagnostics = strategy_diagnostics
        .map(|diagnostic| bind_multi_market_diagnostic(diagnostic, &markets))
        .transpose()?;
    let bound_market_strategy_diagnostics = market_strategy_diagnostics
        .map(|diagnostics| bind_multi_market_diagnostics(diagnostics, &markets))
        .transpose()?;
    let schema_version = if markets
        .iter()
        .any(|market| market.pnl_contribution.is_some())
    {
        12
    } else if bound_market_strategy_diagnostics.is_some() {
        9
    } else if bound_strategy_diagnostics.is_some() {
        8
    } else {
        3
    };
    Ok(ResearchArtifactView {
        schema_version,
        run_kind: Some("multi_market"),
        run_id: report.run_id,
        instrument: report.instrument,
        verification: report.verification,
        summary: report.summary,
        equity: CsvView {
            total: report.equity.total,
            truncated: report.equity.truncated,
            rows: report.equity.rows,
        },
        fills: CsvView {
            total: report.fills.total,
            truncated: report.fills.truncated,
            rows: report.fills.rows,
        },
        events: EventView {
            total: report.events.total,
            offset: report.events.offset,
            truncated: report.events.truncated,
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
/// Independently verify and expose shared-account multi-market L1/L2 backtests. Depth runs use
/// a different persisted artifact from Bar portfolios, so they must be dispatched and replayed
/// explicitly instead of being coerced into either neighboring result shape.
pub(super) fn build_depth(
    outcome_value: &Value,
    paths: &AppPaths,
    requested_page: Option<EventPage>,
    market_strategy_diagnostics: Option<&Value>,
) -> Result<ResearchArtifactView, AppError> {
    let requested: MultiMarketDepthBacktestOutcome = serde_json::from_value(outcome_value.clone())
        .map_err(|error| invalid_report(format!("多市场深度结果结构无效: {error}")))?;
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
    let report = qx_app::read_multi_market_depth_research_report_v1_with_readers(
        &qx_app::MultiMarketDepthResearchReportReadRequestV1 {
            schema_version: qx_app::MULTI_MARKET_DEPTH_RESEARCH_REPORT_SCHEMA_VERSION_V1,
            outcome: requested.clone(),
            page: Some(qx_app::MultiMarketEventReportPageRequestV1 {
                offset: page.offset,
                limit: page.limit,
            }),
        },
        &RunContext::new(CallerCapability::Research, requested.run_id.clone()),
        &artifact_reader,
        &data_reader,
    )?;
    if report.markets.len() > MAX_MULTI_MARKETS {
        return Err(invalid_report("多市场深度报告超出 API 市场数量上限"));
    }
    let bound_market_strategy_diagnostics = market_strategy_diagnostics
        .map(|reports| {
            bind_multi_market_depth_diagnostics(
                reports,
                &report.markets,
                &report.custom_strategy_identities,
            )
        })
        .transpose()?;
    let markets = report
        .markets
        .iter()
        .map(|market| MarketResearchView {
            instrument: market.instrument.clone(),
            data_fingerprint: market.data_fingerprint.clone(),
            fill_count: market.fill_count,
            quality_status: if market.quality_status == "verified" {
                "verified"
            } else {
                "not_provided"
            },
            strategy_ids: market.strategy_ids.clone(),
            bars: empty_bars("not_applicable_depth"),
            pnl_contribution: None,
            data_tier: Some(market.data_tier.clone()),
            depth: Some(DepthView {
                source_status: "verified",
                total: market.depth.total,
                truncated: market.depth.truncated,
                rows: market.depth.rows.clone(),
            }),
        })
        .collect();
    Ok(ResearchArtifactView {
        schema_version: if bound_market_strategy_diagnostics.is_some() {
            11
        } else {
            5
        },
        run_kind: Some("multi_market_depth"),
        run_id: report.run_id,
        instrument: report.instrument,
        verification: report.verification,
        summary: report.summary,
        equity: CsvView {
            total: report.equity.total,
            truncated: report.equity.truncated,
            rows: report.equity.rows,
        },
        fills: CsvView {
            total: report.fills.total,
            truncated: report.fills.truncated,
            rows: report.fills.rows,
        },
        events: EventView {
            total: report.events.total,
            offset: report.events.offset,
            truncated: report.events.truncated,
            rows: report.events.rows,
        },
        bars: empty_bars("not_applicable_depth"),
        depth: None,
        event_trace_available: Some(true),
        event_instrument: None,
        strategy_diagnostics: None,
        market_strategy_diagnostics: bound_market_strategy_diagnostics,
        markets: Some(markets),
        accounts: None,
        account_event_offsets: None,
    })
}
fn bind_multi_market_diagnostic(
    report: &Value,
    markets: &[MarketResearchView],
) -> Result<BoundStrategyDiagnostics, AppError> {
    let strategy_id = report
        .get("strategy_id")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid_report("多市场策略诊断缺少 strategy_id"))?;
    let instrument = report
        .get("instrument")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid_report("多市场策略诊断缺少 instrument"))?;
    let market_view = markets
        .iter()
        .find(|market| market.instrument == instrument)
        .ok_or_else(|| invalid_report("策略诊断标的不存在于已复核多市场输入"))?;
    if !market_view.strategy_ids.iter().any(|id| id == strategy_id) {
        return Err(invalid_report(
            "策略诊断 strategy_id 不属于声明的标的策略槽",
        ));
    }
    let identity = json!({
        "strategy_id": strategy_id,
        "instrument": instrument,
        "data_fingerprint": market_view.data_fingerprint,
    });
    bind_strategy_diagnostics(report, &identity)
}

fn bind_multi_market_diagnostics(
    reports: &Value,
    markets: &[MarketResearchView],
) -> Result<Vec<BoundStrategyDiagnostics>, AppError> {
    let reports = reports
        .as_array()
        .filter(|reports| !reports.is_empty() && reports.len() <= MAX_MULTI_MARKET_DIAGNOSTICS)
        .ok_or_else(|| {
            invalid_report(format!(
                "market_strategy_diagnostics 必须包含 1..={MAX_MULTI_MARKET_DIAGNOSTICS} 份报告"
            ))
        })?;
    let encoded = serde_json::to_vec(reports)
        .map_err(|error| invalid_report(format!("多市场策略诊断报告无法编码: {error}")))?;
    if encoded.len() > MAX_DIAGNOSTIC_BYTES {
        return Err(invalid_report("多市场策略诊断集合超过 1 MiB 上限"));
    }
    let mut identities = std::collections::BTreeSet::new();
    reports
        .iter()
        .map(|report| {
            let strategy_id = report
                .get("strategy_id")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid_report("多市场策略诊断缺少 strategy_id"))?;
            let instrument = report
                .get("instrument")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid_report("多市场策略诊断缺少 instrument"))?;
            if !identities.insert((instrument, strategy_id)) {
                return Err(invalid_report("多市场策略诊断集合包含重复策略/标的身份"));
            }
            bind_multi_market_diagnostic(report, markets)
        })
        .collect()
}

fn bind_multi_market_depth_diagnostics(
    reports: &Value,
    markets: &[qx_app::MultiMarketDepthResearchMarketV1],
    custom_strategies: &[qx_app::StrategyArtifactIdentity],
) -> Result<Vec<BoundStrategyDiagnostics>, AppError> {
    let reports = reports
        .as_array()
        .filter(|reports| !reports.is_empty() && reports.len() <= MAX_MULTI_MARKET_DIAGNOSTICS)
        .ok_or_else(|| {
            invalid_report(format!(
                "market_strategy_diagnostics 必须包含 1..={MAX_MULTI_MARKET_DIAGNOSTICS} 份报告"
            ))
        })?;
    let encoded = serde_json::to_vec(reports)
        .map_err(|error| invalid_report(format!("多市场深度策略诊断报告无法编码: {error}")))?;
    if encoded.len() > MAX_DIAGNOSTIC_BYTES {
        return Err(invalid_report("多市场深度策略诊断集合超过 1 MiB 上限"));
    }
    let mut identities = std::collections::BTreeSet::new();
    reports
        .iter()
        .map(|report| {
            let strategy_id = report
                .get("strategy_id")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid_report("多市场深度策略诊断缺少 strategy_id"))?;
            let instrument = report
                .get("instrument")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid_report("多市场深度策略诊断缺少 instrument"))?;
            if !identities.insert((instrument, strategy_id)) {
                return Err(invalid_report(
                    "多市场深度策略诊断集合包含重复策略/标的身份",
                ));
            }
            let market = markets
                .iter()
                .find(|market| market.instrument == instrument)
                .ok_or_else(|| invalid_report("深度策略诊断标的不存在于已复核输入"))?;
            if !market.strategy_ids.iter().any(|id| id == strategy_id) {
                return Err(invalid_report(
                    "深度策略诊断 strategy_id 与该标的策略槽不一致",
                ));
            }
            let is_callback = custom_strategies
                .iter()
                .any(|identity| identity.strategy_id == strategy_id);
            let expected_input_kind = if !is_callback {
                "depth_midpoint_bar"
            } else if market.data_tier == "l1" {
                "tick"
            } else {
                "order_book"
            };
            if report.get("input_kind").and_then(Value::as_str) != Some(expected_input_kind)
                || report.get("input_events").and_then(Value::as_u64) != Some(market.depth.total)
                || report.get("as_of").and_then(Value::as_u64) != Some(market.source_end_ts)
                || report.get("input_bars").is_some()
            {
                return Err(invalid_report(
                    "深度策略诊断输入类型或事件数与已复核 DepthFrame 不一致",
                ));
            }
            let expected_identity = json!({
                "strategy_id": strategy_id,
                "instrument": instrument,
                "data_fingerprint": market.data_fingerprint,
            });
            bind_strategy_diagnostics(report, &expected_identity)
        })
        .collect()
}
