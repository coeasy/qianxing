use crate::{
    collect_doctor_report_for_config, read_runtime_config, run_paper_pipeline_once_with_report,
};
use qx_runtime::RuntimeSupervisor;
use std::path::Path;

const MAX_PAPER_RISK_AUDIT_ROWS: usize = 100;
const DEFAULT_EVENT_LOG_WARNING_BYTES: u64 = 1024 * 1024 * 1024;

fn event_log_file_bytes(root: &Path, log_name: &str, segmented: bool) -> Result<u64, String> {
    let mut total = 0_u64;
    let mut add_file = |path: &Path| -> Result<(), String> {
        let metadata = match std::fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_file() => metadata,
            Ok(_) => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(format!("读取 {} 大小失败: {error}", path.display())),
        };
        total = total
            .checked_add(metadata.len())
            .ok_or_else(|| "EventLog 文件大小合计溢出".to_string())?;
        Ok(())
    };
    if segmented {
        add_file(&root.join(format!("{log_name}.manifest.json")))?;
        let segments_root = root.join("segments");
        let entries = match std::fs::read_dir(&segments_root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(total),
            Err(error) => {
                return Err(format!(
                    "读取 {} 目录失败: {error}",
                    segments_root.display()
                ));
            }
        };
        let prefix = format!("{log_name}-");
        for entry in entries {
            let entry = entry.map_err(|error| format!("读取 EventLog 分段目录项失败: {error}"))?;
            let path = entry.path();
            let name = entry.file_name();
            if name.to_string_lossy().starts_with(&prefix)
                && path.extension().and_then(|extension| extension.to_str()) == Some("jsonl")
            {
                add_file(&path)?;
            }
        }
    } else {
        add_file(&root.join(format!("{log_name}.json")))?;
    }
    Ok(total)
}

fn paper_execution_model() -> serde_json::Value {
    serde_json::json!({
        "id": "paper_l1_displayed_depth_touch_fifo_v1",
        "name": "L1 displayed-depth touch, FIFO by client order id",
        "price_rule": "buy at best ask; sell at best bid",
        "quantity_rule": "fill up to remaining order quantity and displayed size",
        "limitations": [
            "does not simulate hidden liquidity or queue position",
            "does not model exchange matching latency or market impact",
            "is not the same fill model as Bar/L1/L2 backtest",
        ],
    })
}

