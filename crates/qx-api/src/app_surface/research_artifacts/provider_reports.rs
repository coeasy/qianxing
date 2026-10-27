//! Provider Bar/Depth envelopes routed through the shared rooted research report reader.

use super::*;

pub(super) fn read_bar_report(
    body: &str,
    paths: &AppPaths,
) -> Result<qx_app::ProviderBacktestReportV1, AppError> {
    let provider: qx_app::ProviderBacktestOutcome =
        serde_json::from_str(body).map_err(|error| {
            AppError::new(
                AppErrorCategory::InvalidInput,
                format!("ProviderBacktestOutcome JSON 无效: {error}"),
            )
        })?;
    verify_bar_report(provider, paths).map(|(_, report, _)| report)
}

pub(super) fn read_depth_report(
    body: &str,
    paths: &AppPaths,
) -> Result<qx_app::ProviderDepthBacktestReportV1, AppError> {
    let provider: qx_app::ProviderDepthBacktestOutcome =
        serde_json::from_str(body).map_err(|error| {
            AppError::new(
                AppErrorCategory::InvalidInput,
                format!("ProviderDepthBacktestOutcome JSON 无效: {error}"),
            )
        })?;
    verify_depth_report(provider, paths).map(|(_, report, _)| report)
}

pub(super) fn build(
    request: &Value,
    outcome_value: &Value,
    paths: &AppPaths,
) -> Result<Option<ResearchArtifactView>, AppError> {
    let Some(object) = outcome_value.as_object() else {
        return Ok(None);
    };
    if object.contains_key("manifest") && object.contains_key("dataset_verdict") {
        return build_bar(request, outcome_value, paths).map(Some);
    }
    if object.contains_key("provider_id") && object.contains_key("depth_path") {
        return build_depth(request, outcome_value, paths).map(Some);
    }
    Ok(None)
}

fn build_bar(
    request: &Value,
    outcome_value: &Value,
    paths: &AppPaths,
) -> Result<ResearchArtifactView, AppError> {
    let provider: qx_app::ProviderBacktestOutcome =
        serde_json::from_value(outcome_value.clone())
            .map_err(|error| invalid_report(format!("Provider Bar 结果结构无效: {error}")))?;
    let (provider, report, root) = verify_bar_report(provider, paths)?;
    let metadata = json!({
        "kind": "bar",
        "dataset_verdict": report.dataset_verdict,
        "dataset": {
            "dataset_id": report.manifest.dataset.dataset_id,
            "version": report.manifest.dataset.version,
            "fingerprint": report.manifest.dataset.fingerprint,
            "provider_id": report.manifest.source_lineage.primary_source,
            "provider_version": report.manifest.provider_version,
            "content_hash": report.manifest.content_hash,
            "instrument": report.manifest.instrument,
            "timezone": report.manifest.timezone,
            "tier": report.manifest.tier,
            "quality_report": report.manifest.quality_report,
        }
    });
    render_inner_report(
        request,
        &provider.outcome,
        report.report,
        metadata,
        "provider_bar",
        &root,
        paths,
    )
}

fn verify_bar_report(
    mut provider: qx_app::ProviderBacktestOutcome,
    paths: &AppPaths,
) -> Result<
    (
        qx_app::ProviderBacktestOutcome,
        qx_app::ProviderBacktestReportV1,
        PathBuf,
    ),
    AppError,
> {
    let root = std::fs::canonicalize(&paths.artifact_root)
        .map_err(|error| AppError::from_io("解析 API Provider 产物根目录", &error))?;
    let reader = RootedArtifactReader { root: root.clone() };
    root_bar_artifacts(&mut provider.outcome, &root)?;
    provider.manifest_path = rooted_file(&provider.manifest_path, &root)?;

    let context = RunContext::new(CallerCapability::Research, provider.outcome.run_id.clone());
    let report =
        qx_app::read_provider_backtest_report_v1_with_reader(&provider, &context, &reader)?;
    Ok((provider, report, root))
}

