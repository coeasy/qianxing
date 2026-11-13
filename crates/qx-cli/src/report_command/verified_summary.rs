//! Verified Bar/Depth summary reconstruction and shared report readback.

use crate::report_readout;
use std::path::Path;

/// Accept supported report schema versions before reconstructing verified summaries.
pub(crate) fn require_current_summary_schema(summary: &serde_json::Value) -> Result<(), String> {
    let migration = "迁移方式：使用原 runtime 与 BarFrame 重新运行 `qx-cli backtest <runtime.json> [<bar-frame.json>]`";
    let schema = report_readout::summary_schema_version(summary)
        .ok_or_else(|| format!("回测摘要缺少有效 schema_version；{migration}"))?;
    let complete_app_run_summary = schema == 1
        && report_readout::summary_text(summary, "/run_id").is_some()
        && report_readout::summary_text(summary, "/instrument").is_some()
        && report_readout::summary_text(summary, "/result_hash").is_some()
        && report_readout::summary_text(summary, "/data_fingerprint").is_some()
        && report_readout::summary_text(summary, "/run_manifest").is_some()
        && report_readout::summary_text(summary, "/run_record").is_some()
        && report_readout::summary_number(summary, "/fills").is_some()
        && report_readout::summary_number(summary, "/equity_points").is_some()
        && report_readout::summary_number(summary, "/return_bps").is_some()
        && report_readout::summary_number(summary, "/max_drawdown_bps").is_some()
        && report_readout::summary_block(summary, "input").is_some()
        && report_readout::summary_block(summary, "replay").is_some()
        && report_readout::summary_block(summary, "artifact_locations").is_some();
    if !(5..=8).contains(&schema) && !complete_app_run_summary {
        return Err(format!(
            "回测摘要 schema_version={schema} 已拒绝：仅接受 v5/v6/v7/v8，或字段完整且可由 qx-app 共享核验的 RunSummary v1；旧摘要不具备可验证的输入身份与重放证据。{migration}，再对新生成的 v8 summary 执行 report。"
        ));
    }
    Ok(())
}

pub(super) fn read_verified_depth_summary(
    summary_path: &Path,
    summary: &serde_json::Value,
) -> Result<qx_app::ResearchRunReportV1, String> {
    let locations = report_readout::summary_block(summary, "artifact_locations")
        .and_then(serde_json::Value::as_object)
        .ok_or("深度摘要缺少 artifact_locations，不能构造共享报告身份")?;
    let declared_summary = locations
        .get("summary")
        .and_then(serde_json::Value::as_str)
        .ok_or("深度摘要缺少 summary 工件定位")?;
    let reader = qx_app::artifact_store::FileArtifactReader;
    if !qx_app::artifact_store::ArtifactReader::equivalent_references(
        &reader,
        &summary_path.to_string_lossy(),
        declared_summary,
    )
    .map_err(|error| error.to_string())?
    {
        return Err("所选深度摘要路径与 artifact_locations 不一致".into());
    }
    let required_path = |name: &str| -> Result<String, String> {
        locations
            .get(name)
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_owned)
            .ok_or_else(|| format!("深度摘要缺少 {name} 工件定位"))
    };
    let run_manifest = required_path("run_manifest")?;
    let event_trace = report_readout::summary_text(summary, "/event_trace").or_else(|| {
        let path = Path::new(&run_manifest);
        let name = path.file_name()?.to_str()?.strip_suffix(".run.json")?;
        Some(
            path.with_file_name(format!("{name}.events.jsonl"))
                .to_string_lossy()
                .into_owned(),
        )
    });
    let outcome = qx_app::DepthBacktestOutcome {
        run_id: required_summary_text(summary, "run_id")?,
        manifest_run_id: report_readout::summary_text(summary, "/manifest_run_id")
            .filter(|value| !value.trim().is_empty()),
        instrument: required_summary_text(summary, "instrument")?,
        tier: required_summary_text(summary, "tier")?,
        result_hash: required_summary_text(summary, "result_hash")?,
        data_fingerprint: required_summary_text(summary, "data_fingerprint")?,
        fills: required_summary_u64(summary, "fills")?,
        equity_points: required_summary_u64(summary, "equity_points")?,
        snapshots: required_summary_u64(summary, "bars")?,
        return_bps: i32::try_from(
            report_readout::summary_number(summary, "/return_bps")
                .ok_or("深度摘要缺少有效 return_bps")?,
        )
        .map_err(|_| "深度摘要 return_bps 超出 i32 范围")?,
        max_drawdown_bps: u32::try_from(required_summary_u64(summary, "max_drawdown_bps")?)
            .map_err(|_| "深度摘要 max_drawdown_bps 超出 u32 范围")?,
        artifacts: qx_app::BacktestArtifacts {
            run_manifest,
            summary: summary_path.to_string_lossy().into_owned(),
            equity: required_path("equity")?,
            fills: required_path("fills")?,
            event_trace,
        },
        manifest_digest: String::new(),
        account_id: report_readout::summary_text(summary, "/account_id"),
        display_summary: summary.clone(),
    };
    let context =
        qx_app::RunContext::new(qx_app::CallerCapability::Research, outcome.run_id.clone());
    let report = qx_app::read_depth_run_report_v1_with_reader(&outcome, &context, &reader)
        .map_err(|error| error.to_string())?;
    if !report.verification.verified {
        return Err(format!(
            "深度研究报告复核失败: {}",
            report.verification.mismatches.join("；")
        ));
    }
    if report.summary != *summary {
        return Err("深度摘要在共享报告复核期间发生变化".into());
    }
    Ok(report)
}

