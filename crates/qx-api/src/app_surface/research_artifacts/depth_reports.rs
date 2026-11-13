//! Depth report verification and bounded projection.

use super::*;

pub(super) fn build(
    outcome_value: &Value,
    paths: &AppPaths,
    requested_page: Option<EventPage>,
    preverified_core: Option<qx_app::ResearchRunReportV1>,
) -> Result<ResearchArtifactView, AppError> {
    let encoded = serde_json::to_string(outcome_value).map_err(|error| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            format!("深度回测结果请求编码失败: {error}"),
        )
    })?;
    let mut outcome = qx_app::DepthBacktestOutcome::from_json(&encoded)?;
    let root = std::fs::canonicalize(&paths.artifact_root)
        .map_err(|error| AppError::from_io("解析 API 研究产物根目录", &error))?;
    let artifact_reader = RootedArtifactReader { root: root.clone() };
    outcome.artifacts.run_manifest = rooted_file(&outcome.artifacts.run_manifest, &root)?;
    outcome.artifacts.summary = rooted_file(&outcome.artifacts.summary, &root)?;
    outcome.artifacts.equity = rooted_file(&outcome.artifacts.equity, &root)?;
    outcome.artifacts.fills = rooted_file(&outcome.artifacts.fills, &root)?;
    if let Some(event_trace) = outcome.artifacts.event_trace.as_ref() {
        outcome.artifacts.event_trace = Some(rooted_file(event_trace, &root)?);
    }
    for path in [
        &outcome.artifacts.run_manifest,
        &outcome.artifacts.summary,
        &outcome.artifacts.equity,
        &outcome.artifacts.fills,
    ] {
        check_artifact_size(Path::new(path))?;
    }
    if let Some(event_trace) = outcome.artifacts.event_trace.as_deref() {
        check_artifact_size(Path::new(event_trace))?;
    }
    let declared_summary =
        read_json_with_reader(&artifact_reader, &outcome.artifacts.summary, "深度研究摘要")?;
    ensure_declared_artifact_paths(&declared_summary, &root)?;
    let core = if let Some(core) = preverified_core {
        if core.run_id != outcome.run_id
            || core.instrument != outcome.instrument
            || !core.verification.verified
        {
            return Err(AppError::new(
                AppErrorCategory::Conflict,
                "预复核深度报告与子运行身份不一致",
            ));
        }
        core
    } else {
        qx_app::read_depth_run_report_v1_with_reader(
            &outcome,
            &RunContext::new(CallerCapability::Research, outcome.run_id.clone()),
            &artifact_reader,
        )?
    };
    if core.summary != declared_summary {
        return Err(AppError::new(
            AppErrorCategory::StorageFailure,
            "深度研究摘要在路径校验与公共报告读取之间发生变化",
        ));
    }
    let summary = core.summary;
    let mut verification = core.verification;
    if !verification.verified {
        if let Some(mismatch) = verification.mismatches.iter().find(|mismatch| {
            mismatch.starts_with("RunRecord SHA-256 复核失败")
                || mismatch.starts_with("RunManifest 身份复核失败")
        }) {
            return Err(AppError::new(
                AppErrorCategory::StorageFailure,
                format!("研究报告产物完整性复核失败: {mismatch}"),
            ));
        }
        return Ok(ResearchArtifactView {
            schema_version: 2,
            run_kind: Some("depth"),
            run_id: outcome.run_id,
            instrument: outcome.instrument,
            verification,
            summary,
            equity: into_csv_view(core.equity),
            fills: into_csv_view(core.fills),
            events: empty_event_view(),
            bars: empty_bars("unsupported_input_kind"),
            depth: Some(empty_depth("run_not_verified")),
            event_trace_available: Some(false),
            event_instrument: None,
            strategy_diagnostics: None,
            market_strategy_diagnostics: None,
            markets: None,
            accounts: None,
            account_event_offsets: None,
        });
    }
    let equity = into_csv_view(core.equity);
    let (events, fills) = if let Some(event_trace) = outcome.artifacts.event_trace.as_deref() {
        let (mut events, event_fills) = event_view(
            &artifact_reader,
            event_trace,
            &summary,
            &outcome.artifacts.run_manifest,
            requested_page,
        )?;
        for row in &mut events.rows {
            if let Some(object) = row.as_object_mut() {
                object.insert("bar_context".into(), Value::Null);
                object.insert(
                    "bar_context_status".into(),
                    Value::String("not_applicable_depth".into()),
                );
            }
        }
        let replay_checked_fills =
            validated_fill_view(&artifact_reader, &outcome.artifacts.fills, event_fills)?;
        let fills = into_csv_view(core.fills);
        if fills.total != replay_checked_fills.total || fills.rows != replay_checked_fills.rows {
            return Err(AppError::new(
                AppErrorCategory::StorageFailure,
                "公共深度报告成交样本与 replay-verified 成交产物不一致",
            ));
        }
        verification
            .checks
            .push("深度 EventLog 已完整重放，摘要/RunManifest digest 与成交明细一致".into());
        (events, fills)
    } else {
        let depth_fills = depth_fill_view(&artifact_reader, &outcome.artifacts.fills)?;
        let core_fills = into_csv_view(core.fills);
        if depth_fills.total != core_fills.total || depth_fills.rows != core_fills.rows {
            return Err(AppError::new(
                AppErrorCategory::StorageFailure,
                "公共深度报告成交样本与成交产物不一致",
            ));
        }
        (empty_event_view(), core_fills)
    };
    let depth = raw_depth_view(&summary, &paths.data_root, &root)?;
    Ok(ResearchArtifactView {
        schema_version: 2,
        run_kind: Some("depth"),
        run_id: outcome.run_id,
        instrument: outcome.instrument,
        verification,
        summary,
        equity,
        fills,
        events,
        bars: empty_bars("unsupported_input_kind"),
        depth: Some(depth),
        event_trace_available: Some(outcome.artifacts.event_trace.is_some()),
        event_instrument: None,
        strategy_diagnostics: None,
        market_strategy_diagnostics: None,
        markets: None,
        accounts: None,
        account_event_offsets: None,
    })
}