fn build_depth(
    request: &Value,
    outcome_value: &Value,
    paths: &AppPaths,
) -> Result<ResearchArtifactView, AppError> {
    let provider: qx_app::ProviderDepthBacktestOutcome =
        serde_json::from_value(outcome_value.clone())
            .map_err(|error| invalid_report(format!("Provider 深度结果结构无效: {error}")))?;
    let (provider, report, root) = verify_depth_report(provider, paths)?;
    let metadata = json!({
        "kind": "depth",
        "provider_id": report.provider_id,
        "provider_version": report.provider_version,
        "request_id": report.request_id,
        "retry_chain": report.retry_chain,
        "source_hash": report.source_hash,
        "query_digest": report.query_digest,
        "depth_input_hash": report.depth_input_hash,
    });
    render_inner_report(
        request,
        &provider.outcome,
        report.report,
        metadata,
        "provider_depth",
        &root,
        paths,
    )
}

fn verify_depth_report(
    mut provider: qx_app::ProviderDepthBacktestOutcome,
    paths: &AppPaths,
) -> Result<
    (
        qx_app::ProviderDepthBacktestOutcome,
        qx_app::ProviderDepthBacktestReportV1,
        PathBuf,
    ),
    AppError,
> {
    let root = std::fs::canonicalize(&paths.artifact_root)
        .map_err(|error| AppError::from_io("解析 API Provider 产物根目录", &error))?;
    let reader = RootedArtifactReader { root: root.clone() };
    root_depth_artifacts(&mut provider.outcome, &root)?;
    provider.depth_path = rooted_file(&provider.depth_path, &root)?;

    let context = RunContext::new(CallerCapability::Research, provider.outcome.run_id.clone());
    let report = qx_app::read_provider_depth_backtest_report_v1_with_readers(
        &provider, &context, &reader, &reader,
    )?;
    Ok((provider, report, root))
}

fn root_bar_artifacts(outcome: &mut qx_app::BacktestOutcome, root: &Path) -> Result<(), AppError> {
    outcome.artifacts.run_manifest = rooted_file(&outcome.artifacts.run_manifest, root)?;
    outcome.artifacts.summary = rooted_file(&outcome.artifacts.summary, root)?;
    outcome.artifacts.equity = rooted_file(&outcome.artifacts.equity, root)?;
    outcome.artifacts.fills = rooted_file(&outcome.artifacts.fills, root)?;
    if let Some(path) = outcome.artifacts.event_trace.as_deref() {
        outcome.artifacts.event_trace = Some(rooted_file(path, root)?);
    }
    Ok(())
}

fn root_depth_artifacts(
    outcome: &mut qx_app::DepthBacktestOutcome,
    root: &Path,
) -> Result<(), AppError> {
    outcome.artifacts.run_manifest = rooted_file(&outcome.artifacts.run_manifest, root)?;
    outcome.artifacts.summary = rooted_file(&outcome.artifacts.summary, root)?;
    outcome.artifacts.equity = rooted_file(&outcome.artifacts.equity, root)?;
    outcome.artifacts.fills = rooted_file(&outcome.artifacts.fills, root)?;
    if let Some(path) = outcome.artifacts.event_trace.as_deref() {
        outcome.artifacts.event_trace = Some(rooted_file(path, root)?);
    }
    Ok(())
}

fn render_inner_report(
    request: &Value,
    outcome: &impl serde::Serialize,
    core: qx_app::ResearchRunReportV1,
    provider_metadata: Value,
    run_kind: &'static str,
    provider_input_root: &Path,
    paths: &AppPaths,
) -> Result<ResearchArtifactView, AppError> {
    let mut normalized_request = request.clone();
    normalized_request
        .as_object_mut()
        .ok_or_else(|| invalid_report("研究报告请求必须是 JSON 对象"))?
        .insert(
            "outcome".into(),
            serde_json::to_value(outcome).map_err(|error| {
                AppError::new(
                    AppErrorCategory::InternalInvariant,
                    format!("Provider 内层结果编码失败: {error}"),
                )
            })?,
        );
    let body = serde_json::to_string(&normalized_request).map_err(|error| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            format!("Provider 研究报告请求编码失败: {error}"),
        )
    })?;
    let mut provider_paths = paths.clone();
    // Provider snapshots are frozen beside this run's artifacts, under artifact_root. Keep the
    // shared preview reader's path authorization narrow while allowing it to inspect that input.
    provider_paths.data_root = provider_input_root.to_path_buf();
    let mut view = super::build_with_preverified_core(&body, &provider_paths, Some(core))?;
    view.run_kind = Some(run_kind);
    view.summary
        .as_object_mut()
        .ok_or_else(|| invalid_report("Provider 研究摘要必须为 JSON 对象"))?
        .insert("provider".into(), provider_metadata);
    Ok(view)
}