pub(super) fn read_verified_bar_summary(
    summary_path: &Path,
    summary: &serde_json::Value,
) -> Result<qx_app::ResearchRunReportV1, String> {
    let locations = report_readout::summary_block(summary, "artifact_locations")
        .and_then(serde_json::Value::as_object)
        .ok_or("Bar 摘要缺少 artifact_locations，不能构造共享报告身份")?;
    let declared_summary = locations
        .get("summary")
        .and_then(serde_json::Value::as_str)
        .ok_or("Bar 摘要缺少 summary 工件定位")?;
    let reader = qx_app::artifact_store::FileArtifactReader;
    if !qx_app::artifact_store::ArtifactReader::equivalent_references(
        &reader,
        &summary_path.to_string_lossy(),
        declared_summary,
    )
    .map_err(|error| error.to_string())?
    {
        return Err("所选 Bar 摘要路径与 artifact_locations 不一致".into());
    }
    let required_path = |name: &str| -> Result<String, String> {
        locations
            .get(name)
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_owned)
            .ok_or_else(|| format!("Bar 摘要缺少 {name} 工件定位"))
    };
    let outcome = qx_app::BacktestOutcome {
        run_id: required_summary_text(summary, "run_id")?,
        instrument: required_summary_text(summary, "instrument")?,
        result_hash: required_summary_text(summary, "result_hash")?,
        data_fingerprint: required_summary_text(summary, "data_fingerprint")?,
        fills: required_summary_u64(summary, "fills")?,
        equity_points: required_summary_u64(summary, "equity_points")?,
        return_bps: i32::try_from(
            report_readout::summary_number(summary, "/return_bps")
                .ok_or("Bar 摘要缺少有效 return_bps")?,
        )
        .map_err(|_| "Bar 摘要 return_bps 超出 i32 范围")?,
        max_drawdown_bps: u32::try_from(required_summary_u64(summary, "max_drawdown_bps")?)
            .map_err(|_| "Bar 摘要 max_drawdown_bps 超出 u32 范围")?,
        artifacts: qx_app::BacktestArtifacts {
            run_manifest: required_path("run_manifest")?,
            summary: summary_path.to_string_lossy().into_owned(),
            equity: required_path("equity")?,
            fills: required_path("fills")?,
            event_trace: report_readout::summary_text(summary, "/event_trace"),
        },
        account_id: report_readout::summary_text(summary, "/account_id"),
    };
    let context =
        qx_app::RunContext::new(qx_app::CallerCapability::Research, outcome.run_id.clone());
    let report = qx_app::read_run_report_v1_with_reader(&outcome, &context, &reader)
        .map_err(|error| error.to_string())?;
    if !report.verification.verified {
        return Err(format!(
            "Bar 研究报告复核失败: {}",
            report.verification.mismatches.join("；")
        ));
    }
    if report.summary != *summary {
        return Err("Bar 摘要在共享报告复核期间发生变化".into());
    }
    Ok(report)
}

pub(super) fn required_summary_text(
    summary: &serde_json::Value,
    key: &str,
) -> Result<String, String> {
    report_readout::summary_text(summary, &format!("/{key}"))
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("深度摘要缺少有效 {key}"))
}

pub(super) fn required_summary_u64(summary: &serde_json::Value, key: &str) -> Result<u64, String> {
    let value = report_readout::summary_number(summary, &format!("/{key}"))
        .ok_or_else(|| format!("深度摘要缺少有效 {key}"))?;
    u64::try_from(value).map_err(|_| format!("深度摘要 {key} 超出 u64 范围"))
}
