//! Bounded strategy diagnostics validation, identity binding, and report projection.

use crate::report_readout;
use std::path::Path;

const MAX_STRATEGY_DIAGNOSTICS_BYTES: u64 = 20 * 1024 * 1024;

/// Attach bounded app-backtest strategy diagnostics to the existing interactive research report.
pub(crate) fn write_app_backtest_diagnostic_html(
    summary_path: &Path,
    summary: &serde_json::Value,
    spec: &qx_app::BacktestSpec,
    diagnostics: &serde_json::Value,
) -> Result<(), String> {
    let mut display_summary = summary.clone();
    display_summary["metrics"] = serde_json::json!({
        "return_bps": summary["return_bps"],
        "max_drawdown_bps": summary["max_drawdown_bps"],
        "final_equity_raw": summary["final_equity_raw"],
        "fees_raw": summary["fees_raw"],
        "turnover_raw": summary["turnover_raw"],
    });
    display_summary["account"] = serde_json::json!({
        "initial_cash_raw": spec.initial_cash_raw,
        "currency": spec.settlement_currency,
    });
    display_summary["rejected_orders"] =
        serde_json::Value::from(report_readout::summary_rejected_orders(summary)?);
    display_summary["matching_kernel"] =
        serde_json::Value::String("qx-xingban::BacktestEngine(bar)".into());
    display_summary["risk_rules"] = serde_json::json!({
        "rule_set_version": summary["risk_rule_set_version"],
        "source": "verified RunManifest",
    });
    display_summary["execution_costs"] = serde_json::json!({ "source": summary["fee_model"] });
    display_summary["assumptions"] = serde_json::json!([
        "本地确定性 Bar 回测",
        "策略诊断使用固定初始资金和空持仓上下文",
        "未执行供应商 PIT 可见性验证",
    ]);
    display_summary["strategy_bias_diagnostics"] = diagnostics.clone();
    super::write_research_report_html(summary_path, &display_summary, None)?;
    Ok(())
}

pub(super) fn read_bound_strategy_diagnostics(
    path: &Path,
    summary: &serde_json::Value,
    verified_data_fingerprint: Option<&str>,
) -> Result<serde_json::Value, String> {
    let size = std::fs::metadata(path)
        .map_err(|error| format!("读取策略诊断文件属性失败 {}: {error}", path.display()))?
        .len();
    if size > MAX_STRATEGY_DIAGNOSTICS_BYTES {
        return Err(format!(
            "策略诊断文件超过 {} MiB 上限: {}",
            MAX_STRATEGY_DIAGNOSTICS_BYTES / (1024 * 1024),
            path.display()
        ));
    }
    let payload = std::fs::read_to_string(path)
        .map_err(|error| format!("读取策略诊断文件失败 {}: {error}", path.display()))?;
    let report: serde_json::Value = serde_json::from_str(&payload)
        .map_err(|error| format!("策略诊断 JSON 无效 {}: {error}", path.display()))?;
    let summary_strategy = report_readout::summary_text(summary, "/strategy_id")
        .ok_or("回测摘要缺少可绑定的 strategy_id")?;
    let summary_instrument = report_readout::summary_text(summary, "/instrument")
        .ok_or("回测摘要缺少可绑定的 instrument")?;
    let fingerprint =
        verified_data_fingerprint.ok_or("回测摘要缺少可复核的数据身份，不能绑定策略诊断")?;
    validate_strategy_diagnostics_identity(
        &report,
        &summary_strategy,
        &summary_instrument,
        fingerprint,
    )?;
    Ok(report)
}

pub(crate) fn validate_strategy_diagnostics_identity(
    report: &serde_json::Value,
    strategy_id: &str,
    instrument: &str,
    fingerprint: &str,
) -> Result<(), String> {
    if report
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
        != Some(1)
        || report.get("kind").and_then(serde_json::Value::as_str)
            != Some("qianxing-strategy-bias-diagnostics")
    {
        return Err("策略诊断报告须为 qianxing-strategy-bias-diagnostics v1".into());
    }
    if report
        .get("strategy_id")
        .and_then(serde_json::Value::as_str)
        != Some(strategy_id)
    {
        return Err("策略诊断 strategy_id 与回测策略槽不一致".into());
    }
    if report.get("instrument").and_then(serde_json::Value::as_str) != Some(instrument) {
        return Err("策略诊断 instrument 与回测标的不一致".into());
    }
    if report
        .get("data_fingerprint")
        .and_then(serde_json::Value::as_str)
        != Some(fingerprint)
    {
        return Err("策略诊断 data_fingerprint 与复核后的回测输入不一致".into());
    }
    if report
        .get("as_of")
        .and_then(serde_json::Value::as_u64)
        .is_none()
    {
        return Err("策略诊断缺少有效 as_of".into());
    }
    for (pointer, allowed) in [
        (
            "/lookahead/status",
            &[
                "potential_lookahead_detected",
                "no_change_detected",
                "insufficient_history",
                "insufficient_work_budget",
                "causal_event_contract",
            ][..],
        ),
        (
            "/startup/status",
            &[
                "startup_sensitive",
                "stable_across_requested_windows",
                "insufficient_reference_history",
                "insufficient_history",
            ][..],
        ),
    ] {
        let status = report.pointer(pointer).and_then(serde_json::Value::as_str);
        if !status.is_some_and(|value| allowed.contains(&value)) {
            return Err(format!("策略诊断报告缺少有效 {pointer}"));
        }
    }
    if report.get("data_visibility").is_some() {
        let status = report
            .pointer("/data_visibility/status")
            .and_then(serde_json::Value::as_str);
        if !status.is_some_and(|value| {
            [
                "potential_pit_bias_detected",
                "visibility_unknown",
                "no_pit_bias_detected",
            ]
            .contains(&value)
        }) {
            return Err("策略诊断报告缺少有效 /data_visibility/status".into());
        }
    }
    Ok(())
}
