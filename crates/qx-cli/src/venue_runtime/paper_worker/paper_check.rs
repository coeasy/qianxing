use crate::{
    collect_doctor_report_for_config, read_runtime_config, run_paper_pipeline_once_with_report,
};
use qx_runtime::RuntimeSupervisor;
use std::path::Path;

pub(crate) fn collect_paper_check_report(path: &Path) -> Result<serde_json::Value, String> {
    let config = read_runtime_config(path)?;
    let supervisor_build = RuntimeSupervisor::new(config.clone()).map(|_| ());
    let mut report = collect_doctor_report_for_config(path, &config, supervisor_build)?;
    if report.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
        report["readiness_layers"]["P"] = serde_json::json!({
            "status": "blocked",
            "scope": "paper_execution",
            "evidence": ["static_diagnostics"],
            "detail": "共享静态诊断未通过，未启动 Paper 验收",
        });
        report["paper_check"] = serde_json::json!({
            "status": "blocked",
            "error": "static_diagnostics_unavailable",
        });
        return Ok(report);
    }

    match run_paper_pipeline_once_with_report(path, false) {
        Ok(outcome) => {
            let status = if outcome.new_orders > 0 {
                "pass"
            } else {
                "not_assessed"
            };
            let detail = if outcome.new_orders > 0 {
                "本轮 Paper 调度、策略、执行和 Ledger 链路产生了新订单"
            } else {
                "当日调度复用了既有事实，本轮未重新执行 Paper 订单链路"
            };
            report["readiness_layers"]["P"] = serde_json::json!({
                "status": status,
                "scope": "paper_execution",
                "evidence": if outcome.new_orders > 0 {
                    vec![
                        "paper_scheduler",
                        "paper_strategy",
                        "paper_execution",
                        "paper_ledger",
                        "paper_command_audit",
                    ]
                } else {
                    vec!["paper_scheduler_idempotent_replay"]
                },
                "detail": detail,
            });
            report["paper_check"] = serde_json::json!({
                "status": status,
                "scheduler_worker": outcome.scheduler_id,
                "strategy_workers": outcome.strategy_ids,
                "execution_worker": outcome.execution_id,
                "orders_total": outcome.orders_total,
                "new_orders": outcome.new_orders,
                "ledger_entries_total": outcome.ledger_entries_total,
                "new_ledger_entries": outcome.new_ledger_entries,
                "detail": detail,
            });
            Ok(report)
        }
        Err(error) => {
            report["readiness_layers"]["P"] = serde_json::json!({
                "status": "blocked",
                "scope": "paper_execution",
                "evidence": ["paper_pipeline"],
                "detail": error,
            });
            report["paper_check"] = serde_json::json!({
                "status": "blocked",
                "error": error.clone(),
            });
            report["ok"] = serde_json::json!(false);
            Ok(report)
        }
    }
}