fn paper_risk_audit(path: &Path, config: &qx_runtime::RuntimeConfig) -> serde_json::Value {
    let storage_root =
        crate::path_resolution::effective_storage_root(path, &config.storage.data_dir);
    let accounts = match qx_runtime::configured_account_event_logs(config) {
        Ok(accounts) => accounts,
        Err(error) => {
            return serde_json::json!({"status":"unavailable","error":error});
        }
    };
    let paper_accounts = accounts
        .into_iter()
        .filter(|(_, venue_id, _, _)| {
            qx_core::VenueFamily::parse_option(Some(venue_id)) == Some(qx_core::VenueFamily::Paper)
        })
        .collect::<Vec<_>>();
    let mut logs_scanned = 0_usize;
    let mut decisions_total = 0_usize;
    let mut allowed = 0_usize;
    let mut rejected = 0_usize;
    let mut storage_bytes = 0_u64;
    let mut recent = Vec::new();
    let mut unavailable = Vec::new();

    for (account_id, _venue_id, log_name, currency) in paper_accounts {
        if config.storage.backend == qx_runtime::StorageBackend::Files {
            match event_log_file_bytes(
                &storage_root,
                &log_name,
                config.storage.event_log_segment_events.is_some(),
            ) {
                Ok(bytes) => {
                    storage_bytes = storage_bytes.saturating_add(bytes);
                }
                Err(error) => unavailable.push(format!("{log_name} storage: {error}")),
            }
        }
        let exists = qx_service::pipeline::event_log_exists(config, &storage_root, &log_name);
        match exists {
            Ok(false) => continue,
            Ok(true) => {}
            Err(error) => {
                unavailable.push(format!("{log_name}: {error}"));
                continue;
            }
        }
        let pipeline = match qx_service::pipeline::open_account_pipeline(
            config,
            &storage_root,
            &log_name,
            qx_runtime::OutboxRecovery::ReadOnly,
        ) {
            Ok(pipeline) => pipeline,
            Err(error) => {
                unavailable.push(format!("{log_name}: {error}"));
                continue;
            }
        };
        logs_scanned += 1;
        for event in pipeline.log().events() {
            let qx_core::EventKind::OrderRiskDecision { decision } = &event.kind else {
                continue;
            };
            decisions_total += 1;
            if decision.allowed {
                allowed += 1;
            } else {
                rejected += 1;
            }
            recent.push(serde_json::json!({
                "account_id": account_id,
                "event_log": log_name,
                "event_seq": event.seq,
                "timestamp_ms": event.ts,
                "client_order_id": decision.client_order_id,
                "instrument": decision.instrument.to_string(),
                "allowed": decision.allowed,
                "violations": decision.violations,
                "projected_position_raw": decision.projected_position_raw.map(|value| value.to_string()),
                "projected_margin_raw": decision.projected_margin_raw.map(|value| value.to_string()),
                "reference_price_raw": decision.reference_price_raw.map(|value| value.to_string()),
                "rule_set_version": decision.rule_set_version,
                "reporting_currency": currency,
            }));
        }
    }
    recent.sort_by(|left, right| {
        left.get("timestamp_ms")
            .and_then(serde_json::Value::as_u64)
            .cmp(
                &right
                    .get("timestamp_ms")
                    .and_then(serde_json::Value::as_u64),
            )
            .then_with(|| {
                left.get("event_seq")
                    .and_then(serde_json::Value::as_u64)
                    .cmp(&right.get("event_seq").and_then(serde_json::Value::as_u64))
            })
    });
    let truncated = recent.len().saturating_sub(MAX_PAPER_RISK_AUDIT_ROWS);
    if truncated > 0 {
        recent.drain(..truncated);
    }
    let status = if !unavailable.is_empty() {
        "partial"
    } else if decisions_total == 0 {
        "empty"
    } else {
        "available"
    };
    let warning_threshold_bytes = std::env::var("QIANXING_EVENT_LOG_WARN_BYTES")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_EVENT_LOG_WARNING_BYTES);
    let storage_observation = if config.storage.backend != qx_runtime::StorageBackend::Files {
        serde_json::json!({
            "status": "not_measured",
            "backend": format!("{:?}", config.storage.backend).to_ascii_lowercase(),
            "detail": "SQLite/PostgreSQL 文件或卷大小由数据库运维监控；本地 Paper 检查不估算数据库容量。",
        })
    } else {
        serde_json::json!({
            "status": if storage_bytes >= warning_threshold_bytes { "warning" } else { "observed" },
            "backend": "files",
            "paper_account_event_log_bytes": storage_bytes,
            "warning_threshold_bytes": warning_threshold_bytes,
            "threshold_environment_variable": "QIANXING_EVENT_LOG_WARN_BYTES",
            "action": if storage_bytes >= warning_threshold_bytes {
                "EventLog 按追加事实保留；请规划换 log_name、封段归档、备份和磁盘扩容，不要截断活动日志。"
            } else {
                "这是 Paper 账户 EventLog 当前大小快照；本检查不自动轮转或清理日志。"
            },
        })
    };
    serde_json::json!({
        "status": status,
        "scope": "paper_account_event_logs_read_only",
        "logs_scanned": logs_scanned,
        "decisions_total": decisions_total,
        "allowed": allowed,
        "rejected": rejected,
        "recent_limit": MAX_PAPER_RISK_AUDIT_ROWS,
        "truncated_rows": truncated,
        "unavailable_logs": unavailable,
        "storage_observation": storage_observation,
        "recent_decisions": recent,
        "note": "原始定点字段以十进制字符串返回；仅展示已持久化风控裁决，不代表成交或盈亏。",
    })
}

pub(crate) fn collect_paper_check_report(path: &Path) -> Result<serde_json::Value, String> {
    let config = read_runtime_config(path)?;
    let supervisor_build = RuntimeSupervisor::new(config.clone()).map(|_| ());
    let mut report = collect_doctor_report_for_config(path, &config, supervisor_build)?;
    report["paper_risk_audit"] = paper_risk_audit(path, &config);
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
            "execution_model": paper_execution_model(),
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
                "execution_model": paper_execution_model(),
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
                "execution_model": paper_execution_model(),
            });
            report["ok"] = serde_json::json!(false);
            Ok(report)
        }
    }
}
