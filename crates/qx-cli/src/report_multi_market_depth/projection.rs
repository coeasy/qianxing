//! Verified JSON report projection for persisted multi-market depth artifacts.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub(super) fn market_instruments(report: &Value) -> Result<Vec<Value>, String> {
    report
        .get("markets")
        .and_then(Value::as_array)
        .ok_or_else(|| "多市场深度报告缺少 markets 列表".to_string())?
        .iter()
        .map(|market| {
            market
                .get("instrument")
                .cloned()
                .ok_or_else(|| "多市场深度报告的市场条目缺少 instrument".to_string())
        })
        .collect()
}

pub(super) fn build_report(
    path: &Path,
    artifact: &Value,
    include_verified_benchmark_equity: bool,
) -> Result<Value, String> {
    let outcome = serde_json::from_value::<qx_app::MultiMarketDepthBacktestOutcome>(
        artifact
            .get("outcome")
            .cloned()
            .ok_or("多市场深度产物缺少 outcome")?,
    )
    .map_err(|error| format!("多市场深度 outcome 无效: {error}"))?;
    let request = qx_app::MultiMarketDepthResearchReportReadRequestV1 {
        schema_version: qx_app::MULTI_MARKET_DEPTH_RESEARCH_REPORT_SCHEMA_VERSION_V1,
        outcome: outcome.clone(),
        page: None,
    };
    let context =
        qx_app::RunContext::new(qx_app::CallerCapability::Research, outcome.run_id.clone());
    // Bind the outcome's canonical locator to the exact file selected by the CLI. This keeps
    // edited copies from silently falling back to the original run artifact during verification.
    let selected_artifact_reader = SelectedDepthArtifactReader {
        selected_reference: outcome.artifact_path.clone(),
        selected_path: path.to_path_buf(),
        fallback: qx_app::artifact_store::FileArtifactReader,
    };
    let report = qx_app::read_multi_market_depth_research_report_v1_with_readers(
        &request,
        &context,
        &selected_artifact_reader,
        &qx_app::artifact_store::FileArtifactReader,
    )
    .map_err(|error| error.to_string())?;
    let summary = report.summary;
    let equity = report.equity;
    let verified_equity = report.verified_equity;
    let metrics = summary
        .get("metrics")
        .ok_or("多市场深度应用报告缺少 metrics")?;
    let markets = report
        .markets
        .into_iter()
        .map(|market| {
            json!({
                "instrument": market.instrument,
                "data_tier": market.data_tier,
                "source": market.source,
                "content_hash": market.content_hash,
                "data_fingerprint": market.data_fingerprint,
                "snapshot_count": market.depth.total,
                "source_end_ts": market.source_end_ts,
                "fills": market.fill_count,
                "quality_status": market.quality_status,
                "dataset_quality": market.dataset_quality,
                "depth": market.depth,
            })
        })
        .collect::<Vec<_>>();
    let mut projection = json!({
        "run_id": report.run_id,
        "account_id": report.account_id,
        "result_hash": summary["result_hash"],
        "data_fingerprint": summary["data_fingerprint"],
        "markets_total": summary["markets_total"],
        "successful_fills": summary["successful_fills"],
        "pending_orders": summary["pending_orders"],
        "equity_points": summary["equity_points"],
        "final_equity_raw": metrics["final_equity_raw"],
        "return_bps": metrics["return_bps"],
        "max_drawdown_bps": metrics["max_drawdown_bps"],
        "metrics_definition": qx_app::artifact_store::backtest_metrics_definition_json(),
        "metrics_exact": summary["metrics_exact"],
        "equity": equity,
        "verification": report.verification,
        "summary_path": path,
        "markets": markets
    });
    if include_verified_benchmark_equity {
        projection["verified_benchmark_equity"] = json!(verified_equity);
    }
    Ok(projection)
}

struct SelectedDepthArtifactReader {
    selected_reference: String,
    selected_path: PathBuf,
    fallback: qx_app::artifact_store::FileArtifactReader,
}

impl qx_app::artifact_store::ArtifactReader for SelectedDepthArtifactReader {
    fn open(
        &self,
        reference: &str,
    ) -> Result<qx_app::artifact_store::ArtifactStream, qx_app::AppError> {
        if reference == self.selected_reference {
            self.fallback.open(&self.selected_path.to_string_lossy())
        } else {
            self.fallback.open(reference)
        }
    }

    fn equivalent_references(&self, left: &str, right: &str) -> Result<bool, qx_app::AppError> {
        self.fallback.equivalent_references(left, right)
    }
}
